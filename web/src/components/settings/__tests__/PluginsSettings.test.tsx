// @vitest-environment jsdom
//
// Contract test for the PluginsSettings management panel (#268): the enable
// toggle emits the right setPluginEnabled payload, the two-phase install
// renders the 409 capability prompt without writing, approval re-sends with
// confirm_capabilities, and uninstall asks for confirmation first.

import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, waitFor } from "@testing-library/react";

import type { DiscoveredPlugin, PluginListResponse, PluginMutationResult, PluginUpdateStatus } from "../../../lib/api";

const fetchPlugins = vi.fn<[], Promise<PluginListResponse | null>>();
const setPluginEnabled = vi.fn<[string, boolean], Promise<boolean>>();
const installPlugin = vi.fn<[string, boolean, string | undefined], Promise<PluginMutationResult>>();
const updatePlugin = vi.fn<[string, boolean, string | undefined], Promise<PluginMutationResult>>();
const uninstallPlugin = vi.fn<[string], Promise<boolean>>();
const fetchPluginUpdates = vi.fn<[], Promise<{ updates: Record<string, PluginUpdateStatus> } | null>>();
const discoverPlugins = vi.fn<[], Promise<{ plugins: DiscoveredPlugin[] } | null>>();

vi.mock("../../../lib/api", () => ({
  fetchPlugins: () => fetchPlugins(),
  setPluginEnabled: (id: string, enabled: boolean) => setPluginEnabled(id, enabled),
  installPlugin: (source: string, confirm: boolean, hash?: string) => installPlugin(source, confirm, hash),
  updatePlugin: (id: string, confirm: boolean, hash?: string) => updatePlugin(id, confirm, hash),
  uninstallPlugin: (id: string) => uninstallPlugin(id),
  fetchPluginUpdates: () => fetchPluginUpdates(),
  discoverPlugins: () => discoverPlugins(),
}));

// Imported after the mock is registered.
import { PluginsSettings } from "../PluginsSettings";

function listResponse(overrides: Partial<PluginListResponse> = {}): PluginListResponse {
  return {
    plugins: [
      {
        id: "aoe.status",
        name: "Agent Status Detection",
        version: "1.1.0",
        description: "Detects agent session status.",
        source: "builtin",
        trust: "builtin",
        enabled: true,
        grant: "granted",
        active: true,
        capabilities: ["pane-read"],
        has_runtime: true,
        setting_count: 1,
        builtin: true,
      },
      {
        id: "example.plugin",
        name: "Example",
        version: "0.1.0",
        description: "A community plugin.",
        source: "github:owner/repo",
        trust: "community",
        enabled: true,
        grant: "granted",
        active: true,
        capabilities: ["sessions-read"],
        has_runtime: true,
        setting_count: 0,
        builtin: false,
      },
    ],
    load_errors: [],
    isolation_summary: "runs as a regular process",
    ...overrides,
  };
}

beforeEach(() => {
  fetchPlugins.mockReset();
  setPluginEnabled.mockReset();
  installPlugin.mockReset();
  updatePlugin.mockReset();
  uninstallPlugin.mockReset();
  fetchPluginUpdates.mockReset();
  discoverPlugins.mockReset();
  fetchPlugins.mockResolvedValue(listResponse());
  setPluginEnabled.mockResolvedValue(true);
  uninstallPlugin.mockResolvedValue(true);
});

describe("PluginsSettings contract", () => {
  it("disable toggle emits setPluginEnabled(id, false)", async () => {
    const { findByLabelText } = render(<PluginsSettings />);
    const toggle = await findByLabelText("Enable Agent Status Detection");
    fireEvent.click(toggle);
    await waitFor(() => {
      expect(setPluginEnabled).toHaveBeenCalledWith("aoe.status", false);
    });
  });

  it("install is two-phase: 409 prompt renders capabilities, approval re-sends confirmed", async () => {
    installPlugin.mockResolvedValueOnce({
      kind: "prompt",
      prompt: {
        needs_confirmation: true,
        id: "new.plugin",
        name: "New Plugin",
        version: "1.0.0",
        description: "Wants capabilities.",
        capabilities: ["net-fetch", "fs-read"],
        previous_capabilities: null,
        trust: "community",
        source: "github:o/r",
        featured: "verified",
        manifest_hash: "sha256:staged",
        isolation_summary: "runs as a regular process",
      },
    });
    installPlugin.mockResolvedValueOnce({ kind: "ok", message: "Installed new.plugin 1.0.0" });

    const { findByLabelText, findByRole, findByText } = render(<PluginsSettings />);
    const input = await findByLabelText("Plugin source");
    fireEvent.change(input, { target: { value: "o/r" } });
    fireEvent.click(await findByText("Install"));

    // Phase 1: unconfirmed request, prompt rendered, nothing written.
    await waitFor(() => {
      expect(installPlugin).toHaveBeenCalledWith("o/r", false, undefined);
    });
    const dialog = await findByRole("dialog");
    expect(dialog.textContent).toContain("net-fetch");
    expect(dialog.textContent).toContain("fs-read");
    expect(dialog.textContent).toContain("not an OS sandbox");
    expect(dialog.textContent).toContain("validated by the AoE maintainers");

    // Phase 2: approval re-sends with confirm_capabilities: true.
    fireEvent.click(await findByText("Approve and continue"));
    await waitFor(() => {
      expect(installPlugin).toHaveBeenCalledWith("o/r", true, "sha256:staged");
    });
    await findByText("Installed new.plugin 1.0.0");
  });

  it("cancelling the capability prompt sends nothing further", async () => {
    installPlugin.mockResolvedValueOnce({
      kind: "prompt",
      prompt: {
        needs_confirmation: true,
        id: "new.plugin",
        name: "New Plugin",
        version: "1.0.0",
        description: "",
        capabilities: ["net-fetch"],
        previous_capabilities: null,
        trust: "community",
        source: "github:o/r",
        featured: "not_featured",
        manifest_hash: "sha256:other",
        isolation_summary: "runs as a regular process",
      },
    });
    const { findByLabelText, findByRole, findByText, queryByRole } = render(<PluginsSettings />);
    fireEvent.change(await findByLabelText("Plugin source"), { target: { value: "o/r" } });
    fireEvent.click(await findByText("Install"));
    await findByRole("dialog");
    fireEvent.click(await findByText("Cancel"));
    await waitFor(() => {
      expect(queryByRole("dialog")).toBeNull();
    });
    expect(installPlugin).toHaveBeenCalledTimes(1);
  });

  it("update button asks unconfirmed first; uninstall requires window.confirm", async () => {
    updatePlugin.mockResolvedValue({ kind: "ok", message: "up to date" });
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(false);
    const { findByText } = render(<PluginsSettings />);

    fireEvent.click(await findByText("Update"));
    await waitFor(() => {
      expect(updatePlugin).toHaveBeenCalledWith("example.plugin", false, undefined);
    });

    fireEvent.click(await findByText("Uninstall"));
    expect(confirmSpy).toHaveBeenCalled();
    expect(uninstallPlugin).not.toHaveBeenCalled();
    confirmSpy.mockRestore();
  });

  it("check for updates renders the available badge from the updates map", async () => {
    fetchPluginUpdates.mockResolvedValue({
      updates: { "example.plugin": { status: "available" } },
    });
    const { findByText, queryByText } = render(<PluginsSettings />);
    expect(queryByText("update available")).toBeNull();
    fireEvent.click(await findByText("Check for updates"));
    await waitFor(() => {
      expect(fetchPluginUpdates).toHaveBeenCalledTimes(1);
    });
    await findByText("update available");
    await findByText("1 plugin can be updated.");
  });

  it("discover runs only on click, badges results, and installs through the two-phase flow", async () => {
    discoverPlugins.mockResolvedValue({
      plugins: [
        { slug: "acme/aoe-review", description: "Curated review plugin", stars: 42, featured: true, installed: false },
        { slug: "rando/aoe-thing", description: null, stars: 3, featured: false, installed: false },
        { slug: "owner/repo", description: "Already here", stars: 7, featured: false, installed: true },
      ],
    });
    installPlugin.mockResolvedValue({ kind: "ok", message: "Installed acme.review 1.0.0" });

    const { findByText, getByTestId, queryByTestId } = render(<PluginsSettings />);
    // Never fetched on load.
    await findByText("Search GitHub");
    expect(discoverPlugins).not.toHaveBeenCalled();

    fireEvent.click(await findByText("Search GitHub"));
    await waitFor(() => {
      expect(discoverPlugins).toHaveBeenCalledTimes(1);
    });

    const featured = getByTestId("discovered-acme/aoe-review");
    expect(featured.textContent).toContain("featured");
    const unvetted = getByTestId("discovered-rando/aoe-thing");
    expect(unvetted.textContent).toContain("unvetted");
    const installed = getByTestId("discovered-owner/repo");
    expect(installed.textContent).toContain("installed");
    // Installed results offer no install button.
    expect(installed.querySelector("button")).toBeNull();
    expect(queryByTestId("discovered-missing")).toBeNull();

    // Installing a result goes through the same unconfirmed-first flow.
    const installButton = featured.querySelector("button");
    expect(installButton).not.toBeNull();
    fireEvent.click(installButton!);
    await waitFor(() => {
      expect(installPlugin).toHaveBeenCalledWith("acme/aoe-review", false, undefined);
    });
  });

  it("load errors are surfaced, not swallowed", async () => {
    fetchPlugins.mockResolvedValue(listResponse({ load_errors: ["plugins/bad: manifest is invalid"] }));
    const { findByText } = render(<PluginsSettings />);
    await findByText(/manifest is invalid/);
  });
});
