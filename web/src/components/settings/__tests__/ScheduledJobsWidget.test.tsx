// @vitest-environment jsdom
//
// Behavioral coverage for the scheduled-jobs settings widget (#2886): the cron
// picker generates a 5-field expression from a preset, adding a job persists the
// full array with a generated id and the built cron, and an invalid raw cron
// blocks the save with an inline error.
//
// The tool / model / approval-mode / project fields are capability-driven
// pickers (#2887): the tool comes from the wizard's agent list
// (GET /api/agents) and already selects the runtime (built-in binaries and
// custom ACP agents alike), model and approval mode from the recall catalog the
// tool advertised (GET /api/acp/option-catalog), the project from the registry
// (GET /api/projects) plus the shared directory browser, and the group from a
// datalist of existing groups (GET /api/groups) that still accepts a new name.
// All four sources are mocked here the way the sibling AcpDefaultsWidget test
// mocks the api module.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { ScheduledJobsWidget } from "../ScheduledJobsWidget";
import { validateCron } from "../cronValidation";
import * as api from "../../../lib/api";
import type { AgentInfo } from "../../../lib/types";

vi.mock("../../../lib/api", () => ({
  fetchAgents: vi.fn(),
  fetchAcpOptionCatalog: vi.fn(),
  fetchProjects: vi.fn(),
  fetchGroups: vi.fn(),
  getHomePath: vi.fn(),
  browseFilesystem: vi.fn(),
}));

function agent(name: string, over: Partial<AgentInfo> = {}): AgentInfo {
  return {
    name,
    kind: "builtin",
    binary: name,
    host_only: false,
    installed: true,
    install_hint: "",
    acp_capable: true,
    acp_installed: true,
    ...over,
  };
}

// codex advertises model + mode choices; claude has no catalog entry, so its
// model / approval-mode fields degrade to free text.
const CATALOG = {
  version: 1,
  agents: {
    codex: {
      updated_at: "2026-01-01T00:00:00Z",
      options: [
        {
          id: "model",
          name: "Model",
          category: "model" as const,
          current_value: "",
          options: [
            { value: "gpt-5", name: "GPT-5" },
            { value: "gpt-5-mini", name: "GPT-5 mini" },
          ],
        },
        {
          id: "mode",
          name: "Mode",
          category: "mode" as const,
          current_value: "",
          options: [
            { value: "plan", name: "Plan" },
            { value: "yolo", name: "Yolo" },
          ],
        },
      ],
    },
  },
};

const DESCRIPTOR = {
  section: "scheduling",
  field: "jobs",
  category: "Scheduling",
  label: "Scheduled Jobs",
  description: "",
  widget: { kind: "custom" as const, id: "scheduled-jobs" },
  web_write: { policy: "allow" as const },
  profile_overridable: false,
  validation: { rule: "none" as const },
  advanced: false,
};

beforeEach(() => {
  vi.spyOn(crypto, "randomUUID").mockReturnValue("00000000-0000-0000-0000-000000000000");
  vi.mocked(api.fetchAgents).mockResolvedValue([
    agent("claude"),
    agent("codex"),
    agent("gemini", { installed: false }),
  ]);
  vi.mocked(api.fetchAcpOptionCatalog).mockResolvedValue(CATALOG);
  vi.mocked(api.fetchProjects).mockResolvedValue([{ name: "repo", path: "/repo", scope: "global", pinned: false }]);
  vi.mocked(api.fetchGroups).mockResolvedValue([
    { path: "Nightly", session_count: 2 },
    { path: "Scheduled", session_count: 1 },
  ]);
  vi.mocked(api.getHomePath).mockResolvedValue("/home");
  vi.mocked(api.browseFilesystem).mockResolvedValue({
    entries: [{ name: "myrepo", path: "/home/myrepo", is_dir: true, is_git_repo: true }],
    has_more: false,
    ok: true,
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("validateCron", () => {
  it("accepts picker-generated expressions", () => {
    expect(validateCron("0 8 * * *")).toBeNull();
    expect(validateCron("*/30 * * * *")).toBeNull();
    expect(validateCron("30 9 * * 1")).toBeNull();
  });

  it("rejects wrong field count and out-of-range values", () => {
    expect(validateCron("0 8 * *")).toMatch(/exactly 5 fields/);
    expect(validateCron("99 8 * * *")).toMatch(/minute/);
    expect(validateCron("0 25 * * *")).toMatch(/hour/);
  });
});

it("cron picker generates the cron string for the Daily preset", () => {
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={vi.fn()} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  const cron = screen.getByLabelText("Cron expression") as HTMLInputElement;
  const time = screen.getByLabelText("Time") as HTMLInputElement;

  // Prove the picker drives the raw field, not just the seed default.
  fireEvent.change(time, { target: { value: "23:59" } });
  expect(cron.value).toBe("59 23 * * *");

  fireEvent.change(time, { target: { value: "08:00" } });
  expect(cron.value).toBe("0 8 * * *");
});

it("Weekly preset generates a day-of-week cron", () => {
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={vi.fn()} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  fireEvent.change(screen.getByLabelText("Frequency"), { target: { value: "weekly" } });
  fireEvent.change(screen.getByLabelText("Time"), { target: { value: "09:30" } });
  fireEvent.change(screen.getByLabelText("Day of week"), { target: { value: "1" } });

  expect((screen.getByLabelText("Cron expression") as HTMLInputElement).value).toBe("30 9 * * 1");
});

it("opens an existing weekly job on its own frequency and preserves the schedule", () => {
  const save = vi.fn();
  const job = {
    id: "job-1",
    owner_profile: "",
    name: "Weekly report",
    schedule: "30 9 * * 1",
    enabled: true,
    tool: "claude",
    prompt: "summarize",
    group: "Scheduled",
  };
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[job]} save={save} />);

  fireEvent.click(screen.getByText("Edit"));

  // The picker opens on Weekly (not the default Daily), so weekly-only controls
  // are present and the raw cron is untouched.
  expect((screen.getByLabelText("Frequency") as HTMLSelectElement).value).toBe("weekly");
  expect((screen.getByLabelText("Day of week") as HTMLSelectElement).value).toBe("1");
  expect((screen.getByLabelText("Cron expression") as HTMLInputElement).value).toBe("30 9 * * 1");

  // Editing an unrelated field does not silently rewrite the schedule as daily.
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Renamed" } });
  fireEvent.click(screen.getByText("Save job"));

  expect(save).toHaveBeenCalledWith([{ ...job, name: "Renamed" }]);
});

it("adds a job and saves the full array with a generated id and built cron", () => {
  const save = vi.fn();
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={save} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Daily triage" } });
  fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "Review open PRs" } });
  fireEvent.change(screen.getByLabelText("Time"), { target: { value: "08:00" } });

  fireEvent.click(screen.getByText("Save job"));

  expect(save).toHaveBeenCalledTimes(1);
  expect(save).toHaveBeenCalledWith([
    {
      id: "00000000-0000-0000-0000-000000000000",
      owner_profile: "",
      name: "Daily triage",
      schedule: "0 8 * * *",
      enabled: true,
      tool: "claude",
      prompt: "Review open PRs",
      group: "Scheduled",
    },
  ]);
});

it("keeps the editor open when save is rejected and closes it once save succeeds", async () => {
  const save = vi.fn().mockResolvedValueOnce(false).mockResolvedValueOnce(true);
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={save} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Daily triage" } });
  fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "Review open PRs" } });

  // A rejected save keeps the form open with the draft intact, so the entry is
  // not lost.
  fireEvent.click(screen.getByText("Save job"));
  await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
  expect(screen.getByText("Save job")).toBeTruthy();
  expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("Daily triage");

  // A successful save closes the editor back to the list.
  fireEvent.click(screen.getByText("Save job"));
  await waitFor(() => expect(screen.queryByText("Save job")).toBeNull());
  expect(screen.getByText("+ Add scheduled job")).toBeTruthy();
});

it("blocks save and shows an error for an invalid raw cron", () => {
  const save = vi.fn();
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={save} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Broken" } });
  fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "do it" } });
  fireEvent.change(screen.getByLabelText("Cron expression"), { target: { value: "99 8 * * *" } });

  fireEvent.click(screen.getByText("Save job"));

  expect(save).not.toHaveBeenCalled();
  expect(screen.getByText(/Invalid minute field/)).toBeTruthy();
});

it("toggles a job's enabled flag and persists the whole array", () => {
  const save = vi.fn();
  const job = {
    id: "job-1",
    owner_profile: "",
    name: "Nightly",
    schedule: "0 3 * * *",
    enabled: true,
    tool: "claude",
    prompt: "run",
    group: "Scheduled",
  };
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[job]} save={save} />);

  fireEvent.click(screen.getByLabelText("Enable Nightly"));
  expect(save).toHaveBeenCalledWith([{ ...job, enabled: false }]);
});

it("Every-N-minutes preset generates a step cron and reflects the input", () => {
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={vi.fn()} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  fireEvent.change(screen.getByLabelText("Frequency"), { target: { value: "minutes" } });
  const everyN = screen.getByLabelText("Every N minutes") as HTMLInputElement;
  fireEvent.change(everyN, { target: { value: "15" } });

  expect((screen.getByLabelText("Cron expression") as HTMLInputElement).value).toBe("*/15 * * * *");
});

it("Hourly preset generates a minute-of-hour cron and reflects the input", () => {
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={vi.fn()} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  fireEvent.change(screen.getByLabelText("Frequency"), { target: { value: "hourly" } });
  fireEvent.change(screen.getByLabelText("At minute"), { target: { value: "45" } });

  expect((screen.getByLabelText("Cron expression") as HTMLInputElement).value).toBe("45 * * * *");
});

it("opens an existing every-N-minutes job on the minutes frequency", () => {
  const job = {
    id: "job-1",
    owner_profile: "",
    name: "Poll",
    schedule: "*/10 * * * *",
    enabled: true,
    tool: "claude",
    prompt: "poll",
    group: "Scheduled",
  };
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[job]} save={vi.fn()} />);
  fireEvent.click(screen.getByText("Edit"));

  expect((screen.getByLabelText("Frequency") as HTMLSelectElement).value).toBe("minutes");
  expect((screen.getByLabelText("Every N minutes") as HTMLInputElement).value).toBe("10");
});

it("opens an existing hourly job on the hourly frequency", () => {
  const job = {
    id: "job-1",
    owner_profile: "",
    name: "Hourly",
    schedule: "20 * * * *",
    enabled: true,
    tool: "claude",
    prompt: "run",
    group: "Scheduled",
  };
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[job]} save={vi.fn()} />);
  fireEvent.click(screen.getByText("Edit"));

  expect((screen.getByLabelText("Frequency") as HTMLSelectElement).value).toBe("hourly");
  expect((screen.getByLabelText("At minute") as HTMLInputElement).value).toBe("20");
});

it("falls back to custom for an unrecognized cron and leaves the raw field untouched", () => {
  const job = {
    id: "job-1",
    owner_profile: "",
    name: "Monthly",
    schedule: "0 8 1 * *",
    enabled: true,
    tool: "claude",
    prompt: "run",
    group: "Scheduled",
  };
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[job]} save={vi.fn()} />);
  fireEvent.click(screen.getByText("Edit"));

  expect((screen.getByLabelText("Frequency") as HTMLSelectElement).value).toBe("custom");
  expect((screen.getByLabelText("Cron expression") as HTMLInputElement).value).toBe("0 8 1 * *");

  // Re-selecting "custom" runs buildCron's custom arm, which returns null so the
  // raw expression is deliberately left as-is.
  fireEvent.change(screen.getByLabelText("Frequency"), { target: { value: "custom" } });
  expect((screen.getByLabelText("Cron expression") as HTMLInputElement).value).toBe("0 8 1 * *");
});

it("requires name, prompt, and tool before saving", () => {
  const save = vi.fn();
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={save} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  fireEvent.click(screen.getByText("Save job"));
  expect(screen.getByText("Name is required.")).toBeTruthy();

  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Job" } });
  fireEvent.click(screen.getByText("Save job"));
  expect(screen.getByText("Prompt is required.")).toBeTruthy();

  fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "do it" } });
  // The tool picker always carries the empty "Select a tool…" choice, so a user
  // can clear it and the required-field guard still fires.
  fireEvent.change(screen.getByLabelText("Tool"), { target: { value: "" } });
  fireEvent.click(screen.getByText("Save job"));
  expect(screen.getByText("Tool is required.")).toBeTruthy();

  expect(save).not.toHaveBeenCalled();
});

it("selecting a tool swaps its capability fields to catalog-driven dropdowns", async () => {
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={vi.fn()} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  // claude has no cached options, so model/approval start as free-text inputs.
  await waitFor(() => expect((screen.getByLabelText("Tool") as HTMLSelectElement).value).toBe("claude"));
  expect((screen.getByLabelText("Model") as HTMLElement).tagName).toBe("INPUT");
  expect((screen.getByLabelText("Approval mode") as HTMLElement).tagName).toBe("INPUT");

  // Switching to codex (which advertised choices) turns them into dropdowns.
  fireEvent.change(screen.getByLabelText("Tool"), { target: { value: "codex" } });

  const model = screen.getByLabelText("Model") as HTMLSelectElement;
  const mode = screen.getByLabelText("Approval mode") as HTMLSelectElement;
  expect(model.tagName).toBe("SELECT");
  expect(mode.tagName).toBe("SELECT");
  expect(within(model).getByRole("option", { name: "GPT-5" })).toBeTruthy();
  expect(within(model).getByRole("option", { name: "Agent default" })).toBeTruthy();
  expect(within(mode).getByRole("option", { name: "Plan" })).toBeTruthy();
  expect(within(mode).getByRole("option", { name: "Default (agent bypass)" })).toBeTruthy();
});

it("persists an advertised model/mode, a registered project, and a chosen group", async () => {
  const save = vi.fn();
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={save} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Full job" } });
  fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "work" } });

  // Wait for the agent list to load, then drive the capability pickers.
  await waitFor(() =>
    expect(within(screen.getByLabelText("Tool") as HTMLElement).getByRole("option", { name: "codex" })).toBeTruthy(),
  );
  fireEvent.change(screen.getByLabelText("Tool"), { target: { value: "codex" } });
  fireEvent.change(screen.getByLabelText("Model"), { target: { value: "gpt-5" } });
  fireEvent.change(screen.getByLabelText("Approval mode"), { target: { value: "plan" } });

  await screen.findByRole("option", { name: "repo (/repo)" });
  fireEvent.change(screen.getByLabelText("Project"), { target: { value: "/repo" } });

  fireEvent.change(screen.getByLabelText("Group"), { target: { value: "Nightly" } });
  fireEvent.click(screen.getByLabelText("Enabled"));

  fireEvent.click(screen.getByText("Save job"));

  expect(save).toHaveBeenCalledWith([
    {
      id: "00000000-0000-0000-0000-000000000000",
      owner_profile: "",
      name: "Full job",
      schedule: "0 8 * * *",
      enabled: false,
      tool: "codex",
      model: "gpt-5",
      approval_mode: "plan",
      project: "/repo",
      prompt: "work",
      group: "Nightly",
    },
  ]);
});

it("omits blank optional fields and defaults the group when project is left as scratch", () => {
  const save = vi.fn();
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={save} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Minimal" } });
  fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "go" } });
  fireEvent.change(screen.getByLabelText("Group"), { target: { value: "" } });

  fireEvent.click(screen.getByText("Save job"));

  const [[persisted]] = save.mock.calls;
  const job = persisted[0];
  expect(job.project).toBeUndefined();
  expect(job.model).toBeUndefined();
  expect(job.approval_mode).toBeUndefined();
  expect(job.group).toBe("Scheduled");
});

it("suggests existing groups in a datalist while still accepting a new name", async () => {
  const save = vi.fn();
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={save} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  // The datalist offers the existing group names once /api/groups resolves.
  await waitFor(() => {
    const opts = Array.from(document.querySelectorAll("#scheduled-job-groups option")).map(
      (o) => (o as HTMLOptionElement).value,
    );
    expect(opts).toContain("Nightly");
    expect(opts).toContain("Scheduled");
  });

  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Custom group job" } });
  fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "go" } });
  // A brand-new name that is not in the suggestions is still accepted.
  fireEvent.change(screen.getByLabelText("Group"), { target: { value: "Brand New" } });

  fireEvent.click(screen.getByText("Save job"));

  const [[persisted]] = save.mock.calls;
  expect(persisted[0].group).toBe("Brand New");
});

it("browses the filesystem to select a project directory", async () => {
  const save = vi.fn();
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={save} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Browsed" } });
  fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "go" } });

  // Opening the browse sentinel mounts the shared directory browser.
  fireEvent.change(screen.getByLabelText("Project"), { target: { value: "__browse__" } });
  const repo = await screen.findByText("myrepo");
  fireEvent.click(repo);

  // Selecting a repo closes the browser and stamps the path onto the job.
  await waitFor(() => expect(screen.queryByText("myrepo")).toBeNull());
  fireEvent.click(screen.getByText("Save job"));

  const [[persisted]] = save.mock.calls;
  expect(persisted[0].project).toBe("/home/myrepo");
});

it("edits an existing job and persists the update in place", () => {
  const save = vi.fn();
  const job = {
    id: "job-1",
    owner_profile: "profile-a",
    name: "Old name",
    schedule: "0 8 * * *",
    enabled: true,
    tool: "claude",
    prompt: "old prompt",
    group: "Scheduled",
  };
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[job]} save={save} />);

  fireEvent.click(screen.getByText("Edit"));
  fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "new prompt" } });
  fireEvent.click(screen.getByText("Save job"));

  expect(save).toHaveBeenCalledWith([{ ...job, prompt: "new prompt" }]);
});

it("removes a job and persists the array without it", () => {
  const save = vi.fn();
  const jobs = [
    {
      id: "job-1",
      owner_profile: "",
      name: "First",
      schedule: "0 8 * * *",
      enabled: true,
      tool: "claude",
      prompt: "a",
      group: "Scheduled",
    },
    {
      id: "job-2",
      owner_profile: "",
      name: "Second",
      schedule: "0 9 * * *",
      enabled: true,
      tool: "claude",
      prompt: "b",
      group: "Scheduled",
    },
  ];
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={jobs} save={save} />);

  fireEvent.click(screen.getByLabelText("Remove First"));
  expect(save).toHaveBeenCalledWith([jobs[1]]);
});
