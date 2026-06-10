//! Needs-update detection and opt-in auto-update for installed plugins.
//!
//! Checks are cheap on purpose: a GitHub source compares the lockfile's
//! recorded clone commit against `git ls-remote HEAD` (no clone), a local
//! path re-hashes the source directory. Auto-update reuses the two-phase
//! `install::update` flow with an always-declining confirm callback, so an
//! update that changes the declared capability set is never applied
//! silently; it surfaces as "needs approval" and waits for a manual
//! `aoe plugin update`.

use anyhow::{Context, Result};
use serde::Serialize;

use super::lockfile::{LockRecord, Lockfile};
use super::PluginSource;

/// Result of one plugin's update check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum UpdateStatus {
    UpToDate,
    Available,
    /// The check could not decide (no commit on record, source missing,
    /// network failure); the reason is user-facing.
    Unknown {
        reason: String,
    },
}

/// Check one installed plugin against its recorded source.
pub fn check_one(record: &LockRecord) -> UpdateStatus {
    match &record.source {
        PluginSource::Builtin => UpdateStatus::UpToDate,
        PluginSource::GitHub { slug } => {
            let Some(installed) = &record.commit else {
                return UpdateStatus::Unknown {
                    reason: "no commit recorded; run `aoe plugin update` once to backfill".into(),
                };
            };
            match remote_head(slug) {
                Ok(remote) if &remote == installed => UpdateStatus::UpToDate,
                Ok(_) => UpdateStatus::Available,
                Err(e) => UpdateStatus::Unknown {
                    reason: format!("{e:#}"),
                },
            }
        }
        PluginSource::Path { path } => {
            match super::integrity::tree_hash(std::path::Path::new(path)) {
                Ok(hash) if hash == record.tree_hash => UpdateStatus::UpToDate,
                Ok(_) => UpdateStatus::Available,
                Err(e) => UpdateStatus::Unknown {
                    reason: format!("source directory unreadable: {e:#}"),
                },
            }
        }
    }
}

fn remote_head(slug: &str) -> Result<String> {
    let url = format!("https://github.com/{slug}.git");
    let output = std::process::Command::new("git")
        .args(["ls-remote", &url, "HEAD"])
        .output()
        .context("running git ls-remote")?;
    if !output.status.success() {
        anyhow::bail!(
            "git ls-remote {url} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("git ls-remote {url} returned no HEAD"))
}

/// Check every installed (non-builtin) plugin. Network calls run
/// sequentially; callers wanting concurrency wrap this in spawn_blocking.
pub fn check_all() -> Result<Vec<(String, UpdateStatus)>> {
    let lockfile = Lockfile::load()?;
    Ok(lockfile
        .iter()
        .filter(|(_, rec)| rec.source != PluginSource::Builtin)
        .map(|(id, rec)| (id.clone(), check_one(rec)))
        .collect())
}

/// One plugin's auto-update outcome, for logs and the CLI table.
#[derive(Debug)]
pub enum AutoUpdateResult {
    Updated { version: String },
    NeedsApproval,
    Failed { error: String },
}

/// Update every plugin with an available update, silently but safely: the
/// confirm callback always declines, so a capability-changing update is
/// left pending instead of granted behind the user's back. The featured
/// index still verifies inside `install::update`.
pub fn auto_update_all() -> Result<Vec<(String, AutoUpdateResult)>> {
    let mut results = Vec::new();
    for (id, status) in check_all()? {
        if status != UpdateStatus::Available {
            continue;
        }
        let outcome = super::install::update(&id, &mut |_| false);
        let result = match outcome {
            Ok(super::install::InstallOutcome::Updated { version, .. }) => {
                AutoUpdateResult::Updated { version }
            }
            Ok(super::install::InstallOutcome::Declined) => AutoUpdateResult::NeedsApproval,
            Ok(_) => continue,
            Err(e) => AutoUpdateResult::Failed {
                error: format!("{e:#}"),
            },
        };
        results.push((id, result));
    }
    Ok(results)
}

/// Run `auto_update_all` and report through tracing; the shared startup
/// hook for the TUI and the serve daemon.
pub fn auto_update_and_log() {
    match auto_update_all() {
        Ok(results) => {
            for (id, result) in results {
                match result {
                    AutoUpdateResult::Updated { version } => {
                        tracing::info!(target: "plugin", plugin = %id, %version, "auto-updated");
                    }
                    AutoUpdateResult::NeedsApproval => {
                        tracing::warn!(
                            target: "plugin",
                            plugin = %id,
                            "update available but it changes declared capabilities; \
                             run `aoe plugin update {id}` to review"
                        );
                    }
                    AutoUpdateResult::Failed { error } => {
                        tracing::warn!(target: "plugin", plugin = %id, %error, "auto-update failed");
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(target: "plugin", error = %format!("{e:#}"), "auto-update sweep failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn record(source: PluginSource, tree_hash: &str, commit: Option<&str>) -> LockRecord {
        LockRecord {
            version: "1.0.0".into(),
            source,
            manifest_hash: "sha256:m".into(),
            tree_hash: tree_hash.into(),
            commit: commit.map(str::to_string),
            installed_at: Utc::now(),
        }
    }

    #[test]
    fn path_source_compares_tree_hash() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("aoe-plugin.toml"), "id = \"x\"").unwrap();
        let hash = crate::plugin::integrity::tree_hash(dir.path()).unwrap();
        let source = PluginSource::Path {
            path: dir.path().display().to_string(),
        };

        assert_eq!(
            check_one(&record(source.clone(), &hash, None)),
            UpdateStatus::UpToDate
        );
        std::fs::write(dir.path().join("extra.txt"), "new file").unwrap();
        assert_eq!(
            check_one(&record(source, &hash, None)),
            UpdateStatus::Available
        );
    }

    #[test]
    fn missing_path_source_is_unknown_not_an_error() {
        let source = PluginSource::Path {
            path: "/nonexistent/plugin-source".into(),
        };
        assert!(matches!(
            check_one(&record(source, "sha256:t", None)),
            UpdateStatus::Unknown { .. }
        ));
    }

    #[test]
    fn github_without_recorded_commit_is_unknown() {
        let source = PluginSource::GitHub {
            slug: "owner/repo".into(),
        };
        let status = check_one(&record(source, "sha256:t", None));
        assert!(
            matches!(&status, UpdateStatus::Unknown { reason } if reason.contains("backfill")),
            "{status:?}"
        );
    }
}
