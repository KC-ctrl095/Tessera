// Where a web view goes: the component (and its input handling) is a chunk
// of its own, loaded with the first web view shown. A plain signal rather
// than lazy()/Suspense: a tile that turns into a web view mid-update (a
// spawned tile given its address) must render it, not wait on a boundary.
import { Show, createSignal, type JSX } from "solid-js";
import { Dynamic } from "solid-js/web";
import type { Whiteboard } from "../../proto/Whiteboard";

type Mod = typeof import("./WebView");
const [mod, setMod] = createSignal<Mod | null>(null);
let loading: Promise<void> | null = null;

/** Start loading it (a web view is about to be shown). */
export function loadWebView() {
  loading ??= import("./WebView").then(
    (m) => void setMod(() => m),
    () => {
      loading = null; // try again next time
    },
  );
  return loading;
}

export function WebViewSlot(props: { paneId: string; board?: Whiteboard; fallback?: JSX.Element }) {
  void loadWebView();
  return (
    <Show when={mod()} fallback={props.fallback ?? <div class="board web-view" />}>
      {(m) => <Dynamic component={m().default} paneId={props.paneId} board={props.board} />}
    </Show>
  );
}
