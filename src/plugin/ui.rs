//! Host-side store for plugin UI contributions (D9 of the plugin-system
//! design, the answer to "plugins add and change components in TUI and web").
//!
//! Plugins never render. A worker pushes small typed payloads for the
//! contributions its manifest declares (`ui.state.set` / `ui.state.remove` /
//! `ui.notify`); the host validates payload shape against the slot, caches
//! it here, and both surfaces render from the cache with their own widgets.
//! The TUI draw loop reads synchronously and never awaits a worker; the web
//! reads `GET /api/ui/state` and re-polls cheaply on the revision counter.
//!
//! State is ephemeral by design: canonical plugin data belongs in
//! `plugin_meta`. A worker restart repushes; a disable/uninstall evicts via
//! [`evict_except`] on registry reload.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use aoe_plugin_api::UiSlot;
use serde::{Deserialize, Serialize};

use super::RwLockSafe;

/// Severity tints shared by every slot payload; both surfaces map them to
/// theme colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

/// One block inside a card or panel. A deliberately tiny vocabulary the
/// host can render coherently in a terminal and in React.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", deny_unknown_fields)]
pub enum Block {
    Text {
        text: String,
        #[serde(default)]
        severity: Severity,
    },
    /// Key-value rows.
    Kv { items: Vec<(String, String)> },
    /// Bulleted list.
    List { items: Vec<String> },
    /// One labelled number, rendered large where space allows.
    Metric { label: String, value: String },
}

/// Typed payload per slot; shape is the validation. `deny_unknown_fields`
/// keeps the vocabulary closed so a future field is an explicit api bump.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum UiPayload {
    /// `status-bar-segment`, `session-list-row-badge`,
    /// `session-detail-header-badge`.
    Badge {
        text: String,
        #[serde(default)]
        severity: Severity,
        #[serde(default)]
        tooltip: String,
    },
    /// `session-list-column`: one cell; the manifest title is the header.
    Cell {
        text: String,
        #[serde(default)]
        severity: Severity,
        /// Optional numeric key so the column can order rows.
        #[serde(default)]
        sort_key: Option<f64>,
    },
    /// `session-list-sort-key`: higher sorts first under this mode.
    SortKey {
        key: f64,
        #[serde(default)]
        reason: String,
    },
    /// `session-list-filter-facet`.
    Facets { values: Vec<String> },
    /// `dashboard-card`, `session-detail-panel`.
    Blocks {
        #[serde(default)]
        severity: Severity,
        blocks: Vec<Block>,
    },
}

const MAX_TEXT: usize = 200;
const MAX_BLOCKS: usize = 32;
const MAX_ITEMS: usize = 64;
const MAX_FACETS: usize = 8;

impl UiPayload {
    /// Slot compatibility plus size caps; rejects oversized pushes before
    /// they reach any renderer.
    fn validate_for(&self, slot: UiSlot) -> Result<()> {
        let ok = matches!(
            (self, slot),
            (
                UiPayload::Badge { .. },
                UiSlot::StatusBarSegment
                    | UiSlot::SessionListRowBadge
                    | UiSlot::SessionDetailHeaderBadge
            ) | (UiPayload::Cell { .. }, UiSlot::SessionListColumn)
                | (UiPayload::SortKey { .. }, UiSlot::SessionListSortKey)
                | (UiPayload::Facets { .. }, UiSlot::SessionListFilterFacet)
                | (
                    UiPayload::Blocks { .. },
                    UiSlot::DashboardCard | UiSlot::SessionDetailPanel
                )
        );
        if !ok {
            bail!("payload kind does not match slot {}", slot.as_str());
        }
        let too_long = |s: &str| s.chars().count() > MAX_TEXT;
        match self {
            UiPayload::Badge { text, tooltip, .. } => {
                if too_long(text) || too_long(tooltip) {
                    bail!("badge text/tooltip longer than {MAX_TEXT} chars");
                }
            }
            UiPayload::Cell { text, .. } => {
                if too_long(text) {
                    bail!("cell text longer than {MAX_TEXT} chars");
                }
            }
            UiPayload::SortKey { key, reason } => {
                if !key.is_finite() {
                    bail!("sort key must be finite");
                }
                if too_long(reason) {
                    bail!("sort reason longer than {MAX_TEXT} chars");
                }
            }
            UiPayload::Facets { values } => {
                if values.len() > MAX_FACETS || values.iter().any(|v| too_long(v)) {
                    bail!("at most {MAX_FACETS} facets of {MAX_TEXT} chars");
                }
            }
            UiPayload::Blocks { blocks, .. } => {
                if blocks.len() > MAX_BLOCKS {
                    bail!("at most {MAX_BLOCKS} blocks");
                }
                for block in blocks {
                    match block {
                        Block::Text { text, .. } => {
                            if too_long(text) {
                                bail!("block text longer than {MAX_TEXT} chars");
                            }
                        }
                        Block::Kv { items } => {
                            if items.len() > MAX_ITEMS
                                || items.iter().any(|(k, v)| too_long(k) || too_long(v))
                            {
                                bail!("kv block too large");
                            }
                        }
                        Block::List { items } => {
                            if items.len() > MAX_ITEMS || items.iter().any(|i| too_long(i)) {
                                bail!("list block too large");
                            }
                        }
                        Block::Metric { label, value } => {
                            if too_long(label) || too_long(value) {
                                bail!("metric block too large");
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

/// What a piece of state applies to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UiScope {
    Global,
    Session(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct StateKey {
    plugin_id: String,
    contribution_id: String,
    scope: UiScope,
}

#[derive(Debug, Clone)]
struct StateValue {
    payload: UiPayload,
    expires_at: Option<Instant>,
}

/// One rendered-ready entry returned by the snapshot queries.
#[derive(Debug, Clone, Serialize)]
pub struct UiEntry {
    pub plugin_id: String,
    pub contribution_id: String,
    pub slot: UiSlot,
    /// Manifest title (column header, panel title, sort mode name).
    pub title: String,
    pub priority: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub payload: UiPayload,
}

/// A plugin-emitted notification, host-rendered on both surfaces.
#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    pub plugin_id: String,
    pub title: String,
    pub body: String,
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Monotonic id; clients keep the highest seen to spot new ones.
    pub seq: u64,
}

const MAX_NOTIFICATIONS: usize = 50;

#[derive(Default)]
struct UiStore {
    state: HashMap<StateKey, StateValue>,
    notifications: std::collections::VecDeque<Notification>,
}

static STORE: RwLock<Option<UiStore>> = RwLock::new(None);
/// Bumped on every mutation; cheap change detection for web polling and
/// TUI redraw triggers.
static REVISION: AtomicU64 = AtomicU64::new(0);
static NOTIFICATION_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

fn with_store<T>(f: impl FnOnce(&mut UiStore) -> T) -> T {
    let mut guard = STORE.write_safe();
    let store = guard.get_or_insert_with(UiStore::default);
    // Sweep expired entries on every write so TTL actually caps memory and
    // read-side scans in a long-lived daemon; reads only filter.
    let now = Instant::now();
    store
        .state
        .retain(|_, value| value.expires_at.map(|at| at > now).unwrap_or(true));
    f(store)
}

fn read_store<T>(f: impl FnOnce(&UiStore) -> T) -> T {
    let guard = STORE.read_safe();
    match guard.as_ref() {
        Some(store) => f(store),
        None => f(&UiStore::default()),
    }
}

/// Look up the declared contribution; pushing for an undeclared id is
/// refused, which is the ownership check (manifests are user-approved).
fn contribution_of(
    plugin_id: &str,
    contribution_id: &str,
) -> Result<aoe_plugin_api::UiContribution> {
    let registry = super::registry();
    let Some(plugin) = registry.get(plugin_id).filter(|p| p.active()) else {
        bail!("plugin {plugin_id} is not active");
    };
    plugin
        .manifest
        .ui
        .iter()
        .find(|c| c.id == contribution_id)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!("plugin {plugin_id} declares no ui contribution {contribution_id:?}")
        })
}

/// Validate and store one push from a plugin worker.
pub fn set_state(
    plugin_id: &str,
    contribution_id: &str,
    session_id: Option<String>,
    ttl_ms: Option<u64>,
    payload: UiPayload,
) -> Result<()> {
    let contribution = contribution_of(plugin_id, contribution_id)?;
    payload.validate_for(contribution.slot)?;
    let scope = match (contribution.slot.session_scoped(), session_id) {
        (true, Some(id)) => UiScope::Session(id),
        (true, None) => bail!("slot {} needs a session_id", contribution.slot.as_str()),
        (false, None) => UiScope::Global,
        (false, Some(_)) => bail!(
            "slot {} is global; drop session_id",
            contribution.slot.as_str()
        ),
    };
    with_store(|store| {
        store.state.insert(
            StateKey {
                plugin_id: plugin_id.to_string(),
                contribution_id: contribution_id.to_string(),
                scope,
            },
            StateValue {
                payload,
                expires_at: ttl_ms.map(|ms| Instant::now() + Duration::from_millis(ms)),
            },
        );
    });
    REVISION.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Remove one piece of state (all sessions when `session_id` is None on a
/// session-scoped contribution).
pub fn remove_state(
    plugin_id: &str,
    contribution_id: &str,
    session_id: Option<String>,
) -> Result<()> {
    contribution_of(plugin_id, contribution_id)?;
    with_store(|store| {
        store.state.retain(|key, _| {
            !(key.plugin_id == plugin_id
                && key.contribution_id == contribution_id
                && session_id
                    .as_ref()
                    .map(|id| key.scope == UiScope::Session(id.clone()))
                    .unwrap_or(true))
        });
    });
    REVISION.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Append a notification (capped ring). Notifications render host-side:
/// the TUI status bar and the web top bar show the newest, both surfaces
/// list the ring.
pub fn notify(
    plugin_id: &str,
    title: String,
    body: String,
    severity: Severity,
    session_id: Option<String>,
) -> Result<()> {
    let registry = super::registry();
    if registry.get(plugin_id).filter(|p| p.active()).is_none() {
        bail!("plugin {plugin_id} is not active");
    }
    if title.is_empty() || title.chars().count() > MAX_TEXT || body.chars().count() > MAX_TEXT {
        bail!("notification title must be 1..={MAX_TEXT} chars, body <= {MAX_TEXT}");
    }
    with_store(|store| {
        store.notifications.push_back(Notification {
            plugin_id: plugin_id.to_string(),
            title,
            body,
            severity,
            session_id,
            seq: NOTIFICATION_SEQ.fetch_add(1, Ordering::Relaxed) + 1,
        });
        while store.notifications.len() > MAX_NOTIFICATIONS {
            store.notifications.pop_front();
        }
    });
    REVISION.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Drop state and notifications of every plugin NOT in `active_ids`; called
/// by `reload_registry` so a disabled or uninstalled plugin's UI vanishes.
pub fn evict_except(active_ids: &std::collections::HashSet<String>) {
    with_store(|store| {
        store
            .state
            .retain(|key, _| active_ids.contains(&key.plugin_id));
        store
            .notifications
            .retain(|n| active_ids.contains(&n.plugin_id));
    });
    REVISION.fetch_add(1, Ordering::Relaxed);
}

/// All live entries for one slot, expired state skipped, ordered by
/// priority desc then plugin id; session-scoped slots can narrow to one
/// session.
pub fn entries(slot: UiSlot, session_id: Option<&str>) -> Vec<UiEntry> {
    let registry = super::registry();
    let now = Instant::now();
    let mut out: Vec<UiEntry> = read_store(|store| {
        store
            .state
            .iter()
            .filter(|(_, value)| value.expires_at.map(|at| at > now).unwrap_or(true))
            .filter_map(|(key, value)| {
                let plugin = registry.get(&key.plugin_id).filter(|p| p.active())?;
                let contribution = plugin
                    .manifest
                    .ui
                    .iter()
                    .find(|c| c.id == key.contribution_id && c.slot == slot)?;
                let entry_session = match &key.scope {
                    UiScope::Global => None,
                    UiScope::Session(id) => {
                        if let Some(wanted) = session_id {
                            if wanted != id {
                                return None;
                            }
                        }
                        Some(id.clone())
                    }
                };
                Some(UiEntry {
                    plugin_id: key.plugin_id.clone(),
                    contribution_id: key.contribution_id.clone(),
                    slot,
                    title: contribution.title.clone(),
                    priority: contribution.priority,
                    session_id: entry_session,
                    payload: value.payload.clone(),
                })
            })
            .collect()
    });
    out.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.plugin_id.cmp(&b.plugin_id))
            .then_with(|| a.contribution_id.cmp(&b.contribution_id))
    });
    out
}

/// Every live entry across all slots (the web snapshot endpoint).
pub fn all_entries() -> Vec<UiEntry> {
    [
        UiSlot::StatusBarSegment,
        UiSlot::DashboardCard,
        UiSlot::SessionListRowBadge,
        UiSlot::SessionListColumn,
        UiSlot::SessionListSortKey,
        UiSlot::SessionListFilterFacet,
        UiSlot::SessionDetailHeaderBadge,
        UiSlot::SessionDetailPanel,
    ]
    .into_iter()
    .flat_map(|slot| entries(slot, None))
    .collect()
}

/// Newest-first notification ring.
pub fn notifications() -> Vec<Notification> {
    read_store(|store| store.notifications.iter().rev().cloned().collect())
}

/// Declared contributions for one slot across active plugins (priority desc
/// then plugin id), regardless of whether state was pushed yet. Renderers
/// use this for stable structure (column headers, sort modes); state fills
/// in per [`entries`].
pub fn declared(slot: UiSlot) -> Vec<(String, aoe_plugin_api::UiContribution)> {
    let registry = super::registry();
    let mut out: Vec<(String, aoe_plugin_api::UiContribution)> = registry
        .active()
        .flat_map(|p| {
            p.manifest
                .ui
                .iter()
                .filter(|c| c.slot == slot)
                .map(|c| (p.id().to_string(), c.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    out.sort_by(|a, b| {
        b.1.priority
            .cmp(&a.1.priority)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.id.cmp(&b.1.id))
    });
    out
}

/// Session-keyed view of one slot's entries (row badges, cells, facets).
pub fn entries_by_session(slot: UiSlot) -> HashMap<String, Vec<UiEntry>> {
    let mut map: HashMap<String, Vec<UiEntry>> = HashMap::new();
    for entry in entries(slot, None) {
        if let Some(session_id) = entry.session_id.clone() {
            map.entry(session_id).or_default().push(entry);
        }
    }
    map
}

/// The numeric keys one sort-key contribution pushed, per session.
pub fn sort_keys(plugin_id: &str, contribution_id: &str) -> HashMap<String, f64> {
    entries(UiSlot::SessionListSortKey, None)
        .into_iter()
        .filter(|e| e.plugin_id == plugin_id && e.contribution_id == contribution_id)
        .filter_map(|e| match (e.session_id, e.payload) {
            (Some(session), UiPayload::SortKey { key, .. }) => Some((session, key)),
            _ => None,
        })
        .collect()
}

/// Facet values per session flattened into one searchable string, appended
/// to the session search haystack so typing a facet value filters the list.
pub fn facet_haystack(session_id: &str) -> String {
    entries(UiSlot::SessionListFilterFacet, Some(session_id))
        .into_iter()
        .filter_map(|e| match e.payload {
            UiPayload::Facets { values } => Some(values.join(" ")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn payload_slot_compatibility_and_caps() {
        let badge = UiPayload::Badge {
            text: "blocked".into(),
            severity: Severity::Error,
            tooltip: String::new(),
        };
        assert!(badge.validate_for(UiSlot::SessionListRowBadge).is_ok());
        assert!(badge.validate_for(UiSlot::StatusBarSegment).is_ok());
        assert!(badge.validate_for(UiSlot::DashboardCard).is_err());

        let huge = UiPayload::Badge {
            text: "x".repeat(MAX_TEXT + 1),
            severity: Severity::Info,
            tooltip: String::new(),
        };
        assert!(huge.validate_for(UiSlot::StatusBarSegment).is_err());

        let nan = UiPayload::SortKey {
            key: f64::NAN,
            reason: String::new(),
        };
        assert!(nan.validate_for(UiSlot::SessionListSortKey).is_err());

        let blocks = UiPayload::Blocks {
            severity: Severity::Info,
            blocks: vec![Block::Metric {
                label: "Needs review".into(),
                value: "3".into(),
            }],
        };
        assert!(blocks.validate_for(UiSlot::DashboardCard).is_ok());
        assert!(blocks.validate_for(UiSlot::SessionListSortKey).is_err());
    }

    #[test]
    fn payload_json_shape_is_tagged_and_closed() {
        let payload: UiPayload = serde_json::from_value(serde_json::json!({
            "kind": "badge", "text": "hi", "severity": "warning"
        }))
        .unwrap();
        assert!(matches!(payload, UiPayload::Badge { .. }));
        assert!(serde_json::from_value::<UiPayload>(serde_json::json!({
            "kind": "badge", "text": "hi", "nope": 1
        }))
        .is_err());
    }
}
