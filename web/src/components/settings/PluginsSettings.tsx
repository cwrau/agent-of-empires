import { useCallback, useEffect, useState } from "react";

import {
  discoverPlugins,
  fetchPluginUpdates,
  fetchPlugins,
  installPlugin,
  setPluginEnabled,
  uninstallPlugin,
  updatePlugin,
  type DiscoveredPlugin,
  type PluginCapabilityPrompt,
  type PluginInfo,
  type PluginListResponse,
  type PluginUpdateStatus,
} from "../../lib/api";

/// Plugin management: list every known plugin with trust and grant state,
/// enable/disable, install from a GitHub slug or path, update, uninstall.
/// Installs are two-phase: the server answers 409 with the declared
/// capability set, the user approves it here, and only the confirmed retry
/// writes anything (#268).
export function PluginsSettings() {
  const [data, setData] = useState<PluginListResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [installSource, setInstallSource] = useState("");
  const [prompt, setPrompt] = useState<PluginCapabilityPrompt | null>(null);
  /// What the pending capability prompt confirms: a fresh install (re-send
  /// the source) or an update of an installed plugin (re-send its id).
  const [pendingAction, setPendingAction] = useState<
    { kind: "install"; source: string } | { kind: "update"; id: string } | null
  >(null);
  /// Filled by the explicit "Check for updates" button; checks do network
  /// IO per GitHub plugin, so they never run on load.
  const [updateStatuses, setUpdateStatuses] = useState<Record<string, PluginUpdateStatus> | null>(null);
  /// Filled by the explicit "Discover plugins" button; the GitHub topic
  /// search never runs on panel load.
  const [discovered, setDiscovered] = useState<DiscoveredPlugin[] | null>(null);

  const reload = useCallback(async () => {
    const next = await fetchPlugins();
    if (next) {
      setData(next);
      setError(null);
    } else {
      setError("Failed to load plugins.");
    }
  }, []);

  useEffect(() => {
    // Deferred a tick: the lint forbids synchronous setState chains inside
    // an effect body (same pattern as SettingsView's schema load).
    const timer = setTimeout(() => {
      void reload();
    }, 0);
    return () => clearTimeout(timer);
  }, [reload]);

  const onToggle = async (plugin: PluginInfo, enabled: boolean) => {
    setBusy(true);
    setNotice(null);
    try {
      const ok = await setPluginEnabled(plugin.id, enabled);
      if (!ok) setError(`Failed to ${enabled ? "enable" : "disable"} ${plugin.id}.`);
      await reload();
    } finally {
      setBusy(false);
    }
  };

  const runMutation = async (
    action: { kind: "install"; source: string } | { kind: "update"; id: string },
    confirmed: boolean,
  ) => {
    setBusy(true);
    setNotice(null);
    setError(null);
    try {
      // Approval binds to the manifest hash from the prompt the user saw;
      // if the source changed since, the server answers with a fresh 409
      // prompt instead of installing something unreviewed.
      const expectedHash = confirmed ? prompt?.manifest_hash : undefined;
      const result =
        action.kind === "install"
          ? await installPlugin(action.source, confirmed, expectedHash)
          : await updatePlugin(action.id, confirmed, expectedHash);
      if (result.kind === "prompt") {
        setPrompt(result.prompt);
        setPendingAction(action);
      } else if (result.kind === "ok") {
        setNotice(result.message);
        setPrompt(null);
        setPendingAction(null);
        if (action.kind === "install") setInstallSource("");
        await reload();
      } else {
        setError(result.message);
        setPrompt(null);
        setPendingAction(null);
      }
    } finally {
      setBusy(false);
    }
  };

  const onCheckUpdates = async () => {
    setBusy(true);
    setNotice(null);
    setError(null);
    try {
      const result = await fetchPluginUpdates();
      if (result) {
        setUpdateStatuses(result.updates);
        const available = Object.values(result.updates).filter((s) => s.status === "available").length;
        setNotice(
          available === 0
            ? "All community plugins are up to date."
            : `${available} plugin${available === 1 ? "" : "s"} can be updated.`,
        );
      } else {
        setError("Update check failed.");
      }
    } finally {
      setBusy(false);
    }
  };

  const onDiscover = async () => {
    setBusy(true);
    setNotice(null);
    setError(null);
    try {
      const result = await discoverPlugins();
      if (result) {
        setDiscovered(result.plugins);
        setNotice(`${result.plugins.length} plugin repositor${result.plugins.length === 1 ? "y" : "ies"} found.`);
      } else {
        setError("Plugin discovery failed.");
      }
    } finally {
      setBusy(false);
    }
  };

  const onUninstall = async (plugin: PluginInfo) => {
    if (!window.confirm(`Uninstall ${plugin.name}? Its files, grant, and config entry are removed.`)) return;
    setBusy(true);
    setNotice(null);
    try {
      const ok = await uninstallPlugin(plugin.id);
      if (ok) {
        setNotice(`Uninstalled ${plugin.id}`);
      } else {
        setError(`Failed to uninstall ${plugin.id}.`);
      }
      await reload();
    } finally {
      setBusy(false);
    }
  };

  if (!data && !error) {
    return <p className="text-sm text-text-dim">Loading plugins…</p>;
  }

  return (
    <div className="space-y-4">
      {error && <p className="text-sm text-red-400">{error}</p>}
      {notice && <p className="text-sm text-green-400">{notice}</p>}

      {data && data.load_errors.length > 0 && (
        <div className="rounded border border-yellow-700 bg-yellow-950/40 p-3 text-xs text-yellow-300">
          <p className="mb-1 font-semibold">Plugin load problems</p>
          {data.load_errors.map((e) => (
            <p key={e}>{e}</p>
          ))}
        </div>
      )}

      <div className="space-y-3">
        {data?.plugins.map((plugin) => (
          <div
            key={plugin.id}
            className="rounded border border-surface-700 bg-surface-850 p-3"
            data-testid={`plugin-${plugin.id}`}
          >
            <div className="flex items-start justify-between gap-3">
              <div className="min-w-0">
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-medium">{plugin.name}</span>
                  <span className="text-xs text-text-dim">v{plugin.version}</span>
                  <span
                    className={`rounded px-1.5 py-0.5 text-[10px] uppercase tracking-wide ${
                      plugin.trust === "builtin" ? "bg-blue-900/60 text-blue-300" : "bg-amber-900/60 text-amber-300"
                    }`}
                  >
                    {plugin.trust}
                  </span>
                  {plugin.grant !== "granted" && (
                    <span className="rounded bg-red-900/60 px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-red-300">
                      {plugin.grant === "stale" ? "grant stale" : "not granted"}
                    </span>
                  )}
                  {updateStatuses?.[plugin.id]?.status === "available" && (
                    <span className="rounded bg-emerald-900/60 px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-emerald-300">
                      update available
                    </span>
                  )}
                </div>
                <p className="mt-1 text-xs text-text-dim">{plugin.description}</p>
                <p className="mt-1 text-[11px] text-text-dim">
                  {plugin.source}
                  {plugin.capabilities.length > 0 && <> · capabilities: {plugin.capabilities.join(", ")}</>}
                </p>
              </div>
              <div className="flex shrink-0 items-center gap-2">
                {!plugin.builtin && (
                  <>
                    <button
                      type="button"
                      className="rounded border border-surface-700 px-2 py-1 text-xs hover:bg-surface-800"
                      disabled={busy}
                      onClick={() => void runMutation({ kind: "update", id: plugin.id }, false)}
                    >
                      Update
                    </button>
                    <button
                      type="button"
                      className="rounded border border-red-800 px-2 py-1 text-xs text-red-400 hover:bg-red-950/40"
                      disabled={busy}
                      onClick={() => void onUninstall(plugin)}
                    >
                      Uninstall
                    </button>
                  </>
                )}
                <label className="flex items-center gap-1 text-xs">
                  <input
                    type="checkbox"
                    role="switch"
                    aria-label={`Enable ${plugin.name}`}
                    checked={plugin.enabled}
                    disabled={busy}
                    onChange={(e) => void onToggle(plugin, e.target.checked)}
                  />
                  Enabled
                </label>
              </div>
            </div>
          </div>
        ))}
      </div>

      {data && data.plugins.some((p) => !p.builtin) && (
        <button
          type="button"
          className="rounded border border-surface-700 px-3 py-1 text-sm hover:bg-surface-800 disabled:opacity-50"
          disabled={busy}
          onClick={() => void onCheckUpdates()}
        >
          Check for updates
        </button>
      )}

      <div className="rounded border border-surface-700 bg-surface-850 p-3">
        <p className="mb-2 text-sm font-medium">Install a plugin</p>
        <p className="mb-2 text-xs text-text-dim">
          GitHub slug (owner/repo) or a directory path on the server host. Community plugins ask for their declared
          capabilities before anything is written.{" "}
          {data?.isolation_summary && <>Note: a community plugin {data.isolation_summary}.</>}
        </p>
        <div className="flex gap-2">
          <input
            type="text"
            placeholder="owner/repo"
            aria-label="Plugin source"
            className="min-w-0 flex-1 rounded border border-surface-700 bg-surface-900 px-2 py-1 text-sm"
            value={installSource}
            onChange={(e) => setInstallSource(e.target.value)}
          />
          <button
            type="button"
            className="rounded bg-brand-600 px-3 py-1 text-sm text-white disabled:opacity-50"
            disabled={busy || installSource.trim() === ""}
            onClick={() => void runMutation({ kind: "install", source: installSource.trim() }, false)}
          >
            Install
          </button>
        </div>
      </div>

      <div className="rounded border border-surface-700 bg-surface-850 p-3">
        <p className="mb-2 text-sm font-medium">Discover plugins</p>
        <p className="mb-2 text-xs text-text-dim">
          Searches GitHub for repositories tagged <code>aoe-plugin</code>. Runs only when you click; plugins not marked
          featured are unvetted community code, reviewed by nobody, and still go through the capability approval before
          anything is written.
        </p>
        <button
          type="button"
          className="rounded border border-surface-700 px-3 py-1 text-sm hover:bg-surface-800 disabled:opacity-50"
          disabled={busy}
          onClick={() => void onDiscover()}
        >
          Search GitHub
        </button>
        {discovered && discovered.length === 0 && (
          <p className="mt-2 text-xs text-text-dim">No plugin repositories found.</p>
        )}
        {discovered && discovered.length > 0 && (
          <ul className="mt-3 space-y-2">
            {discovered.map((found) => (
              <li
                key={found.slug}
                className="flex items-start justify-between gap-3 rounded border border-surface-700 p-2"
                data-testid={`discovered-${found.slug}`}
              >
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-mono text-sm">{found.slug}</span>
                    <span
                      className={`rounded px-1.5 py-0.5 text-[10px] uppercase tracking-wide ${
                        found.installed
                          ? "bg-surface-700 text-text-dim"
                          : found.featured
                            ? "bg-emerald-900/60 text-emerald-300"
                            : "bg-amber-900/60 text-amber-300"
                      }`}
                    >
                      {found.installed ? "installed" : found.featured ? "featured" : "unvetted"}
                    </span>
                    <span className="text-[11px] text-text-dim">{found.stars} stars</span>
                  </div>
                  {found.description && <p className="mt-1 text-xs text-text-dim">{found.description}</p>}
                </div>
                {!found.installed && (
                  <button
                    type="button"
                    className="shrink-0 rounded border border-surface-700 px-2 py-1 text-xs hover:bg-surface-800"
                    disabled={busy}
                    onClick={() => void runMutation({ kind: "install", source: found.slug }, false)}
                  >
                    Install
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
      </div>

      {prompt && pendingAction && (
        <div
          className="rounded border border-amber-700 bg-amber-950/30 p-3"
          role="dialog"
          aria-label="Capability approval"
        >
          <p className="text-sm font-medium">
            {prompt.previous_capabilities ? "Capability change" : "Approve capabilities"}: {prompt.name} v
            {prompt.version}
          </p>
          <p className="mt-1 text-xs text-text-dim">{prompt.description}</p>
          {prompt.featured === "verified" && (
            <p className="mt-1 text-xs text-emerald-400">
              Featured plugin: this release matches its hash validated by the AoE maintainers.
            </p>
          )}
          {prompt.featured === "unknown_version" && (
            <p className="mt-1 text-xs text-amber-300">
              Featured plugin, but v{prompt.version} has no validated hash yet; treat it as unvalidated.
            </p>
          )}
          <ul className="mt-2 list-inside list-disc text-xs">
            {prompt.capabilities.length === 0 ? (
              <li>No runtime capabilities requested (declarative contributions only).</li>
            ) : (
              prompt.capabilities.map((c) => <li key={c}>{c}</li>)
            )}
          </ul>
          <p className="mt-2 text-[11px] text-amber-300">
            Capability gating limits what the plugin can ask aoe to do; it is not an OS sandbox. This plugin{" "}
            {prompt.isolation_summary}.
          </p>
          <div className="mt-3 flex gap-2">
            <button
              type="button"
              className="rounded bg-brand-600 px-3 py-1 text-sm text-white"
              disabled={busy}
              onClick={() => void runMutation(pendingAction, true)}
            >
              Approve and continue
            </button>
            <button
              type="button"
              className="rounded border border-surface-700 px-3 py-1 text-sm"
              disabled={busy}
              onClick={() => {
                setPrompt(null);
                setPendingAction(null);
              }}
            >
              Cancel
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
