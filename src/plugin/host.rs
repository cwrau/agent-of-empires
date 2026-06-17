//! Tier 1 plugin worker host: spawn, supervise, and speak ndjson JSON-RPC
//! with plugin workers (D1, D8 of the plugin-system design).
//!
//! A worker is the executable named by the manifest's `[runtime]` section
//! (builtin plugins run `aoe __plugin-worker --id <id>` from the current
//! binary), spawned through the active [`super::sandbox::SandboxBackend`]
//! with piped stdio. The wire format is one JSON-RPC 2.0 object per line:
//!
//! - host -> worker requests: contributed actions, commands, status batches;
//! - worker -> host requests: the capability-gated host API below;
//! - host -> worker notifications: subscribed bus events (`events.event`).
//!
//! Every worker-initiated call passes through [`authorize`]: a method whose
//! capability was not granted for the worker's exact manifest hash is refused
//! (acceptance criterion 4 of #268). Workers are expected to exit on stdin
//! EOF; the host kills them on shutdown and respawns crashed workers within a
//! small per-process budget.
//!
//! The host is deliberately synchronous (std threads + channels): it is
//! called from the TUI event loop, one-shot CLI handlers, and (via
//! `spawn_blocking`) the server, and none of those want a runtime handoff.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use aoe_plugin_api::Capability;
use serde_json::{json, Value};
use tracing::{debug, warn};

use super::registry::LoadedPlugin;

/// Default timeout for a host -> worker call.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// Max events handed to a subscriber per replay pass.
const REPLAY_LIMIT: usize = 1000;

/// Spawns per plugin per process before the host refuses to respawn.
const RESPAWN_BUDGET: u32 = 3;

/// Hard cap on a single worker stdout/stderr line. `BufRead::read_line` grows
/// an unbounded `String`; a worker streaming bytes without a newline would
/// OOM the host. 8 MiB is far above any real JSON-RPC line.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Depth of a worker's pending-write queue. Callers `try_send` and never
/// block: a worker that stops reading its stdin fills the OS pipe, the
/// dedicated writer thread blocks on it, this queue fills, and further sends
/// fail fast as a wire error (the worker is then killed) instead of hanging
/// the TUI/daemon caller. See finding on `writeln!(stdin)` blocking.
const WRITE_QUEUE_CAPACITY: usize = 64;

/// Depth of a worker's host-call dispatch queue. The reader thread hands
/// worker -> host requests here so its only job is parse-and-route; the
/// dedicated dispatch thread does the heavy host I/O (storage reads, the
/// cross-process flock) without ever blocking stdout draining.
const DISPATCH_QUEUE_CAPACITY: usize = 64;

/// Max topics a single `events.subscribe` may register, and the max length of
/// each: `topic_matches` runs over the list per delivered event, so an
/// unbounded list is a CPU-DoS on every publish.
const MAX_SUBSCRIBE_TOPICS: usize = 64;
const MAX_TOPIC_BYTES: usize = 256;

/// A crash older than this no longer counts against the respawn budget, so a
/// plugin that fails once a day is not permanently disabled.
const RESPAWN_WINDOW: Duration = Duration::from_secs(300);

// Non-poisoning lock recovery is shared crate-wide; see `super::LockSafe`.
use super::LockSafe;

static HOST: OnceLock<PluginHost> = OnceLock::new();

/// The process-wide host. Workers spawn on first use per plugin.
pub fn host() -> &'static PluginHost {
    HOST.get_or_init(PluginHost::new)
}

pub struct PluginHost {
    workers: Mutex<HashMap<String, Arc<Worker>>>,
    /// Recent spawn timestamps per plugin. The budget counts only spawns
    /// inside [`RESPAWN_WINDOW`], so a plugin that crashes occasionally over a
    /// long-running daemon is not permanently tombstoned; a genuine crash loop
    /// (>= [`RESPAWN_BUDGET`] within the window) still trips it.
    recent_spawns: Mutex<HashMap<String, Vec<std::time::Instant>>>,
}

struct Worker {
    plugin_id: String,
    capabilities: Vec<Capability>,
    child: Mutex<Child>,
    /// Lines to write to the worker's stdin. A dedicated writer thread owns
    /// the `ChildStdin` and drains this; senders `try_send` so a blocked pipe
    /// never blocks the caller.
    writer_tx: mpsc::SyncSender<String>,
    /// Worker -> host requests handed off from the reader to the dispatch
    /// thread, keeping host I/O off the stdout-draining path.
    dispatch_tx: mpsc::SyncSender<HostCall>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>,
    alive: AtomicBool,
    /// Whether this worker already has a live event-forwarder thread; caps it
    /// to one so a subscribe loop cannot spawn unbounded threads.
    has_forwarder: AtomicBool,
}

/// A worker -> host request queued for the dispatch thread.
struct HostCall {
    id: u64,
    method: String,
    params: Value,
}

/// A freshly spawned worker and the raw handles its IO threads need; the
/// threads start in [`start_io`] once the `Arc<Worker>` exists.
struct SpawnedWorker {
    worker: Worker,
    stdout: std::process::ChildStdout,
    stdin: ChildStdin,
    writer_rx: mpsc::Receiver<String>,
    dispatch_rx: mpsc::Receiver<HostCall>,
}

impl PluginHost {
    fn new() -> Self {
        Self {
            workers: Mutex::new(HashMap::new()),
            recent_spawns: Mutex::new(HashMap::new()),
        }
    }

    /// Call `method` on `plugin_id`'s worker, spawning it if needed.
    pub fn call(&self, plugin_id: &str, method: &str, params: Value) -> Result<Value> {
        self.call_with_timeout(plugin_id, method, params, CALL_TIMEOUT)
    }

    pub fn call_with_timeout(
        &self,
        plugin_id: &str,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let worker = self.ensure_worker(plugin_id)?;
        let outcome = call_worker(&worker, method, params, timeout);
        if matches!(outcome, Err(CallError::Wire(_))) {
            // A dead or hung worker poisons every later call; kill it and
            // let the respawn budget decide whether it gets another life.
            self.mark_dead(&worker);
        }
        outcome.map_err(|e| anyhow!("plugin {plugin_id} {method}: {e}"))
    }

    /// Kill and forget every worker. Called on daemon/TUI shutdown.
    pub fn shutdown(&self) {
        let workers: Vec<Arc<Worker>> = self.workers.lock_safe().drain().map(|(_, w)| w).collect();
        for worker in workers {
            self.mark_dead(&worker);
        }
    }

    /// Kill every worker AND reset the respawn budgets. Called on registry
    /// reload (enable/disable/install/uninstall/update): a disabled or
    /// updated plugin must not keep its old worker, subscriptions, or grant
    /// set alive, and an intentional disable/enable cycle starts with a
    /// fresh budget. Still-active plugins respawn on their next call.
    pub fn reset(&self) {
        self.shutdown();
        self.recent_spawns.lock_safe().clear();
    }

    fn mark_dead(&self, worker: &Arc<Worker>) {
        reap_worker(worker);
        self.workers
            .lock_safe()
            .retain(|_, w| !Arc::ptr_eq(w, worker));
    }

    fn ensure_worker(&self, plugin_id: &str) -> Result<Arc<Worker>> {
        // Single-flight: the workers lock is held across check, spawn, and
        // insert, so two concurrent first calls cannot both observe "no
        // worker" and spawn duplicates (each burning respawn budget). The
        // spawn itself is per-plugin rare and fast; holding the map lock
        // through it is the simple correct shape.
        let mut workers = self.workers.lock_safe();
        if let Some(worker) = workers.get(plugin_id) {
            if worker.alive.load(Ordering::SeqCst) {
                return Ok(worker.clone());
            }
        }
        // A dead entry can linger: a stdin write failure flips `alive` to
        // false without tearing the child down, and the reader's terminal
        // mark_dead is skipped once the Arc is dropped. Reap it before the
        // insert below overwrites the slot, or the old `Child` would drop
        // without `wait()` and leak a zombie (or an orphaned third-party
        // process). The reader thread is keyed to a Weak, so dropping our
        // strong ref here is safe.
        if let Some(old) = workers.remove(plugin_id) {
            reap_worker(&old);
        }
        let registry = super::registry();
        let plugin = registry
            .get(plugin_id)
            .filter(|p| p.active())
            .ok_or_else(|| anyhow!("plugin {plugin_id} is not active"))?;
        {
            let now = std::time::Instant::now();
            let mut spawns = self.recent_spawns.lock_safe();
            let times = spawns.entry(plugin_id.to_string()).or_default();
            // Drop spawns older than the window so an occasional crash decays
            // instead of accumulating toward a permanent tombstone.
            times.retain(|t| now.duration_since(*t) < RESPAWN_WINDOW);
            if times.len() as u32 >= RESPAWN_BUDGET {
                bail!(
                    "plugin {plugin_id} worker crashed {RESPAWN_BUDGET} times within \
                     {}s; not respawning (disable and re-enable the plugin to reset)",
                    RESPAWN_WINDOW.as_secs()
                );
            }
            times.push(now);
        }
        let SpawnedWorker {
            worker,
            stdout,
            stdin,
            writer_rx,
            dispatch_rx,
        } = spawn_worker(plugin)?;
        let worker = Arc::new(worker);
        workers.insert(plugin_id.to_string(), worker.clone());
        drop(workers);
        // The IO threads can call mark_dead (which takes the workers lock) the
        // moment they start, so they must spawn after the lock is released.
        start_io(&worker, stdout, stdin, writer_rx, dispatch_rx);
        Ok(worker)
    }
}

/// How a single worker call failed. Wire failures (dead pipe, timeout) kill
/// the worker; RPC errors are the plugin answering "no" and leave it alive.
#[derive(Debug)]
enum CallError {
    Wire(String),
    Rpc(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Wire(msg) => write!(f, "{msg}"),
            CallError::Rpc(msg) => write!(f, "{msg}"),
        }
    }
}

/// One request/response round trip on an already-spawned worker.
fn call_worker(
    worker: &Arc<Worker>,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, CallError> {
    let id = worker.next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    worker.pending.lock_safe().insert(id, tx);
    let line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    // Non-blocking hand-off to the writer thread: a full queue means the
    // worker stopped reading its stdin (the writer thread is blocked on the
    // OS pipe), so fail fast as a wire error rather than block the caller.
    if worker.writer_tx.try_send(line.to_string()).is_err() {
        worker.pending.lock_safe().remove(&id);
        return Err(CallError::Wire(
            "worker write queue full or closed".to_string(),
        ));
    }
    match rx.recv_timeout(timeout) {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(rpc_error)) => Err(CallError::Rpc(rpc_error)),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            worker.pending.lock_safe().remove(&id);
            Err(CallError::Wire(format!(
                "timed out after {timeout:?}; worker killed"
            )))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(CallError::Wire("worker exited mid-call".to_string()))
        }
    }
}

/// Resolve the worker executable: manifest entrypoint relative to the plugin
/// root for installed plugins, the current binary's hidden `__plugin-worker`
/// subcommand for builtins.
fn worker_command(plugin: &LoadedPlugin) -> Result<(PathBuf, Vec<String>, PathBuf)> {
    let runtime = plugin
        .manifest
        .runtime
        .as_ref()
        .ok_or_else(|| anyhow!("plugin {} declares no [runtime] section", plugin.id()))?;
    match &plugin.root {
        Some(root) => {
            let entrypoint = root.join(&runtime.entrypoint);
            if !entrypoint.is_file() {
                bail!(
                    "plugin {} entrypoint missing: {}",
                    plugin.id(),
                    entrypoint.display()
                );
            }
            // Path::join does not normalize, so an entrypoint of "/bin/sh"
            // (absolute) or "../../bin/python3" (traversal) escapes the plugin
            // root and spawns a host binary the capability prompt never named.
            // Manifest validation rejects those shapes at parse time; this is
            // the spawn-time backstop (also resolves a symlinked root like
            // /tmp -> /private/tmp consistently).
            let canon_root = root
                .canonicalize()
                .with_context(|| format!("canonicalizing plugin root {}", root.display()))?;
            let canon_entry = entrypoint
                .canonicalize()
                .with_context(|| format!("canonicalizing entrypoint {}", entrypoint.display()))?;
            if !canon_entry.starts_with(&canon_root) {
                bail!(
                    "plugin {} entrypoint resolves outside the plugin root: {}",
                    plugin.id(),
                    canon_entry.display()
                );
            }
            Ok((canon_entry, runtime.args.clone(), root.clone()))
        }
        None => {
            let exe = std::env::current_exe().context("resolving current executable")?;
            let mut args = vec![
                "__plugin-worker".to_string(),
                "--id".to_string(),
                plugin.id().to_string(),
            ];
            args.extend(runtime.args.clone());
            let workdir = crate::session::get_app_dir()?;
            Ok((exe, args, workdir))
        }
    }
}

fn spawn_worker(plugin: &LoadedPlugin) -> Result<SpawnedWorker> {
    let (entrypoint, args, workdir) = worker_command(plugin)?;
    let mut cmd = super::sandbox::backend().command(&entrypoint, &args, &workdir);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning plugin {} worker", plugin.id()))?;
    let id = plugin.id().to_string();
    let stripped = |stream| anyhow!("sandbox backend stripped {stream} for {id}");
    let stdin = child.stdin.take().ok_or_else(|| stripped("stdin"))?;
    let stdout = child.stdout.take().ok_or_else(|| stripped("stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| stripped("stderr"))?;

    let (writer_tx, writer_rx) = mpsc::sync_channel::<String>(WRITE_QUEUE_CAPACITY);
    let (dispatch_tx, dispatch_rx) = mpsc::sync_channel::<HostCall>(DISPATCH_QUEUE_CAPACITY);

    let worker = Worker {
        plugin_id: id.clone(),
        capabilities: plugin.manifest.capabilities.clone(),
        child: Mutex::new(child),
        writer_tx,
        dispatch_tx,
        next_id: AtomicU64::new(1),
        pending: Mutex::new(HashMap::new()),
        alive: AtomicBool::new(true),
        has_forwarder: AtomicBool::new(false),
    };

    // stderr -> tracing, one capped line at a time (this thread needs no
    // Arc, so it starts here rather than in start_io).
    let id_for_stderr = id.clone();
    spawn_named(&format!("plugin-{id}-stderr"), move || {
        let mut reader = BufReader::new(stderr);
        let mut buf = Vec::new();
        loop {
            match read_capped_line(&mut reader, &mut buf) {
                Ok(0) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    warn!(target: "plugin", plugin = %id_for_stderr, "worker stderr: {}", line.trim_end());
                }
                Err(e) => {
                    warn!(target: "plugin", plugin = %id_for_stderr, "worker stderr closed: {e}");
                    break;
                }
            }
        }
    });

    debug!(target: "plugin", plugin = %id, "worker spawned");
    Ok(SpawnedWorker {
        worker,
        stdout,
        stdin,
        writer_rx,
        dispatch_rx,
    })
}

/// Spawn a named OS thread, so a stuck host shows the plugin and role in
/// `gstack` / `lldb thread list` instead of `<unnamed>`.
fn spawn_named(name: &str, f: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.to_string()).spawn(f);
}

/// Read one `\n`-terminated line into `buf` (including the newline), capped at
/// [`MAX_LINE_BYTES`]. `Ok(0)` is EOF; an over-cap line is an error so a
/// newline-less flood cannot grow `buf` without bound and OOM the host.
fn read_capped_line<R: BufRead>(reader: &mut R, buf: &mut Vec<u8>) -> std::io::Result<usize> {
    buf.clear();
    loop {
        let available = match reader.fill_buf() {
            Ok(b) => b,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            return Ok(buf.len());
        }
        if let Some(i) = available.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&available[..=i]);
            reader.consume(i + 1);
            return Ok(buf.len());
        }
        buf.extend_from_slice(available);
        let consumed = available.len();
        reader.consume(consumed);
        if buf.len() > MAX_LINE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("worker line exceeded {MAX_LINE_BYTES} bytes"),
            ));
        }
    }
}

/// Reap a worker's child and wake its pending callers without touching the
/// workers map. Shared by `mark_dead` and the dead-entry sweep in
/// `ensure_worker` (which already holds the map lock, so it must not re-enter
/// it through `mark_dead`).
fn reap_worker(worker: &Worker) {
    worker.alive.store(false, Ordering::SeqCst);
    // Let the forwarder thread (if any) observe death on its next event and
    // exit, and allow a respawned worker to subscribe again.
    worker.has_forwarder.store(false, Ordering::SeqCst);
    if let Ok(mut child) = worker.child.lock() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let mut pending = worker.pending.lock_safe();
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err("worker died".to_string()));
    }
}

/// Start the per-worker IO threads (reader, writer, dispatch). Split from
/// `spawn_worker` because they need the final `Arc` to route responses, run
/// host calls, and tear the worker down on EOF.
fn start_io(
    worker: &Arc<Worker>,
    stdout: std::process::ChildStdout,
    stdin: ChildStdin,
    writer_rx: mpsc::Receiver<String>,
    dispatch_rx: mpsc::Receiver<HostCall>,
) {
    let id = worker.plugin_id.clone();

    // Writer: owns ChildStdin, drains the bounded queue. A write failure flips
    // `alive` so the next call observes the death; the reader's EOF then reaps.
    let weak_writer = Arc::downgrade(worker);
    spawn_named(&format!("plugin-{id}-writer"), move || {
        let mut stdin = stdin;
        while let Ok(line) = writer_rx.recv() {
            if writeln!(stdin, "{line}")
                .and_then(|_| stdin.flush())
                .is_err()
            {
                if let Some(worker) = weak_writer.upgrade() {
                    worker.alive.store(false, Ordering::SeqCst);
                }
                break;
            }
        }
    });

    // Dispatch: runs worker -> host calls off the reader thread (heavy storage
    // I/O and the cross-process flock must not block stdout draining).
    let weak_dispatch = Arc::downgrade(worker);
    spawn_named(&format!("plugin-{id}-dispatch"), move || {
        while let Ok(call) = dispatch_rx.recv() {
            let Some(worker) = weak_dispatch.upgrade() else {
                break;
            };
            let response = match handle_host_call(&worker, &call.method, call.params) {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": call.id, "result": result }),
                Err(e) => json!({
                    "jsonrpc": "2.0", "id": call.id,
                    "error": { "code": -32000, "message": format!("{e:#}") },
                }),
            };
            write_line(&worker, &response);
        }
    });

    // Reader: parse-and-route only.
    let weak_reader = Arc::downgrade(worker);
    spawn_named(&format!("plugin-{id}-reader"), move || {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            match read_capped_line(&mut reader, &mut buf) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    if let Some(worker) = weak_reader.upgrade() {
                        warn!(target: "plugin", plugin = %worker.plugin_id, "worker stdout error: {e}");
                    }
                    break;
                }
            }
            let Some(worker) = weak_reader.upgrade() else {
                break;
            };
            let text = String::from_utf8_lossy(&buf);
            let trimmed = text.trim_end();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
                warn!(target: "plugin", plugin = %worker.plugin_id, "non-JSON worker line ignored");
                continue;
            };
            route_message(&worker, msg);
        }
        if let Some(worker) = weak_reader.upgrade() {
            // Full teardown, not just a flag flip: a worker that closed
            // stdout without exiting would otherwise keep running as a
            // stray process once a replacement is spawned. mark_dead kills
            // and reaps the child, removes it from the map, and drains
            // pending callers.
            host().mark_dead(&worker);
            debug!(target: "plugin", plugin = %worker.plugin_id, "worker stdout closed");
        }
    });
}

fn route_message(worker: &Arc<Worker>, msg: Value) {
    let id = msg.get("id").and_then(Value::as_u64);
    let method = msg.get("method").and_then(Value::as_str);
    match (id, method) {
        // Response to a host -> worker call.
        (Some(id), None) => {
            let outcome = if let Some(error) = msg.get("error") {
                Err(error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string())
            } else {
                Ok(msg.get("result").cloned().unwrap_or(Value::Null))
            };
            if let Some(tx) = worker.pending.lock_safe().remove(&id) {
                let _ = tx.send(outcome);
            }
        }
        // Worker -> host request: hand to the dispatch thread so the heavy
        // host I/O runs off this reader (keeping stdout draining). A full
        // queue is answered immediately with a busy error so the worker is
        // never left waiting on a request the host silently dropped.
        (Some(id), Some(method)) => {
            let params = msg.get("params").cloned().unwrap_or(Value::Null);
            let call = HostCall {
                id,
                method: method.to_string(),
                params,
            };
            if worker.dispatch_tx.try_send(call).is_err() {
                let response = json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32000, "message": "host dispatch queue saturated" },
                });
                write_line(worker, &response);
            }
        }
        // Worker notification; nothing defined in v1.
        (None, Some(method)) => {
            debug!(target: "plugin", plugin = %worker.plugin_id, method, "worker notification ignored");
        }
        _ => {}
    }
}

fn write_line(worker: &Worker, value: &Value) {
    // Non-blocking hand-off to the writer thread; a full/closed queue means
    // the worker is not draining its stdin, so mark it dead.
    if worker.writer_tx.try_send(value.to_string()).is_err() {
        worker.alive.store(false, Ordering::SeqCst);
    }
}

/// The single authorization gate: every worker-initiated method maps to a
/// capability, and an ungranted capability is a refusal, not a no-op.
fn authorize(plugin_id: &str, granted: &[Capability], method: &str) -> Result<()> {
    let needed = match method {
        "events.publish" => Capability::EventsPublish,
        "events.subscribe" => Capability::EventsSubscribe,
        "sessions.list" | "session.meta.get" => Capability::SessionsRead,
        "session.meta.set" | "session.meta.cas" => Capability::SessionsMetaWrite,
        // UI pushes are authorized by the manifest's declared contributions
        // (checked per call in the ui store), not by a capability: the
        // content is host-validated display state, never code.
        "ui.state.set" | "ui.state.remove" | "ui.notify" => return Ok(()),
        other => bail!("unknown host method {other:?}"),
    };
    if granted.contains(&needed) {
        Ok(())
    } else {
        bail!(
            "plugin {plugin_id} called {method} without the {} capability",
            needed.as_str()
        )
    }
}

fn handle_host_call(worker: &Arc<Worker>, method: &str, params: Value) -> Result<Value> {
    authorize(&worker.plugin_id, &worker.capabilities, method)?;
    match method {
        "events.publish" => {
            let topic = params
                .get("topic")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("events.publish needs a topic"))?;
            let own_prefix = format!("plugin.{}.", worker.plugin_id);
            if !topic.starts_with(&own_prefix) {
                bail!(
                    "plugin {} may only publish under {own_prefix}*",
                    worker.plugin_id
                );
            }
            let payload = params.get("payload").cloned().unwrap_or(Value::Null);
            let seq = crate::events::global()?.publish(topic, payload)?;
            Ok(json!({ "seq": seq }))
        }
        "events.subscribe" => {
            let topics: Vec<String> = params
                .get("topics")
                .and_then(|t| serde_json::from_value(t.clone()).ok())
                .ok_or_else(|| anyhow!("events.subscribe needs topics: [string]"))?;
            // Bound the subscription so a worker cannot turn every published
            // event into a huge linear scan in the forwarder thread.
            if topics.len() > MAX_SUBSCRIBE_TOPICS {
                bail!(
                    "events.subscribe accepts at most {MAX_SUBSCRIBE_TOPICS} topics (got {})",
                    topics.len()
                );
            }
            if let Some(too_long) = topics.iter().find(|t| t.len() > MAX_TOPIC_BYTES) {
                bail!(
                    "events.subscribe topic exceeds {MAX_TOPIC_BYTES} bytes: {:?}",
                    &too_long[..MAX_TOPIC_BYTES.min(too_long.len())]
                );
            }
            // One forwarder thread per worker: a second subscribe replaces the
            // first (the old thread exits when it next sees the flag cleared),
            // so a subscribe loop cannot spawn unbounded threads.
            if worker.has_forwarder.swap(true, Ordering::SeqCst) {
                bail!("this worker already has an active subscription");
            }
            let after_seq = params.get("after_seq").and_then(Value::as_u64);
            start_event_forwarder(worker, topics, after_seq)?;
            Ok(json!({ "subscribed": true }))
        }
        "sessions.list" => {
            let storage = crate::session::Storage::new_unwatched("")?;
            let instances = storage.load()?;
            let mut value = serde_json::to_value(&instances)?;
            // plugin_meta is a per-plugin namespace; a sessions-read grant must
            // not expose peer plugins' private session metadata. Scope each
            // instance's plugin_meta to the calling worker (session.meta.get
            // applies the same scoping for single-session reads).
            if let Some(arr) = value.as_array_mut() {
                for inst in arr {
                    if let Some(meta) = inst.get_mut("plugin_meta").and_then(Value::as_object_mut) {
                        meta.retain(|k, _| k == &worker.plugin_id);
                    }
                }
            }
            Ok(value)
        }
        "session.meta.get" => {
            let session_id = required_str(&params, "session_id")?;
            let storage = crate::session::Storage::new_unwatched("")?;
            let instances = storage.load()?;
            let instance = instances
                .iter()
                .find(|i| i.id == session_id)
                .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
            Ok(instance
                .plugin_meta
                .get(&worker.plugin_id)
                .cloned()
                .unwrap_or(Value::Null))
        }
        "session.meta.set" => {
            let session_id = required_str(&params, "session_id")?.to_string();
            let value = params
                .get("value")
                .cloned()
                .ok_or_else(|| anyhow!("session.meta.set needs a value"))?;
            set_meta(&worker.plugin_id, &session_id, None, value).map(|outcome| json!(outcome))
        }
        "session.meta.cas" => {
            let session_id = required_str(&params, "session_id")?.to_string();
            let expected = params.get("expected").cloned().unwrap_or(Value::Null);
            let value = params
                .get("value")
                .cloned()
                .ok_or_else(|| anyhow!("session.meta.cas needs a value"))?;
            set_meta(&worker.plugin_id, &session_id, Some(expected), value)
                .map(|outcome| json!(outcome))
        }
        "ui.state.set" => {
            let contribution_id = required_str(&params, "contribution_id")?.to_string();
            let session_id = params
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            let ttl_ms = params.get("ttl_ms").and_then(Value::as_u64);
            let payload: super::ui::UiPayload = serde_json::from_value(
                params
                    .get("payload")
                    .cloned()
                    .ok_or_else(|| anyhow!("ui.state.set needs a payload"))?,
            )
            .map_err(|e| anyhow!("invalid ui payload: {e}"))?;
            super::ui::set_state(
                &worker.plugin_id,
                &contribution_id,
                session_id,
                ttl_ms,
                payload,
            )?;
            Ok(json!({ "ok": true }))
        }
        "ui.state.remove" => {
            let contribution_id = required_str(&params, "contribution_id")?.to_string();
            let session_id = params
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            super::ui::remove_state(&worker.plugin_id, &contribution_id, session_id)?;
            Ok(json!({ "ok": true }))
        }
        "ui.notify" => {
            let title = required_str(&params, "title")?.to_string();
            let body = params
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let severity: super::ui::Severity = params
                .get("severity")
                .map(|s| serde_json::from_value(s.clone()))
                .transpose()
                .map_err(|e| anyhow!("invalid severity: {e}"))?
                .unwrap_or_default();
            let session_id = params
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            super::ui::notify(&worker.plugin_id, title, body, severity, session_id)?;
            Ok(json!({ "ok": true }))
        }
        _ => unreachable!("authorize() rejects unknown methods"),
    }
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string param {key:?}"))
}

#[derive(serde::Serialize)]
struct MetaWriteOutcome {
    swapped: bool,
    current: Value,
}

/// Write the plugin's namespace in a session's `plugin_meta`, optionally
/// compare-and-swap. Publishes `session.meta.changed` on success.
fn set_meta(
    plugin_id: &str,
    session_id: &str,
    expected: Option<Value>,
    value: Value,
) -> Result<MetaWriteOutcome> {
    let storage = crate::session::Storage::new_unwatched("")?;
    let outcome = storage.update(|instances, _groups| {
        let instance = instances
            .iter_mut()
            .find(|i| i.id == session_id)
            .ok_or_else(|| anyhow!("unknown session {session_id}"))?;
        let current = instance
            .plugin_meta
            .get(plugin_id)
            .cloned()
            .unwrap_or(Value::Null);
        if let Some(expected) = &expected {
            if &current != expected {
                return Ok(MetaWriteOutcome {
                    swapped: false,
                    current,
                });
            }
        }
        if value.is_null() {
            instance.plugin_meta.remove(plugin_id);
        } else {
            instance
                .plugin_meta
                .insert(plugin_id.to_string(), value.clone());
        }
        Ok(MetaWriteOutcome {
            swapped: true,
            current: value.clone(),
        })
    })?;
    if outcome.swapped {
        if let Ok(bus) = crate::events::global() {
            let _ = bus.publish(
                crate::events::topics::SESSION_META_CHANGED,
                json!({ "session_id": session_id, "plugin_id": plugin_id }),
            );
        }
    }
    Ok(outcome)
}

/// Forward matching bus events to the worker as `events.event` notifications:
/// replay from `after_seq` first, then the live broadcast, on a dedicated
/// thread (broadcast lag falls back to replay, mirroring the ACP ws path).
fn start_event_forwarder(
    worker: &Arc<Worker>,
    topics: Vec<String>,
    after_seq: Option<u64>,
) -> Result<()> {
    let bus = crate::events::global()?.clone();
    let weak = Arc::downgrade(worker);
    let id = worker.plugin_id.clone();
    spawn_named(&format!("plugin-{id}-events"), move || {
        // Capture the floor BEFORE subscribing: an event published between
        // this read and the live receiver attaching has seq > floor, so the
        // replay below delivers it instead of the live loop dropping it via
        // the `seq <= last_seq` filter (the cold-subscribe lost-event race).
        let floor = bus.highest_seq();
        let mut rx = bus.subscribe();
        let mut last_seq = after_seq.unwrap_or(floor);
        // Always replay from last_seq. For an explicit after_seq it is the
        // caller's resume point; for a fresh subscribe it is `floor`, so the
        // replay covers only the subscribe-race window and nothing older.
        match bus.replay_from(&topics, last_seq, REPLAY_LIMIT) {
            Ok(events) => {
                for event in events {
                    last_seq = event.seq;
                    let Some(worker) = weak.upgrade() else { return };
                    notify_event(&worker, &event);
                }
            }
            Err(e) => warn!(target: "plugin", "event replay failed: {e:#}"),
        }
        loop {
            match rx.blocking_recv() {
                Ok(event) => {
                    if event.seq <= last_seq
                        || !topics
                            .iter()
                            .any(|p| crate::events::topic_matches(p, &event.topic))
                    {
                        continue;
                    }
                    last_seq = event.seq;
                    let Some(worker) = weak.upgrade() else { return };
                    if !worker.alive.load(Ordering::SeqCst) {
                        return;
                    }
                    notify_event(&worker, &event);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    match bus.replay_from(&topics, last_seq, REPLAY_LIMIT) {
                        Ok(events) => {
                            for event in events {
                                last_seq = event.seq;
                                let Some(worker) = weak.upgrade() else { return };
                                notify_event(&worker, &event);
                            }
                        }
                        Err(e) => warn!(target: "plugin", "event replay failed: {e:#}"),
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
    Ok(())
}

fn notify_event(worker: &Worker, event: &crate::events::BusEvent) {
    let line = json!({
        "jsonrpc": "2.0",
        "method": "events.event",
        "params": { "seq": event.seq, "topic": event.topic, "payload": event.payload },
    });
    write_line(worker, &line);
}

#[cfg(test)]
mod tests {
    use super::*;
    use aoe_plugin_api::PluginManifest;

    fn spawn_and_start(plugin: &LoadedPlugin) -> Arc<Worker> {
        let SpawnedWorker {
            worker,
            stdout,
            stdin,
            writer_rx,
            dispatch_rx,
        } = spawn_worker(plugin).unwrap();
        let worker = Arc::new(worker);
        start_io(&worker, stdout, stdin, writer_rx, dispatch_rx);
        worker
    }

    fn script_plugin(dir: &std::path::Path, script_body: &str) -> LoadedPlugin {
        let manifest_toml = r#"
id = "test.worker"
name = "Test Worker"
version = "0.1.0"
api_version = 1

[runtime]
entrypoint = "worker.sh"
"#;
        let script_path = dir.join("worker.sh");
        std::fs::write(&script_path, script_body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        LoadedPlugin {
            manifest: PluginManifest::from_toml_str(manifest_toml).unwrap(),
            manifest_hash: "sha256:test".into(),
            root: Some(dir.to_path_buf()),
            source: super::super::PluginSource::Path {
                path: dir.display().to_string(),
            },
            enabled: true,
            grant: super::super::grants::GrantStatus::Granted,
            settings: toml::Table::new(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn call_round_trips_against_a_script_worker() {
        let dir = tempfile::tempdir().unwrap();
        // Answers the first request with a fixed result for id 1, then exits
        // on stdin EOF like a well-behaved worker.
        let plugin = script_plugin(
            dir.path(),
            "#!/bin/sh\nread line\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\\n'\n",
        );
        let worker = spawn_and_start(&plugin);
        let result = call_worker(&worker, "test.run", json!({}), Duration::from_secs(5)).unwrap();
        assert_eq!(result, json!({"ok": true}));
        let _ = worker.child.lock().unwrap().kill();
    }

    #[cfg(unix)]
    #[test]
    fn rpc_error_and_timeout_are_distinguished() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = script_plugin(
            dir.path(),
            "#!/bin/sh\nread line\nprintf '{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":1,\"message\":\"nope\"}}\\n'\nsleep 60\n",
        );
        let worker = spawn_and_start(&plugin);
        // First call: a clean RPC refusal.
        let err = call_worker(&worker, "test.run", json!({}), Duration::from_secs(5)).unwrap_err();
        assert!(
            matches!(err, CallError::Rpc(ref m) if m == "nope"),
            "{err:?}"
        );
        // Second call: the script never answers again; wire timeout.
        let err =
            call_worker(&worker, "test.run", json!({}), Duration::from_millis(200)).unwrap_err();
        assert!(matches!(err, CallError::Wire(_)), "{err:?}");
        let _ = worker.child.lock().unwrap().kill();
    }

    #[test]
    fn read_capped_line_aborts_on_a_newline_less_flood() {
        // A worker streaming bytes without a newline must not grow buf without
        // bound: the helper errors once it crosses MAX_LINE_BYTES instead.
        struct Endless;
        impl std::io::Read for Endless {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                b.fill(b'x');
                Ok(b.len())
            }
        }
        let mut reader = BufReader::new(Endless);
        let mut buf = Vec::new();
        let err = read_capped_line(&mut reader, &mut buf).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(buf.len() <= MAX_LINE_BYTES + 64 * 1024);
    }

    #[test]
    fn read_capped_line_reads_one_line_and_eof() {
        let mut reader = BufReader::new(&b"first\nsecond"[..]);
        let mut buf = Vec::new();
        assert_eq!(read_capped_line(&mut reader, &mut buf).unwrap(), 6);
        assert_eq!(&buf, b"first\n");
        // Trailing line with no newline, then EOF (Ok(0)).
        assert_eq!(read_capped_line(&mut reader, &mut buf).unwrap(), 6);
        assert_eq!(&buf, b"second");
        assert_eq!(read_capped_line(&mut reader, &mut buf).unwrap(), 0);
    }

    #[test]
    fn authorize_refuses_undeclared_capabilities() {
        // Acceptance criterion 4 of #268: undeclared capability use is refused.
        let granted = [Capability::SessionsRead];
        assert!(authorize("p", &granted, "sessions.list").is_ok());
        assert!(authorize("p", &granted, "events.publish").is_err());
        assert!(authorize("p", &granted, "session.meta.set").is_err());
        assert!(authorize("p", &granted, "made.up.method").is_err());
    }
}
