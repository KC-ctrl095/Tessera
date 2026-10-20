// Messages agents send the user (`notify`), while the app is open.
//
// The shell (desktop, Android) shows a system notification when the app
// isn't in front; in front, a toast here does instead. A browser shows a
// system notification while its tab is hidden, if the user allowed them
// (Settings → Notifications), and a toast otherwise. Opening the chat marks
// its messages seen on every device.
import { isCheckAck } from "./completion";
import { claudeLimitText } from "./claude-limit";
import type { UserNotification } from "../proto/UserNotification";
import { backend } from "./backend";
import { call } from "./store";
import { dismissToast, toast, toasts } from "./ui";
import { isTauri, lsGet, lsSet } from "./util";
import { openSession, paneSession, workspace } from "./workspace";

const BROWSER_KEY = "tessera.browserNotifications";

/** This browser shows system notifications (the user turned it on and allowed it). */
export function browserNotificationsOn(): boolean {
  return !isTauri && "Notification" in window && Notification.permission === "granted" && lsGet(BROWSER_KEY, false);
}

/** Turn this browser's notifications on (asks for permission) or off. */
export async function setBrowserNotifications(on: boolean): Promise<boolean> {
  if (isTauri || !("Notification" in window)) return false;
  if (on && Notification.permission !== "granted") {
    const p = await Notification.requestPermission();
    if (p !== "granted") {
      lsSet(BROWSER_KEY, false);
      return false;
    }
  }
  lsSet(BROWSER_KEY, on);
  return on;
}

/** The chat on screen right now. */
function onScreen(sessionId: string | undefined): boolean {
  // A window in the background may still be "visible": only the one in front counts.
  if (!sessionId || document.visibilityState !== "visible" || !document.hasFocus()) return false;
  const f = workspace.focused();
  return !!f && paneSession(f)?.info.id === sessionId;
}

function show(n: UserNotification) {
  if (n.quiet || isCheckAck(n.body) || isCheckAck(n.title)) return;
  if (n.private) n = { ...n, title: "Tessera", body: "New activity. Open Tessera for details.", from: "" };
  const body = claudeLimitText(n.body);
  const open = n.session_id ? () => openSession(n.session_id!) : undefined;
  const here = onScreen(n.session_id ?? undefined);
  if (here) {
    // Seen as it arrives; other devices take theirs away.
    lastSeen.set(n.session_id!, 0);
    chatSeen(n.session_id!);
    return;
  }
  const key = n.source_key || n.session_id || `n${n.id}`;
  dropShown((id, s) => id !== n.id && (s === n.session_id && !!s));
  for (const [id, old] of shown) {
    if (old.key === key) { dismissToast(old.toast); shown.delete(id); }
  }
  for (const [id, old] of systemShown) {
    if (old.key === key) { old.notification.close(); systemShown.delete(id); }
  }
  if (browserNotificationsOn() && document.visibilityState !== "visible") {
    // Browsers have no native preview/category policy, so every OS alert is
    // generic. Rich content stays in the open app and inbox.
    const sys = new Notification("Tessera", {
      body: "New activity. Open Tessera for details.",
      tag: key,
      silent: Date.now() - (lastPost.get(key) ?? 0) < 10_000,
    });
    systemShown.set(n.id, { notification: sys, session: n.session_id ?? undefined, key });
    lastPost.set(key, Date.now());
    if (lastPost.size > 500) lastPost.delete(lastPost.keys().next().value!);
    if (systemShown.size > 500) {
      const id = systemShown.keys().next().value!;
      systemShown.get(id)?.notification.close();
      systemShown.delete(id);
    }
    sys.onclick = () => {
      window.focus();
      open?.();
      sys.close();
    };
    return;
  }
  // Desktop app out of focus: the shell shows it as a system notification.
  if (isTauri && !document.hasFocus() && !/Android/i.test(navigator.userAgent)) return;
  // Who sent it shows above the title; tapping the toast opens its chat.
  const id = toast({
    level: n.urgent ? "warning" : "info",
    title: n.title,
    message: body,
    from: n.from && n.from !== n.title ? n.from : undefined,
    action: open ? { label: "Open", run: open } : undefined,
    open,
    key: `notification:${key}`,
    timeout: 12_000,
  });
  for (const [k, s] of shown) if (!toasts().some((t) => t.id === s.toast)) shown.delete(k);
  // From the chat on screen: seen at once, but the toast stays its time.
  if (!here) shown.set(n.id, { toast: id, session: n.session_id ?? undefined, key });
}

/** Agents' messages shown as toasts here: their toast, and the chat they came from. */
const shown = new Map<number, { toast: string; session?: string; key: string }>();
const systemShown = new Map<number, { notification: Notification; session?: string; key: string }>();
const lastPost = new Map<string, number>();

/** Seen (here or on another device): its toast goes too. */
function dropShown(match: (id: number, session?: string) => boolean) {
  for (const [id, s] of systemShown) {
    if (!match(id, s.session)) continue;
    s.notification.close();
    systemShown.delete(id);
  }
  for (const [id, s] of shown) {
    if (!match(id, s.session)) continue;
    dismissToast(s.toast);
    shown.delete(id);
  }
}

const lastSeen = new Map<string, number>();

/** The user is looking at this chat: its messages are no longer news anywhere. */
export function chatSeen(sessionId: string) {
  dropShown((_, s) => s === sessionId);
  const now = Date.now();
  if (now - (lastSeen.get(sessionId) ?? 0) < 15_000) return;
  lastSeen.set(sessionId, now);
  void call({ method: "seen_notifications", params: { session_id: sessionId } }).catch(() => {});
}

/** Signed out, or another server: its messages' toasts go (other notices stay). */
export function resetAlerts() {
  dropShown(() => true);
  // One already on screen may not be in `shown` (from the chat that was open).
  for (const t of toasts()) if (t.key?.startsWith("notification:")) dismissToast(t.id);
  lastSeen.clear();
  lastPost.clear();
}

let watching = false;
export function watchAlerts() {
  if (watching) return;
  watching = true;
  backend.onEvent((e) => {
    if (e.type === "notification" && e.notification.sent && !e.notification.seen) show(e.notification);
    else if (e.type === "notifications_seen") dropShown((id) => e.ids.includes(id));
  });
}
