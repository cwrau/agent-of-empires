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

    #[test]
    fn fingerprint_ignores_prompt_edits() {
        let a = job("a", "0 8 * * *");
        let mut b = a.clone();
        b.prompt = "different prompt".to_string();
        assert_eq!(a.schedule_fingerprint(), b.schedule_fingerprint());
        b.schedule = "0 9 * * *".to_string();
        assert_ne!(a.schedule_fingerprint(), b.schedule_fingerprint());
    }
}
