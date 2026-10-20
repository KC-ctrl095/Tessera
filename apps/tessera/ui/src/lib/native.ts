// Optional native commands of the Tauri shell. Each is feature-detected: in a
// browser, or a shell that doesn't have the command yet, calls resolve to
// `null` instead of throwing.
import { isTauri, isTauriMobile } from "./util";

const missing = new Set<string>();

function isMissing(e: unknown): boolean {
  const m = String((e as { message?: string })?.message ?? e).toLowerCase();
  return m.includes("not found") || m.includes("unknown command") || m.includes("not allowed") || m.includes("no such command");
}

/** Run a Tauri command; `null` when there is no such command here. Other errors throw. */
export async function native<T>(cmd: string, args?: Record<string, unknown>): Promise<T | null> {
  if (!isTauri || missing.has(cmd)) return null;
  const { invoke } = await import("@tauri-apps/api/core");
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    if (isMissing(e)) {
      missing.add(cmd);
      return null;
    }
    throw e;
  }
}

/** The desktop app (it can run a local server); not phones or browsers. */
export const isDesktopApp = isTauri && !isTauriMobile;

export interface LocalServerInfo {
  /** The `tessera` CLI is installed. */
  available: boolean;
  running: boolean;
  url: string | null;
  /** Already saved in the app. */
  saved: boolean;
}

export const localServerDetect = () => native<LocalServerInfo>("local_server_detect");
/** Start (or install as a login service) the local server and connect; resolves to the server id. */
export const localServerStart = (autostart: boolean, attempt?: number) =>
  native<string>("local_server_start", { autostart, attempt });
/**
 * Stop waiting for the `localServerStart` begun at `attempt` (and any before
 * it); what it started carries on.
 */
export const localServerCancel = (attempt?: number) => native<null>("local_server_cancel", { attempt });

/** A step of `localServerStart` while it runs. */
export interface LocalServerStep {
  stage: string;
  message: string;
}

/** Follow the steps of `localServerStart`; the result stops following. */
export async function onLocalServerStep(cb: (step: LocalServerStep) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<LocalServerStep>("tessera://local-server-progress", (e) => {
    const p = e.payload;
    if (p && typeof p.message === "string") cb({ stage: String(p.stage ?? ""), message: p.message });
  });
}
/**
 * Why the server on this computer didn't start again when the app opened
 * (Windows desktop app): `null` once it did, or when there was nothing to start.
 */
export const localServerRecovery = () => native<string | null>("local_server_recovery");
/** Try starting it again; rejects with why it still didn't. */
export const localServerResume = () => native<null>("local_server_resume");
/** Follow `localServerRecovery` as it changes. */
export async function onLocalServerRecovery(cb: (why: string | null) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<string | null>("tessera://local-server-recovery", (e) => cb(typeof e.payload === "string" ? e.payload : null));
}
/** Restart the server on this computer (desktop only), e.g. after changing where it listens. */
export const localServerRestart = () => native<null>("local_server_restart");

/** "Keep Tessera running": the server on this computer starts when the user signs in (`tessera service`). */
export interface KeepRunningInfo {
  /** This computer can run it that way (Windows; Linux with systemd). */
  supported: boolean;
  /** It starts at sign-in. */
  installed: boolean;
  running: boolean;
  /** A Linux system service: only root changes it. */
  system: boolean;
}
export const localServerKeepRunning = () => native<KeepRunningInfo>("local_server_keep_running");
/** Turn it on or off; the server restarts once (chats carry on) and the app reconnects. */
export const localServerSetKeepRunning = (on: boolean) => native<KeepRunningInfo>("local_server_set_keep_running", { on });

/** Tell the shell the app is in front again: it checks the socket at once and reconnects if stale. */
export function appResumed() {
  void native("app_resumed").catch(() => {});
}

/** What the user chose (camelCase, as the shell sends it). Previews are off by default. */
export interface NotificationSettings {
  enabled: boolean;
  approvals: boolean;
  finished: boolean;
  subagents: boolean;
  jobs: boolean;
  errors: boolean;
  /** Put message text in notifications (off: generic text only). */
  previews: boolean;
  keepAlive: "while_busy" | "always" | "off";
  /** Quiet hours: notifications arrive silently from `quietFrom` to `quietTo` ("22:00"). */
  quiet: boolean;
  quietFrom: string;
  quietTo: string;
}

/** Settings plus what the platform says. */
export interface NotificationsState extends NotificationSettings {
  permission: "granted" | "denied" | "prompt";
  /** Android: battery optimisation still applies to the app; null elsewhere. */
  batteryOptimized: boolean | null;
  /** The keep-alive setting does something here (Android). */
  keepAliveAvailable: boolean;
}

export const notificationsGet = () => native<NotificationsState>("notifications_get");
export const notificationsSet = (settings: NotificationSettings) => native<NotificationsState>("notifications_set", { settings });
export const notificationsRequestPermission = () => native<NotificationsState>("notifications_request_permission");
export const notificationsTest = () => native<NotificationsState>("notifications_test");
export const batteryOptimizationRequest = () => native<NotificationsState>("battery_optimization_request");
export const notificationsOpenSettings = () => native<null>("notifications_open_settings");
/** The UI shows this session: its notification is no longer news. */
export function notificationsSeen(sessionId: string) {
  void native("notifications_seen", { sessionId }).catch(() => {});
}
/** The session a notification tap launched the app for (in the last minute). */
export const notificationsTakeOpened = () => native<string | null>("notifications_take_opened").catch(() => null);

/** A notification was tapped: `tessera://open-session { sessionId }`. */
export async function onOpenSession(cb: (sessionId: string) => void): Promise<void> {
  if (!isTauri) return;
  const { listen } = await import("@tauri-apps/api/event");
  await listen<{ sessionId?: string }>("tessera://open-session", (e) => {
    const id = e.payload?.sessionId;
    if (typeof id === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(id)) cb(id);
  });
}

// ── whiteboards ──────────────────────────────────────────────────────────
// Board content is untrusted. The shell serves an HTML board from its own
// origin (no network, its own CSP: a custom protocol on phones, a
// token-guarded loopback server on desktops) and proxies a `local`
// board's web app to a device-local port; the UI only frames what these
// return. Each resolves to `null` in a browser or a shell without them.

/**
 * Pin the effective parent frame policy before any untrusted page is loaded.
 * Parent CSP is the cross-platform backstop for subframe self-navigation,
 * including WebViews that omit native navigation callbacks for subframes.
 */
let framesPinned = false;
export function pinFrames(url: string | null) {
  if (framesPinned || !url) return;
  let u: URL;
  try {
    u = new URL(url);
  } catch {
    return;
  }
  if (!/^\/(board|preview)\//.test(u.pathname)) return;
  let origin: string;
  if (isTauri) {
    if (u.protocol !== "http:" || !((u.hostname === "127.0.0.1" && u.port) || (u.hostname === "tessera-board.localhost" && !u.port))) return;
    origin = u.origin;
  } else {
    if (u.protocol !== "http:" && u.protocol !== "https:") return;
    origin = u.origin;
  }
  const meta = document.createElement("meta");
  meta.httpEquiv = "Content-Security-Policy";
  meta.content = `frame-src ${origin}/board/ ${origin}/preview/`;
  document.head.append(meta);
  framesPinned = true;
}
const pinned = async (url: Promise<string | null>) => {
  const u = await url;
  pinFrames(u);
  return u;
};

/** A URL on the board origin serving this version of an HTML board (append `?theme=light` for light; keep the query, it may carry a token). */
export const boardFrameUrl = (id: string, version: number) => pinned(native<string>("board_frame_url", { id, version }));
/** `http://127.0.0.1:<port>/…?__tessera=<token>`: this device's proxy to a `local` board's web app (keep the query). For the browser (`external`) it is named `<random>.localhost`, so the page shares no cookies with other services on this computer. */
export const boardLocalUrl = (id: string, external = false) => native<string>("board_local_url", { id, external });
/** A URL on the board origin serving `html` (a note's HTML, previewed) like an HTML board (append `?theme=light` for light). */
export const previewFrameUrl = (html: string) => pinned(native<string>("preview_frame_url", { html }));

// ── board snapshots ("the agent sees what you see") ─────────────────────
// Only lib/snapshot.ts calls this, with a whiteboard tile's content rect
// measured while the tile is on screen. The shell copies exactly that rect
// of the app's own web view (frames included), scales it to ≤1600 px on the
// long side and returns it; nothing is stored. `null` in a browser or a
// shell that can't (macOS desktops, for now).

/** A rect in CSS pixels, and the viewport it is measured in. */
export interface CaptureRect {
  x: number;
  y: number;
  width: number;
  height: number;
  viewportWidth: number;
  viewportHeight: number;
}

/** As `SnapshotImage` in the protocol: base64 PNG or JPEG. */
export interface CapturedImage {
  mime: string;
  data: string;
  width: number;
  height: number;
}

export const boardCapture = (rect: CaptureRect) => native<CapturedImage>("board_capture", { rect });
