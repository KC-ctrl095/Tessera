// Android-only lifecycle bridge. No engines or chat history on an ordinary boot.
import { createEffect, createRoot, createSignal, onCleanup } from "solid-js";
import { native } from "./native";
import { state } from "./store";
import { toastError } from "./ui";

export function watchNativeAssistant() {
  return createRoot((dispose) => {
    const [hint, setHint] = createSignal(0);
    let stopped = false, checking = false;
    const pull = async () => {
      if (stopped || checking || !state.ready || document.hidden) return;
      checking = true;
      const revision = hint();
      try {
        // `listen`: the system's gesture sent it; another app's only opens the chat.
        const taken = await native<{ pending: boolean; listen?: boolean }>("assistant_take_pending");
        if (taken?.pending) {
          const { openAssistant } = await import("./phoneAssistant");
          if (!stopped && state.ready && !document.hidden) await openAssistant(!!taken.listen);
        }
      } catch (e) { toastError(e, "Phone assistant"); }
      finally {
        checking = false;
        if (!stopped && hint() !== revision) void pull();
      }
    };
    // Register before pulling the cold-start flag. Both paths consume the same
    // native flag once, so a warm intent racing boot cannot open two chats.
    void import("@tauri-apps/api/core").then(async ({ Channel }) => {
      if (stopped) return;
      const events = new Channel<unknown>();
      events.onmessage = () => setHint((n) => n + 1);
      await native("assistant_watch", { events });
      setHint((n) => n + 1);
    }).catch((e) => toastError(e, "Phone assistant"));
    createEffect(() => { hint(); if (state.ready) void pull(); });
    const visible = () => { if (!document.hidden) setHint((n) => n + 1); };
    document.addEventListener("visibilitychange", visible);
    onCleanup(() => { stopped = true; document.removeEventListener("visibilitychange", visible); });
    return dispose;
  });
}
