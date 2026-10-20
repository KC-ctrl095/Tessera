// Desktop, with a mouse: the tile under the pointer is where typing goes,
// like Hyprland. Typing with no text box focused lands in the hovered chat's
// message box (no click needed), and — unless turned off in Settings — the
// focus follows the pointer from one chat to another, taking the keyboard
// along when it was in the chat you left.
//
// Never taken over: keys in any other field (titles, search, dialogs, the
// ask panel), boards' frames (their keys never reach the app), shortcuts
// (Ctrl, Alt, ⌘), IME composition, and anything while a menu, popover or
// dialog is open.
import { liveDrag } from "./drag";
import { prefs } from "./prefs";
import { mosaicsOpen } from "./mosaicsPage";
import { dialog, overviewOpen, paletteOpen } from "./ui";
import { focusComposer, focusPane, hasComposer, typeIntoComposer, workspace } from "./workspace";

const finePointer = matchMedia("(hover: hover) and (pointer: fine)");

/** The chat tile under the mouse on the mosaic on screen, if any. */
let hovered: string | null = null;
let composing = false;

const paneAt = (t: EventTarget | null): string | null => {
  const el = (t as Element | null)?.closest?.(".workspace [data-pane-id]") as HTMLElement | null;
  const id = el?.dataset.paneId ?? null;
  return id && workspace.paneWorkspace(id) === workspace.active() ? id : null;
};

/** Something is over the tiles (a menu, a popover, a dialog, an overview page). */
const overlayOpen = () => mosaicsOpen() || paletteOpen() || !!dialog() || overviewOpen() || !!document.querySelector(".popover, .modal-backdrop, .overview");

/** A field of its own is focused: its keys stay there. */
function ownsKeys(el: Element | null): boolean {
  if (!el || el === document.body || el === document.documentElement) return false;
  if (el instanceof HTMLIFrameElement) return true;
  if (el.matches("input, textarea, select, [contenteditable]:not([contenteditable=false]), [role=textbox], [role=combobox]")) return true;
  return !!el.closest("[role=dialog], [role=menu], [role=listbox], .popover");
}

/** The message box of another tile has the keyboard (not a title, a search, a queued message…). */
const composerOf = (el: Element | null): string | null => {
  if (!(el instanceof HTMLTextAreaElement) || !el.closest(".composer")) return null;
  return (el.closest("[data-pane-id]") as HTMLElement | null)?.dataset.paneId ?? null;
};

function onMove(e: PointerEvent) {
  if (e.pointerType !== "mouse" || !finePointer.matches) return;
  const id = paneAt(e.target);
  hovered = id;
  // Any movement over a chat focuses it (Hyprland's follow_mouse), not only entering it.
  if (!id || !prefs.focusFollowsMouse) return;
  // Only chats take the focus by hovering (a board takes it with a click).
  if (!hasComposer(id) || workspace.focused() === id) return;
  if (e.buttons || liveDrag() || composing || overlayOpen()) return;
  const active = document.activeElement;
  const from = composerOf(active);
  // Keys in another field stay there; the message box of the chat we leave follows us.
  if (!from && ownsKeys(active)) return;
  focusPane(id);
  if (from && from !== id) focusComposer(id);
}

function onKey(e: KeyboardEvent) {
  if (e.defaultPrevented || e.isComposing || composing || !finePointer.matches) return;
  // AltGr makes characters (on Windows it arrives as Ctrl+Alt); other modifiers are shortcuts.
  if (!e.getModifierState?.("AltGraph") && (e.ctrlKey || e.metaKey || e.altKey)) return;
  const dead = e.key === "Dead";
  if (!dead && [...e.key].length !== 1) return;
  const id = hovered;
  if (!id || workspace.paneWorkspace(id) !== workspace.active() || !hasComposer(id)) return;
  const active = document.activeElement;
  if (ownsKeys(active) || overlayOpen()) return;
  // Space and Enter belong to a focused button.
  if (e.key === " " && active?.closest("button, a, [role=button], [tabindex]")) return;
  if (workspace.focused() !== id) focusPane(id);
  if (dead) {
    // An accent to come: let the message box receive the next key itself.
    focusComposer(id);
    return;
  }
  e.preventDefault();
  typeIntoComposer(id, e.key);
}

let installed = false;
/** Install once (desktop and web; it does nothing without a mouse). */
export function installHoverTyping() {
  if (installed) return;
  installed = true;
  document.addEventListener("pointermove", onMove, { passive: true });
  document.addEventListener("pointerleave", () => (hovered = null));
  window.addEventListener("blur", () => (hovered = null));
  document.addEventListener("compositionstart", () => (composing = true), true);
  document.addEventListener("compositionend", () => (composing = false), true);
  window.addEventListener("keydown", onKey);
}
