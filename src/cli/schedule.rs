//! `aoe schedule` CLI: manage cron-scheduled sessions (#2886).
//!
//! Jobs are stored in the global config `[scheduling]` section, each tagged with
//! the profile that owns it so only that profile's daemon fires it. The daemon
//! (`aoe serve`) must be running for jobs to actually fire.

use anyhow::{bail, Result};
use clap::{Args, Subcommand};

use crate::session::config::update_config;
use crate::session::schedule::{validate_cron, ScheduledJob, DEFAULT_SCHEDULE_GROUP};

#[derive(Subcommand)]
pub enum ScheduleCommands {
    /// List scheduled jobs
    #[command(alias = "ls")]
    List(ScheduleListArgs),
    /// Add a scheduled job
    Add(ScheduleAddArgs),
    /// Remove a scheduled job by id or name
    #[command(alias = "rm")]
    Remove(ScheduleTargetArgs),
    /// Enable a scheduled job by id or name
    Enable(ScheduleTargetArgs),
    /// Disable a scheduled job by id or name
    Disable(ScheduleTargetArgs),
}

#[derive(Args)]
pub struct ScheduleListArgs {
    /// Emit JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct ScheduleAddArgs {
    /// Display name for the job (also titles the spawned session)
    #[arg(long)]
    pub name: String,

    /// Cron expression, host-local time (e.g. "0 8 * * *" for 8am daily)
    #[arg(long)]
    pub cron: String,

    /// The prompt delivered to the session when it starts
    #[arg(long)]
    pub prompt: String,

    /// Tool / built-in agent key
    #[arg(long, default_value = "claude")]
    pub tool: String,

    /// Structured-view agent name, when different from the tool
    #[arg(long)]
    pub agent: Option<String>,

    /// Model override
    #[arg(long)]
    pub model: Option<String>,

    /// ACP session-mode applied post-spawn (e.g. a read-only / plan mode) so the
    /// unattended run does not block on approvals. Omit to use the agent's
    /// default bypass mode.
    #[arg(long)]
    pub approval_mode: Option<String>,

    /// Project path to run in. Omit for a scratch (project-less) session.
    #[arg(long)]
    pub project: Option<String>,

    /// Group the spawned session is filed under
    #[arg(long, default_value = DEFAULT_SCHEDULE_GROUP)]
    pub group: String,

    /// Add the job disabled (does not fire until enabled)
    #[arg(long)]
    pub disabled: bool,
}

#[derive(Args)]
pub struct ScheduleTargetArgs {
    /// Job id or unique name
    pub target: String,
}

pub async fn run(profile: &str, command: ScheduleCommands) -> Result<()> {
    match command {
        ScheduleCommands::List(args) => list(profile, args),
        ScheduleCommands::Add(args) => add(profile, args),
        ScheduleCommands::Remove(args) => remove(args),
        ScheduleCommands::Enable(args) => set_enabled(args, true),
        ScheduleCommands::Disable(args) => set_enabled(args, false),
    }
}

/// A job matches `target` when it equals the id exactly, or the name exactly and
/// uniquely. Returns the index or an error explaining the miss/ambiguity.
fn find_index(jobs: &[ScheduledJob], target: &str) -> Result<usize> {
    if let Some(i) = jobs.iter().position(|j| j.id == target) {
        return Ok(i);
    }
    let matches: Vec<usize> = jobs
        .iter()
        .enumerate()
        .filter(|(_, j)| j.name == target)
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [] => bail!("no scheduled job with id or name '{target}'"),
        [i] => Ok(*i),
        _ => bail!("name '{target}' is ambiguous; remove/enable by id instead"),
    }
}

fn add(profile: &str, args: ScheduleAddArgs) -> Result<()> {
    validate_cron(&args.cron).map_err(|e| anyhow::anyhow!("invalid cron '{}': {e}", args.cron))?;

    let job = ScheduledJob {
        id: uuid::Uuid::new_v4().to_string(),
        name: args.name,
        schedule: args.cron,
        enabled: !args.disabled,
        tool: args.tool,
        agent: args.agent,
        model: args.model,
        approval_mode: args.approval_mode,
        project: args.project,
        prompt: args.prompt,
        group: args.group,
        owner_profile: profile.to_string(),
    };
    job.validate().map_err(|e| anyhow::anyhow!(e))?;
    let id = job.id.clone();

    update_config(|c| c.scheduling.jobs.push(job))?;
    println!("Added scheduled job {id}");
    println!("Note: the daemon (`aoe serve`) must be running for jobs to fire.");
    Ok(())
}

fn remove(args: ScheduleTargetArgs) -> Result<()> {
    let removed = update_config(|c| {
        let i = find_index(&c.scheduling.jobs, &args.target)?;
        Ok::<_, anyhow::Error>(c.scheduling.jobs.remove(i))
    })??;
    println!("Removed scheduled job {} ({})", removed.id, removed.name);
    Ok(())
}

fn set_enabled(args: ScheduleTargetArgs, enabled: bool) -> Result<()> {
    let (id, name) = update_config(|c| {
        let i = find_index(&c.scheduling.jobs, &args.target)?;
        c.scheduling.jobs[i].enabled = enabled;
        Ok::<_, anyhow::Error>((
            c.scheduling.jobs[i].id.clone(),
            c.scheduling.jobs[i].name.clone(),
        ))
    })??;
    println!(
        "{} scheduled job {id} ({name})",
        if enabled { "Enabled" } else { "Disabled" }
    );
    Ok(())
}

fn list(profile: &str, args: ScheduleListArgs) -> Result<()> {
    let config = crate::session::config::Config::load()?;
    let jobs = &config.scheduling.jobs;

    if args.json {
        println!("{}", serde_json::to_string_pretty(jobs)?);
        return Ok(());
    }

    if jobs.is_empty() {
        println!("No scheduled jobs. Add one with `aoe schedule add`.");
        return Ok(());
    }

    for job in jobs {
        let owner = if job.owner_profile.is_empty() || job.owner_profile == profile {
            String::new()
        } else {
            format!("  [profile: {}]", job.owner_profile)
        };
        println!(
            "{}  {}  {:>3}  {}  {}{}",
            job.id,
            if job.enabled { "on " } else { "off" },
            job.tool,
            job.schedule,
            job.name,
            owner,
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, name: &str) -> ScheduledJob {
        ScheduledJob {
            id: id.to_string(),
            name: name.to_string(),
            schedule: "0 8 * * *".to_string(),
            enabled: true,
            tool: "claude".to_string(),
            agent: None,
            model: None,
            approval_mode: None,
            project: None,
            prompt: "x".to_string(),
            group: DEFAULT_SCHEDULE_GROUP.to_string(),
            owner_profile: "default".to_string(),
        }
    }

    #[test]
    fn find_by_id_and_unique_name() {
        let jobs = vec![job("id-a", "morning"), job("id-b", "evening")];
        assert_eq!(find_index(&jobs, "id-a").unwrap(), 0);
        assert_eq!(find_index(&jobs, "evening").unwrap(), 1);
    }

    #[test]
    fn find_missing_errors() {
        let jobs = vec![job("id-a", "morning")];
        assert!(find_index(&jobs, "nope").is_err());
    }

    #[test]
    fn find_ambiguous_name_errors() {
        let jobs = vec![job("id-a", "dup"), job("id-b", "dup")];
        assert!(find_index(&jobs, "dup").is_err());
        // But the id is still unambiguous.
        assert_eq!(find_index(&jobs, "id-b").unwrap(), 1);
    }
}
