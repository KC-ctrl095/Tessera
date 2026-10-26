// Tiny per-pane command bus (slash commands / palette → pane header pickers).
export type PaneAction = "model" | "thinking" | "permission" | "rename" | "context" | "fast" | "find" | "voice";

const target = new EventTarget();

export function paneAction(paneId: string, action: PaneAction) {
  target.dispatchEvent(new CustomEvent(paneId, { detail: action }));
}

export function onPaneAction(paneId: string, cb: (a: PaneAction) => void): () => void {
  const h = (e: Event) => cb((e as CustomEvent<PaneAction>).detail);
  target.addEventListener(paneId, h);
  return () => target.removeEventListener(paneId, h);
}
