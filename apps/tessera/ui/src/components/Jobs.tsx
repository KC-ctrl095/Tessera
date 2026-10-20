// Scheduled tasks: list + editor, shared by the scheduled task's own view and the
// bot editor's "Duties" section (a duty is a job with `bot_id`).
import { For, Show, createEffect, createMemo, createSignal, onCleanup, type JSX } from "solid-js";
import "../styles/settings.css";
import type { Bot } from "../proto/Bot";
import type { Job } from "../proto/Job";
import type { PermissionMode } from "../proto/PermissionMode";
import { confirmYolo, deleteJob, permissionInfo, permissionOptions, runJobNow } from "../lib/actions";
import { api, botById, state } from "../lib/store";
import { browseDir, errorMessage, setBotEditor, setSettingsTab, toast, toastError } from "../lib/ui";
import { relTime, tildify } from "../lib/util";
import { openSession } from "../lib/workspace";
import { BotAvatar } from "./bots";
import { avatarGlyph } from "../avatars/value";
import { Icon, Spinner } from "./icons";
import { Segmented, Toggle } from "./primitives";
import { SchedulePicker } from "./SchedulePicker";

export function validSchedule(s: string): boolean {
  const v = s.trim();
  // As the server reads schedules (crates/core/src/jobs.rs `next_run`).
  if (/^@(hourly|daily|midnight|weekly|monthly)$/.test(v)) return true;
  const every = /^@every\s*(\d+)\s*(s|secs?|m|mins?|h|hr|hours?|d|days?)?$/i.exec(v);
  if (every) {
    const unit = (every[2] ?? "m").toLowerCase()[0];
    return Number(every[1]) * { s: 1, m: 60, h: 3600, d: 86400 }[unit as "s" | "m" | "h" | "d"] >= 60;
  }
  const parts = v.split(/\s+/);
  return (parts.length === 5 || parts.length === 6) && parts.every((p) => /^[\d*/,\-?LW#A-Za-z]+$/.test(p));
}

const DAYS: Record<string, string> = {
  "*": "Every day",
  "1-5": "Every weekday",
  "0,6": "Weekends",
  "6,0": "Weekends",
  "0": "Sundays",
  "1": "Mondays",
  "2": "Tuesdays",
  "3": "Wednesdays",
  "4": "Thursdays",
  "5": "Fridays",
  "6": "Saturdays",
  "7": "Sundays",
};

/** "Every weekday at 08:00", "Every 30 minutes", … */
export function describeSchedule(s: string): string {
  const v = s.trim();
  const every = v.match(/^@every\s+(\d+)(s|m|h|d)$/);
  if (every) {
    const word = { s: "second", m: "minute", h: "hour", d: "day" }[every[2] as "s"];
    return every[1] === "1" ? `Every ${word}` : `Every ${every[1]} ${word}s`;
  }
  if (v === "0 * * * *" || v === "@hourly") return "Every hour";
  const m = v.match(/^(\d+) (\d+) \* \* (\S+)$/);
  if (m && DAYS[m[3]]) return `${DAYS[m[3]]} at ${m[2].padStart(2, "0")}:${m[1].padStart(2, "0")}`;
  return validSchedule(v) ? "Custom cron schedule" : "Invalid schedule";
}

export function emptyJob(bot?: Bot): Job {
  return {
    id: "",
    name: "",
    schedule: bot ? "0 8 * * 1-5" : "0 9 * * *",
    prompt: "",
    cwd: bot?.cwd ?? state.server?.home ?? "",
    enabled: true,
    permission: bot?.permission ?? "default",
    ...(bot ? { bot_id: bot.id } : {}),
  };
}

/** How a run went, in plain words (`Job.last_status`). */
export const RUN_WORD: Record<string, string> = {
  ok: "finished",
  error: "failed",
  running: "running",
  stopped: "stopped",
  paused: "paused",
  interrupted: "interrupted",
};

/** A local date input's value for a time, and back (the end of that day). */
const dateValue = (ms?: number) => {
  if (!ms) return "";
  const d = new Date(ms - 1);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
};
export const nextLocalMidnight = (v: string) => {
  const [y, m, d] = v.split("-").map(Number);
  return new Date(y, m - 1, d + 1, 0, 0, 0).getTime();
};

/** How its last run went, its limits and why it ended (list rows and its own view). */
export function JobRunState(props: { job: Job }) {
  const j = () => props.job;
  return (
    <>
      <Show when={j().last_run}>
        <span class="job-last">
          Last {relTime(j().last_run!)}
          <Show when={j().last_status}>
            {" "}
            ·{" "}
            <span classList={{ "ok-text": j().last_status === "ok", "danger-text": ["error", "paused", "interrupted"].includes(j().last_status!) }}>
              {RUN_WORD[j().last_status!] ?? j().last_status}
            </span>
          </Show>
        </span>
      </Show>
      <Show when={j().max_runs}>
        <span class="job-limit">
          Run {j().runs ?? 0} of {j().max_runs}
        </span>
      </Show>
      <Show when={j().until}>
        <span class="job-limit">Until {new Date(j().until! - 1).toLocaleDateString(undefined, { month: "short", day: "numeric" })}</span>
      </Show>
      <Show when={j().ended && !j().enabled}>
        <span class="job-ended">Ended: {j().ended}</span>
      </Show>
    </>
  );
}

/**
 * Jobs list with toggle, run now, edit and delete. With `local` the jobs are duties still held in a
 * form (their bot isn't saved yet, or they couldn't be created): toggling and removing change that
 * list instead of the server, and there is nothing to run. `errors` (by job id) says why one failed.
 */
export function JobList(props: {
  jobs: Job[];
  onEdit: (j: Job) => void;
  empty: JSX.Element;
  showBot?: boolean;
  local?: { onToggle: (j: Job, enabled: boolean) => void; onRemove: (j: Job) => void };
  errors?: Record<string, string>;
}) {
  const [running, setRunning] = createSignal<string | null>(null);

  const toggle = async (j: Job, enabled: boolean) => {
    if (props.local) return props.local.onToggle(j, enabled);
    try {
      await api.saveJob({ ...j, enabled });
    } catch (e) {
      toastError(e);
    }
  };
  const runNow = async (j: Job) => {
    setRunning(j.id);
    await runJobNow(j);
    setRunning(null);
  };
  const remove = async (j: Job) => {
    if (props.local) return props.local.onRemove(j);
    await deleteJob(j);
  };

  return (
    <Show when={props.jobs.length} fallback={props.empty}>
      <For each={props.jobs}>
        {(j) => (
          <div class="card job" classList={{ local: !!props.local }}>
            <div class="job-head">
              <Toggle checked={j.enabled} onChange={(v) => toggle(j, v)} label={`Enable ${j.name}`} />
              <Show when={props.showBot && botById(j.bot_id)}>
                <BotAvatar bot={botById(j.bot_id)} size={22} title={`Runs as ${botById(j.bot_id)!.name}`} />
              </Show>
              <div class="job-info">
                <div class="strong">{j.name}</div>
                <div class="muted small">
                  {describeSchedule(j.schedule)} · <span class="mono">{j.schedule}</span>
                  <Show when={j.permission === "auto"}>
                    {" "}
                    · <span class="yolo-text">YOLO</span>
                  </Show>
                </div>
              </div>
              <div class="job-actions">
                <Show when={!props.local}>
                  <button type="button" class="btn btn-sm" disabled={running() === j.id} onClick={() => runNow(j)}>
                    <Show when={running() === j.id} fallback={<Icon name="play" size={14} />}>
                      <Spinner size={13} />
                    </Show>
                    Run now
                  </button>
                </Show>
                <button type="button" class="icon-btn" aria-label={`Edit ${j.name}`} onClick={() => props.onEdit({ ...j })}>
                  <Icon name="pencil" size={16} />
                </button>
                <button type="button" class="icon-btn danger-text" aria-label={`Delete ${j.name}`} onClick={() => remove(j)}>
                  <Icon name="trash" size={16} />
                </button>
              </div>
            </div>
            <p class="job-prompt">{j.prompt}</p>
            <div class="job-meta muted small">
              <span>
                <Icon name="folder" size={12} /> {tildify(j.cwd, state.server?.home) || "default"}
              </span>
              <Show when={j.next_run && j.enabled}>
                <span>Next {relTime(j.next_run!)}</span>
              </Show>
              <JobRunState job={j} />
              <Show when={j.last_session}>
                <button
                  type="button"
                  class="link-btn"
                  onClick={() => {
                    setSettingsTab(null);
                    setBotEditor(null);
                    openSession(j.last_session!, { newPane: true });
                  }}
                >
                  Open last run
                </button>
              </Show>
            </div>
            <Show when={props.errors?.[j.id]}>
              <div class="form-error job-error" role="alert">
                {props.errors![j.id]}
              </div>
            </Show>
          </div>
        )}
      </For>
    </Show>
  );
}

/**
 * Create or edit a job. With `lockBot` the "Run as" choice is fixed (bot duties). With `onSubmit` the
 * checked job goes there instead of to the server (a duty drafted before its bot exists; throw to
 * show a problem in the form). `bot` names a bot that isn't saved yet; `onDirty` reports unsent edits.
 */
export function JobEditor(props: {
  job: Job;
  onDone: () => void;
  lockBot?: boolean;
  onSubmit?: (job: Job) => void | Promise<void>;
  saveLabel?: string;
  bot?: { name: string };
  error?: string;
  onDirty?: (dirty: boolean) => void;
}) {
  const [j, setJ] = createSignal<Job>({ ...props.job });
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(props.error ?? null);
  const set = <K extends keyof Job>(k: K, v: Job[K]) => setJ((x) => ({ ...x, [k]: v }));
  const models = createMemo(() => state.providers.filter((p) => p.authenticated).flatMap((p) => p.models.map((m) => ({ p, m }))));
  const bot = () => botById(j().bot_id) ?? props.bot;
  const initial = JSON.stringify(props.job);
  createEffect(() => props.onDirty?.(JSON.stringify(j()) !== initial));
  onCleanup(() => props.onDirty?.(false));

  const save = async (e: Event) => {
    e.preventDefault();
    setError(null);
    const job = j();
    if (!job.name.trim() || !job.prompt.trim()) return setError("Name and prompt are required.");
    if (!validSchedule(job.schedule)) return setError("The schedule is not a valid cron expression or @every interval.");
    setBusy(true);
    try {
      const clean: Job = { ...job, name: job.name.trim(), schedule: job.schedule.trim() };
      if (!clean.bot_id) delete clean.bot_id;
      // A limit taken away is sent as 0 (left out, the server keeps it).
      if (!clean.until && props.job.until) clean.until = 0;
      if (!clean.max_runs && props.job.max_runs) clean.max_runs = 0;
      if (props.onSubmit) await props.onSubmit(clean);
      else {
        await api.saveJob(clean);
        toast({ level: "success", message: bot() ? `${bot()!.name} has a new duty` : "Scheduled task saved", timeout: 2000 });
      }
      props.onDone();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="card form-card">
      <form class="stack" onSubmit={save}>
        <div class="row gap">
          <button type="button" class="icon-btn" aria-label="Back" onClick={props.onDone}>
            <Icon name="arrowLeft" />
          </button>
          <h4 class="grow">{props.job.id ? (props.lockBot ? "Edit duty" : "Edit scheduled task") : props.lockBot ? "New duty" : "New scheduled task"}</h4>
          <label class="inline-toggle">
            <Toggle checked={j().enabled} onChange={(v) => set("enabled", v)} label="Enabled" />
            <span>Enabled</span>
          </label>
        </div>
        <Show when={state.jobs.find((x) => x.id === props.job.id && x.last_run)}>
          {(live) => (
            <div class="job-meta muted small job-state">
              <JobRunState job={live()} />
              <Show when={live().last_session}>
                <button
                  type="button"
                  class="link-btn"
                  onClick={() => {
                    openSession(live().last_session!, { newPane: true });
                    props.onDone();
                  }}
                >
                  Open last run
                </button>
              </Show>
            </div>
          )}
        </Show>
        <label class="field">
          <span class="field-label">Name</span>
          <input class="input" value={j().name} onInput={(e) => set("name", e.currentTarget.value)} placeholder={props.lockBot ? "Morning briefing" : "Nightly dependency audit"} />
        </label>
        <div class="field">
          <span class="field-label">How often</span>
          <SchedulePicker value={j().schedule} onChange={(v) => set("schedule", v)} />
        </div>
        <label class="field">
          <span class="field-label">What should it do?</span>
          <textarea
            class="input"
            rows={4}
            value={j().prompt}
            onInput={(e) => set("prompt", e.currentTarget.value)}
            placeholder={props.lockBot ? "Brief me on today's calendar, anything urgent in my inbox and the news I follow." : "Check my inbox and summarize anything urgent."}
          />
        </label>
        <Show when={!props.lockBot && state.bots.length}>
          <label class="field">
            <span class="field-label">Run as</span>
            <select class="input" value={j().bot_id ?? ""} onChange={(e) => set("bot_id", e.currentTarget.value || undefined)}>
              <option value="">Tessera (no bot)</option>
              <For each={state.bots}>{(b) => <option value={b.id}>{`${avatarGlyph(b.avatar)} ${b.name} — ${b.role}`.trim()}</option>}</For>
            </select>
            <span class="field-help">A task that runs as a bot is one of its duties: it uses that bot's persona, tools and memory.</span>
          </label>
        </Show>
        <div class="grid2">
          <label class="field">
            <span class="field-label">Folder</span>
            <div class="row gap">
              <input class="input mono grow" value={j().cwd} onInput={(e) => set("cwd", e.currentTarget.value)} />
              <button
                type="button"
                class="btn"
                onClick={async () => {
                  const p = await browseDir(j().cwd || state.server?.home || "/", "The task's folder");
                  if (p) set("cwd", p);
                }}
              >
                <Icon name="folder" size={15} /> Browse
              </button>
            </div>
          </label>
          <label class="field">
            <span class="field-label">Model</span>
            <select
              class="input"
              value={j().model ? `${j().model!.provider}/${j().model!.model}` : ""}
              onChange={(e) => {
                const v = e.currentTarget.value;
                if (!v) set("model", undefined);
                else {
                  const i = v.indexOf("/");
                  set("model", { provider: v.slice(0, i), model: v.slice(i + 1) });
                }
              }}
            >
              <option value="">{bot() ? `${bot()!.name}'s model` : "Server default"}</option>
              <For each={models()}>{({ p, m }) => <option value={`${m.provider || p.id}/${m.id}`}>{`${p.name} · ${m.name}`}</option>}</For>
            </select>
          </label>
        </div>
        <div class="grid2 job-limits">
          <label class="field">
            <span class="field-label">Until (optional)</span>
            <input
              class="input"
              type="date"
              value={dateValue(j().until)}
              onInput={(e) => set("until", e.currentTarget.value ? nextLocalMidnight(e.currentTarget.value) : undefined)}
            />
          </label>
          <label class="field">
            <span class="field-label">At most this many runs (optional)</span>
            <input
              class="input"
              type="number"
              min="1"
              inputmode="numeric"
              value={j().max_runs ?? ""}
              onInput={(e) => {
                const n = Math.floor(Number(e.currentTarget.value));
                set("max_runs", n > 0 ? n : undefined);
              }}
            />
          </label>
          <span class="field-help">For a check that only matters for a while: after its last run or date, the task ends by itself.</span>
        </div>
        <div class="field">
          <span class="field-label">How much freedom</span>
          <Segmented<PermissionMode>
            label="How much freedom"
            value={j().permission}
            onChange={async (v) => {
              if (await confirmYolo(v)) set("permission", v);
            }}
            options={permissionOptions()}
          />
          <span class="field-help">{permissionInfo(j().permission).desc}. Scheduled tasks run on their own; approvals wait until you answer them.</span>
        </div>
        <Show when={error()}>
          <div class="form-error">{error()}</div>
        </Show>
        <div class="form-actions">
          <button type="button" class="btn btn-ghost" onClick={props.onDone}>
            Cancel
          </button>
          <button type="submit" class="btn btn-primary" disabled={busy()}>
            <Show when={busy()}>
              <Spinner size={14} />
            </Show>
            {props.saveLabel ?? (props.lockBot ? "Save duty" : "Save")}
          </button>
        </div>
      </form>
    </div>
  );
}
