//! Opt-in plugin discovery via GitHub topic search (#268).
//!
//! Discovery only runs on an explicit user action (a CLI command, a TUI
//! keypress, a dashboard button); nothing scans the network in the
//! background. Results are repositories tagged with the `aoe-plugin` topic,
//! sorted curated-first: anything not in the embedded featured index is
//! unvetted community code and every surface labels it as such before the
//! install flow's capability prompt runs.

use anyhow::{Context, Result};
use serde::Serialize;

use crate::github::{GitHubClient, GitHubClientConfig, GitHubSearchRepo};

/// The GitHub topic that marks a repository as an AoE plugin.
pub const PLUGIN_TOPIC: &str = "aoe-plugin";

const SEARCH_PAGE_SIZE: u8 = 50;

/// One discoverable plugin repository, ready for any surface to render.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredPlugin {
    /// `owner/repo`, the exact string the install flow accepts.
    pub slug: String,
    pub description: Option<String>,
    pub stars: u64,
    /// In the curated featured index (install validates its tree hash).
    pub featured: bool,
    /// A plugin from this slug is already installed.
    pub installed: bool,
}

/// Search GitHub for plugin repositories and mark each against the featured
/// index and the local install state.
pub async fn discover() -> Result<Vec<DiscoveredPlugin>> {
    let client = GitHubClient::unauthenticated(GitHubClientConfig {
        api_base: crate::github::DEFAULT_GITHUB_API_BASE.to_string(),
        user_agent: crate::github::DEFAULT_USER_AGENT.to_string(),
        timeout: std::time::Duration::from_secs(10),
    })
    .context("building GitHub client")?;
    let repos = client
        .search_repositories_by_topic(PLUGIN_TOPIC, SEARCH_PAGE_SIZE)
        .await
        .context("searching GitHub for aoe-plugin repositories")?;

    let installed_slugs: Vec<String> = crate::plugin::registry()
        .all()
        .iter()
        .filter_map(|p| match &p.source {
            crate::plugin::PluginSource::GitHub { slug } => Some(slug.clone()),
            _ => None,
        })
        .collect();

    Ok(mark(repos, |slug| {
        (
            crate::plugin::featured::index().contains_slug(slug),
            installed_slugs.iter().any(|s| s == slug),
        )
    }))
}

/// Blocking wrapper for the synchronous CLI/TUI call sites. Runs the fetch
/// on a fresh thread with the ambient runtime handle, since `block_on`
/// directly on a runtime worker thread panics.
pub fn discover_blocking() -> Result<Vec<DiscoveredPlugin>> {
    let handle = tokio::runtime::Handle::current();
    std::thread::spawn(move || handle.block_on(discover()))
        .join()
        .map_err(|_| anyhow::anyhow!("discovery thread panicked"))?
}

/// Pure marking and ordering: featured first, then stars, then slug for a
/// stable tie-break. Archived repositories are dropped; an archived plugin
/// is not something to point new installs at.
fn mark(
    repos: Vec<GitHubSearchRepo>,
    state_for: impl Fn(&str) -> (bool, bool),
) -> Vec<DiscoveredPlugin> {
    let mut found: Vec<DiscoveredPlugin> = repos
        .into_iter()
        .filter(|r| !r.archived)
        .map(|r| {
            let (featured, installed) = state_for(&r.full_name);
            DiscoveredPlugin {
                slug: r.full_name,
                description: r.description,
                stars: r.stargazers_count,
                featured,
                installed,
            }
        })
        .collect();
    found.sort_by(|a, b| {
        b.featured
            .cmp(&a.featured)
            .then(b.stars.cmp(&a.stars))
            .then(a.slug.cmp(&b.slug))
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(full_name: &str, stars: u64, archived: bool) -> GitHubSearchRepo {
        GitHubSearchRepo {
            full_name: full_name.to_string(),
            description: Some(format!("{full_name} plugin")),
            stargazers_count: stars,
            archived,
        }
    }

    #[test]
    fn featured_sort_first_then_stars_and_archived_dropped() {
        let repos = vec![
            repo("a/popular", 90, false),
            repo("b/curated", 5, false),
            repo("c/dead", 999, true),
            repo("d/small", 1, false),
        ];
        let found = mark(repos, |slug| (slug == "b/curated", slug == "d/small"));
        let slugs: Vec<&str> = found.iter().map(|p| p.slug.as_str()).collect();
        assert_eq!(slugs, ["b/curated", "a/popular", "d/small"]);
        assert!(found[0].featured);
        assert!(!found[1].featured);
        assert!(found[2].installed);
    }
}
