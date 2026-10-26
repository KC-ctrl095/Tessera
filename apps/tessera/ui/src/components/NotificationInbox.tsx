// The notification center (opened by the bell): what notified the user, newest first; a tap opens
// its chat. Loaded on demand.
import { For, Show, createSignal } from "solid-js";
import type { UserNotification } from "../proto/UserNotification";
import {
  clearNotifications,
  inboxEpoch,
  inboxItems,
  inboxOpen,
  inboxStatus,
  inboxUnread,
  readNotifications,
  refreshInbox,
  setInboxOpen,
  unread,
} from "../lib/notificationInbox";
import { api, call } from "../lib/store";
import { openSession } from "../lib/workspace";
import { relTime } from "../lib/util";
import { Icon, type IconName } from "./icons";
import { Modal } from "./primitives";
import "../styles/notification-inbox.css";

const KIND_ICON: Record<string, IconName> = {
  finished: "circleCheck",
  error: "alertCircle",
  job: "calendar",
  approval: "hand",
  helper: "users",
  machine: "monitor",
  system: "info",
};

const when = (ms: number) => new Date(ms).toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });

export default function NotificationInbox() {
  const [filter, setFilter] = createSignal<"all" | "unread">("all");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const items = () => inboxItems().filter((n) => filter() === "all" || unread(n));
  const readIds = () => inboxItems().filter((n) => n.sent && n.seen).map((n) => n.id);
  const act = async (f: () => Promise<void>, failed: string) => {
    setBusy(true);
    setError("");
    try {
      await f();
    } catch {
      setError(failed);
    } finally {
      setBusy(false);
    }
  };
  const turnDown = async (n: UserNotification, mute: boolean) => {
    await call({ method: "set_notification_source", params: { key: n.source_key!, level: mute || (n.level ?? (n.quiet ? "silent" : "page")) !== "page" ? "silent" : "digest" } });
    await refreshInbox();
  };
  const open = async (n: UserNotification) => {
    if (!n.session_id || busy()) return;
    const epoch = inboxEpoch();
    setBusy(true);
    setError("");
    try {
      // Checked now (it may have been deleted or made private since): never an empty tile.
      await api.getSession(n.session_id, Number.MAX_SAFE_INTEGER);
      if (!inboxOpen() || epoch !== inboxEpoch()) return;
      if (unread(n)) await readNotifications([n.id]);
      if (!inboxOpen() || epoch !== inboxEpoch()) return;
      openSession(n.session_id);
      setInboxOpen(false);
    } catch (e) {
      const code = (e as { code?: string }).code;
      setError(
        code === "not_found" || code === "unauthorized"
          ? "That chat isn't here any more: it was deleted, or it's a private chat on another device."
          : "Couldn't open that chat. Check the connection and try again.",
      );
      void refreshInbox();
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal title="Notifications" class="notification-inbox" size="md" onClose={() => setInboxOpen(false)}>
      <div class="inbox-toolbar">
        <div class="inbox-filters" role="group" aria-label="Show">
          <button type="button" aria-pressed={filter() === "all"} onClick={() => setFilter("all")}>
            All
          </button>
          <button type="button" aria-pressed={filter() === "unread"} onClick={() => setFilter("unread")}>
            Unread{inboxUnread() ? ` (${inboxUnread()})` : ""}
          </button>
        </div>
        <div class="inbox-actions">
          <button
            type="button"
            class="btn btn-ghost btn-sm inbox-read-all"
            disabled={busy() || !inboxUnread()}
            onClick={() => void act(() => readNotifications(inboxItems().filter(unread).map((n) => n.id)), "Couldn't mark them read. Try again.")}
          >
            Mark all read
          </button>
          <button
            type="button"
            class="btn btn-ghost btn-sm inbox-clear"
            disabled={busy() || !readIds().length}
            onClick={() => void act(() => clearNotifications(readIds()), "Couldn't clear them. Try again.")}
          >
            Clear read
          </button>
        </div>
      </div>
      <div class="inbox-content">
        <Show when={inboxStatus() === "loading"}>
          <p class="inbox-empty" role="status">
            Loading…
          </p>
        </Show>
        <Show when={inboxStatus() === "unsupported"}>
          <p class="inbox-empty" role="status">
            Update your Tessera server to see past notifications here. Notifications and reminders still arrive as before.
          </p>
        </Show>
        <Show when={inboxStatus() === "error"}>
          <div class="inbox-empty" role="status">
            <p>Couldn't load notifications. Check the connection and try again.</p>
            <button type="button" class="btn btn-sm" onClick={() => void refreshInbox()}>
              Try again
            </button>
          </div>
        </Show>
        <Show when={error()}>
          <p class="inbox-error" role="alert">
            {error()}
          </p>
        </Show>
        <Show when={inboxStatus() === "ready"}>
          <Show
            when={items().length}
            fallback={
              <p class="inbox-empty">{filter() === "unread" ? "Nothing new." : "Nothing yet. Messages from agents, scheduled tasks, approvals that waited and finished work show up here."}</p>
            }
          >
            <ul class="inbox-list">
              <For each={items()}>
                {(n) => (
                  <li data-notification={n.id} data-kind={n.kind || "message"} classList={{ unread: unread(n) }}>
                    <button
                      type="button"
                      class="inbox-item"
                      disabled={!n.session_id || busy()}
                      onClick={() => void open(n)}
                      aria-label={`${n.title}${n.session_id ? ", open its chat" : ""}`}
                    >
                      <span class="inbox-icon" aria-hidden="true">
                        <Icon name={KIND_ICON[n.kind ?? ""] ?? "chat"} size={16} />
                      </span>
                      <span class="inbox-text">
                        <span class="inbox-item-head">
                          <strong>{n.title}<Show when={(n.repeat_count ?? 1) > 1}> ×{n.repeat_count}</Show></strong>
                          <time dateTime={new Date(n.due_at).toISOString()} title={when(n.due_at)}>
                            {n.sent ? relTime(n.due_at) : `at ${when(n.due_at)}`}
                          </time>
                        </span>
                        <Show when={n.from && n.from !== n.title}>
                          <span class="inbox-from">{n.from}</span>
                        </Show>
                        <Show when={n.body}>
                          <span class="inbox-message">{n.body}</span>
                        </Show>
                        <span class="inbox-meta">{n.level ? n.level[0].toUpperCase() + n.level.slice(1) : n.quiet ? "Silent" : "Page"} · {n.source || "chat"} · {n.reason || "a message for you"}</span>
                        <Show when={!n.sent}>
                          <span class="inbox-meta">Reminder, still to come</span>
                        </Show>
                      </span>
                    </button>
                    <Show when={unread(n)}>
                      <button type="button" class="inbox-mark" disabled={busy()} aria-label={`Mark “${n.title}” read`} data-tip="Mark read" onClick={() => void act(() => readNotifications([n.id]), "Couldn't mark it read. Try again.")}>
                        <Icon name="check" size={16} />
                      </button>
                    </Show>
                    <Show when={n.sent && n.seen}>
                      <button type="button" class="inbox-mark" disabled={busy()} aria-label={`Clear “${n.title}”`} data-tip="Clear" onClick={() => void act(() => clearNotifications([n.id]), "Couldn't clear it. Try again.")}>
                        <Icon name="x" size={15} />
                      </button>
                    </Show>
                    <Show when={n.source_key && !n.private && n.kind !== "test" && n.kind !== "digest"}>
                      <div class="inbox-source-actions">
                        <button type="button" disabled={busy()} onClick={() => void act(() => turnDown(n, true), "Could not mute this source.")}>Mute this source</button>
                        <button type="button" disabled={busy() || n.level === "silent"} onClick={() => void act(() => turnDown(n, false), "Could not turn down this source.")}>Less like this</button>
                      </div>
                    </Show>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </Show>
      </div>
      <Show when={inboxStatus() === "ready"}>
        <footer class="inbox-footer">The last 30 days. Read here or on any of your devices, it's read everywhere.</footer>
      </Show>
    </Modal>
  );
}
