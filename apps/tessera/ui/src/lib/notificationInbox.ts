// The notification center: everything that notified the user (agents' messages, scheduled runs,
// approvals that waited, helpers' reports, chats that finished, machines), kept by the server.
// Loaded after the app starts (its state for the bell is in inboxState.ts, on the first screen).
import { createEffect, createRoot } from "solid-js";
import type { UserNotification } from "../proto/UserNotification";
import { backend } from "./backend";
import { inboxEpoch, inboxStatus, inboxUnread, resetInbox, setInboxItems, setInboxStatus, unread } from "./inboxState";
import { call, onBootstrapped, onSessionRemoved, state } from "./store";
import { paneSession, workspace } from "./workspace";
import { isTauri } from "./util";

export * from "./inboxState";

let revision = 0;
let watching = false;
let timer: ReturnType<typeof setTimeout> | undefined;

/** The chat on screen right now (in front, focused). */
function onScreen(): string | undefined {
  if (document.visibilityState !== "visible" || !document.hasFocus()) return undefined;
  const f = workspace.focused();
  return f ? paneSession(f)?.info.id : undefined;
}

export async function refreshInbox() {
  if (!state.ready || state.conn.state !== "connected") return;
  const gen = inboxEpoch();
  const rev = ++revision;
  try {
    const list = await call<UserNotification[]>({ method: "list_notification_inbox" });
    if (gen !== inboxEpoch() || rev !== revision) return;
    setInboxItems(list);
    setInboxStatus("ready");
    // What happened in the chat the user is looking at is no news.
    const here = onScreen();
    const seen = here ? list.filter((n) => unread(n) && n.session_id === here).map((n) => n.id) : [];
    if (seen.length) void readNotifications(seen).catch(() => {});
  } catch (e) {
    if (gen !== inboxEpoch() || rev !== revision) return;
    // No fallback to `list_notifications`: an older server can't promise who sees what here.
    // Its system notifications and toasts still work.
    const err = e as { code?: string; message?: string };
    const unsupported = err.code === "bad_request" && /unknown variant|unknown method|invalid request/i.test(err.message ?? String(e));
    setInboxItems([]);
    setInboxStatus(unsupported ? "unsupported" : "error");
  }
}

export async function readNotifications(ids: number[]) {
  if (!ids.length) return;
  const gen = inboxEpoch();
  revision++; // lists already on their way can't undo this
  await call({ method: "seen_notifications", params: { ids } });
  if (gen !== inboxEpoch()) return;
  // A list begun during the mutation may contain newer activity. Do not
  // invalidate it at reply time: patch only these observed revision IDs,
  // then request a final authoritative snapshot after the mutation.
  setInboxItems((list) => list.map((n) => (ids.includes(n.id) && n.sent ? { ...n, seen: true } : n)));
  void refreshInbox();
}

/** Forget these (sent ones; a reminder still to come is cancelled instead). */
export async function clearNotifications(ids: number[]) {
  if (!ids.length) return;
  const gen = inboxEpoch();
  revision++;
  await call({ method: "clear_notifications", params: { ids } });
  if (gen !== inboxEpoch()) return;
  setInboxItems((list) => list.filter((n) => !ids.includes(n.id)));
  void refreshInbox();
}

export function watchInbox() {
  if (watching) return;
  watching = true;
  // A folded phone's way to the bell (the sidebar button on each tile) shows a dot while
  // something is unread.
  createRoot(() => createEffect(() => document.documentElement.classList.toggle("inbox-news", inboxUnread() > 0)));
  onBootstrapped(() => void refreshInbox());
  onSessionRemoved((id) => setInboxItems((list) => list.filter((n) => n.session_id !== id)));
  backend.onStatus((s) => {
    if (s.state === "connected" && !isTauri) {
      // The browser also understands typed alerts. Native shells negotiate
      // themselves so their legacy fallback stays single-owner.
      void backend.call({ method: "negotiate_notifications" }).catch(() => {});
    }
    if (s.state !== "connected") {
      clearTimeout(timer);
      resetInbox();
    }
  });
  backend.onEvent((e) => {
    if (e.type !== "notifications_seen" && e.type !== "notification" && e.type !== "resync") return;
    if (inboxStatus() === "unsupported") return;
    // Ask again rather than keep what was broadcast: the server decides what this login sees.
    clearTimeout(timer);
    timer = setTimeout(() => void refreshInbox(), 120);
  });
  // Started after the first sync: catch up now.
  if (state.ready) void refreshInbox();
}
