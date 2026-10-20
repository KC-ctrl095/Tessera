// Machines: the user's computers running a Tessera node, which agents may
// use within what each one allows. Both live in Settings → Devices:
// - every device sees the server's list on each device's entry. Other
//   devices can only make access smaller (pause, revoke); nothing there turns
//   a machine on or widens it.
// - the desktop app's own entry ("This computer") holds the node on this
//   very computer, set up through the app's local commands (Tauri), which
//   run `tessera node` here and never go through the server.
import { createSignal } from "solid-js";
import type { DeviceInfo } from "../proto/DeviceInfo";
import type { DirEntry } from "../proto/DirEntry";
import type { MachineActivity } from "../proto/MachineActivity";
import type { MachineAsk } from "../proto/MachineAsk";
import type { MachineInfo } from "../proto/MachineInfo";
import type { MachinePreset } from "../proto/MachinePreset";
import type { MachineScope } from "../proto/MachineScope";
import type { NodeLogEntry } from "../proto/NodeLogEntry";
import type { NodeStatus } from "../proto/NodeStatus";
import type { StartupChange } from "../proto/StartupChange";
import { native } from "./native";
import { backend } from "./backend";
import { relTime, truncate, tryParseJson } from "./util";

// Shapes: crates/proto (MachineInfo, MachineScope, NodeStatus, NodeLogEntry…).
export type { DeviceInfo, MachineActivity, MachineAsk, MachineInfo, MachinePreset, MachineScope, NodeLogEntry, NodeStatus, StartupChange };

// ── the server's list (any device) ───────────────────────────────────────

// (Through the backend directly: lib/store.ts forwards `machines_changed` here.)
export const listMachines = () => backend.call<MachineInfo[]>({ method: "list_machines" });
/** Pause or resume; resuming never gives more than the machine itself allows. */
export const setMachinePaused = (id: string, paused: boolean) => backend.call<MachineInfo>({ method: "set_machine_paused", params: { id, paused } });
/** The machine drops off; it must be turned on again on that computer. */
export const revokeMachine = (id: string) => backend.call<null>({ method: "revoke_machine", params: { id } });

/** The last list seen, for naming machines in tool and approval cards. */
const [known, setKnown] = createSignal<MachineInfo[]>([]);
let loading: Promise<void> | null = null;
/** Settings → Devices loaded the list: cards use it too. */
export function rememberMachines(list: MachineInfo[]) {
  setKnown(list);
  loading ??= Promise.resolve();
}
/** Load the list once (cards call this when they show a machine call). */
const load = () =>
  (loading = listMachines().then(
    (l) => void setKnown(l),
    () => void (loading = null),
  ));
export function ensureMachines() {
  if (!loading) load();
}

const changeListeners = new Set<() => void>();
/** `machines_changed`: a machine came, went, was paused or changed what it allows. */
export function onMachinesChanged(cb: () => void) {
  changeListeners.add(cb);
  return () => changeListeners.delete(cb);
}
export function machinesChanged() {
  if (loading) load();
  for (const cb of changeListeners) cb();
}

// ── plain words ──────────────────────────────────────────────────────────

export function fmtLimit(secs: number): string {
  if (secs < 60) return `${secs} seconds`;
  if (secs < 3600) return `${Math.round(secs / 60)} min`;
  const h = secs / 3600;
  return `${Number.isInteger(h) ? h : h.toFixed(1)} hour${h === 1 ? "" : "s"}`;
}

/** Short folder name for a summary: `/home/ana/proj` → `~/proj`, `/srv/data/www/site` → `…/www/site`. `home`: that machine's, when known. */
export function shortPath(path: string, home?: string | null): string {
  let p = path;
  if (home && (p === home || p.startsWith(home + "/"))) p = "~" + p.slice(home.length);
  // Otherwise the usual places read as `~`.
  else p = p.replace(/^\/(?:home|Users)\/[^/]+(?=\/|$)/, "~").replace(/^C:\\Users\\[^\\]+(?=\\|$)/i, "~");
  if (p.length <= 28) return p;
  const parts = p.split("/").filter(Boolean);
  return "…/" + parts.slice(-2).join("/");
}


/** A machine's `os` (e.g. `windows x86_64, shell Git Bash (bash)`), or this computer's when left out. */
export const isWindowsOs = (os?: string): boolean => (os === undefined ? /win/i.test(navigator.platform) : os.startsWith("windows"));

/** Explicit backends win; old nodes fall back only to their own reported OS. */
export function machinePlatform(os = "", features?: string[] | null) {
  const windows = /^win/i.test(os);
  const mac = /^(mac|darwin)/i.test(os);
  const unix = /^(linux|mac|darwin|.*bsd|android|illumos|solaris)/i.test(os);
  const has = (name: string, legacy: boolean) => features == null ? legacy : features.includes(name);
  return { windows, mac, desktop: has("desktop", /^linux/i.test(os)) || has("desktop_session", false), admin: has("admin", unix), terminal: has("terminal", unix), sandbox: has("sandbox", unix && !mac), webviews: has("webviews", unix) };
}

// ── machines in Settings → Devices (lib/devices.ts groups them) ─────────

/** "ran cargo build", "read ~/notes.txt"… */
export function activityText(a: MachineActivity, home?: string | null): string {
  const t = a.action === "run" ? a.target : shortPath(a.target, home);
  const verb = { run: "ran", read: "read", list: "listed", write: "wrote", edit: "edited" }[a.action] ?? a.action;
  return `${verb} ${truncate(t, 70)}`;
}

/** The machine Settings → Devices → Activity opens on (set by "Activity" on a machine in Devices). */
export const [activityFocus, setActivityFocus] = createSignal<string | null>(null);

/** How an action ended, in a word, and its tone. */
export function outcomeInfo(o: string): { label: string; tone: "" | "ok" | "warn" | "err" } {
  switch (o) {
    case "done":
      return { label: "Done", tone: "" };
    case "failed":
      return { label: "Failed", tone: "err" };
    case "refused":
      return { label: "Refused", tone: "err" };
    case "declined":
      return { label: "Declined", tone: "warn" };
    case "stopped":
      return { label: "Stopped", tone: "warn" };
  }
  return { label: o, tone: "" };
}

/** "2 min ago · in “Build the site” · Homelab sweeper" */
export function activityMeta(a: MachineActivity): string {
  return [relTime(a.ts), a.bot ? a.bot : a.chat ? `in “${truncate(a.chat, 40)}”` : ""].filter(Boolean).join(" · ");
}

/** Where to get Tessera on another computer (lib/repo.ts). */
export { INSTALL_COMMAND, INSTALL_COMMAND_WINDOWS, QUICK_START_URL as INSTALL_URL } from "./repo";

/** The one command that pairs another computer and turns it on as a machine. */
export const enableCommand = (link: string) => `tessera node enable '${link.replace(/'/g, "")}'`;

export function osLabel(os: string): string {
  const o = os.toLowerCase();
  if (o.startsWith("linux")) return "Linux";
  if (o.startsWith("mac") || o === "darwin") return "macOS";
  if (o.startsWith("win")) return "Windows";
  return os || "Unknown system";
}

// ── machine calls in chats ───────────────────────────────────────────────

/** The `machines` tool, or `desktop` (a machine's screen). */
export const isMachineTool = (name: string) => /^(machines?|desktop)$/i.test(name);

/** A machine by id, as last listed (loads the list once; reactive). */
export function knownMachine(id: string): MachineInfo | undefined {
  ensureMachines();
  return known().find((x) => x.id === id);
}

/** The machine a call names: its display name when known (by id or name), else what the agent wrote. */
export function machineName(ref: string): string {
  const m = known().find((x) => x.id === ref) ?? known().find((x) => x.name.toLowerCase() === ref.toLowerCase());
  return m?.name ?? ref;
}

/**
 * A `machines` call in plain words: `{ machine: "Desktop", text: "run cargo build in ~/proj" }`.
 * `machine` is null for calls about no single machine (listing them). `summary`: the server's
 * approval summary ("on Desktop: cargo build"), which names the machine when the arguments don't.
 */
export function machineCall(args: unknown, summary?: string, full = false): { machine: string | null; action: string; text: string } {
  const parsed = typeof args === "string" ? tryParseJson(args) : args;
  const o = (parsed && typeof parsed === "object" ? parsed : {}) as Record<string, unknown>;
  const str = (v: unknown) => (typeof v === "string" ? v.trim() : "");
  const ref = str(o.machine) || str(o.machine_id) || str(o.id) || str(o.name) || (/^on (.+?): /i.exec(summary ?? "")?.[1] ?? "");
  const action = str(o.action).toLowerCase() || (str(o.command) ? "run" : ref ? "" : "list");
  const path = str(o.path) || str(o.file_path) || str(o.dir);
  const cwd = str(o.cwd) || str(o.workdir) || str(o.dir);
  // Approvals show it all (what is approved is what runs); cards and lines show the first line.
  const firstLine = (s: string) => (!full && s.includes("\n") ? s.split("\n")[0] + " …" : s);
  let text: string;
  switch (action) {
    case "run":
    case "exec":
    case "bash":
    case "command":
      // As administrator (root, outside the sandbox): said first, so an approval can't read as an ordinary command.
      text = `${o.admin === true ? "run as administrator" : "run"} ${firstLine(str(o.command) || str(o.cmd))}${cwd ? ` in ${cwd}` : ""}`;
      break;
    case "read":
      text = `read ${path}`;
      break;
    case "write":
      text = `write ${path}`;
      break;
    case "edit":
      text = `edit ${path}`;
      break;
    case "ls":
    case "list_dir":
    case "list_files":
      text = `list ${path || "files"}`;
      break;
    case "list":
    case "status":
      text = ref ? `check ${machineName(ref)}` : "list your machines";
      break;
    // The `desktop` tool: a machine's screen, mouse and keyboard.
    case "screenshot":
      text = "look at the screen";
      break;
    case "screens":
      text = "list the screens";
      break;
    case "click": {
      const how = o.button === "right" ? "right-click" : o.button === "middle" ? "middle-click" : o.clicks === 2 ? "double-click" : "click";
      text = `${how} at ${o.x},${o.y}`;
      break;
    }
    case "move":
      text = `move the pointer to ${o.x},${o.y}`;
      break;
    case "drag":
      text = `drag from ${o.x},${o.y} to ${o.to_x},${o.to_y}`;
      break;
    case "scroll": {
      const dy = Number(o.dy) || 0;
      const dx = Number(o.dx) || 0;
      text = `scroll ${[dy ? `${dy > 0 ? "down" : "up"} ${Math.abs(dy)}` : "", dx ? `${dx > 0 ? "right" : "left"} ${Math.abs(dx)}` : ""].filter(Boolean).join(" and ")} at ${o.x},${o.y}`;
      break;
    }
    case "type": {
      // What gets typed is shown, as a command is: the user approves exactly that,
      // with Enter and Tab visible (they can send a message or run a command).
      const raw = typeof o.text === "string" ? o.text : "";
      text = `type “${raw.replace(/\r\n?|\n/g, "⏎").replace(/\t/g, "⇥")}”`;
      break;
    }
    case "key":
      text = `press ${str(o.keys)}`;
      break;
    default:
      text = [action, path || str(o.command)].filter(Boolean).join(" ");
  }
  // The desktop tool: which screen, and a screenshot after acting.
  if (typeof o.screen === "number" && o.screen > 1) text += ` on screen ${o.screen}`;
  if (o.look === true && action !== "screenshot") text += ", then look at the screen";
  // The agent may leave the machine out when there is only one.
  const only = !ref && action !== "list" && known().length === 1 ? known()[0].name : null;
  text = text.trim() || "use it";
  return { machine: ref ? machineName(ref) : only, action, text: full ? text : truncate(text, 160) };
}

/** "On Desktop: run cargo build in ~/proj" (or "List your machines"). */
export function machineCallLine(args: unknown, summary?: string, full = false): string {
  const c = machineCall(args, summary, full);
  return c.machine ? `On ${c.machine}: ${c.text}` : c.text.charAt(0).toUpperCase() + c.text.slice(1);
}

// ── this computer (desktop app; local commands only) ─────────────────────

/** `this_computer_*`: the node here (`tessera node status`), and what the app knows around it. */
export interface ThisComputer {
  /** `null`: the `tessera` program isn't installed here. */
  node: NodeStatus | null;
  /** The server this app uses (its name): turning on without a `tessera connect` login hands it this app's pairing. */
  appServer: string | null;
  /** This app's server runs on this very computer: agents already reach it through their usual tools. */
  serverHere: boolean;
}

/** `null`: not the desktop app, or a shell without these commands (an older app). */
export const nodeStatus = () => native<ThisComputer>("this_computer_status");
export const nodeEnable = (on: boolean) => native<ThisComputer>("this_computer_enable", { on });
export const nodeSetScope = (scope: MachineScope) => native<ThisComputer>("this_computer_scope", { scope });
/** Choose a preset (with its folders), and/or "Ask first", and/or "Web views here". */
export const nodePreset = (preset: MachinePreset | null, folders?: string[], ask?: MachineAsk, webviews?: boolean) =>
  native<ThisComputer>("this_computer_preset", { preset, folders: folders ?? null, ask: ask ?? null, webviews: webviews ?? null });

/** What "Web views here" means, plainly (the same words everywhere). */
/** "Terminal from my devices" (this computer's entry in Settings → Devices), in plain words. */
export const TERMINAL_HELP =
  "Your paired devices (only them, never agents) may open a terminal here: a command line as your account on this computer. Turning it on trusts your Tessera server with this account: this computer takes the server's word for which device asks.";
/** The warning before administrator commands (the same text as the terminal's, `tessera node admin on`). */
export const ADMIN_WARNING =
  "Administrator lets agents run commands on this computer as root (with sudo), outside Tessera's sandbox. Such a command can change anything here: system files, other accounts, installed programs, and Tessera's own settings on this computer. Each one asks you first and shows you the exact command, unless you choose otherwise. Turn it on only for work you want done that way, and off again when you're done.";
/** "Administrator" choices (this computer's entry), in plain words. */
export const ADMIN_CHOICES: { value: "off" | "ask" | "chat"; label: string; help: string; warn?: boolean }[] = [
  { value: "off", label: "Off", help: "Agents can't run commands as administrator here: their commands run in Tessera's sandbox, where sudo doesn't work." },
  { value: "ask", label: "Ask each time", help: "Agents may run commands here as administrator (root, with sudo), outside the sandbox. Each one waits for your approval, showing the exact command, in every chat." },
  { value: "chat", label: "As the chat says", help: "Administrator commands follow this computer's “Ask first” and each chat's mode: in a YOLO chat they run without asking you.", warn: true },
];
/** Save the password sudo asks for here (checked with sudo; kept on this computer only), or forget it (`null`). */
export const nodeAdminPassword = (password: string | null) => native<ThisComputer>("this_computer_admin_password", { password });
export const WEBVIEWS_HELP = "Chats that may use this computer can open web pages in a browser running here. Those pages can reach this computer's local network and services on localhost.";
/** Who can use this computer: `null` any chat, or these bots' ids. */
export const nodeAccess = (bots: string[] | null) => native<ThisComputer>("this_computer_access", { bots });
/** Rename this computer as a machine (what agents and approvals call it); only here. */
export const nodeName = (name: string) => native<ThisComputer>("this_computer_name", { name });
/** Stop every agent here (what the Stop bar does), or resume after a Stop: only on this computer. */
export const nodeStop = (stopped: boolean) => native<ThisComputer>("this_computer_stop", { stopped });
/** Install Tessera Guard (Windows): Windows asks an administrator, and this waits for the answer. */
export const nodeGuardInstall = () => native<ThisComputer>("this_computer_guard_install");
export const nodeStartupChanges = (limit = 100) => native<StartupChange[]>("this_computer_startup_changes", { limit });
export const nodeRestore = (id: number) => native<StartupChange>("this_computer_restore", { id });
export const nodeService = (on: boolean) => native<ThisComputer>("this_computer_service", { on });
export const nodeLog = (limit = 200) => native<NodeLogEntry[]>("this_computer_log", { limit });
/** Folders on this computer, for the folder picker (not the server's). */
export const localListDir = (path: string) => native<DirEntry[]>("this_computer_list_dir", { path });
export const localHome = () => native<string>("this_computer_home");

export const TIME_LIMITS = [60, 300, 900, 3600] as const;

/** A log line in plain words. */
export function logLine(e: NodeLogEntry): { label: string; tone: "ok" | "warn" | "err" | "info" | ""; action: boolean } {
  switch (e.event) {
    case "done":
      return { label: "Done", tone: "", action: true };
    case "started":
      return { label: "Running", tone: "info", action: true };
    case "failed":
      return { label: "Failed", tone: "err", action: true };
    case "refused":
      return { label: "Refused", tone: "err", action: true };
    case "cancelled":
      return { label: "Stopped", tone: "warn", action: true };
    case "timed_out":
      return { label: "Timed out", tone: "warn", action: true };
    case "enabled":
      return { label: "Turned on", tone: "ok", action: false };
    case "disabled":
      return { label: "Turned off", tone: "", action: false };
    case "scope_changed":
      return { label: "Access changed", tone: "", action: false };
    case "connected":
      return { label: "Connected", tone: "", action: false };
    case "disconnected":
      return { label: "Disconnected", tone: "", action: false };
    case "paused":
      return { label: "Paused", tone: "warn", action: false };
    case "resumed":
      return { label: "Resumed", tone: "", action: false };
    case "revoked":
      return { label: "Revoked", tone: "err", action: false };
    case "startup_changed":
      return { label: "Startup files changed", tone: "warn", action: false };
    case "restored":
      return { label: "Put back", tone: "ok", action: false };
    case "terminal_opened":
      return { label: "Terminal opened", tone: "info", action: false };
    case "terminal_closed":
      return { label: "Terminal closed", tone: "", action: false };
  }
  return { label: String(e.event), tone: "", action: !!e.action };
}

/** One line per action (its latest event: the log is newest first), plus everything else. */
export function compactLog(entries: NodeLogEntry[]): NodeLogEntry[] {
  const seen = new Set<string>();
  return entries.filter((e) => {
    if (!e.call_id) return true;
    if (seen.has(e.call_id)) return false;
    seen.add(e.call_id);
    return true;
  });
}
