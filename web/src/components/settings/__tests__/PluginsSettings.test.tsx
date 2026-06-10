// @vitest-environment jsdom
//
// Contract test for the PluginsSettings management panel (#268): the enable
// toggle emits the right setPluginEnabled payload, the two-phase install
// renders the 409 capability prompt without writing, approval re-sends with
// confirm_capabilities, and uninstall asks for confirmation first.

import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, waitFor } from "@testing-library/react";

import type { PluginListResponse, PluginMutationResult, PluginUpdateStatus } from "../../../lib/api";

const fetchPlugins = vi.fn<[], Promise<PluginListResponse | null>>();
const setPluginEnabled = vi.fn<[string, boolean], Promise<boolean>>();
const installPlugin = vi.fn<[string, boolean], Promise<PluginMutationResult>>();
const updatePlugin = vi.fn<[string, boolean], Promise<PluginMutationResult>>();
const uninstallPlugin = vi.fn<[string], Promise<boolean>>();
const fetchPluginUpdates = vi.fn<[], Promise<{ updates: Record<string, PluginUpdateStatus> } | null>>();

vi.mock("../../../lib/api", () => ({
  fetchPlugins: () => fetchPlugins(),
  setPluginEnabled: (id: string, enabled: boolean) => setPluginEnabled(id, enabled),
  installPlugin: (source: string, confirm: boolean) => installPlugin(source, confirm),
  updatePlugin: (id: string, confirm: boolean) => updatePlugin(id, confirm),
  uninstallPlugin: (id: string) => uninstallPlugin(id),
  fetchPluginUpdates: () => fetchPluginUpdates(),
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
      expect(installPlugin).toHaveBeenCalledWith("o/r", false);
    });
    const dialog = await findByRole("dialog");
    expect(dialog.textContent).toContain("net-fetch");
    expect(dialog.textContent).toContain("fs-read");
    expect(dialog.textContent).toContain("not an OS sandbox");
    expect(dialog.textContent).toContain("validated by the AoE maintainers");

    // Phase 2: approval re-sends with confirm_capabilities: true.
    fireEvent.click(await findByText("Approve and continue"));
    await waitFor(() => {
      expect(installPlugin).toHaveBeenCalledWith("o/r", true);
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
      expect(updatePlugin).toHaveBeenCalledWith("example.plugin", false);
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

  it("load errors are surfaced, not swallowed", async () => {
    fetchPlugins.mockResolvedValue(listResponse({ load_errors: ["plugins/bad: manifest is invalid"] }));
    const { findByText } = render(<PluginsSettings />);
    await findByText(/manifest is invalid/);
  });
});
