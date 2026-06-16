//! Plugin-contributed status detection (D7): Tier 0 declarative rules
//! evaluated in-core, Tier 1 batched RPC to the plugin worker.
//!
//! `detect()` is consulted by `detect_status_from_content` BEFORE the builtin
//! per-agent detectors, so an active plugin claiming an agent owns its
//! detection; disabling the plugin falls back to the builtin behavior with no
//! residue. The wildcard agent `"*"` applies only to tools that have no
//! builtin agent entry and no explicit plugin rules, which gives custom
//! `--cmd` agents (previously hardcoded Idle) basic detection.
//!
//! Tier 1 calls are batched per plugin per poll window: snapshots accumulate
//! and one `status.detect_batch` flushes them with per-snapshot result/error
//! isolation, a byte cap per snapshot, and a cached-previous-status fallback
//! when the worker is slow or sick (the supervisor's respawn budget handles a
//! crashing worker).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use aoe_plugin_api::{DetectionMode, DetectionRule, StatusKind};
use serde_json::{json, Value};
use tracing::{debug, warn};

use crate::session::Status;

/// Pane text beyond this is truncated before entering a batch.
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;

/// A cached per-session result is served without a new flush for this long.
const CACHE_FRESH: Duration = Duration::from_millis(900);

/// Per-batch worker deadline; on expiry the cached status answers instead.
const BATCH_TIMEOUT: Duration = Duration::from_millis(800);

struct CompiledRule {
    status: Status,
    priority: i32,
    contains: Vec<String>,
    regex: Option<regex::Regex>,
    default: bool,
}

struct CompiledAgent {
    rules: Vec<CompiledRule>,
}

struct RpcAgent {
    plugin_id: String,
    method: String,
}

/// Everything detection needs, rebuilt when the registry reloads.
#[derive(Default)]
struct Snapshot {
    declarative: HashMap<String, Arc<CompiledAgent>>,
    rpc: HashMap<String, Arc<RpcAgent>>,
    /// The `"*"` contribution and its owning plugin, applied to tools with
    /// no builtin agent entry.
    wildcard: Option<(String, Arc<CompiledAgent>)>,
}

static SNAPSHOT: RwLock<Option<Arc<Snapshot>>> = RwLock::new(None);

/// Drop the compiled snapshot; the next `detect` rebuilds from the current
/// registry. Called by `super::reload_registry`.
pub fn invalidate_cache() {
    *SNAPSHOT.write().expect("status snapshot lock") = None;
    // A disable/enable cycle also resets per-session cached results so a
    // re-enabled plugin starts clean.
    if let Some(batcher) = BATCHER.get() {
        batcher.cache.lock().expect("status cache lock").clear();
    }
}

fn status_from_kind(kind: StatusKind) -> Status {
    match kind {
        StatusKind::Running => Status::Running,
        StatusKind::Waiting => Status::Waiting,
        StatusKind::Idle => Status::Idle,
        StatusKind::Error => Status::Error,
        // StatusKind is #[non_exhaustive]; an unknown future kind maps to Idle.
        _ => Status::Idle,
    }
}

fn compile_rules(rules: &[DetectionRule]) -> CompiledAgent {
    let mut compiled: Vec<CompiledRule> = rules
        .iter()
        .filter_map(|r| {
            let regex = match &r.regex {
                // Compiled once at snapshot build; the regex crate has no
                // backtracking, so hostile pane text cannot blow up matching.
                Some(src) => match regex::Regex::new(src) {
                    Ok(re) => Some(re),
                    Err(e) => {
                        warn!(target: "plugin", regex = %src, "invalid detection regex skipped: {e}");
                        return None;
                    }
                },
                None => None,
            };
            Some(CompiledRule {
                status: status_from_kind(r.status),
                priority: r.priority,
                contains: r.contains.iter().map(|s| s.to_lowercase()).collect(),
                regex,
                default: r.default,
            })
        })
        .collect();
    compiled.sort_by_key(|r| std::cmp::Reverse(r.priority));
    CompiledAgent { rules: compiled }
}

fn build_snapshot() -> Arc<Snapshot> {
    let registry = super::registry();
    let mut snapshot = Snapshot::default();
    for plugin in registry.active() {
        for contribution in &plugin.manifest.status_detection {
            match &contribution.mode {
                DetectionMode::Declarative { rules } => {
                    let compiled = Arc::new(compile_rules(rules));
                    if contribution.agent == "*" {
                        snapshot.wildcard = Some((plugin.id().to_string(), compiled));
                    } else {
                        snapshot
                            .declarative
                            .insert(contribution.agent.clone(), compiled);
                    }
                }
                DetectionMode::Rpc { method } => {
                    snapshot.rpc.insert(
                        contribution.agent.clone(),
                        Arc::new(RpcAgent {
                            plugin_id: plugin.id().to_string(),
                            method: method.clone(),
                        }),
                    );
                }
                // DetectionMode is #[non_exhaustive]; an unknown future mode
                // contributes no detector rather than failing the snapshot.
                _ => {}
            }
        }
    }
    Arc::new(snapshot)
}

fn snapshot() -> Arc<Snapshot> {
    if let Some(snap) = SNAPSHOT.read().expect("status snapshot lock").as_ref() {
        return snap.clone();
    }
    let snap = build_snapshot();
    *SNAPSHOT.write().expect("status snapshot lock") = Some(snap.clone());
    snap
}

impl CompiledAgent {
    fn evaluate(&self, clean_lower: &str) -> Option<Status> {
        for rule in &self.rules {
            if rule.default {
                continue;
            }
            let contains_hit = !rule.contains.is_empty()
                && rule
                    .contains
                    .iter()
                    .all(|n| clean_lower.contains(n.as_str()));
            let regex_hit = rule
                .regex
                .as_ref()
                .is_some_and(|re| re.is_match(clean_lower));
            if contains_hit || regex_hit {
                return Some(rule.status);
            }
        }
        self.rules.iter().find(|r| r.default).map(|r| r.status)
    }
}

/// Plugin detection for one pane snapshot. `session` keys the Tier 1 result
/// cache; callers without a session identity (one-shot content checks) only
/// get the declarative tier. Returns `None` when no active plugin claims the
/// tool, sending the caller to the builtin detector.
pub fn detect(session: Option<&str>, tool: &str, clean: &str) -> Option<Status> {
    let snap = snapshot();
    let clean_lower = clean.to_lowercase();
    if let Some(agent) = snap.declarative.get(tool) {
        return agent.evaluate(&clean_lower);
    }
    if let Some(rpc) = snap.rpc.get(tool) {
        // The plugin OWNS this tool: never fall through to the builtin
        // detector while it is active, or detection flaps between plugin
        // and builtin answers on cold caches and worker hiccups. Idle is
        // the deterministic plugin-owned placeholder until the first batch
        // answers; session-less callers (one-shot content checks) get the
        // same placeholder because there is no cache identity to consult.
        if let Some(session) = session {
            return Some(detect_rpc(rpc, session, tool, clean).unwrap_or(Status::Idle));
        }
        return Some(Status::Idle);
    }
    // Wildcard: only tools with no builtin agent entry, so plugin rules can
    // serve custom agents without shadowing first-party detectors. Gated by
    // the owning plugin's `custom_agent_rules` setting when it declares one.
    if crate::agents::get_agent(tool).is_none() {
        if let Some((plugin_id, agent)) = snap.wildcard.as_ref() {
            let enabled =
                super::settings::resolve(&super::registry(), plugin_id, "custom_agent_rules")
                    .map(|r| r.value.as_bool().unwrap_or(true))
                    .unwrap_or(true);
            if enabled {
                return agent.evaluate(&clean_lower);
            }
        }
    }
    None
}

/// One queued pane snapshot: (session_id, agent, pane_text tail).
type PendingSnapshot = (String, String, String);

struct Batcher {
    /// Per-plugin pending snapshots, flushed as one detect_batch call.
    pending: Mutex<HashMap<String, Vec<PendingSnapshot>>>,
    /// (plugin, session) -> last detected status.
    cache: Mutex<HashMap<(String, String), (Status, Instant)>>,
    last_flush: Mutex<HashMap<String, Instant>>,
}

static BATCHER: OnceLock<Batcher> = OnceLock::new();

fn batcher() -> &'static Batcher {
    BATCHER.get_or_init(|| Batcher {
        pending: Mutex::new(HashMap::new()),
        cache: Mutex::new(HashMap::new()),
        last_flush: Mutex::new(HashMap::new()),
    })
}

fn truncate_snapshot(text: &str) -> String {
    if text.len() <= MAX_SNAPSHOT_BYTES {
        return text.to_string();
    }
    // Keep the tail: prompts and spinners live at the bottom of the pane.
    let start = text.len() - MAX_SNAPSHOT_BYTES;
    let start = (start..text.len())
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(start);
    text[start..].to_string()
}

fn detect_rpc(rpc: &RpcAgent, session: &str, tool: &str, clean: &str) -> Option<Status> {
    let b = batcher();
    let key = (rpc.plugin_id.clone(), session.to_string());
    if let Some((status, at)) = b.cache.lock().expect("status cache lock").get(&key) {
        if at.elapsed() < CACHE_FRESH {
            return Some(*status);
        }
    }
    {
        let mut pending = b.pending.lock().expect("status pending lock");
        let entries = pending.entry(rpc.plugin_id.clone()).or_default();
        entries.retain(|(s, _, _)| s != session);
        entries.push((
            session.to_string(),
            tool.to_string(),
            truncate_snapshot(clean),
        ));
    }
    flush(rpc, b);
    let cached = b
        .cache
        .lock()
        .expect("status cache lock")
        .get(&key)
        .map(|(s, _)| *s);
    cached
}

/// One detect_batch per plugin per poll window: the first caller after the
/// window flushes everything pending for that plugin; followers ride the
/// refreshed cache.
fn flush(rpc: &RpcAgent, b: &Batcher) {
    {
        let mut last = b.last_flush.lock().expect("status flush lock");
        let stamp = last
            .entry(rpc.plugin_id.clone())
            .or_insert_with(|| Instant::now() - CACHE_FRESH - Duration::from_millis(1));
        if stamp.elapsed() < CACHE_FRESH {
            return;
        }
        *stamp = Instant::now();
    }
    let snapshots: Vec<PendingSnapshot> = {
        let mut pending = b.pending.lock().expect("status pending lock");
        pending.remove(&rpc.plugin_id).unwrap_or_default()
    };
    if snapshots.is_empty() {
        return;
    }
    let params = json!({
        "snapshots": snapshots
            .iter()
            .map(|(session_id, agent, pane_text)| json!({
                "session_id": session_id,
                "agent": agent,
                "pane_text": pane_text,
            }))
            .collect::<Vec<_>>(),
    });
    match super::host::host().call_with_timeout(&rpc.plugin_id, &rpc.method, params, BATCH_TIMEOUT)
    {
        Ok(result) => {
            let mut cache = b.cache.lock().expect("status cache lock");
            let now = Instant::now();
            for item in result
                .get("results")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(session_id) = item.get("session_id").and_then(Value::as_str) else {
                    continue;
                };
                // Per-snapshot isolation: an errored snapshot keeps its
                // cached status; its siblings still update.
                if let Some(error) = item.get("error").and_then(Value::as_str) {
                    debug!(target: "plugin", plugin = %rpc.plugin_id, session = session_id, "detect_batch snapshot error: {error}");
                    continue;
                }
                let Some(status) = item
                    .get("status")
                    .and_then(Value::as_str)
                    .and_then(|s| serde_json::from_value::<StatusKind>(json!(s)).ok())
                else {
                    continue;
                };
                cache.insert(
                    (rpc.plugin_id.clone(), session_id.to_string()),
                    (status_from_kind(status), now),
                );
            }
        }
        Err(e) => {
            // Worker slow or sick: cached statuses answer until the
            // supervisor's respawn budget either heals or retires it.
            debug!(target: "plugin", plugin = %rpc.plugin_id, "detect_batch failed: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(toml_rules: &str) -> CompiledAgent {
        #[derive(serde::Deserialize)]
        struct Doc {
            rules: Vec<DetectionRule>,
        }
        let doc: Doc = toml::from_str(toml_rules).unwrap();
        compile_rules(&doc.rules)
    }

    #[test]
    fn declarative_rules_priority_and_default() {
        let agent = rules(
            r#"
            [[rules]]
            status = "running"
            priority = 100
            contains = ["esc to interrupt"]

            [[rules]]
            status = "waiting"
            priority = 90
            regex = "\\(y/n\\)|approve"

            [[rules]]
            status = "idle"
            priority = 0
            default = true
            "#,
        );
        assert_eq!(
            agent.evaluate("working... esc to interrupt"),
            Some(Status::Running)
        );
        assert_eq!(agent.evaluate("continue? (y/n)"), Some(Status::Waiting));
        assert_eq!(agent.evaluate("$ waiting at shell"), Some(Status::Idle));
        // Higher priority wins when both match.
        assert_eq!(
            agent.evaluate("approve? esc to interrupt"),
            Some(Status::Running)
        );
    }

    #[test]
    fn invalid_regex_is_skipped_not_fatal() {
        let agent = rules(
            r#"
            [[rules]]
            status = "waiting"
            priority = 10
            regex = "([unclosed"

            [[rules]]
            status = "idle"
            default = true
            "#,
        );
        assert_eq!(agent.evaluate("anything"), Some(Status::Idle));
    }

    #[test]
    fn snapshot_truncation_keeps_the_tail() {
        let text = format!("{}END", "x".repeat(MAX_SNAPSHOT_BYTES + 100));
        let truncated = truncate_snapshot(&text);
        assert!(truncated.len() <= MAX_SNAPSHOT_BYTES);
        assert!(truncated.ends_with("END"));
    }
}
