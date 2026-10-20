// All mosaics: a full page of every mosaic, drawn as its tiles, to pick one
// and go into it. It is the level above a mosaic's own tile overview (on
// phones they lead to each other). Ctrl+Tab or Alt+` opens it (Ctrl+Tab
// belongs to the browser in a web page); inside, the arrows (or Tab) choose,
// Enter opens, 1–9 go straight to a mosaic, Esc goes back. On touch screens
// it opens from the Mosaics row of the drawer, the tile overview (its button,
// or pinching in) and a long-press on the tiles button of a tile header.
import { createSignal } from "solid-js";
import { touchUi } from "./util";
import { focusComposer, switchWorkspace, usedWorkspaces, workspace } from "./workspace";

const [mosaicsOpen, setMosaicsOpen] = createSignal(false);

/** Fingers (or buttons) down right now: the page opened by a long-press opens under a finger. */
const down = new Set<number>();
document.addEventListener("pointerdown", (e) => down.add(e.pointerId), true);
for (const t of ["pointerup", "pointercancel"] as const) document.addEventListener(t, (e) => down.delete(e.pointerId), true);
export const pointerHeld = () => down.size > 0;
/** Index into `mosaicsOrder()`. */
const [selected, setSelected] = createSignal(0);
export { mosaicsOpen, selected, setSelected };

/** The mosaics on the page, by number. */
export const mosaicsOrder = (): number[] => usedWorkspaces();

/** Open the page with the mosaic on screen selected. */
export function openMosaics() {
  setSelected(Math.max(0, mosaicsOrder().indexOf(workspace.active())));
  setMosaicsOpen(true);
}

export function closeMosaics() {
  setMosaicsOpen(false);
}

/** Go into a mosaic (default: the selected one). */
export function pickMosaic(index: number | undefined = mosaicsOrder()[selected()]) {
  closeMosaics();
  if (index === undefined) return;
  switchWorkspace(index);
  if (!touchUi()) focusComposer();
}

/** How many cards a row of the page holds (for ↑ and ↓), read off the page itself. */
function columns(): number {
  const cards = Array.from(document.querySelectorAll<HTMLElement>(".mosaics-page .mz-card"));
  if (!cards.length) return 1;
  const top = cards[0].offsetTop;
  return Math.max(1, cards.filter((c) => c.offsetTop === top).length);
}

const step = (d: number) => {
  const n = mosaicsOrder().length;
  if (n) setSelected((i) => Math.min(n - 1, Math.max(0, i + d)));
};

/**
 * Keys while no dialog is open. Returns true when the key was the page's
 * (it is then consumed: nothing typed reaches a message box behind it).
 */
export function mosaicsKeyDown(e: KeyboardEvent): boolean {
  const hotkey = (e.key === "Tab" && e.ctrlKey && !e.altKey && !e.metaKey) || (e.code === "Backquote" && e.altKey && !e.ctrlKey && !e.metaKey);
  if (!mosaicsOpen()) {
    if (!hotkey) return false;
    e.preventDefault();
    e.stopPropagation();
    openMosaics();
    return true;
  }
  if (e.key === "Control" || e.key === "Alt" || e.key === "Shift" || e.key === "Meta") return true;
  // The page's own menus and dialogs (rename) keep their keys.
  if (document.querySelector(".popover, .modal-backdrop")) return false;
  e.preventDefault();
  e.stopPropagation();
  if (hotkey) {
    closeMosaics();
    return true;
  }
  const n = mosaicsOrder().length;
  switch (e.key) {
    case "Tab":
      step(e.shiftKey ? -1 : 1);
      break;
    case "ArrowRight":
      step(1);
      break;
    case "ArrowLeft":
      step(-1);
      break;
    case "ArrowDown":
      if (selected() + columns() < n) setSelected(selected() + columns());
      break;
    case "ArrowUp":
      if (selected() - columns() >= 0) setSelected(selected() - columns());
      break;
    case "Home":
      setSelected(0);
      break;
    case "End":
      setSelected(Math.max(0, n - 1));
      break;
    case "Enter":
    case " ":
      pickMosaic();
      break;
    case "Escape":
      closeMosaics();
      break;
    default: {
      // 1…9 go straight into that mosaic.
      const d = /^Digit([1-9])$/.exec(e.code) ?? /^Numpad([1-9])$/.exec(e.code);
      if (d) {
        const i = Number(d[1]) - 1;
        if (mosaicsOrder().includes(i)) pickMosaic(i);
      }
    }
  }
  return true;
}
