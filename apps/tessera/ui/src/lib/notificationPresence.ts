// Notification presence and cold-start taps are installed just after first paint.
import { createEffect, createRoot, createSignal } from "solid-js";
import { call, onBootstrapped, state } from "./store";
import { notificationsSeen, notificationsTakeOpened, onOpenSession } from "./native";
import { chatSeen } from "./alerts";
import { openSession, paneSession, workspace } from "./workspace";

export function watchNotifications() {
  let presenceSupported = true;
  let presenceServer = "";
  let pending: string | null = null;
  const open = (id: string) => {
    if (state.ready) openSession(id);
    else pending = id;
  };
  // Native taps are retained by the shell, including one that cold-started it.
  void onOpenSession(open)
    .then(() => notificationsTakeOpened())
    .then((id) => {
      if (id && /^[A-Za-z0-9_-]{1,128}$/.test(id)) open(id);
    });
  onBootstrapped(() => {
    presenceSupported = true;
    if (!pending) return;
    const id = pending;
    pending = null;
    openSession(id);
  });
  const [visible, setVisible] = createSignal(document.visibilityState === "visible" && document.hasFocus());
  const visibility = () => setVisible(document.visibilityState === "visible" && document.hasFocus());
  document.addEventListener("visibilitychange", visibility);
  window.addEventListener("focus", visibility);
  window.addEventListener("blur", visibility);
  const presence = () => {
    if (!state.ready || state.conn.state !== "connected") return;
    const server = `${state.conn.serverId}:${state.conn.endpoint}`;
    if (presenceServer !== server) { presenceServer = server; presenceSupported = true; }
    if (!presenceSupported) return;
    const f = workspace.focused();
    const id = f ? paneSession(f)?.info.id : undefined;
    void call({ method: "notification_presence", params: { active: visible(), session_ids: visible() && id ? [id] : [] } }).catch(() => {
      if (presenceServer === server) presenceSupported = false;
    });
  };
  setInterval(presence, 15_000);
  window.addEventListener("pagehide", () => {
    setVisible(false);
    presence();
  });
  let viewed = "";
  createRoot(() =>
    createEffect(() => {
      presence();
      const f = workspace.focused();
      const id = f ? paneSession(f)?.info.id : undefined;
      const next = visible() && id ? `${presenceServer}:${id}` : "";
      if (next && next !== viewed && id) {
        notificationsSeen(id);
        chatSeen(id);
      }
      viewed = next;
    }),
  );
}
