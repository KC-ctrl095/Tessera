// Global, non-persisted UI state: toasts, dialogs, overlays.
import { createSignal } from "solid-js";
import type { DirEntry } from "../proto/DirEntry";
import type { Job } from "../proto/Job";
import { RpcError } from "./transport";
import { uid } from "./util";

export type ToastLevel = "info" | "success" | "warning" | "error";
export interface Toast {
  id: string;
  level: ToastLevel;
  title?: string;
  message: string;
  action?: { label: string; run: () => void };
  /** Who sent it (an agent's message: the bot or the chat), shown small above the title. */
  from?: string;
  /** What tapping the toast itself does (an agent's message opens its chat). */
  open?: () => void;
  /** A newer toast with the same key replaces this one instead of piling up. */
  key?: string;
}

const [toasts, setToasts] = createSignal<Toast[]>([]);
export { toasts };

/** At most this many at once: the oldest goes first. */
const MAX_TOASTS = 5;
/** Time left per toast; paused while the pointer or a finger is on one. */
const timers = new Map<string, { left: number; at: number; handle?: ReturnType<typeof setTimeout> }>();
let holds = 0;

function arm(id: string) {
  const t = timers.get(id);
  if (!t || holds > 0) return;
  t.at = Date.now();
  t.handle = setTimeout(() => dismissToast(id), t.left);
}

export function toast(t: Omit<Toast, "id"> & { timeout?: number }): string {
  const id = uid("t");
  // The same message again (or the same key) replaces the old one: no pile of copies.
  const same = (o: Toast) => (t.key ? o.key === t.key : !o.key && o.level === t.level && o.title === t.title && o.message === t.message);
  const { timeout: ms, ...rest } = t;
  setToasts((l) => [...l.filter((o) => !same(o)).slice(-(MAX_TOASTS - 1)), { ...rest, id }]);
  for (const k of [...timers.keys()]) if (!toasts().some((o) => o.id === k)) dropTimer(k);
  const timeout = ms ?? (t.level === "error" ? 8000 : 4500);
  if (timeout > 0) {
    timers.set(id, { left: timeout, at: Date.now() });
    arm(id);
  }
  return id;
}

function dropTimer(id: string) {
  clearTimeout(timers.get(id)?.handle);
  timers.delete(id);
}

export function dismissToast(id: string) {
  dropTimer(id);
  setToasts((l) => l.filter((t) => t.id !== id));
}

/**
 * The pointer (or a finger) is on a toast: none of them goes away until it
 * leaves, and then each gets at least a moment more. Calls pair up.
 */
export function holdToasts(on: boolean) {
  const was = holds > 0;
  holds = Math.max(0, holds + (on ? 1 : -1));
  if (was === holds > 0) return;
  const now = Date.now();
  for (const [id, t] of timers) {
    if (holds > 0) {
      clearTimeout(t.handle);
      t.handle = undefined;
      t.left = Math.max(0, t.left - (now - t.at));
    } else {
      t.left = Math.max(t.left, 1500);
      arm(id);
    }
  }
}

/** The top bar's free middle, where toasts show when there is one (TopBar sets it). */
const [toastSlot, setToastSlot] = createSignal<HTMLElement | null>(null);
export { toastSlot, setToastSlot };

export function errorMessage(e: unknown): string {
  if (e instanceof RpcError) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

export function toastError(e: unknown, title?: string) {
  console.warn(title ?? "error", e);
  toast({ level: "error", title, message: errorMessage(e) });
}

// ── Dialogs ──────────────────────────────────────────────────────────────

export interface ConfirmSpec {
  kind: "confirm";
  title: string;
  body?: string;
  confirmLabel?: string;
  /** The other choice's words (default "Cancel"). */
  cancelLabel?: string;
  danger?: boolean;
  resolve: (ok: boolean) => void;
}
export interface PromptSpec {
  kind: "prompt";
  title: string;
  label?: string;
  value?: string;
  placeholder?: string;
  confirmLabel?: string;
  /** A sentence above the field. */
  body?: string;
  /** A password: hidden as typed, never filled in or kept by the browser. */
  secret?: boolean;
  resolve: (v: string | null) => void;
}
export type DialogSpec = ConfirmSpec | PromptSpec;

const [dialog, setDialog] = createSignal<DialogSpec | null>(null);
export { dialog, setDialog };

export function confirmDialog(o: Omit<ConfirmSpec, "kind" | "resolve">): Promise<boolean> {
  return new Promise((resolve) => setDialog({ ...o, kind: "confirm", resolve }));
}

export function promptDialog(o: Omit<PromptSpec, "kind" | "resolve">): Promise<string | null> {
  return new Promise((resolve) => setDialog({ ...o, kind: "prompt", resolve }));
}

// ── Overlays ─────────────────────────────────────────────────────────────

/**
 * Settings' sections, their pages, and Help & tour. Older names (links in guides and the
 * palette, other versions of the app) still work: Settings.tsx sends each to its new home, and
 * bots, scheduled tasks and archived chats (no longer in Settings) to theirs (App.tsx).
 */
export type SettingsTab =
  | "appearance"
  | "notifications"
  | "models"
  | "usage"
  | "safety"
  | "secrets"
  | "commands"
  | "memory"
  | "web"
  | "devices"
  | "activity"
  | "connection"
  | "help"
  // Older names.
  | "general"
  | "providers"
  | "voice"
  | "search"
  | "skills"
  | "security"
  | "terminal"
  | "machines"
  | "thiscomputer"
  | "servers"
  | "bots"
  | "jobs"
  | "archived";
/** Older names whose home is outside Settings now (App.tsx takes them there). */
export const SETTINGS_ELSEWHERE: SettingsTab[] = ["bots", "jobs", "archived"];
export const [settingsTab, setSettingsTab] = createSignal<SettingsTab | null>(null);
/** The setup wizard (first run, or Settings → Run setup again). */
export const [setupOpen, setSetupOpen] = createSignal(false);
export const openSetup = () => setSetupOpen(true);
/** The phone's Mosaic overview (all tiles of the current mosaic as cards). */
export const [overviewOpen, setOverviewOpen] = createSignal(false);
/** Settings was opened at no particular section: a phone shows its list of sections first (Settings.tsx). */
export const [settingsAtList, setSettingsAtList] = createSignal(false);
/** Settings, at `tab`; without one, at the start (a phone: the list of sections; wider screens: Appearance). */
export const openSettings = (tab?: SettingsTab) => {
  setSettingsAtList(!tab);
  setSettingsTab(tab ?? "appearance");
};

export const [paletteOpen, setPaletteOpen] = createSignal(false);
export const [drawerOpen, setDrawerOpen] = createSignal(false);
/** "Add a server" dialog; `repair` re-pairs an existing server (replacing it on success). */
export const [addServerOpen, setAddServerOpen] = createSignal<false | { repair?: string }>(false);
export const [cheatsheetOpen, setCheatsheetOpen] = createSignal(false);
/** The connection has been down for more than a blink (the "Offline" pill shows); short blips never count. */
export const [offlineLate, setOfflineLate] = createSignal(false);
/** The coach marks were started (components/Tour.tsx): the app loads and shows them from then on. */
export const [tourStarted, setTourStarted] = createSignal(false);

/** A scheduled task's editor (New → More → Scheduled task, the sidebar's Scheduled tab): `job` null makes a new one. */
export const [jobEditor, setJobEditor] = createSignal<{ job: Job | null } | null>(null);
export const openJobEditor = (job: Job | null = null) => setJobEditor({ job });
/** The archived chats, a list of their own (the line under the sidebar's chats). */
export const [archivedOpen, setArchivedOpen] = createSignal(false);

/** Bot editor: `id` null creates a new bot. */
export const [botEditor, setBotEditor] = createSignal<{ id: string | null } | null>(null);
export const openBotEditor = (id: string | null = null) => setBotEditor({ id });

/** New-channel dialog, optionally preselecting members (bot ids). */
export const [channelEditor, setChannelEditor] = createSignal<{ members: string[] } | null>(null);
export const openChannelEditor = (members: string[] = []) => setChannelEditor({ members });

/** Where the folder picker looks: the server (default), or this device (Settings → Devices → This computer). */
export interface DirSource {
  list: (path: string) => Promise<DirEntry[] | null>;
  home?: string;
  /** Makes a folder here; a source without it (This computer, for now) has no "New folder". */
  create?: (parent: string, name: string) => Promise<DirEntry>;
}
export interface DirBrowserSpec {
  start: string;
  title?: string;
  source?: DirSource;
  resolve: (path: string | null) => void;
}
export const [dirBrowser, setDirBrowser] = createSignal<DirBrowserSpec | null>(null);
export function browseDir(start: string, title?: string, source?: DirSource): Promise<string | null> {
  return new Promise((resolve) => setDirBrowser({ start, title, source, resolve }));
}

/** The rewind dialog: back to just before the user message at `seq` (its text returns to `paneId`'s composer). */
export interface RewindSpec {
  sessionId: string;
  seq: number;
  paneId: string | null;
}
export const [rewindDialog, setRewindDialog] = createSignal<RewindSpec | null>(null);

/** A transient highlight request: scroll the pane's transcript to `seq`. */
export const [jumpTarget, setJumpTarget] = createSignal<{ paneId: string; sessionId: string; seq: number } | null>(null);
