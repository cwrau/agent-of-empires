# Scheduled Sessions

Scheduled sessions run a preset prompt against an agent on a cron schedule,
unattended. A job says "at this time, spawn a session with this agent/model in
this project and send it this prompt." Spawned sessions are filed under a
`Scheduled` group so they do not muddle with your interactive ones.

A common use is a daily check: every morning, spin up an agent to review open
PRs, summarize what changed overnight, or run a triage prompt.

## Requirements

The `aoe serve` daemon must be running for jobs to fire; the scheduler lives in
the daemon. If the daemon is stopped, nothing fires.

Scheduling is available only for structured-view agents (the ones that run over
ACP), since an unattended run has no terminal for you to type into.

## Managing jobs from the CLI

```sh
# Add a job: 8am every day, Claude reviews open PRs in this repo.
aoe schedule add \
  --name "morning pr review" \
  --cron "0 8 * * *" \
  --tool claude \
  --project /path/to/repo \
  --prompt "Review open PRs. Flag anything ready to merge or with unanswered reviews."

# List jobs (add --json for machine-readable output).
aoe schedule list

# Disable / enable without deleting.
aoe schedule disable "morning pr review"
aoe schedule enable "morning pr review"

# Remove a job by name or id.
aoe schedule remove "morning pr review"
```

Add flags:

- `--name` label for the job; also titles the spawned session.
- `--cron` a standard 5-field cron expression, `min hour day-of-month month day-of-week`.
- `--prompt` the prompt delivered once the session starts.
- `--tool` the agent to run (default `claude`).
- `--agent`, `--model` optional agent-name and model overrides.
- `--project` the project path to run in. Omit for a scratch (project-less) session.
- `--approval-mode` the agent session mode applied so the run does not stall on
  approvals (see below).
- `--group` the group to file the session under (default `Scheduled`).
- `--disabled` add the job without enabling it yet.

Jobs are stored in the global config under `[scheduling]`, one `[[scheduling.jobs]]`
table each. You can also manage them from the settings screen in the TUI and the
web dashboard, which offer a picker for building the cron expression.

## Cron syntax and timezone

Expressions are standard 5-field cron, interpreted in the daemon host's local
time. `0 8 * * *` is 8:00 every day; `*/15 * * * *` is every 15 minutes;
`0 9 * * 1` is 9:00 every Monday. Moving the daemon to a machine in another
timezone changes when a job fires; daylight-saving transitions are handled by
the host clock.

## Missed runs

If the daemon is down when a job was due, that occurrence is skipped. The
scheduler does not catch up on restart, so a machine that was asleep overnight
does not fire a burst of backlogged sessions when it wakes. The next future
match fires normally.

## Unattended approvals

An unattended run cannot answer an approval prompt, so a job applies an agent
session mode when it starts. With no `--approval-mode`, the job uses the agent's
default bypass ("yolo") mode so it can act without prompting. Set
`--approval-mode` to a more restricted mode the agent advertises (for example a
read-only or plan mode) when you want the scheduled run to act safely without
full write access.

## Trust

A scheduled run against a repository whose lifecycle hooks have never been
trusted is refused (the daemon logs it and skips the job), the same gate that
protects interactive sessions. Trust the repo once via an interactive session
first, or point the job at a project-less scratch session, before relying on it
to fire.

## Multiple profiles

Each job records the profile that owns it, and only that profile's daemon fires
it. Running daemons for several profiles will not double-fire a shared job.
