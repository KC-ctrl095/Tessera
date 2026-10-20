// Where a whiteboard tile goes: BoardPane (with the image viewer) is a chunk of
// its own, loaded soon after start (App.tsx) or with the first board shown.
// A plain signal like WebViewSlot.tsx, not lazy()/Suspense: a tile that turns
// into a board mid-update renders it as soon as it is there, not after a boundary.
import { Show, createSignal } from "solid-js";

type Mod = typeof import("./BoardPane");
const [mod, setMod] = createSignal<Mod | null>(null);
let loading: Promise<void> | null = null;

/** Start loading the whiteboard tile. */
export function loadBoardPane() {
  loading ??= import("./BoardPane").then(
    (m) => void setMod(() => m),
    () => {
      loading = null; // try again next time
    },
  );
  return loading;
}

export function BoardSlot(props: { paneId: string }) {
  void loadBoardPane();
  return (
    <Show when={mod()} fallback={<div class="board" />}>
      {(m) => {
        const Pane = m().BoardPane;
        return <Pane paneId={props.paneId} />;
      }}
    </Show>
  );
}
