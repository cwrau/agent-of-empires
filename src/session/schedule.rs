//! Scheduled-session job definitions (#2886).
//!
//! A [`ScheduledJob`] is a preset prompt the daemon fires against an agent on a
//! cron schedule. Jobs live in the global/profile config (`[scheduling]`), never
//! in a repo's `.agent-of-empires/config.toml` (they run unattended host-side
//! work, so `scheduling` is omitted from `REPO_OVERRIDABLE_SECTIONS`). Each job
//! records the profile that owns it so only that profile's daemon runs it; a
//! shared list would otherwise fire once per running daemon.
//!
//! Cron expressions are interpreted in the daemon host's local time. Moving the
//! daemon to a machine in another timezone changes when a job fires.

use serde::{Deserialize, Serialize};

/// Group scheduled sessions land in by default, so they do not muddle with
/// interactive ones.
pub const DEFAULT_SCHEDULE_GROUP: &str = "Scheduled";

fn default_true() -> bool {
    true
}

fn default_group() -> String {
    DEFAULT_SCHEDULE_GROUP.to_string()
}

/// One scheduled job: what to run, when, and as whom.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScheduledJob {
    /// Stable identity (uuid v4). Generated on add; required. Enable/disable and
    /// the runtime cursor key on this, so it must survive renames and edits.
    pub id: String,

    /// Human-readable label. Also used to title spawned sessions.
    pub name: String,

    /// Cron expression, host-local time. Standard 5-field (min hour dom mon dow).
    pub schedule: String,

    /// Whether the scheduler fires this job. Disabled jobs are kept but skipped.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Tool / built-in agent key (e.g. `claude`), same as `aoe add --tool`.
    pub tool: String,

    /// Structured-view agent name, when different from `tool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,

    /// Model override for the spawned session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// ACP session-mode id applied after the worker spawns, so an unattended run
    /// does not block forever on an approval prompt. `None` means the agent's
    /// default bypass ("yolo") mode; set a read-only / plan mode for a safe
    /// unattended run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_mode: Option<String>,

    /// Working project path. `None` runs a scratch (project-less) session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,

    /// The prompt delivered to the session once it starts.
    pub prompt: String,

    /// Group the spawned session is filed under.
    #[serde(default = "default_group")]
    pub group: String,

    /// Profile that owns this job. Only that profile's daemon fires it.
    #[serde(default)]
    pub owner_profile: String,
}

impl ScheduledJob {
    /// The value whose change should reset the runtime cursor (re-seed to now so
    /// a schedule edit does not retroactively fire). Prompt/model/name edits do
    /// not reset firing; only the schedule and enabled-state matter.
    pub fn schedule_fingerprint(&self) -> (bool, &str) {
        (self.enabled, self.schedule.as_str())
    }

    /// Validate a single job in isolation (does not check id uniqueness across a
    /// set; see [`SchedulingConfig::validate`]).
    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("job id is empty".to_string());
        }
        if self.name.trim().is_empty() {
            return Err(format!("job {}: name is empty", self.id));
        }
        if self.tool.trim().is_empty() {
            return Err(format!("job {}: tool is empty", self.id));
        }
        if self.prompt.trim().is_empty() {
            return Err(format!("job {}: prompt is empty", self.id));
        }
        validate_cron(&self.schedule)
            .map_err(|e| format!("job {}: invalid cron '{}': {e}", self.id, self.schedule))?;
        Ok(())
    }
}

/// The `[scheduling]` config section: the list of jobs for this scope.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchedulingConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<ScheduledJob>,
}

impl SchedulingConfig {
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Validate the whole set: each job is well-formed and ids are unique.
    /// Duplicate or missing ids are a hard error rather than a silent drop, so a
    /// hand-edited config that copy-pasted a job block fails loudly instead of
    /// clobbering the runtime cursor or double-firing.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for job in &self.jobs {
            job.validate()?;
            if !seen.insert(job.id.as_str()) {
                return Err(format!("duplicate job id: {}", job.id));
            }
        }
        Ok(())
    }
}

/// Validate a cron expression, returning a human-readable error on failure.
/// Host-local interpretation; the scheduler evaluates occurrences against the
/// local clock.
pub fn validate_cron(expr: &str) -> Result<(), String> {
    croner::Cron::new(expr)
        .parse()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The occurrence to fire if `schedule` has a match in the half-open window
/// `(last_check, now]`, else `None`. Returns at most one occurrence (the
/// earliest in the window), so a delayed tick that spans several matches fires
/// only once rather than bursting. Occurrences are interpreted in the timezone
/// of the supplied timestamps; the scheduler passes host-local time.
///
/// This is the pure core of the skip-missed policy: seeding `last_check` to
/// "now" at startup means occurrences that elapsed while the daemon was down are
/// never in a future window and so are skipped.
pub fn next_due<Tz: chrono::TimeZone>(
    schedule: &str,
    last_check: &chrono::DateTime<Tz>,
    now: &chrono::DateTime<Tz>,
) -> Option<chrono::DateTime<Tz>> {
    let cron = croner::Cron::new(schedule).parse().ok()?;
    let next = cron.find_next_occurrence(last_check, false).ok()?;
    if next <= *now {
        Some(next)
    } else {
        None
    }
}

/// Per-job runtime cursor. Held in memory only (never persisted): the
/// skip-missed policy relies on cursors seeding to "now" every startup so
/// downtime is discarded.
#[derive(Debug, Clone)]
pub struct JobCursor<Tz: chrono::TimeZone> {
    /// `(enabled, schedule)` when the cursor was last seeded. A change means the
    /// job was enabled/disabled or rescheduled, so we reseed rather than fire
    /// retroactively.
    fingerprint: (bool, String),
    /// Upper bound of the last evaluated window. Advances only when the job
    /// fires, so an occurrence is caught even if it lands between ticks.
    last_check: chrono::DateTime<Tz>,
}

/// Cursors keyed by job id.
pub type Cursors<Tz> = std::collections::HashMap<String, JobCursor<Tz>>;

/// Pure scheduler step: given the current jobs, the prior cursors, and `now`,
/// return the ids to fire and the updated cursor map.
///
/// Rules (the debate's converged semantics):
/// - A job absent from `prev` is seeded to `now` and does NOT fire (new job /
///   fresh daemon skips past occurrences).
/// - A job whose `(enabled, schedule)` fingerprint changed is reseeded to `now`
///   and does NOT fire (an edit never retroactively fires).
/// - An unchanged enabled job fires at most once if an occurrence falls in
///   `(last_check, now]`; on fire its cursor advances to `now`.
/// - Disabled jobs never fire.
/// - Cursors for jobs no longer present are dropped (only current jobs are
///   carried forward).
pub fn plan_tick<Tz: chrono::TimeZone>(
    jobs: &[ScheduledJob],
    prev: &Cursors<Tz>,
    now: &chrono::DateTime<Tz>,
) -> (Vec<String>, Cursors<Tz>) {
    let mut fires = Vec::new();
    let mut next: Cursors<Tz> = std::collections::HashMap::with_capacity(jobs.len());

    for job in jobs {
        let fp = (job.enabled, job.schedule.clone());
        let cursor = match prev.get(&job.id) {
            // New job or changed fingerprint: reseed to now, do not fire.
            None => JobCursor {
                fingerprint: fp,
                last_check: now.clone(),
            },
            Some(c) if c.fingerprint != fp => JobCursor {
                fingerprint: fp,
                last_check: now.clone(),
            },
            // Unchanged: evaluate the window.
            Some(c) => {
                let mut last_check = c.last_check.clone();
                if job.enabled && next_due(&job.schedule, &c.last_check, now).is_some() {
                    fires.push(job.id.clone());
                    last_check = now.clone();
                }
                JobCursor {
                    fingerprint: fp,
                    last_check,
                }
            }
        };
        next.insert(job.id.clone(), cursor);
    }

    (fires, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, schedule: &str) -> ScheduledJob {
        ScheduledJob {
            id: id.to_string(),
            name: "daily pr check".to_string(),
            schedule: schedule.to_string(),
            enabled: true,
            tool: "claude".to_string(),
            agent: None,
            model: None,
            approval_mode: None,
            project: None,
            prompt: "check open PRs".to_string(),
            group: DEFAULT_SCHEDULE_GROUP.to_string(),
            owner_profile: "default".to_string(),
        }
    }

    #[test]
    fn valid_job_passes() {
        assert!(job("a", "0 8 * * *").validate().is_ok());
    }

    #[test]
    fn invalid_cron_rejected() {
        assert!(job("a", "not a cron").validate().is_err());
        assert!(validate_cron("0 8 * * *").is_ok());
        assert!(validate_cron("77 8 * * *").is_err());
    }

    #[test]
    fn empty_fields_rejected() {
        let mut j = job("a", "0 8 * * *");
        j.prompt = "  ".to_string();
        assert!(j.validate().is_err());
        let mut j = job("", "0 8 * * *");
        j.prompt = "x".to_string();
        assert!(j.validate().is_err());
    }

    #[test]
    fn duplicate_ids_rejected() {
        let cfg = SchedulingConfig {
            jobs: vec![job("dup", "0 8 * * *"), job("dup", "0 9 * * *")],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn unique_ids_pass() {
        let cfg = SchedulingConfig {
            jobs: vec![job("a", "0 8 * * *"), job("b", "0 9 * * *")],
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = SchedulingConfig {
            jobs: vec![job("a", "*/5 * * * *")],
        };
        let toml = toml::to_string_pretty(&cfg).unwrap();
        let back: SchedulingConfig = toml::from_str(&toml).unwrap();
        assert_eq!(cfg.jobs, back.jobs);
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<chrono::Utc> {
        use chrono::TimeZone;
        chrono::Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn next_due_fires_when_occurrence_in_window() {
        // daily at 08:00; window (07:00, 08:30] contains 08:00.
        let got = next_due(
            "0 8 * * *",
            &utc(2026, 7, 16, 7, 0),
            &utc(2026, 7, 16, 8, 30),
        );
        assert_eq!(got, Some(utc(2026, 7, 16, 8, 0)));
    }

    #[test]
    fn next_due_none_when_no_occurrence_in_window() {
        // window (08:30, 09:00] has no daily-08:00 occurrence.
        let got = next_due(
            "0 8 * * *",
            &utc(2026, 7, 16, 8, 30),
            &utc(2026, 7, 16, 9, 0),
        );
        assert_eq!(got, None);
    }

    #[test]
    fn next_due_collapses_multiple_occurrences_to_one() {
        // every 10 min; window (08:00, 08:35] has 08:10/08:20/08:30 -> only 08:10.
        let got = next_due(
            "*/10 * * * *",
            &utc(2026, 7, 16, 8, 0),
            &utc(2026, 7, 16, 8, 35),
        );
        assert_eq!(got, Some(utc(2026, 7, 16, 8, 10)));
    }

    #[test]
    fn next_due_includes_occurrence_exactly_at_now() {
        let got = next_due(
            "0 8 * * *",
            &utc(2026, 7, 16, 7, 59),
            &utc(2026, 7, 16, 8, 0),
        );
        assert_eq!(got, Some(utc(2026, 7, 16, 8, 0)));
    }

    #[test]
    fn next_due_invalid_cron_is_none() {
        assert_eq!(
            next_due("nonsense", &utc(2026, 7, 16, 7, 0), &utc(2026, 7, 16, 9, 0)),
            None
        );
    }

    #[test]
    fn fingerprint_ignores_prompt_edits() {
        let a = job("a", "0 8 * * *");
        let mut b = a.clone();
        b.prompt = "different prompt".to_string();
        assert_eq!(a.schedule_fingerprint(), b.schedule_fingerprint());
        b.schedule = "0 9 * * *".to_string();
        assert_ne!(a.schedule_fingerprint(), b.schedule_fingerprint());
    }

    #[test]
    fn plan_tick_seeds_new_job_without_firing() {
        let jobs = vec![job("a", "0 8 * * *")];
        let (fires, cursors) = plan_tick(&jobs, &Cursors::new(), &utc(2026, 7, 16, 9, 0));
        assert!(
            fires.is_empty(),
            "a fresh job must not fire on its seed tick"
        );
        assert!(cursors.contains_key("a"));
    }

    #[test]
    fn plan_tick_fires_once_when_due() {
        let jobs = vec![job("a", "0 8 * * *")];
        let (_, c0) = plan_tick(&jobs, &Cursors::new(), &utc(2026, 7, 16, 7, 0));
        let (fires, c1) = plan_tick(&jobs, &c0, &utc(2026, 7, 16, 8, 30));
        assert_eq!(fires, vec!["a".to_string()]);
        // Next tick in the same day does not refire.
        let (fires2, _) = plan_tick(&jobs, &c1, &utc(2026, 7, 16, 8, 31));
        assert!(fires2.is_empty());
    }

    #[test]
    fn plan_tick_skips_missed_occurrences_after_reseed() {
        // Seed at 09:00 (after the 08:00 occurrence). It must never fire for the
        // already-elapsed 08:00: this is the daemon-was-down case.
        let jobs = vec![job("a", "0 8 * * *")];
        let (_, c0) = plan_tick(&jobs, &Cursors::new(), &utc(2026, 7, 16, 9, 0));
        let (fires, _) = plan_tick(&jobs, &c0, &utc(2026, 7, 16, 9, 30));
        assert!(fires.is_empty());
    }

    #[test]
    fn plan_tick_disabled_job_never_fires() {
        let mut j = job("a", "0 8 * * *");
        j.enabled = false;
        let jobs = vec![j];
        let (_, c0) = plan_tick(&jobs, &Cursors::new(), &utc(2026, 7, 16, 7, 0));
        let (fires, _) = plan_tick(&jobs, &c0, &utc(2026, 7, 16, 8, 30));
        assert!(fires.is_empty());
    }

    #[test]
    fn plan_tick_reenable_does_not_catch_up() {
        // Enabled then disabled then re-enabled around the occurrence: no catch-up.
        let mut disabled = job("a", "0 8 * * *");
        disabled.enabled = false;
        let enabled = job("a", "0 8 * * *");
        let (_, c0) = plan_tick(
            std::slice::from_ref(&enabled),
            &Cursors::new(),
            &utc(2026, 7, 16, 7, 0),
        );
        // Disable before the occurrence (fingerprint change reseeds).
        let (_, c1) = plan_tick(
            std::slice::from_ref(&disabled),
            &c0,
            &utc(2026, 7, 16, 7, 30),
        );
        // Re-enable after the occurrence: fingerprint change reseeds to 08:30, no fire.
        let (fires, _) = plan_tick(
            std::slice::from_ref(&enabled),
            &c1,
            &utc(2026, 7, 16, 8, 30),
        );
        assert!(fires.is_empty());
    }

    #[test]
    fn plan_tick_drops_removed_job_cursor() {
        let jobs = vec![job("a", "0 8 * * *")];
        let (_, c0) = plan_tick(&jobs, &Cursors::new(), &utc(2026, 7, 16, 7, 0));
        assert!(c0.contains_key("a"));
        let (_, c1) = plan_tick(&[], &c0, &utc(2026, 7, 16, 7, 30));
        assert!(!c1.contains_key("a"), "removed job cursor must be dropped");
    }
}
