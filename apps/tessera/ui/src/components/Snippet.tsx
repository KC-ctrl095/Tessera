import { For, Show, createMemo } from "solid-js";
import { onDemand } from "../lib/onDemand";

/** The words-and-marks code (lib/snippet.ts) loads the first time a search or note list needs it. */
export const snippetLib = onDemand(() => import("../lib/snippet"));

/** One line of a search hit or note preview: plain words (unless `raw`), the match in bold. */
export function Snippet(props: { text: string; query?: string; raw?: boolean }) {
  void snippetLib.load();
  const pieces = createMemo(() => snippetLib.get()?.pieces(props.text, props.query, props.raw));
  return (
    <Show when={pieces()} fallback={props.text}>
      <For each={pieces()}>{(p) => (p.hit ? <mark>{p.text}</mark> : p.text)}</For>
    </Show>
  );
}
