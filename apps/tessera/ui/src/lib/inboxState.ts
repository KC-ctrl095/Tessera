// What the first screen needs of the notification center (the bell and its count). The rest
// (fetching, reading, clearing, following events) loads after the app starts: notificationInbox.ts.
// Never saved here: a sign-out, another server or a lost connection clears it.
import { createSignal } from "solid-js";
import type { UserNotification } from "../proto/UserNotification";

export const [inboxItems, setInboxItems] = createSignal<UserNotification[]>([]);
export const [inboxOpen, setInboxOpen] = createSignal(false);
export const [inboxStatus, setInboxStatus] = createSignal<"loading" | "ready" | "unsupported" | "error">("loading");
export const unread = (n: UserNotification) => n.sent && !n.seen;
export const inboxUnread = () => inboxItems().filter(unread).length;
let generation = 0;
/** Changes with every reset: a reply from before one is dropped. */
export const inboxEpoch = () => generation;

/** Signed out, another server, or the connection went: nothing of it stays. */
export function resetInbox() {
  generation++;
  setInboxOpen(false);
  setInboxItems([]);
  setInboxStatus("loading");
}
