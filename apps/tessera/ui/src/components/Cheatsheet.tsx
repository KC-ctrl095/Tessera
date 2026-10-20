import { For } from "solid-js";
import "../styles/cheatsheet.css";
import { setCheatsheetOpen } from "../lib/ui";
import { Modal } from "./primitives";

const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
const mod = isMac ? "⌘" : "Ctrl";
const alt = isMac ? "⌥" : "Alt";

const GROUPS: { title: string; items: [string[], string][] }[] = [
  {
    title: "Tiles",
    items: [
      [[`${alt} ↵`, `${mod} N`], "New chat in a new tile (reuses an unused one)"],
      [[`${alt} W`], "Close tile"],
      [[`${alt} F`], "Maximize / restore tile"],
      [[`${alt} Space`], `Cycle layout: dwindle → master → columns → rows → grid → monocle (or pick one in the palette, ${mod} K)`],
      [["New chat ▾ (top bar)"], "A chat with one click; the ▾ for everything else new: a private chat, a note, a web view, and under More a bot, channel, scheduled task, terminal or mosaic"],
    ],
  },
  {
    title: "Focus & move",
    items: [
      [[`${alt} H J K L`, `${alt} ← ↓ ↑ →`], "Focus the tile in that direction"],
      [[`${alt} ⇧ H J K L`], "Swap with the tile in that direction"],
      [[`${alt} ${mod} H J K L`], "Resize: shrink/grow width (H/L) and height (K/J)"],
    ],
  },
  {
    title: "Mosaics",
    items: [
      [[`${mod} Tab`, `${alt} \``], "All mosaics, on a page of their own: arrows choose, ↵ opens, 1…9 go straight there, Esc goes back"],
      [[`${alt} 1…9`], "Switch to a mosaic (an unused number opens a new one)"],
      [[`${alt} ⇧ 1…9`], "Move the focused tile to that mosaic (and follow)"],
    ],
  },
  {
    title: "Mouse",
    items: [
      [["Click"], "Focus a tile"],
      [["Drag divider"], "Resize · double-click to equalize"],
      [["Type over a tile"], "Keys go to the chat under the pointer, no click needed"],
      [["Drag tile header"], "Swap (centre), move to a tile's edge, snap to a side or corner of the mosaic, or drop on a mosaic"],
      [["Double-click tile header"], "Maximize / restore the tile"],
      [["Click sidebar chat"], "Open it in a new tile (its ⋯ menu: Open in selected tile)"],
      [["Drag sidebar chat"], "Drop on a tile edge to split there, centre to replace"],
    ],
  },
  {
    title: "General",
    items: [
      [[`${mod} K`], "Command palette — type @handle to chat with a bot"],
      [[`${alt} /`], "This cheatsheet"],
      [[`${mod} B`], "Toggle sidebar"],
      [[`${mod} ,`], "Settings"],
      [["Y / A / N"], "Answer a focused approval"],
      [["↵"], "While the agent works: queue a follow-up (sent when it's done)"],
      [[`${mod} ↵`, "/steer"], "While the agent works: steer it now (read at its next step)"],
      [[`${mod} ⇧ ↵`], "While the agent works: stop it and send this now"],
    ],
  },
];

export function Cheatsheet() {
  return (
    <Modal onClose={() => setCheatsheetOpen(false)} title="Keyboard shortcuts" size="lg" class="cheatsheet">
      <div class="modal-body cheat-body">
        <For each={GROUPS}>
          {(g) => (
            <section class="cheat-group">
              <h3>{g.title}</h3>
              <dl>
                <For each={g.items}>
                  {([keys, desc]) => (
                    <>
                      <dt>
                        <For each={keys}>{(k) => <kbd>{k}</kbd>}</For>
                      </dt>
                      <dd>{desc}</dd>
                    </>
                  )}
                </For>
              </dl>
            </section>
          )}
        </For>
      </div>
    </Modal>
  );
}
