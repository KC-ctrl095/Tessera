// Tiny eager facade. Orchestration and capture both wait for a voice tap.
import { createContext, createSignal, getOwner, onCleanup, runWithOwner, useContext } from "solid-js";
import type { SessionState } from "./store";
import type { View } from "./voice/flow";
import type { VoiceEngineChoice, createVoiceHost as loadedHost } from "./voice/host";
import { onDemand } from "./onDemand";
import { connectionGeneration } from "./connectionGeneration";

const hostPart = onDemand(() => import("./voice/host"), "voice");
export type { VoiceEngineChoice } from "./voice/host";
/** For explicit user setup actions only, never a silent cloud fallback. */
export async function enableVoice(engine: VoiceEngineChoice) {
  const part = hostPart.get() ?? await hostPart.load();
  return part?.enableVoice(engine);
}
export function createVoiceHost(paneId: string, session: () => SessionState | undefined) {
  const owner = getOwner();
  const [host, setHost] = createSignal<ReturnType<typeof loadedHost>>();
  const idle: View = { phase: "idle", conversation: false, level: 0, note: "" };
  const [pending, setPending] = createSignal<View>(idle);
  let generation = 0, disposed = false;
  let sender: ((text: string) => Promise<void>) | undefined;
  let unbind: (() => void) | undefined;
  const stop = () => { generation++; host()?.stop(); setPending(idle); };
  onCleanup(() => { disposed = true; stop(); });
  return {
    view: () => host()?.view() ?? pending(),
    choice: () => host()?.choice(),
    choose: (engine: VoiceEngineChoice) => host()?.choose(engine),
    error: () => host()?.error() ?? hostPart.error(),
    stop,
    finish: (send = false) => { const active = host(); if (active) active.finish(send); else stop(); },
    bindSend(send: (text: string) => Promise<void>) {
      unbind?.(); sender = send; unbind = host()?.bindSend(send);
      return () => { if (sender === send) { sender = undefined; unbind?.(); unbind = undefined; } };
    },
    async start(conversation: boolean, selection?: [number, number]) {
      const gen = ++generation;
      const connection = connectionGeneration(), id = session()?.info.id;
      setPending({ phase: "starting", conversation, level: 0, note: "Loading voice…" });
      let part = hostPart.get();
      if (!part) {
        const escape = (e: KeyboardEvent) => { if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); stop(); } };
        window.addEventListener("keydown", escape, true);
        try { part = await (hostPart.error() ? hostPart.retry() : hostPart.load()); }
        finally { window.removeEventListener("keydown", escape, true); }
      }
      if (disposed || gen !== generation || !part || connection !== connectionGeneration() || id !== session()?.info.id || document.hidden) { if (!disposed && gen === generation) setPending(idle); return; }
      let active = host();
      if (!active) {
        active = runWithOwner(owner, () => part.createVoiceHost(paneId, session))!;
        if (sender) unbind = active.bindSend(sender);
        setHost(active);
      }
      await active.start(conversation, selection);
    },
  };
}
export const VoiceContext = createContext<ReturnType<typeof createVoiceHost>>();
export const useVoice = () => useContext(VoiceContext)!;
