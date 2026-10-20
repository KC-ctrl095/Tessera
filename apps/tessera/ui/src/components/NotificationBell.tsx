// The way into the notification center (the bell): in the top bar on a computer and an unfolded
// phone, beside Settings in a folded phone's sidebar, once there is something in it (a newcomer's
// first screen stays simple). The center itself loads when first opened.
import { Show, lazy } from "solid-js";
import { inboxItems, inboxOpen, inboxUnread, setInboxOpen } from "../lib/inboxState";
import { setDrawerOpen } from "../lib/ui";
import { Icon } from "./icons";

const NotificationInbox = lazy(() => import("./NotificationInbox"));

export function openInbox() {
  setDrawerOpen(false);
  setInboxOpen(true);
  void import("../lib/notificationInbox").then((m) => m.refreshInbox());
}

export function NotificationBell(props: { class?: string }) {
  const label = () => `Notifications${inboxUnread() ? `, ${inboxUnread()} unread` : ""}`;
  return (
    <Show when={inboxItems().length || inboxOpen()}>
      <button type="button" class={`notification-bell ${props.class ?? "tb-icon"}`} aria-label={label()} aria-haspopup="dialog" data-tip="Notifications" onClick={openInbox}>
        <Icon name="bell" size={17} />
        <Show when={inboxUnread()}>
          <span class="notification-badge" aria-hidden="true">
            {inboxUnread() > 99 ? "99+" : inboxUnread()}
          </span>
        </Show>
      </button>
    </Show>
  );
}

/** Above the responsive headers, so folding or unfolding never closes it. */
export function NotificationOverlay() {
  return (
    <Show when={inboxOpen()}>
      <NotificationInbox />
    </Show>
  );
}
