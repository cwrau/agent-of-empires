//! Core event bus: durable topic-keyed sequence log plus best-effort live
//! broadcast plus replay-from-seq, generalizing the pattern proven by the ACP
//! event store (`src/acp/event_store.rs`).
//!
//! The bus is publication only. Writes flow through core services (storage
//! mutators, tmux operations) which then publish post-mutation facts; the bus
//! never enforces invariants and is never the mutation authority. Plugins
//! subscribe over JSON-RPC (`events.subscribe { topics, after_seq }`) and
//! publish under their own `plugin.<id>.*` namespace through a
//! capability-checked `events.publish`.
//!
//! High-volume data (pane snapshots) never enters the durable log; only
//! semantic facts (`status.changed`, `session.created`) do.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Well-known core topics. Plugins publish under `plugin.<id>.<event>`.
pub mod topics {
    pub const SESSION_CREATED: &str = "session.created";
    pub const SESSION_DELETED: &str = "session.deleted";
    pub const STATUS_CHANGED: &str = "status.changed";
    pub const SESSION_META_CHANGED: &str = "session.meta.changed";
}

/// One published fact: a global monotonically increasing `seq`, a dotted
/// topic, and an arbitrary JSON payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusEvent {
    pub seq: u64,
    pub topic: String,
    pub payload: serde_json::Value,
    /// Unix milliseconds at publication.
    pub published_at: u64,
}

/// Returns true when `topic` matches the subscription `pattern`: either an
/// exact dotted topic or a trailing-`*` prefix wildcard (`plugin.aoe.x.*`).
pub fn topic_matches(pattern: &str, topic: &str) -> bool {
    match pattern.strip_suffix(".*") {
        Some(prefix) => {
            topic == prefix
                || (topic.len() > prefix.len()
                    && topic.starts_with(prefix)
                    && topic.as_bytes()[prefix.len()] == b'.')
        }
        None => pattern == topic,
    }
}

/// The process-wide bus, opened lazily on the app dir's `events.db`.
/// Plugin host APIs and core services publish through this instance.
pub fn global() -> Result<&'static Arc<EventBus>> {
    static GLOBAL: std::sync::OnceLock<Arc<EventBus>> = std::sync::OnceLock::new();
    if let Some(bus) = GLOBAL.get() {
        return Ok(bus);
    }
    let path = crate::session::get_app_dir()?.join("events.db");
    let bus = Arc::new(EventBus::open(&path, DEFAULT_RETENTION_PER_TOPIC)?);
    Ok(GLOBAL.get_or_init(|| bus))
}

/// Durable seq log + live broadcast + replay. One instance per daemon/TUI
/// process, opened on the app dir's `events.db`.
pub struct EventBus {
    conn: Mutex<Connection>,
    next_seq: AtomicU64,
    tx: broadcast::Sender<Arc<BusEvent>>,
    max_events_per_topic: usize,
}

/// Live subscribers more than this many events behind get `Lagged` and are
/// expected to recover via `replay_from`, mirroring the ACP websocket path.
const BROADCAST_CAPACITY: usize = 1024;

/// Default per-topic durable retention.
pub const DEFAULT_RETENTION_PER_TOPIC: usize = 1000;

impl EventBus {
    /// Open (or create) the bus log at `db_path`.
    pub fn open(db_path: &Path, max_events_per_topic: usize) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("opening event bus db at {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::with_connection(conn, max_events_per_topic)
    }

    /// In-memory bus for tests.
    pub fn open_in_memory(max_events_per_topic: usize) -> Result<Self> {
        Self::with_connection(Connection::open_in_memory()?, max_events_per_topic)
    }

    fn with_connection(conn: Connection, max_events_per_topic: usize) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS bus_events (
                seq INTEGER PRIMARY KEY,
                topic TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_bus_events_topic_seq
                ON bus_events(topic, seq);",
        )?;
        let highest: i64 =
            conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM bus_events", [], |row| {
                row.get(0)
            })?;
        let highest = highest as u64;
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Ok(Self {
            conn: Mutex::new(conn),
            next_seq: AtomicU64::new(highest + 1),
            tx,
            max_events_per_topic,
        })
    }

    /// Append a fact to the durable log and broadcast it to live
    /// subscribers. Returns the assigned seq.
    pub fn publish(&self, topic: &str, payload: serde_json::Value) -> Result<u64> {
        let published_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let seq;
        {
            // Allocate the seq under the connection lock and advance the
            // counter only after the row is durable: a failed insert must not
            // move highest_seq() past a row that does not exist, or replay
            // high-water marks would skip it.
            let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
            seq = self.next_seq.load(Ordering::SeqCst);
            conn.execute(
                "INSERT INTO bus_events (seq, topic, payload_json, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![seq as i64, topic, payload.to_string(), published_at as i64],
            )?;
            // Per-topic retention: drop the oldest rows beyond the cap. The
            // insert above is already durable, so a retention failure must
            // not surface as a failed publish (callers would retry and
            // double-publish); it only means extra rows stay until the next
            // successful pass.
            if let Err(e) = conn.execute(
                "DELETE FROM bus_events WHERE topic = ?1 AND seq NOT IN (
                    SELECT seq FROM bus_events WHERE topic = ?1
                    ORDER BY seq DESC LIMIT ?2
                )",
                rusqlite::params![topic, self.max_events_per_topic as i64],
            ) {
                tracing::warn!(target: "events", topic, error = %e, "retention prune failed; rows kept");
            }
            self.next_seq.store(seq + 1, Ordering::SeqCst);
            // Broadcast while still holding the connection lock so live
            // delivery order matches seq-allocation order. Sending after the
            // lock released would let two concurrent publishers race on send,
            // delivering a higher seq first; a caught-up subscriber that drops
            // `seq <= last_seq` would then lose the lower-seq event from the
            // live stream (replay only fires on lag/cold-subscribe). The
            // broadcast send is non-blocking, so holding the lock is cheap.
            let event = Arc::new(BusEvent {
                seq,
                topic: topic.to_string(),
                payload,
                published_at,
            });
            // Send error just means no live subscribers; durability already holds.
            let _ = self.tx.send(event);
        }
        Ok(seq)
    }

    /// Subscribe to the live stream. Callers filter by topic themselves
    /// (subscriber counts are small; fanout filtering belongs to the caller
    /// which knows its grant set).
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<BusEvent>> {
        self.tx.subscribe()
    }

    /// Read back durable events with `seq > after_seq` matching any of
    /// `patterns` (see [`topic_matches`]), oldest first, capped at `limit`.
    pub fn replay_from(
        &self,
        patterns: &[String],
        after_seq: u64,
        limit: usize,
    ) -> Result<Vec<BusEvent>> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = conn.prepare(
            "SELECT seq, topic, payload_json, created_at FROM bus_events
             WHERE seq > ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map([after_seq as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, topic, payload_json, published_at) = row?;
            let (seq, published_at) = (seq as u64, published_at as u64);
            if !patterns.iter().any(|p| topic_matches(p, &topic)) {
                continue;
            }
            out.push(BusEvent {
                seq,
                topic,
                payload: serde_json::from_str(&payload_json).unwrap_or(serde_json::Value::Null),
                published_at,
            });
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Highest seq assigned so far; 0 when the log is empty.
    pub fn highest_seq(&self) -> u64 {
        self.next_seq.load(Ordering::SeqCst) - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_matching_exact_and_wildcard() {
        assert!(topic_matches("status.changed", "status.changed"));
        assert!(!topic_matches("status.changed", "status.changed.extra"));
        assert!(topic_matches("plugin.aoe.x.*", "plugin.aoe.x.tick"));
        assert!(topic_matches("plugin.aoe.x.*", "plugin.aoe.x"));
        assert!(!topic_matches("plugin.aoe.x.*", "plugin.aoe.xy.tick"));
    }

    #[test]
    fn publish_replay_round_trip() {
        let bus = EventBus::open_in_memory(100).unwrap();
        let s1 = bus
            .publish(topics::STATUS_CHANGED, serde_json::json!({"id": "a"}))
            .unwrap();
        let s2 = bus
            .publish(topics::SESSION_CREATED, serde_json::json!({"id": "b"}))
            .unwrap();
        assert!(s2 > s1);
        assert_eq!(bus.highest_seq(), s2);

        let all = bus
            .replay_from(&["status.changed".into(), "session.created".into()], 0, 10)
            .unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].payload["id"], "a");

        let after = bus
            .replay_from(&["session.created".into()], s1, 10)
            .unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].topic, "session.created");
    }

    #[test]
    fn live_subscribers_receive_published_events() {
        let bus = EventBus::open_in_memory(100).unwrap();
        let mut rx = bus.subscribe();
        bus.publish("plugin.aoe.x.tick", serde_json::json!({"n": 1}))
            .unwrap();
        let got = rx.try_recv().unwrap();
        assert_eq!(got.topic, "plugin.aoe.x.tick");
        assert_eq!(got.payload["n"], 1);
    }

    #[test]
    fn per_topic_retention_caps_durable_log() {
        let bus = EventBus::open_in_memory(2).unwrap();
        for n in 0..5 {
            bus.publish("a.topic", serde_json::json!({ "n": n }))
                .unwrap();
        }
        bus.publish("b.topic", serde_json::json!({})).unwrap();
        let kept = bus.replay_from(&["a.topic".into()], 0, 10).unwrap();
        assert_eq!(kept.len(), 2, "oldest a.topic rows pruned");
        assert_eq!(kept[0].payload["n"], 3);
        // Other topics keep their own budget.
        assert_eq!(
            bus.replay_from(&["b.topic".into()], 0, 10).unwrap().len(),
            1
        );
    }
}
