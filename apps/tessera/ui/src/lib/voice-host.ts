// A tiny eager facade. Capture, engines and the conversation load on the first tap.
// The owner is ChatPane, not Composer: sending the first message replaces the hero.
import { createContext, createEffect, createSignal, onCleanup, useContext } from "solid-js";
import { abortSession, startChat } from "./actions";
import { sendMessage, state, type SessionState } from "./store";
import { draftIdentity, getDraft, setDraft, workspaceServer } from "./workspace";
import { onDemand } from "./onDemand";
import { toastError } from "./ui";
import type { Conversation } from "./voice/conversation";
import type { View } from "./voice/flow";

const voicePart = onDemand(() => import("./voice/conversation"), "voice");
let activeStop: (() => void) | undefined;

export function createVoiceHost(paneId: string, session: () => SessionState | undefined) {
  const [view, setView] = createSignal<View>({ phase: "idle", conversation: false, level: 0, note: "" });
  const [controller, setController] = createSignal<Conversation>();
  let loading: Promise<Conversation> | undefined;
  let generation = 0, disposed = false, lastId = session()?.info.id, lastServer = workspaceServer();
  let interrupting: Promise<void> | undefined;
  let sendFromComposer: ((text: string) => Promise<void>) | undefined;
  const bindSend = (send: (text: string) => Promise<void>) => {
    sendFromComposer = send;
    return () => { if (sendFromComposer === send) sendFromComposer = undefined; };
  };
  const stop = () => { generation++; controller()?.stop(); setView({ phase: "idle", conversation: false, level: 0, note: "" }); if (activeStop === stop) activeStop = undefined; };
  createEffect(() => {
    const s = session(), id = s?.info.id, server = workspaceServer();
    if ((lastId && id !== lastId) || server !== lastServer) stop();
    lastId = id; lastServer = server;
    if (!state.ready || state.server?.voice_engine === "off") stop();
    controller()?.update(s);
  });
  const away = () => { if (document.hidden) stop(); };
  const key = (e: KeyboardEvent) => {
    if (view().phase === "idle") return;
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); stop(); }
    else if (e.key === "Enter" && !e.shiftKey && !e.isComposing && !view().conversation) {
      e.preventDefault(); e.stopPropagation();
      if (view().phase === "listening") controller()?.finish(false);
      else stop();
    }
  };
  document.addEventListener("visibilitychange", away);
  document.addEventListener("keydown", key, true);
  window.addEventListener("pagehide", stop);
  onCleanup(() => { disposed = true; stop(); document.removeEventListener("visibilitychange", away); document.removeEventListener("keydown", key, true); window.removeEventListener("pagehide", stop); });
  const start = async (conversation: boolean, selection?: [number, number]) => {
    activeStop?.(); activeStop = stop;
    const gen = ++generation;
    setView({ phase: "starting", conversation, level: 0, note: "Loading voice…" });
    try {
      const part = voicePart.get() ?? await (voicePart.error() ? voicePart.retry() : voicePart.load());
      if (disposed || gen !== generation) return;
      if (!part) { stop(); return; }
      loading ??= Promise.resolve(new part.Conversation({
        view: setView,
        draft: (text) => setDraft(paneId, text),
        async send(text) {
          const identity = draftIdentity(paneId), s = session(), sendingGeneration = generation;
          try {
            await interrupting; interrupting = undefined;
            if (disposed || sendingGeneration !== generation || !state.ready || identity !== draftIdentity(paneId)) throw new Error("Voice ended before the message could be sent.");
            if (sendFromComposer) await sendFromComposer(text);
            else {
              setDraft(paneId, "", identity);
              if (s) await sendMessage(s.info.id, [{ type: "text", text }], "interrupt");
              else await startChat(paneId, [{ type: "text", text }]);
            }
          } catch (e) { setDraft(paneId, text, identity); throw e; }
        },
        interrupt: () => { const id = session()?.info.id; if (id) interrupting = abortSession(id); },
        error: (e) => toastError(e, "Voice"),
      }));
      const c = await loading;
      if (disposed || gen !== generation) return;
      setController(c);
      const draft = getDraft(paneId), [a, b] = selection ?? [draft.length, draft.length];
      await c.start(conversation, session(), conversation ? "" : draft.slice(0, a), conversation ? "" : draft.slice(b));
    } catch (e) { if (!disposed && gen === generation) { loading = undefined; stop(); toastError(e, "Voice"); } }
  };
  return { view, start, stop, bindSend, error: voicePart.error, finish: (send = false) => controller()?.finish(send) };
}
export const VoiceContext = createContext<ReturnType<typeof createVoiceHost>>();
export const useVoice = () => useContext(VoiceContext)!;
