// A maximized tile's way back (tiled desktop): its header shows Restore
// while it fills the mosaic. Tiles maximize from their options, by
// double-clicking the header, or with Alt+F; they move by dragging the
// header to a side, corner or another tile.
import { Show } from "solid-js";
import { toggleMaximize, workspace } from "../lib/workspace";
import { Icon } from "./icons";
import { usePane } from "./PaneContext";
import { ALT } from "./TopBar";

export function RestoreButton() {
  const pane = usePane();
  return (
    <Show when={workspace.mode() === "tile" && workspace.maximized() === pane.paneId}>
      <button type="button" class="icon-btn head-max" aria-label="Restore tile" data-tip="Restore" data-kbd={`${ALT} F`} onClick={() => toggleMaximize(pane.paneId)}>
        <Icon name="minimize" size={14} />
      </button>
    </Show>
  );
}
