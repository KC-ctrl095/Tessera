// Presentation helpers for tool calls: icon, verb and one-line summary.
import type { IconName } from "../components/icons";
import type { Entry } from "../proto/Entry";
import { truncate, tryParseJson } from "./util";
import aliases from "./tool-presentation.json";
import type { ToolResultEntry } from "./store";
import { isMachineTool, machineCall, machineCallLine } from "./machines";

export interface ToolPresentation {
  kind: string;
  args: unknown;
  housekeeping: boolean;
  helper?: "controls" | "read_only";
}

/** Display only. Never feed these aliases/keys back into dispatch or approvals. */
export function toolPresentation(rawName: string, args: unknown): ToolPresentation {
  const name = rawName.replace(/^mcp__tessera__/, "");
  const a = (aliases as Record<string, { kind: string; housekeeping?: boolean; helper?: boolean; action?: string }>)[name];
  const raw = argsObject(args);
  const kind = name === "agents" && raw?.action === "start" ? "agent" : a?.kind ?? name;
  let o = raw ? { ...raw } : undefined;
  if (o && name === "agents" && kind === "agent") delete o.action;
  if (o && /^(read|write|edit|bash|job|grep|glob|task)$/.test(kind)) {
    for (const [key, variants] of [["path", ["file_path", "filePath"]], ["command", ["cmd", "script"]], ["background", ["run_in_background"]], ["id", ["task_id", "shell_id", "taskId"]]] as const) {
      if (!(key in o)) for (const v of variants) if (v in o) { o[key] = o[v]; break; }
      for (const v of variants) delete o[v];
    }
  }
  if (o && a?.action) o.action = a.action;
  if (o && a?.helper && !Array.isArray(o.tasks)) o = { tasks: [{ name: typeof o.description === "string" ? o.description : "Helper", prompt: o.prompt ?? null }], wait: o.run_in_background !== true };
  // Alias insertion order must not change the canonical input's layout.
  if (o && /^(read|write|edit|bash|job|grep|glob|task|agent|web_search|web_fetch|tools)$/.test(kind)) o = Object.fromEntries(Object.keys(o).sort().map(k => [k, o![k]]));
  return { kind, args: o ?? args, housekeeping: !!a?.housekeeping, helper: kind === "agent" ? (a?.helper && !Array.isArray(raw?.tasks) ? "read_only" : "controls") : undefined };
}

/** Only successful bookkeeping is quiet; failed/uncertain/approval calls stay visible. */
export function quietTool(name: string, args: unknown, result?: ToolResultEntry): boolean {
  const d = result?.details as { interrupted?: boolean; stopped?: boolean; status?: string } | undefined;
  return toolPresentation(name, args).housekeeping && !!result && !result.is_error && !d?.interrupted && !d?.stopped && d?.status !== "stopped";
}

export interface ToolMeta {
  icon: IconName;
  /** Past-tense verb for group summaries, e.g. "Ran", "Edited". */
  verb: string;
  /** Present tense for the live activity line, e.g. "Running", "Editing". */
  doing: string;
  noun: [string, string];
}

const META: [RegExp, ToolMeta][] = [
  [/^notify$/i, { icon: "bell", verb: "Sent", doing: "Notifying you:", noun: ["notification", "notifications"] }],
  [/^share_file$/i, { icon: "download", verb: "Shared", doing: "Sharing", noun: ["file", "files"] }],
  [/^machines?$/i, { icon: "laptop", verb: "Used", doing: "Working on", noun: ["machine step", "machine steps"] }],
  [/^browser$/i, { icon: "globe", verb: "Took", doing: "Browsing", noun: ["browser step", "browser steps"] }],
  [/^(bash|shell|sh|exec|run|terminal|command|powershell|job)/i, { icon: "terminal", verb: "Ran", doing: "Running", noun: ["command", "commands"] }],
  [/^(edit|write|apply_patch|patch|str_replace|create|multi_edit|replace)/i, { icon: "pencil", verb: "Edited", doing: "Editing", noun: ["file", "files"] }],
  [/^(read|view|cat|open)/i, { icon: "file", verb: "Read", doing: "Reading", noun: ["file", "files"] }],
  [/^(grep|search|glob|find|ls|list|rg|recall)/i, { icon: "search", verb: "Searched", doing: "Searching", noun: ["time", "times"] }],
  [/^(web|fetch|browse|http|url)/i, { icon: "globe", verb: "Browsed", doing: "Browsing", noun: ["page", "pages"] }],
  // Claude Code's own subagent tool (its "task" is not Tessera's plan ledger).
  [/^(Task|Agent)$/, { icon: "cornerDownRight", verb: "Delegated", doing: "Delegating", noun: ["task", "tasks"] }],
  // Claude Code's question for the user (it comes as Tessera's question card).
  [/^AskUserQuestion$/, { icon: "help", verb: "Asked", doing: "Asking you", noun: ["question", "questions"] }],
  // Claude Code reading or stopping what it runs in the background (not its plan).
  [/^(TaskOutput|TaskStop|BashOutput|KillShell)$/, { icon: "terminal", verb: "Checked", doing: "Checking", noun: ["background job", "background jobs"] }],
  // Claude Code loading tools it keeps in reserve (its task tools, say): housekeeping.
  [/^ToolSearch$/, { icon: "search", verb: "Loaded", doing: "Loading", noun: ["tool", "tools"] }],
  [/^(task|plan|todo)/i, { icon: "listChecks", verb: "Updated", doing: "Updating the plan", noun: ["plan", "plans"] }],
  [/^(memory|remember)/i, { icon: "bookmark", verb: "Updated", doing: "Saving to memory", noun: ["memory", "memories"] }],
  [/^(message_bot|message|ask_bot|dm)/i, { icon: "at", verb: "Messaged", doing: "Messaging", noun: ["bot", "bots"] }],
  [/^(agents?|subagent|delegate|spawn)$/i, { icon: "cornerDownRight", verb: "Delegated", doing: "Delegating", noun: ["task", "tasks"] }],
  [/^(calendar|schedule)/i, { icon: "calendar", verb: "Checked", doing: "Checking", noun: ["calendar", "calendars"] }],
];

/** A `note` call that makes a note: it gets a card of its own in the chat. */
export function makesNote(name: string, args: unknown): boolean {
  return /^note$/i.test(name) && argsObject(args)?.action === "create";
}

export function toolMeta(name: string): ToolMeta {
  name = toolPresentation(name, undefined).kind;
  for (const [re, m] of META) if (re.test(name)) return m;
  return { icon: "wrench", verb: "Used", doing: "Using", noun: ["tool", "tools"] };
}

/** "Running cargo test", "Editing src/main.rs", "Messaging @forge" — for the live activity line. */
export function toolActivity(name: string, args: unknown): { verb: string; detail: string } {
  const p = toolPresentation(name, args);
  name = p.kind;
  args = p.args;
  if (isMachineTool(name)) {
    const c = machineCall(args);
    return c.machine ? { verb: `On ${c.machine}:`, detail: c.text } : { verb: "Checking", detail: "your machines" };
  }
  const m = toolMeta(name);
  const obj = argsObject(args);
  const to = obj?.to ?? obj?.bot ?? obj?.handle ?? obj?.recipient;
  if (/^(message_bot|message|ask_bot|dm)/i.test(name) && typeof to === "string") return { verb: m.doing, detail: "@" + to.replace(/^@/, "") };
  const detail = toolSummary(name, args);
  if (m.icon === "wrench") return { verb: "Using", detail: detail ? `${name} · ${detail}` : name };
  return { verb: m.doing, detail };
}

const KEYS = ["command", "cmd", "path", "file_path", "file", "pattern", "query", "url", "goal", "target", "name", "description", "prompt"];

/** Arguments may be a parsed object (persisted) or a partial JSON string (streaming). */
export function argsObject(args: unknown): Record<string, unknown> | undefined {
  if (typeof args === "string") {
    const v = tryParseJson(args);
    return v && typeof v === "object" ? (v as Record<string, unknown>) : undefined;
  }
  return args && typeof args === "object" ? (args as Record<string, unknown>) : undefined;
}

/** `cargo test`, `src/main.rs`, … */
export function toolSummary(name: string, args: unknown): string {
  const p = toolPresentation(name, args);
  name = p.kind;
  args = p.args;
  const obj = argsObject(args);
  // "select:TaskCreate,TaskUpdate" → "TaskCreate, TaskUpdate"
  if (name === "tools" && typeof obj?.query === "string") return truncate(obj.query.replace(/^select:/, "").replace(/,\s*/g, ", "), 140);
  if (!obj) {
    if (typeof args === "string" && args) {
      // Streaming: pull the first string value out of the partial JSON.
      const m = args.match(/"(?:command|cmd|path|file_path|pattern|query|url|goal)"\s*:\s*"((?:[^"\\]|\\.)*)/);
      if (m) return truncate(m[1].replace(/\\n/g, " ").replace(/\\"/g, '"'), 140);
    }
    return "";
  }
  if (/^(agent|subagent)$/i.test(name) && Array.isArray(obj.tasks)) {
    const names = obj.tasks.map((t, i) => (t && typeof t === "object" && typeof (t as { name?: unknown }).name === "string" ? (t as { name: string }).name : `task ${i + 1}`));
    const n = names.length;
    return truncate(`${n} helper${n === 1 ? "" : "s"}${obj.wait ? "" : " in the background"} · ${names.join(", ")}`, 160);
  }
  if (name === "task") {
    if (typeof obj.goal === "string" && obj.goal.trim()) return truncate(obj.goal, 160);
    if (typeof obj.subject === "string") return truncate(obj.subject, 160);
    if (Array.isArray(obj.plan)) return `${obj.plan.length} plan items`;
    if (Array.isArray(obj.update)) return `${obj.update.length} plan updates`;
    return typeof obj.id === "string" ? `task ${obj.id}${typeof obj.status === "string" ? ` · ${obj.status}` : ""}` : "work ledger";
  }
  if (name === "job") return `${typeof obj.action === "string" ? obj.action : "status"}${typeof obj.id === "string" ? ` ${obj.id}` : ""}`;
  if (isMachineTool(name)) return machineCallLine(args);
  if (name === "agents") return typeof obj.action === "string" ? `${obj.action}${typeof obj.name === "string" ? " " + obj.name : ""}` : "status";
  if (/^browser$/i.test(name)) return browserStep(obj);
  if (name === "notify") {
    if (obj.cancel !== undefined) return `cancel #${String(obj.cancel)}`;
    const what = typeof obj.title === "string" && obj.title.trim() ? obj.title.trim() : typeof obj.body === "string" ? obj.body.trim() : "";
    return truncate(typeof obj.at === "string" && obj.at.trim() ? `${what} · ${obj.at.trim()}` : what, 160);
  }
  // `read` of several files: "a.py, b.md, c.txt".
  if (Array.isArray(obj.paths) && obj.paths.length && obj.paths.every((p) => typeof p === "string")) {
    const all = [...new Set([...(typeof obj.path === "string" && obj.path ? [obj.path] : []), ...(obj.paths as string[])])];
    return truncate(all.join(", "), 160);
  }
  for (const k of KEYS) {
    const v = obj[k];
    if (typeof v === "string" && v.trim()) {
      const line = v.trim().split("\n")[0];
      let s = line + (v.trim().includes("\n") ? " …" : "");
      if (/^(grep|search|rg)/i.test(name) && typeof obj.path === "string" && k === "pattern") s += `  in ${obj.path}`;
      return truncate(s, 160);
    }
  }
  if (Array.isArray(obj.plan)) return `${obj.plan.length} plan items`;
  if (Array.isArray(obj.edits) && typeof obj.path === "string") return `${obj.path} (${obj.edits.length} edits)`;
  const compact = JSON.stringify(obj);
  return compact === "{}" ? "" : truncate(compact, 120);
}

/** The browser tool's step in a few words: "open example.com", "click “Sign in”", "type into [12]". */
export function browserStep(o: Record<string, unknown>): string {
  const str = (v: unknown) => (typeof v === "string" && v.trim() ? v.trim() : "");
  const host = (u: string) => {
    try {
      const x = new URL(/^[a-z]+:\/\//i.test(u) ? u : `https://${u}`);
      return x.host.replace(/^www\./, "") + (x.pathname === "/" ? "" : x.pathname);
    } catch {
      return u;
    }
  };
  const target = () => (str(o.text) ? `“${truncate(str(o.text), 40)}”` : str(o.ref) ? `[${str(o.ref)}]` : typeof o.x === "number" ? `at ${o.x},${o.y}` : "");
  const a = str(o.action).toLowerCase();
  switch (a) {
    case "open":
      return truncate(`open ${host(str(o.url))}`, 120);
    case "show":
      return str(o.reason) ? truncate(`show you the page: ${str(o.reason)}`, 140) : "show you the page";
    case "needs_user":
      return truncate(`needs you: ${str(o.reason) || "over to you in the web view"}`, 140);
    case "navigate":
    case "go":
      return truncate(`go to ${host(str(o.url))}`, 120);
    case "back":
    case "forward":
    case "reload":
      return a;
    case "snapshot":
      return "read the page";
    case "screenshot":
      return "look at the page";
    case "click":
      return `click ${target()}`.trim();
    case "type":
      return `type into ${str(o.ref) ? `[${str(o.ref)}]` : "the page"}${o.submit ? " and submit" : ""}`;
    case "press":
      return `press ${str(o.key)}`.trim();
    case "scroll":
      return `scroll ${str(o.direction) || "down"}`;
    case "select":
      return `choose ${str(o.value) ? `“${truncate(str(o.value), 40)}”` : "an option"}${str(o.ref) ? ` in [${str(o.ref)}]` : ""}`;
    case "wait":
      return str(o.text) ? `wait for “${truncate(str(o.text), 40)}”` : "wait";
    case "tabs":
      return "list the tabs";
    case "close":
      return "close the tab";
    default:
      return a || "browser";
  }
}

export function prettyArgs(args: unknown): string {
  if (typeof args === "string") {
    const v = tryParseJson(args);
    return v === undefined ? args : JSON.stringify(v, null, 2);
  }
  return JSON.stringify(args ?? {}, null, 2);
}

/** Extract a unified diff from `ToolResult.details` if present. */
export function detailsDiff(details: unknown): string | undefined {
  if (details && typeof details === "object" && "diff" in details) {
    const d = (details as { diff?: unknown }).diff;
    if (typeof d === "string" && d.trim()) return d;
  }
  return undefined;
}

export function detailsExitCode(details: unknown): number | undefined {
  if (details && typeof details === "object") {
    const o = details as Record<string, unknown>;
    const v = o.exit_code ?? o.exitCode ?? o.code;
    if (typeof v === "number") return v;
  }
  return undefined;
}

/** "Ran 3 commands, edited 2 files" */
export function groupSummary(names: string[]): string {
  const counts = new Map<string, { meta: ToolMeta; n: number }>();
  for (const n of names) {
    const m = toolMeta(n);
    const key = m.verb + m.noun[0];
    const c = counts.get(key);
    if (c) c.n++;
    else counts.set(key, { meta: m, n: 1 });
  }
  const parts = [...counts.values()].map(({ meta, n }) => `${meta.verb.toLowerCase()} ${n} ${meta.noun[n === 1 ? 0 : 1]}`);
  const s = parts.join(", ");
  return s.charAt(0).toUpperCase() + s.slice(1);
}

export function shellOutput(text: string, details: unknown): { output: string; status: string } {
  const d = details as { exit_code?: unknown; stopped?: boolean; status?: string; interrupted?: boolean } | undefined;
  const m = text.match(/(?:\n|^|\s)\[(exit -?\d+|killed|running) · [\d.]+s\]\s*$/);
  const status = d?.interrupted ? "Outcome unknown" : d?.stopped || d?.status === "stopped" || m?.[1] === "killed" ? "Stopped" : typeof d?.exit_code === "number" ? `exit ${d.exit_code}` : m?.[1] ?? "";
  let output = (m ? text.slice(0, m.index) : text).trimEnd();
  if (m && output === "(no output)") output = "";
  return { output, status };
}

/** The time the server measured for a call; it stores it with the result (`details.duration_ms`). */
function measuredMs(e: { details?: unknown }): number | undefined {
  const ms = (e.details as { duration_ms?: unknown } | null | undefined)?.duration_ms;
  return typeof ms === "number" && Number.isFinite(ms) && ms >= 0 ? ms : undefined;
}

interface FinishedCall {
  ts: number;
  ms: number | undefined;
}

/**
 * How long a finished call ran: the measured time when its result carries one. Older history has
 * none, so the call is taken to run from its assistant message (or the previous result in the same
 * batch) until its result. One rule for the full derivation and the incremental index below.
 */
function callDuration(done: FinishedCall, start: number): number {
  return done.ms ?? Math.max(0, done.ts - start);
}

/** Durations of finished tool calls, derived from the whole history. */
export function toolDurations(entries: readonly Entry[]): Record<string, number> {
  const out: Record<string, number> = {};
  const results = new Map<string, FinishedCall>();
  for (const e of entries) if (e.kind === "tool_result") results.set(e.call_id, { ts: e.ts, ms: measuredMs(e) });
  for (const e of entries) {
    if (e.kind !== "assistant") continue;
    let start = e.ts;
    for (const p of e.content) {
      if (p.type !== "tool_call") continue;
      const done = results.get(p.id);
      if (done === undefined) continue;
      out[p.id] = callDuration(done, start);
      start = Math.max(start, done.ts);
    }
  }
  return out;
}

/** Index appended history once; a result only changes the timings of its own batch of calls. */
export class ToolDurationIndex {
  private entries: readonly Entry[] | null = null;
  private version = -1;
  private length = 0;
  private last: Entry | undefined;
  private calls = new Map<string, Extract<Entry, { kind: "assistant" }>>();
  private results = new Map<string, FinishedCall>();
  private durations: Record<string, number> = {};

  update(entries: readonly Entry[], version: number): Record<string, number> {
    if (entries !== this.entries || version !== this.version || entries.length < this.length || (this.length && entries[this.length - 1] !== this.last)) {
      this.length = 0;
      this.calls.clear();
      this.results.clear();
      this.durations = {};
    }
    const changed = new Set<Extract<Entry, { kind: "assistant" }>>();
    for (let i = this.length; i < entries.length; i++) {
      const e = entries[i];
      if (e.kind === "assistant") {
        for (const p of e.content) if (p.type === "tool_call") this.calls.set(p.id, e);
        changed.add(e);
      } else if (e.kind === "tool_result") {
        this.results.set(e.call_id, { ts: e.ts, ms: measuredMs(e) });
        const call = this.calls.get(e.call_id);
        if (call) changed.add(call);
      }
    }
    for (const e of changed) {
      let start = e.ts;
      for (const p of e.content) {
        if (p.type !== "tool_call") continue;
        const done = this.results.get(p.id);
        if (done === undefined) continue;
        this.durations[p.id] = callDuration(done, start);
        start = Math.max(start, done.ts);
      }
    }
    this.entries = entries;
    this.version = version;
    this.length = entries.length;
    this.last = entries.at(-1);
    return this.durations;
  }
}

/** Compact counts stay eager; the diff table loads only when a step is opened. */
export function diffStats(diff: string): { add: number; del: number } {
  let add = 0;
  let del = 0;
  for (const l of diff.split("\n")) {
    if (l.startsWith("+") && !l.startsWith("+++")) add++;
    else if (l.startsWith("-") && !l.startsWith("---")) del++;
  }
  return { add, del };
}
