// @vitest-environment jsdom
//
// Behavioral coverage for the scheduled-jobs settings widget (#2886): the cron
// picker generates a 5-field expression from a preset, adding a job persists the
// full array with a generated id and the built cron, and an invalid raw cron
// blocks the save with an inline error.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ScheduledJobsWidget } from "../ScheduledJobsWidget";
import { validateCron } from "../cronValidation";

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
  fireEvent.change(screen.getByLabelText("Tool"), { target: { value: "" } });
  fireEvent.click(screen.getByText("Save job"));
  expect(screen.getByText("Tool is required.")).toBeTruthy();

  expect(save).not.toHaveBeenCalled();
});

it("persists every optional field and the in-form enabled toggle", () => {
  const save = vi.fn();
  render(<ScheduledJobsWidget descriptor={DESCRIPTOR} value={[]} save={save} />);
  fireEvent.click(screen.getByText("+ Add scheduled job"));

  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Full job" } });
  fireEvent.change(screen.getByLabelText("Prompt"), { target: { value: "work" } });
  fireEvent.change(screen.getByLabelText("Tool"), { target: { value: "codex" } });
  fireEvent.change(screen.getByLabelText("Agent"), { target: { value: "codex-agent" } });
  fireEvent.change(screen.getByLabelText("Model"), { target: { value: "gpt-5" } });
  fireEvent.change(screen.getByLabelText("Approval mode"), { target: { value: "yolo" } });
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
      agent: "codex-agent",
      model: "gpt-5",
      approval_mode: "yolo",
      project: "/repo",
      prompt: "work",
      group: "Nightly",
    },
  ]);
});

it("omits blank optional fields and defaults the group when project is left empty", () => {
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
  expect(job.agent).toBeUndefined();
  expect(job.group).toBe("Scheduled");
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
