//! Curated featured-plugin index, embedded from `plugins/featured.toml`.
//!
//! A featured plugin is a community plugin the AoE maintainers vouch for at
//! specific releases: the index pins each vetted version to the tree hash of
//! its plugin directory. Install and update verify a fetched tree against
//! the pin; a hash mismatch for a listed version is refused outright, while
//! an unlisted version installs as an ordinary unvalidated community plugin.

use std::sync::LazyLock;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

const EMBEDDED: &str = include_str!("../../plugins/featured.toml");

#[derive(Debug, Default, Deserialize)]
pub struct FeaturedIndex {
    #[serde(default)]
    featured: Vec<FeaturedPlugin>,
}

#[derive(Debug, Deserialize)]
pub struct FeaturedPlugin {
    pub id: String,
    pub slug: String,
    #[serde(default)]
    pub releases: Vec<FeaturedRelease>,
}

#[derive(Debug, Deserialize)]
pub struct FeaturedRelease {
    pub version: String,
    pub tree_hash: String,
}

/// How a staged plugin relates to the featured index; shown on every
/// capability prompt surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeaturedValidation {
    /// Source is not in the index: ordinary community plugin.
    NotFeatured,
    /// Source is featured and this release's tree hash matches the pin.
    Verified,
    /// Source is featured but this version has no pinned hash (newer than
    /// the index, or skipped during curation): treat as unvalidated.
    UnknownVersion,
}

static INDEX: LazyLock<FeaturedIndex> = LazyLock::new(|| {
    toml::from_str(EMBEDDED).expect("plugins/featured.toml must parse; checked by unit test")
});

/// The index compiled into this binary.
pub fn index() -> &'static FeaturedIndex {
    &INDEX
}

impl FeaturedIndex {
    /// Whether a plugin id is part of the curated index. Telemetry uses this
    /// as its identity allowlist: only featured ids are publicly known, so
    /// only they may be named on the wire (`telemetry::plugins`).
    pub fn contains_id(&self, id: &str) -> bool {
        self.featured.iter().any(|p| p.id == id)
    }

    /// Whether a GitHub `owner/repo` slug is a featured source. Discovery
    /// uses this to badge curated results.
    pub fn contains_slug(&self, slug: &str) -> bool {
        self.featured.iter().any(|p| p.slug == slug)
    }

    /// The pinned GitHub slug for a featured plugin id, if any. Auto-update
    /// uses this to refuse a recorded source whose slug drifted from the
    /// index: a same-user process can rewrite `source` in `plugins.lock` to
    /// redirect a featured plugin's silent auto-update to a hostile repo.
    pub fn slug_for_id(&self, id: &str) -> Option<&str> {
        self.featured
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.slug.as_str())
    }

    /// Validate a staged plugin fetched from `slug` against the index.
    /// Errors are security refusals: a featured slug serving a different
    /// plugin id, or a pinned release whose tree hash does not match.
    pub fn validate(
        &self,
        slug: &str,
        manifest_id: &str,
        version: &str,
        tree_hash: &str,
    ) -> Result<FeaturedValidation> {
        let Some(entry) = self.featured.iter().find(|p| p.slug == slug) else {
            return Ok(FeaturedValidation::NotFeatured);
        };
        if entry.id != manifest_id {
            bail!(
                "featured source {slug} is pinned to plugin id {:?} but now serves {:?}; \
                 refusing to install",
                entry.id,
                manifest_id
            );
        }
        let Some(release) = entry.releases.iter().find(|r| r.version == version) else {
            return Ok(FeaturedValidation::UnknownVersion);
        };
        if release.tree_hash != tree_hash {
            bail!(
                "featured plugin {manifest_id} v{version} does not match its validated hash \
                 (expected {}, got {tree_hash}); the source may have been tampered with, \
                 refusing to install",
                release.tree_hash
            );
        }
        Ok(FeaturedValidation::Verified)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_index_parses() {
        // LazyLock would panic deep inside install paths otherwise; fail
        // loudly here instead.
        let _ = index();
    }

    fn fixture() -> FeaturedIndex {
        toml::from_str(
            r#"
[[featured]]
id = "acme.review"
slug = "acme/aoe-review"

[[featured.releases]]
version = "1.0.0"
tree_hash = "sha256:aaaa"
"#,
        )
        .unwrap()
    }

    #[test]
    fn validation_covers_all_outcomes() {
        let index = fixture();
        assert_eq!(
            index
                .validate("other/repo", "other.plugin", "1.0.0", "sha256:zzzz")
                .unwrap(),
            FeaturedValidation::NotFeatured
        );
        assert_eq!(
            index
                .validate("acme/aoe-review", "acme.review", "1.0.0", "sha256:aaaa")
                .unwrap(),
            FeaturedValidation::Verified
        );
        assert_eq!(
            index
                .validate("acme/aoe-review", "acme.review", "2.0.0", "sha256:bbbb")
                .unwrap(),
            FeaturedValidation::UnknownVersion
        );
        let mismatch = index
            .validate("acme/aoe-review", "acme.review", "1.0.0", "sha256:evil")
            .unwrap_err();
        assert!(
            mismatch.to_string().contains("does not match"),
            "{mismatch}"
        );
        let wrong_id = index
            .validate("acme/aoe-review", "evil.review", "1.0.0", "sha256:aaaa")
            .unwrap_err();
        assert!(wrong_id.to_string().contains("now serves"), "{wrong_id}");
    }
}
