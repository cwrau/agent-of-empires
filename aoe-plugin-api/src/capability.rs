use serde::{Deserialize, Serialize};

/// Runtime capabilities a plugin may declare in its manifest.
///
/// Declared capabilities are shown to the user once at install time and
/// persisted pinned to the manifest hash; the host's JSON-RPC authorization
/// middleware refuses any call whose capability was not granted. Tier 0
/// declarative contributions (settings, keybinds, themes, declarative status
/// rules) are implicit and need no capability.
///
/// This gates access to host APIs for cooperative plugins; it is not OS-level
/// process isolation. See the security model section of
/// `docs/development/internals/plugin-system.md`.
///
/// `#[non_exhaustive]`: capabilities grow with `api_version`, so downstream
/// Rust consumers must handle an unknown variant rather than break on a new
/// one. Several variants below are RESERVED forward declarations: they parse
/// in a manifest but the host has no RPC for them yet and returns an "unknown
/// host method" error at call time. The "Capabilities not yet implemented"
/// section of `docs/development/writing-plugins.md` tracks the live set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum Capability {
    /// Read the session list and instance fields.
    SessionsRead,
    /// Write the plugin's own namespace in per-session `plugin_meta`.
    SessionsMetaWrite,
    /// Receive captured tmux pane text (status detection input).
    PaneRead,
    /// Subscribe to event bus topics declared in the manifest.
    EventsSubscribe,
    /// Publish events under the plugin's own `plugin.<id>.*` topics.
    EventsPublish,
    /// RESERVED (not yet implemented): ask the host to spawn a subprocess.
    ProcessSpawn,
    /// RESERVED (not yet implemented): outbound HTTP through the host.
    NetFetch,
    /// RESERVED (not yet implemented): read files through host RPCs.
    FsRead,
    /// RESERVED (not yet implemented): write files through host RPCs.
    FsWrite,
    /// RESERVED (not yet implemented): hook/pane status reconciliation.
    AgentReconcile,
    /// RESERVED (not yet implemented): agent hook install/uninstall.
    AgentHooks,
    /// Place a contributed CLI command at the top level of the command tree.
    CliTopLevel,
}

impl Capability {
    /// Kebab-case form used in manifests, grant files, and prompts.
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::SessionsRead => "sessions-read",
            Capability::SessionsMetaWrite => "sessions-meta-write",
            Capability::PaneRead => "pane-read",
            Capability::EventsSubscribe => "events-subscribe",
            Capability::EventsPublish => "events-publish",
            Capability::ProcessSpawn => "process-spawn",
            Capability::NetFetch => "net-fetch",
            Capability::FsRead => "fs-read",
            Capability::FsWrite => "fs-write",
            Capability::AgentReconcile => "agent-reconcile",
            Capability::AgentHooks => "agent-hooks",
            Capability::CliTopLevel => "cli-top-level",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_uses_kebab_case_matching_as_str() {
        for cap in [
            Capability::SessionsRead,
            Capability::SessionsMetaWrite,
            Capability::PaneRead,
            Capability::EventsSubscribe,
            Capability::EventsPublish,
            Capability::ProcessSpawn,
            Capability::NetFetch,
            Capability::FsRead,
            Capability::FsWrite,
            Capability::AgentReconcile,
            Capability::AgentHooks,
            Capability::CliTopLevel,
        ] {
            let json = serde_json::to_string(&cap).unwrap();
            assert_eq!(json, format!("\"{}\"", cap.as_str()));
            let back: Capability = serde_json::from_str(&json).unwrap();
            assert_eq!(back, cap);
        }
    }

    #[test]
    fn unknown_capability_is_rejected() {
        assert!(serde_json::from_str::<Capability>("\"root-access\"").is_err());
    }
}
