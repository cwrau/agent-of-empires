// Cron-scheduled sessions editor (#2886).
//
// Edits the global `scheduling.jobs` array: a list of jobs, each with a friendly
// cron picker (frequency presets that generate a 5-field expression) plus a raw
// cron escape hatch. The raw field is the source of truth on save; presets just
// fill it. The whole array is persisted wholesale through `save(nextJobs)`.
//
// Cron is validated client-side before save (see cronValidation.ts) because the
// server does not re-validate it: an invalid expression would silently never
// fire.

import { useEffect, useState } from "react";

import { DirectoryBrowser } from "../DirectoryBrowser";
import { fetchAcpOptionCatalog, fetchAgents, fetchGroups, fetchProjects } from "../../lib/api";
import type { AgentOptionEntry } from "../../lib/api";
import type { ConfigOptionCategory, ConfigOptionDescriptor } from "../../lib/acpTypes";
import type { AgentInfo, GroupInfo, ProjectInfo } from "../../lib/types";
import { validateCron } from "./cronValidation";
import type { CustomWidgetProps } from "./customWidgets";

/** Recall catalog keyed by agent name (`fetchAcpOptionCatalog().agents`). */
type OptionCatalog = Record<string, AgentOptionEntry>;

interface ScheduledJob {
  id: string;
  name: string;
  schedule: string;
  enabled: boolean;
  tool: string;
  agent?: string;
  model?: string;
  approval_mode?: string;
  project?: string;
  prompt: string;
  group: string;
  owner_profile: string;
}

function asJobs(value: unknown): ScheduledJob[] {
  return Array.isArray(value) ? (value as ScheduledJob[]) : [];
}

type Frequency = "minutes" | "hourly" | "daily" | "weekly" | "custom";

const WEEKDAYS = [
  { value: "1", label: "Monday" },
  { value: "2", label: "Tuesday" },
  { value: "3", label: "Wednesday" },
  { value: "4", label: "Thursday" },
  { value: "5", label: "Friday" },
  { value: "6", label: "Saturday" },
  { value: "0", label: "Sunday" },
];

interface PickerState {
  frequency: Frequency;
  everyN: number; // minutes for "Every N minutes"
  minute: number; // minute-of-hour for "Hourly"
  time: string; // "HH:MM" for daily / weekly
  weekday: string; // dow for weekly
}

const DEFAULT_PICKER: PickerState = {
  frequency: "daily",
  everyN: 30,
  minute: 0,
  time: "08:00",
  weekday: "1",
};

/** Parse an "HH:MM" time input into numeric hour/minute (leading zeros
 *  stripped so the cron reads "0 8 * * *", not "00 08 * * *"). */
function parseTime(time: string): { h: number; m: number } {
  const [hh, mm] = time.split(":");
  return { h: Number(hh) || 0, m: Number(mm) || 0 };
}

/** Derive the picker state that produced `schedule`, so editing an existing job
 *  opens on its real frequency instead of the default Daily. Recognizes the
 *  same shapes `buildCron` emits; anything else falls back to `custom` with the
 *  raw expression left untouched. */
function parsePicker(schedule: string): PickerState {
  const fields = schedule.trim().split(/\s+/);
  const [min, hour, dom, mon, dow] = fields;
  if (fields.length === 5 && min && hour && dom && mon && dow) {
    const everyN = /^\*\/(\d+)$/.exec(min);
    if (everyN && hour === "*" && dom === "*" && mon === "*" && dow === "*") {
      const n = Number(everyN[1]);
      if (n >= 1 && n <= 59) return { ...DEFAULT_PICKER, frequency: "minutes", everyN: n };
    }
    if (dom === "*" && mon === "*") {
      const m = Number(min);
      const minuteOk = /^\d+$/.test(min) && m >= 0 && m <= 59;
      if (minuteOk && hour === "*" && dow === "*") {
        return { ...DEFAULT_PICKER, frequency: "hourly", minute: m };
      }
      const h = Number(hour);
      const hourOk = /^\d+$/.test(hour) && h >= 0 && h <= 23;
      if (minuteOk && hourOk) {
        const time = `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
        if (dow === "*") return { ...DEFAULT_PICKER, frequency: "daily", time };
        if (/^[0-6]$/.test(dow)) return { ...DEFAULT_PICKER, frequency: "weekly", time, weekday: dow };
      }
    }
  }
  return { ...DEFAULT_PICKER, frequency: "custom" };
}

/** Build the 5-field cron string a preset describes. `custom` returns null so
 *  the caller leaves the raw field alone. */
function buildCron(p: PickerState): string | null {
  switch (p.frequency) {
    case "minutes":
      return `*/${p.everyN} * * * *`;
    case "hourly":
      return `${p.minute} * * * *`;
    case "daily": {
      const { h, m } = parseTime(p.time);
      return `${m} ${h} * * *`;
    }
    case "weekly": {
      const { h, m } = parseTime(p.time);
      return `${m} ${h} * * ${p.weekday}`;
    }
    case "custom":
      return null;
  }
}

interface JobDraft {
  id: string | null;
  name: string;
  schedule: string;
  enabled: boolean;
  tool: string;
  model: string;
  approval_mode: string;
  project: string;
  prompt: string;
  group: string;
}

function emptyDraft(): JobDraft {
  return {
    id: null,
    name: "",
    schedule: "0 8 * * *",
    enabled: true,
    tool: "claude",
    model: "",
    approval_mode: "",
    project: "",
    prompt: "",
    group: "Scheduled",
  };
}

function draftFromJob(job: ScheduledJob): JobDraft {
  return {
    id: job.id,
    name: job.name,
    schedule: job.schedule,
    enabled: job.enabled,
    tool: job.tool || "claude",
    model: job.model ?? "",
    approval_mode: job.approval_mode ?? "",
    project: job.project ?? "",
    prompt: job.prompt,
    group: job.group || "Scheduled",
  };
}

const inputCls =
  "w-full bg-surface-900 border border-surface-700 rounded-md px-3 py-2 text-sm text-text-primary placeholder:text-text-dim focus:border-brand-600 focus:outline-none";
const labelCls = "block text-sm text-text-bright mb-1";

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <label className={labelCls}>{label}</label>
      {children}
    </div>
  );
}

/** Agents the wizard treats as selectable: installed built-ins plus any custom
 *  agent (defined by config, not a resolvable binary). Mirrors
 *  AgentPickerEssentials. */
function selectableAgents(agents: AgentInfo[]): AgentInfo[] {
  return agents.filter((a) => a.kind === "custom" || a.installed);
}

/** `<select>` options for the tool/agent pickers: an empty choice, the
 *  selectable agents, and, when the saved value is not among them, an
 *  "(unverified)" entry so a stale or hand-entered name stays selected. */
function agentOptions(agents: AgentInfo[], current: string, emptyLabel: string): { value: string; label: string }[] {
  const opts = [{ value: "", label: emptyLabel }];
  const selectable = selectableAgents(agents);
  for (const a of selectable) opts.push({ value: a.name, label: a.name });
  if (current && !selectable.some((a) => a.name === current)) {
    opts.push({ value: current, label: `${current} (unverified)` });
  }
  return opts;
}

function optionByCategory(
  entry: AgentOptionEntry | undefined,
  category: ConfigOptionCategory,
): ConfigOptionDescriptor | undefined {
  return entry?.options.find((o) => o.category === category);
}

/** `<select>` options for a capability field (model / approval mode): the
 *  "default" empty choice, the advertised choices, and an "(unverified)" entry
 *  preserving a saved value the agent no longer advertises. */
function capabilityOptions(
  descriptor: ConfigOptionDescriptor | undefined,
  saved: string,
  defaultLabel: string,
): { value: string; label: string }[] {
  const opts = [{ value: "", label: defaultLabel }];
  const choices = descriptor?.options ?? [];
  for (const c of choices) opts.push({ value: c.value, label: c.name || c.value });
  if (saved && !choices.some((c) => c.value === saved)) {
    opts.push({ value: saved, label: `${saved} (unverified)` });
  }
  return opts;
}

function SelectRow({
  label,
  ariaLabel,
  value,
  onChange,
  options,
}: {
  label: string;
  ariaLabel: string;
  value: string;
  onChange: (v: string) => void;
  options: { value: string; label: string }[];
}) {
  return (
    <Field label={label}>
      <select aria-label={ariaLabel} value={value} onChange={(e) => onChange(e.target.value)} className={inputCls}>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </Field>
  );
}

/** A capability field is a dropdown when the agent advertised choices for the
 *  category, else a free-text input. Falls back to text when no catalog entry
 *  exists yet (no structured session has run for the agent) so the value stays
 *  editable. */
function CapabilityRow({
  label,
  ariaLabel,
  descriptor,
  value,
  onChange,
  defaultLabel,
  placeholder,
}: {
  label: string;
  ariaLabel: string;
  descriptor: ConfigOptionDescriptor | undefined;
  value: string;
  onChange: (v: string) => void;
  defaultLabel: string;
  placeholder: string;
}) {
  if (descriptor && descriptor.options.length > 0) {
    return (
      <SelectRow
        label={label}
        ariaLabel={ariaLabel}
        value={value}
        onChange={onChange}
        options={capabilityOptions(descriptor, value, defaultLabel)}
      />
    );
  }
  return (
    <Field label={label}>
      <input
        type="text"
        aria-label={ariaLabel}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        className={inputCls + " font-mono"}
      />
    </Field>
  );
}

function GroupRow({ value, onChange, groups }: { value: string; onChange: (v: string) => void; groups: GroupInfo[] }) {
  const names = Array.from(new Set(groups.map((g) => g.path).filter(Boolean)));
  return (
    <Field label="Group">
      <input
        type="text"
        aria-label="Group"
        list="scheduled-job-groups"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder="Scheduled"
        className={inputCls}
      />
      <datalist id="scheduled-job-groups">
        {names.map((n) => (
          <option key={n} value={n} />
        ))}
      </datalist>
    </Field>
  );
}

const BROWSE_SENTINEL = "__browse__";

/** Project picker: scratch (project-less), the registered projects, an
 *  "(unverified)" entry keeping a hand-picked path selected, and a sentinel that
 *  opens the same directory browser the wizard uses. */
function ProjectRow({
  projects,
  value,
  onChange,
}: {
  projects: ProjectInfo[];
  value: string;
  onChange: (v: string) => void;
}) {
  const [browsing, setBrowsing] = useState(false);
  const known = projects.some((p) => p.path === value);
  const options = [{ value: "", label: "Scratch (project-less)" }];
  for (const p of projects) options.push({ value: p.path, label: p.name ? `${p.name} (${p.path})` : p.path });
  if (value && !known) options.push({ value, label: value });
  options.push({ value: BROWSE_SENTINEL, label: "Browse filesystem…" });

  return (
    <Field label="Project (optional)">
      <select
        aria-label="Project"
        value={browsing ? BROWSE_SENTINEL : value}
        onChange={(e) => {
          const v = e.target.value;
          if (v === BROWSE_SENTINEL) {
            setBrowsing(true);
          } else {
            setBrowsing(false);
            onChange(v);
          }
        }}
        className={inputCls}
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      {browsing && (
        <div className="mt-2 rounded-md border border-surface-700 bg-surface-900/50 p-2">
          <DirectoryBrowser
            initialPath={value || undefined}
            onSelect={(path) => {
              onChange(path);
              setBrowsing(false);
            }}
          />
          <button
            type="button"
            onClick={() => setBrowsing(false)}
            className="mt-2 text-xs text-text-dim hover:text-text-primary"
          >
            Cancel browse
          </button>
        </div>
      )}
    </Field>
  );
}

function CronPicker({ schedule, onScheduleChange }: { schedule: string; onScheduleChange: (next: string) => void }) {
  const [picker, setPicker] = useState<PickerState>(() => parsePicker(schedule));

  const apply = (next: PickerState) => {
    setPicker(next);
    const cron = buildCron(next);
    if (cron !== null) onScheduleChange(cron);
  };

  return (
    <div className="space-y-2 rounded-md border border-surface-700 bg-surface-900/50 p-3">
      <Field label="Frequency">
        <select
          aria-label="Frequency"
          value={picker.frequency}
          onChange={(e) => apply({ ...picker, frequency: e.target.value as Frequency })}
          className={inputCls}
        >
          <option value="minutes">Every N minutes</option>
          <option value="hourly">Hourly</option>
          <option value="daily">Daily</option>
          <option value="weekly">Weekly</option>
          <option value="custom">Custom (raw cron)</option>
        </select>
      </Field>

      {picker.frequency === "minutes" && (
        <Field label="Every (minutes)">
          <input
            type="number"
            aria-label="Every N minutes"
            min={1}
            max={59}
            value={picker.everyN}
            onChange={(e) => apply({ ...picker, everyN: Math.max(1, Math.min(59, Number(e.target.value) || 1)) })}
            className={inputCls}
          />
        </Field>
      )}

      {picker.frequency === "hourly" && (
        <Field label="At minute">
          <input
            type="number"
            aria-label="At minute"
            min={0}
            max={59}
            value={picker.minute}
            onChange={(e) => apply({ ...picker, minute: Math.max(0, Math.min(59, Number(e.target.value) || 0)) })}
            className={inputCls}
          />
        </Field>
      )}

      {(picker.frequency === "daily" || picker.frequency === "weekly") && (
        <Field label="Time">
          <input
            type="time"
            aria-label="Time"
            value={picker.time}
            onChange={(e) => apply({ ...picker, time: e.target.value || "00:00" })}
            className={inputCls}
          />
        </Field>
      )}

      {picker.frequency === "weekly" && (
        <Field label="Day of week">
          <select
            aria-label="Day of week"
            value={picker.weekday}
            onChange={(e) => apply({ ...picker, weekday: e.target.value })}
            className={inputCls}
          >
            {WEEKDAYS.map((d) => (
              <option key={d.value} value={d.value}>
                {d.label}
              </option>
            ))}
          </select>
        </Field>
      )}

      <Field label="Cron expression">
        <input
          type="text"
          aria-label="Cron expression"
          value={schedule}
          onChange={(e) => onScheduleChange(e.target.value)}
          placeholder="min hour day month weekday"
          className={inputCls + " font-mono"}
        />
      </Field>
      <p className="text-xs text-text-dim">
        5-field cron, host-local time. Presets fill this in; edit it directly for anything custom.
      </p>
    </div>
  );
}

function JobForm({
  draft,
  agents,
  catalog,
  projects,
  groups,
  onCancel,
  onSubmit,
}: {
  draft: JobDraft;
  agents: AgentInfo[];
  catalog: OptionCatalog;
  projects: ProjectInfo[];
  groups: GroupInfo[];
  onCancel: () => void;
  onSubmit: (draft: JobDraft) => void;
}) {
  const [local, setLocal] = useState<JobDraft>(draft);
  const [error, setError] = useState<string | null>(null);
  const set = (patch: Partial<JobDraft>) => setLocal((prev) => ({ ...prev, ...patch }));

  // Models and approval modes are looked up against the tool, which already
  // selects the runtime (built-in binaries and custom ACP agents alike).
  const agentKey = local.tool.trim();
  const entry = catalog[agentKey];
  const modelDesc = optionByCategory(entry, "model");
  const modeDesc = optionByCategory(entry, "mode");

  const submit = () => {
    if (!local.name.trim()) {
      setError("Name is required.");
      return;
    }
    if (!local.prompt.trim()) {
      setError("Prompt is required.");
      return;
    }
    if (!local.tool.trim()) {
      setError("Tool is required.");
      return;
    }
    const cronError = validateCron(local.schedule);
    if (cronError) {
      setError(cronError);
      return;
    }
    setError(null);
    onSubmit(local);
  };

  return (
    <div className="space-y-3 rounded-md border border-brand-600/40 bg-surface-850 p-3">
      <Field label="Name">
        <input
          type="text"
          aria-label="Name"
          value={local.name}
          onChange={(e) => set({ name: e.target.value })}
          placeholder="Daily PR triage"
          className={inputCls}
        />
      </Field>

      <CronPicker schedule={local.schedule} onScheduleChange={(schedule) => set({ schedule })} />

      <Field label="Prompt">
        <textarea
          aria-label="Prompt"
          value={local.prompt}
          onChange={(e) => set({ prompt: e.target.value })}
          rows={3}
          placeholder="What should the session do?"
          className={inputCls + " resize-y"}
        />
      </Field>

      <div className="grid gap-3 sm:grid-cols-2">
        <SelectRow
          label="Tool"
          ariaLabel="Tool"
          value={local.tool}
          onChange={(v) => set({ tool: v })}
          options={agentOptions(agents, local.tool, "Select a tool…")}
        />
        <CapabilityRow
          label="Model (optional)"
          ariaLabel="Model"
          descriptor={modelDesc}
          value={local.model}
          onChange={(v) => set({ model: v })}
          defaultLabel="Agent default"
          placeholder="Agent default"
        />
        <CapabilityRow
          label="Approval mode (optional)"
          ariaLabel="Approval mode"
          descriptor={modeDesc}
          value={local.approval_mode}
          onChange={(v) => set({ approval_mode: v })}
          defaultLabel="Default (agent bypass)"
          placeholder="Bypass (yolo)"
        />
        <GroupRow value={local.group} onChange={(v) => set({ group: v })} groups={groups} />
      </div>

      <ProjectRow projects={projects} value={local.project} onChange={(v) => set({ project: v })} />

      <label className="flex items-center gap-2 text-sm text-text-primary">
        <input
          type="checkbox"
          aria-label="Enabled"
          checked={local.enabled}
          onChange={(e) => set({ enabled: e.target.checked })}
          className="accent-brand-600"
        />
        Enabled
      </label>

      {error && <div className="text-xs text-status-error">{error}</div>}

      <div className="flex gap-2">
        <button
          type="button"
          onClick={submit}
          className="rounded-md bg-brand-600 px-3 py-1.5 text-sm font-medium text-surface-950 hover:bg-brand-500"
        >
          Save job
        </button>
        <button
          type="button"
          onClick={onCancel}
          className="rounded-md px-3 py-1.5 text-sm text-text-dim hover:text-text-primary"
        >
          Cancel
        </button>
      </div>
    </div>
  );
}

function JobRow({
  job,
  onToggle,
  onEdit,
  onRemove,
}: {
  job: ScheduledJob;
  onToggle: () => void;
  onEdit: () => void;
  onRemove: () => void;
}) {
  return (
    <div className="flex items-center gap-3 rounded-md border border-surface-700 bg-surface-850 p-3">
      <button
        type="button"
        role="switch"
        aria-checked={job.enabled}
        aria-label={`Enable ${job.name}`}
        onClick={onToggle}
        className={`relative inline-flex h-6 w-10 shrink-0 items-center rounded-full transition-colors ${job.enabled ? "bg-brand-600" : "bg-surface-700"}`}
      >
        <span
          className={`inline-block h-4 w-4 rounded-full bg-white shadow-sm transition-transform ${job.enabled ? "translate-x-5" : "translate-x-1"}`}
        />
      </button>
      <div className="min-w-0 flex-1">
        <div className="truncate text-sm text-text-primary">{job.name}</div>
        <div className="truncate text-xs text-text-dim">
          <span className="font-mono">{job.schedule}</span>
          {" · "}
          <span className="font-mono">{job.tool}</span>
        </div>
      </div>
      <button
        type="button"
        onClick={onEdit}
        className="rounded px-2 py-1 text-xs text-text-dim hover:text-text-primary"
      >
        Edit
      </button>
      <button
        type="button"
        aria-label={`Remove ${job.name}`}
        onClick={onRemove}
        className="rounded px-2 py-1 text-xs text-text-dim hover:text-status-error"
      >
        Remove
      </button>
    </div>
  );
}

export function ScheduledJobsWidget({ descriptor, value, save }: CustomWidgetProps) {
  const jobs = asJobs(value);
  // null = no form open; "new" = adding; otherwise the id being edited.
  const [editing, setEditing] = useState<string | null>(null);

  // Picker sources, loaded once: the wizard's agent list (tool/agent), the
  // recall catalog of advertised models/modes per agent, the project registry,
  // and the existing group names. Each degrades gracefully to an empty list so
  // a fetch failure leaves the form usable (text fallbacks / free-typed group).
  const [agents, setAgents] = useState<AgentInfo[]>([]);
  const [catalog, setCatalog] = useState<OptionCatalog>({});
  const [projects, setProjects] = useState<ProjectInfo[]>([]);
  const [groups, setGroups] = useState<GroupInfo[]>([]);

  useEffect(() => {
    let alive = true;
    void (async () => {
      const [a, c, p, g] = await Promise.all([
        fetchAgents().catch(() => [] as AgentInfo[]),
        fetchAcpOptionCatalog().catch(() => ({ version: 1, agents: {} as OptionCatalog })),
        fetchProjects().catch(() => [] as ProjectInfo[]),
        fetchGroups().catch(() => [] as GroupInfo[]),
      ]);
      if (!alive) return;
      setAgents(a);
      setCatalog(c.agents ?? {});
      setProjects(p);
      setGroups(g);
    })();
    return () => {
      alive = false;
    };
  }, []);

  const persist = async (next: ScheduledJob[]) => (await save(next)) !== false;

  const submitDraft = async (draft: JobDraft) => {
    const base = {
      name: draft.name.trim(),
      schedule: draft.schedule.trim(),
      enabled: draft.enabled,
      tool: draft.tool.trim(),
      model: draft.model.trim() || undefined,
      approval_mode: draft.approval_mode.trim() || undefined,
      project: draft.project.trim() || undefined,
      prompt: draft.prompt.trim(),
      group: draft.group.trim() || "Scheduled",
    };
    if (draft.id === null) {
      const job: ScheduledJob = { id: crypto.randomUUID(), owner_profile: "", ...base };
      if (await persist([...jobs, job])) setEditing(null);
    } else {
      const existing = jobs.find((j) => j.id === draft.id);
      const job: ScheduledJob = {
        id: draft.id,
        owner_profile: existing?.owner_profile ?? "",
        ...base,
      };
      if (await persist(jobs.map((j) => (j.id === draft.id ? job : j)))) setEditing(null);
    }
  };

  const toggle = (id: string) => persist(jobs.map((j) => (j.id === id ? { ...j, enabled: !j.enabled } : j)));
  const remove = (id: string) => {
    persist(jobs.filter((j) => j.id !== id));
    if (editing === id) setEditing(null);
  };

  const editingJob = editing && editing !== "new" ? jobs.find((j) => j.id === editing) : undefined;

  return (
    <div className="space-y-3">
      <div>
        <label className="block text-sm text-text-bright">{descriptor.label}</label>
        {descriptor.description && <div className="mt-0.5 text-xs text-text-dim">{descriptor.description}</div>}
      </div>

      {jobs.length === 0 && editing !== "new" && <p className="text-xs text-text-dim">No scheduled jobs yet.</p>}

      {jobs.map((job) =>
        editing === job.id ? (
          <JobForm
            key={job.id}
            draft={draftFromJob(job)}
            agents={agents}
            catalog={catalog}
            projects={projects}
            groups={groups}
            onCancel={() => setEditing(null)}
            onSubmit={submitDraft}
          />
        ) : (
          <JobRow
            key={job.id}
            job={job}
            onToggle={() => toggle(job.id)}
            onEdit={() => setEditing(job.id)}
            onRemove={() => remove(job.id)}
          />
        ),
      )}

      {editing === "new" ? (
        <JobForm
          draft={emptyDraft()}
          agents={agents}
          catalog={catalog}
          projects={projects}
          groups={groups}
          onCancel={() => setEditing(null)}
          onSubmit={submitDraft}
        />
      ) : (
        !editingJob && (
          <button
            type="button"
            onClick={() => setEditing("new")}
            className="rounded-md border border-surface-700 px-3 py-1.5 text-sm text-brand-500 hover:text-brand-400"
          >
            + Add scheduled job
          </button>
        )
      )}
    </div>
  );
}
