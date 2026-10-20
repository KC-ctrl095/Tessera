// The one way to start something new, the same on every device: the top bar's
// "New chat" and its chevron on computers and the unfolded Fold, the tile
// header's New button (top right) on phones. Everyday things first; the rest
// one step deeper, behind More (it opens in place, no second menu or sheet).
import { newPrivateChat } from "../lib/actions";
import { spawnTile } from "../lib/spawn";
import { openBotEditor, openChannelEditor, openJobEditor } from "../lib/ui";
import { touchUi } from "../lib/util";
import { firstFreeWorkspace, newWorkspace, workspace } from "../lib/workspace";
import { Icon } from "./icons";
import { openMenu, type MenuItem } from "./primitives";
import { ALT, MOD } from "./TopBar";

/** The New menu's rows; `after` runs once one is picked (a phone's drawer closes). */
export function newItems(after: () => void = () => {}): MenuItem[] {
  const pick = (fn: () => unknown) => () => {
    fn();
    after();
  };
  const freeMosaic = firstFreeWorkspace();
  return [
    { label: "Chat", icon: "chat", hint: touchUi() ? undefined : `${MOD} N`, onSelect: pick(() => spawnTile("chat")) },
    { label: "Private chat", icon: "veil", description: "Only on this device, nothing saved", onSelect: pick(() => newPrivateChat()) },
    { label: "Note", icon: "note", description: "Write and keep notes", onSelect: pick(() => spawnTile("note")) },
    { label: "Web view", icon: "globe", description: "A browser you and the agent share", onSelect: pick(() => spawnTile("url")) },
    {
      label: "More",
      description: "Bot, channel, scheduled task, terminal, mosaic",
      more: [
        { label: "Bot", icon: "bot", description: "A helper with its own name and job", onSelect: pick(() => openBotEditor()) },
        { label: "Channel", icon: "hash", description: "A room where your bots work together", onSelect: pick(() => openChannelEditor()) },
        { label: "Scheduled task", icon: "clock", description: "A prompt that runs on its own", onSelect: pick(() => openJobEditor()) },
        { label: "Terminal", icon: "terminal", description: "A command line on your server or a computer, for you only", onSelect: pick(() => spawnTile("terminal")) },
        {
          label: "Mosaic",
          icon: "newMosaic",
          description: "A new screen of tiles",
          hint: touchUi() || freeMosaic < 0 ? undefined : `${ALT} ${freeMosaic + 1}`,
          disabled: freeMosaic < 0,
          onSelect: pick(() => newWorkspace()),
        },
      ],
    },
  ];
}

/** Open the New menu at `anchor` (a compact menu, never a big sheet). */
export function openNewMenu(anchor: HTMLElement, after?: () => void) {
  openMenu(anchor, newItems(after), "bottom-end", { sheet: false, class: "new-menu" });
}

/** A phone's New button, at the top right of the tile on screen. */
export function NewTileButton() {
  return (
    <button
      type="button"
      class="icon-btn head-new"
      aria-label="New"
      aria-haspopup="menu"
      data-keep-focus
      onClick={(e) => openNewMenu(e.currentTarget)}
    >
      <Icon name="compose" size={18} />
    </button>
  );
}

/** Whether tiles' headers carry New (phones: no top bar there). */
export const newInHeader = () => workspace.mode() === "mobile";
