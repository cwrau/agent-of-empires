//! Daemon-side cron scheduler for scheduled sessions (#2886).
//!
//! A supervised background task ticks every 30s, evaluates the profile's
//! scheduled jobs through the pure [`plan_tick`] step, and for each job that
//! fires it spawns a structured-view session and delivers the job's prompt.
//! The runtime cursors live only in memory: seeding them to "now" every startup
//! is what makes the skip-missed policy discard downtime rather than firing a
//! backlog of elapsed occurrences.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast::error::RecvError;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::acp::state::Event;
use crate::server::session_spawn::{spawn_structured_session, StructuredSessionSpec};
use crate::server::AcpBroadcastFrame;
use crate::session::schedule::{plan_tick, Cursors, ScheduledJob, DEFAULT_SCHEDULE_GROUP};

use super::AppState;

/// How long a fired job waits for its worker to become ready and accept the
/// prompt before we give up, so a worker that never starts cannot wedge the
/// per-job task forever.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

/// Upper bound on how long a single fired run is held in flight while waiting
/// for its turn to complete. Prevents a session that never emits `Stopped` (a
/// crashed worker, a dropped broadcast) from pinning the in-flight guard and
/// silently disabling the job forever.
const RUN_MAX_LIFETIME: Duration = Duration::from_secs(30 * 60);

/// The supervised tick loop. Runs until `state.shutdown` is cancelled.
pub async fn schedule_loop(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    // Runtime cursors, held across ticks (never persisted).
    let mut cursors: Cursors<chrono::Local> = Cursors::new();
    // Ids of jobs whose per-job task is still running, so a job that fires again
    // before its previous run finishes is skipped rather than overlapped.
    let in_flight: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = state.shutdown.cancelled() => break,
        }

        // Fail closed: a config load or validation error skips the tick WITHOUT
        // touching cursors, so a transient read failure is never mistaken for
        // "all jobs removed" (which would drop the cursors and let a later
        // recovery refire).
        let cfg = match crate::session::profile_config::resolve_config(&state.profile) {
            Ok(cfg) => cfg,
            Err(e) => {
                warn!(
                    target: "server.scheduler",
                    profile = %state.profile,
                    "skipping tick: failed to load config: {e}"
                );
                continue;
            }
        };
        // Only run jobs this daemon owns, that individually validate, and whose
        // id is unique. A malformed job (e.g. a cron typo added via the settings
        // UI, which does not re-validate cron server-side) is skipped and logged
        // rather than halting every job. Ownership is an exact match on the
        // profile: a job with no owner never fires (the CLI and the save path
        // both stamp a concrete owner), so multiple profile daemons can never
        // double-fire the same job.
        let mut seen_ids = std::collections::HashSet::new();
        let owned: Vec<ScheduledJob> = cfg
            .scheduling
            .jobs
            .into_iter()
            .filter(|j| j.owner_profile == state.profile)
            .filter(|j| match j.validate() {
                Ok(()) => true,
                Err(e) => {
                    warn!(target: "server.scheduler", job = %j.id, "skipping invalid job: {e}");
                    false
                }
            })
            .filter(|j| {
                if seen_ids.insert(j.id.clone()) {
                    true
                } else {
                    warn!(target: "server.scheduler", job = %j.id, "skipping duplicate job id");
                    false
                }
            })
            .collect();

        let now = chrono::Local::now();
        let (fire_ids, next_cursors) = plan_tick(&owned, &cursors, &now);
        cursors = next_cursors;

        for id in fire_ids {
            let Some(job) = owned.iter().find(|j| j.id == id).cloned() else {
                continue;
            };

            {
                let mut guard = in_flight.lock().await;
                if !guard.insert(id.clone()) {
                    info!(
                        target: "server.scheduler",
                        job = %id,
                        "skipping fire: previous run of this job is still in flight"
                    );
                    continue;
                }
            }

            let state = state.clone();
            let in_flight = in_flight.clone();
            tokio::spawn(async move {
                run_job(&state, &job).await;
                in_flight.lock().await.remove(&job.id);
            });
        }
    }
}

/// Pure mapping from a scheduled job to the create-core spec. A job with a
/// `project` runs there; a project-less job runs as a scratch session. When the
/// job has no explicit `approval_mode` the spawn requests the agent's default
/// bypass ("yolo") mode so the unattended run does not stall on approvals; a
/// specific mode is applied post-spawn instead.
fn job_to_spec(job: &ScheduledJob, profile: &str) -> StructuredSessionSpec {
    let group = if job.group.trim().is_empty() {
        DEFAULT_SCHEDULE_GROUP.to_string()
    } else {
        job.group.clone()
    };
    let (path, scratch) = match &job.project {
        Some(p) => (p.clone(), false),
        None => (String::new(), true),
    };
    StructuredSessionSpec {
        title: Some(job.name.clone()),
        path,
        group,
        tool: job.tool.clone(),
        worktree_enabled: false,
        worktree_branch: None,
        create_new_branch: false,
        base_branch: None,
        sandbox: false,
        sandbox_image: None,
        yolo_mode: job.approval_mode.is_none(),
        extra_env: vec![],
        extra_args: String::new(),
        command_override: String::new(),
        extra_repo_paths: vec![],
        scratch,
        trust_hooks: None,
        custom_instruction: None,
        profile: profile.to_string(),
        view: crate::session::View::Structured,
        agent_name: job.agent.clone(),
        agent_model: job.model.clone(),
        agent_effort: None,
        import_acp_session_id: None,
        fork_seed: None,
    }
}

/// Spawn the session for a fired job, apply its approval mode, and deliver its
/// prompt. Best-effort: every failure is logged and swallowed so one bad job
/// never affects the tick loop or other jobs.
async fn run_job(state: &Arc<AppState>, job: &ScheduledJob) {
    let spec = job_to_spec(job, &state.profile);
    let outcome = match spawn_structured_session(state, spec).await {
        Ok(outcome) => outcome,
        Err(e) => {
            warn!(
                target: "server.scheduler",
                job = %job.id,
                "failed to spawn scheduled session: {e:#}"
            );
            return;
        }
    };
    let id = outcome.instance.id.clone();

    // The spawn core downgrades a tool that cannot run over ACP to a terminal
    // session. Delivering an ACP prompt to it would never reach the agent and
    // would leave an idle session, so stop here (the session is still visible in
    // the Scheduled group for the user to inspect and delete).
    if !outcome.instance.is_structured() {
        warn!(
            target: "server.scheduler",
            job = %job.id,
            session = %id,
            "tool '{}' is not structured-view capable; created the session but did not deliver the prompt",
            job.tool
        );
        return;
    }

    info!(
        target: "server.scheduler",
        job = %job.id,
        session = %id,
        "fired scheduled session"
    );

    // Subscribe before sending so the turn-end wait below cannot miss a fast
    // `Stopped`.
    let mut events = state.acp_events_tx.subscribe();

    // Apply the session mode before the prompt so approvals are handled per the
    // job's policy. An explicit approval_mode wins; otherwise fall back to the
    // agent's default bypass mode (the spawn already requests it via
    // `yolo_mode`, so this is a belt-and-suspenders reassert for the None case).
    let agent_key = job
        .agent
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(job.tool.as_str());
    let mode_id = match &job.approval_mode {
        Some(mode) => Some(mode.clone()),
        None => crate::acp::agent_profiles::resolve(agent_key)
            .yolo_mode_id
            .map(str::to_string),
    };
    if let Some(mode_id) = mode_id {
        if let Err(e) = state.acp_supervisor.set_mode(&id, &mode_id).await {
            // An explicit mode that fails to apply must not be silently ignored:
            // the run would proceed under an unverified permission mode. Abort
            // prompt delivery instead. A failed fallback reassert (None case) is
            // non-fatal since the spawn already requested the bypass mode.
            if job.approval_mode.is_some() {
                warn!(
                    target: "server.scheduler",
                    job = %job.id,
                    session = %id,
                    "explicit approval mode '{mode_id}' failed to apply; not delivering the prompt: {e}"
                );
                return;
            }
            warn!(
                target: "server.scheduler",
                job = %job.id,
                session = %id,
                "default mode reassert failed (continuing): {e}"
            );
        }
    }

    // Record the prompt in the transcript, then send it. A worker that never
    // becomes ready is bounded by the timeout rather than hanging the task.
    state
        .acp_supervisor
        .publish_user_prompt_with_attachments(&id, job.prompt.clone(), &[])
        .await;
    match tokio::time::timeout(
        PROMPT_TIMEOUT,
        state.acp_supervisor.send_prompt(&id, &job.prompt, &[]),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            warn!(
                target: "server.scheduler",
                job = %job.id,
                session = %id,
                "send_prompt failed: {e}"
            );
            return;
        }
        Err(_) => {
            warn!(
                target: "server.scheduler",
                job = %job.id,
                session = %id,
                "send_prompt timed out after {}s",
                PROMPT_TIMEOUT.as_secs()
            );
            return;
        }
    }

    // Hold the job in flight until the turn it just started actually completes,
    // so a later cron occurrence cannot spawn an overlapping run. Bounded by
    // RUN_MAX_LIFETIME so a session that never emits `Stopped` cannot pin the
    // guard forever.
    wait_for_turn_end(&mut events, &id).await;
}

/// Block until the session emits a terminal turn signal (`Stopped` or a startup
/// failure), the broadcast closes, or `RUN_MAX_LIFETIME` elapses.
async fn wait_for_turn_end(
    events: &mut tokio::sync::broadcast::Receiver<AcpBroadcastFrame>,
    id: &str,
) {
    let deadline = tokio::time::sleep(RUN_MAX_LIFETIME);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => {
                warn!(
                    target: "server.scheduler",
                    session = %id,
                    "run exceeded {}s without a turn-end signal; releasing the in-flight guard",
                    RUN_MAX_LIFETIME.as_secs()
                );
                return;
            }
            recv = events.recv() => match recv {
                Ok(frame) if frame.session_id == id => {
                    if matches!(&*frame.event, Event::Stopped { .. } | Event::AgentStartupError { .. }) {
                        return;
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str) -> ScheduledJob {
        ScheduledJob {
            id: id.to_string(),
            name: "nightly build".to_string(),
            schedule: "0 2 * * *".to_string(),
            enabled: true,
            tool: "claude".to_string(),
            agent: None,
            model: None,
            approval_mode: None,
            project: None,
            prompt: "run the build".to_string(),
            group: DEFAULT_SCHEDULE_GROUP.to_string(),
            owner_profile: String::new(),
        }
    }

    #[test]
    fn job_to_spec_scratch_when_no_project() {
        let spec = job_to_spec(&job("a"), "default");
        assert!(spec.scratch);
        assert!(spec.path.is_empty());
        assert_eq!(spec.group, DEFAULT_SCHEDULE_GROUP);
        assert_eq!(spec.title.as_deref(), Some("nightly build"));
        assert_eq!(spec.profile, "default");
        // No explicit approval mode requests the default bypass at spawn.
        assert!(spec.yolo_mode);
        assert!(!spec.worktree_enabled);
        assert!(!spec.sandbox);
    }

    #[test]
    fn job_to_spec_uses_project_when_set() {
        let mut j = job("b");
        j.project = Some("/home/me/repo".to_string());
        let spec = job_to_spec(&j, "work");
        assert!(!spec.scratch);
        assert_eq!(spec.path, "/home/me/repo");
        assert_eq!(spec.profile, "work");
    }

    #[test]
    fn job_to_spec_passes_through_agent_and_model() {
        let mut j = job("c");
        j.agent = Some("codex".to_string());
        j.model = Some("gpt-5".to_string());
        j.group = "Reports".to_string();
        let spec = job_to_spec(&j, "default");
        assert_eq!(spec.agent_name.as_deref(), Some("codex"));
        assert_eq!(spec.agent_model.as_deref(), Some("gpt-5"));
        assert_eq!(spec.group, "Reports");
    }

    #[test]
    fn job_to_spec_explicit_mode_disables_yolo() {
        let mut j = job("d");
        j.approval_mode = Some("plan".to_string());
        let spec = job_to_spec(&j, "default");
        assert!(!spec.yolo_mode);
    }

    #[test]
    fn job_to_spec_empty_group_defaults() {
        let mut j = job("e");
        j.group = "  ".to_string();
        let spec = job_to_spec(&j, "default");
        assert_eq!(spec.group, DEFAULT_SCHEDULE_GROUP);
    }
}
