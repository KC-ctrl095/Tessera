// Pictures handled like a document editor's: a selected picture gets a
// handle on its right edge to make it wider or narrower, and (with a mouse
// or pen) it can be dragged to another place in the note. Phones move it
// from its menu (Move up / Move down). Everything is plain markdown: the
// width goes in the alt text as `![alt|300](assets/…)` (what Obsidian and
// others read too), a move moves the picture's line.
import { EditorView } from "@codemirror/view";

/** The narrowest a picture can be made, in px. */
const MIN_WIDTH = 64;

/** `![alt](src)` again with `alt` and `width` changed (null: as wide as it is, up to the page). */
export function setPicture(view: EditorView, from: number, to: number, alt: string, width: number | null) {
  const src = view.state.sliceDoc(from, to);
  const m = /^!\[[^\]\n]*\](\([^)\n]*\))$/.exec(src);
  if (!m) return;
  const label = `${alt.replace(/[[\]\n|]+/g, " ").trim()}${width ? `|${Math.round(width)}` : ""}`;
  view.dispatch({ changes: { from, to, insert: `![${label}]${m[1]}` }, userEvent: "input.picture" });
}

/** A line with nothing on it. */
const blank = (view: EditorView, n: number) => n >= 1 && n <= view.state.doc.lines && !view.state.doc.line(n).text.trim();

/**
 * Move the picture at `from`–`to` next to the line at `pos`: before it or
 * after it, as a paragraph of its own. One step to undo. Returns false when
 * it would stay where it is.
 */
export function movePicture(view: EditorView, from: number, to: number, pos: number, before: boolean): boolean {
  const doc = view.state.doc;
  const src = doc.sliceString(from, to);
  const line = doc.lineAt(from);
  const whole = line.text.trim() === src;
  // Taken out with its line, and the blank line after it (or else the one before), so no gap is left.
  let delFrom = whole ? line.from : from;
  let delTo = whole ? Math.min(doc.length, line.to + 1) : to;
  if (whole && blank(view, line.number + 1)) delTo = Math.min(doc.length, doc.line(line.number + 1).to + 1);
  else if (whole && line.number === doc.lines && blank(view, line.number - 1)) delFrom = doc.line(line.number - 1).from;
  const target = doc.lineAt(Math.max(0, Math.min(pos, doc.length)));
  const at = before ? target.from : target.to;
  if (at >= delFrom && at <= delTo) return false;
  if (target.number === line.number) return false;
  let insert: string;
  if (before) {
    const prevBlank = target.number === 1 || blank(view, target.number - 1);
    insert = `${prevBlank ? "" : "\n"}${src}\n\n`;
  } else {
    const nextText = target.number < doc.lines && !blank(view, target.number + 1);
    insert = `\n\n${src}${nextText ? "\n" : ""}`;
  }
  // Where the picture's text starts within what goes in.
  const offset = before ? (insert.startsWith("\n") ? 1 : 0) : 2;
  const changes = view.state.changes([
    { from: delFrom, to: delTo },
    { from: at, insert },
  ]);
  const start = changes.mapPos(at, -1) + offset;
  view.dispatch({ changes, selection: { anchor: start, head: start + src.length }, userEvent: "move.picture", scrollIntoView: true });
  return true;
}

/** Move the picture one paragraph up (-1) or down (1). */
export function movePictureBy(view: EditorView, from: number, to: number, dir: -1 | 1): boolean {
  const doc = view.state.doc;
  const line = doc.lineAt(from);
  let n = line.number + dir;
  while (blank(view, n)) n += dir;
  if (n < 1 || n > doc.lines) return false;
  // The whole paragraph it passes.
  while (!blank(view, n + dir) && n + dir >= 1 && n + dir <= doc.lines) n += dir;
  return movePicture(view, from, to, doc.line(n).from, dir < 0);
}

/** The width the page gives pictures (the text column). */
function columnWidth(view: EditorView): number {
  const cs = getComputedStyle(view.contentDOM);
  return view.contentDOM.clientWidth - parseFloat(cs.paddingLeft || "0") - parseFloat(cs.paddingRight || "0");
}

/** What a picture's element needs from its widget at the time of a drag. */
export interface PictureRef {
  from: number;
  to: number;
  alt: string;
  width: number | null;
}

/**
 * The resize handle and dragging for a picture's element `el`. `current`
 * gives where its markdown is now. `onDone` is told a drag happened, so the
 * click that ends it doesn't also open the picture's menu. Returns the
 * handle (kept as the element's last child).
 */
export function pictureGestures(el: HTMLElement, view: EditorView, current: () => PictureRef, onDone: () => void): HTMLElement {
  const handle = document.createElement("span");
  handle.className = "cm-pic-handle";
  handle.setAttribute("role", "slider");
  handle.setAttribute("aria-label", "Picture width");
  handle.title = "Drag to make it wider or narrower";
  el.append(handle);

  handle.addEventListener("pointerdown", (e) => {
    if (e.button !== 0 || !view.state.facet(EditorView.editable)) return;
    e.preventDefault();
    e.stopPropagation();
    const startX = e.clientX;
    const startW = el.getBoundingClientRect().width;
    const max = Math.max(MIN_WIDTH, columnWidth(view));
    let w = startW;
    handle.setPointerCapture(e.pointerId);
    el.classList.add("resizing");
    const move = (ev: PointerEvent) => {
      w = Math.round(Math.max(MIN_WIDTH, Math.min(max, startW + ev.clientX - startX)));
      el.style.width = `${w}px`;
      el.classList.add("sized");
      handle.setAttribute("aria-valuenow", String(w));
    };
    const up = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      handle.removeEventListener("pointercancel", up);
      el.classList.remove("resizing");
      onDone();
      if (Math.abs(w - startW) < 2) return;
      const p = current();
      // As wide as the page: no width at all (it follows the tile's width).
      setPicture(view, p.from, p.to, p.alt, w >= max - 6 ? null : Math.round(w / 10) * 10);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
    handle.addEventListener("pointercancel", up);
  });

  // Dragging the picture itself to another place: mouse and pen (a finger scrolls the page).
  el.addEventListener("pointerdown", (e) => {
    if (e.button !== 0 || e.pointerType === "touch" || e.target === handle || !view.state.facet(EditorView.editable)) return;
    const sx = e.clientX;
    const sy = e.clientY;
    let dragging = false;
    let drop: { pos: number; before: boolean } | null = null;
    const marker = document.createElement("div");
    marker.className = "cm-pic-drop";
    const id = e.pointerId;
    const move = (ev: PointerEvent) => {
      if (!dragging) {
        if (Math.hypot(ev.clientX - sx, ev.clientY - sy) < 6) return;
        dragging = true;
        try {
          el.setPointerCapture(id);
        } catch {
          /* already let go */
        }
        el.classList.add("dragging");
        view.scrollDOM.append(marker);
      }
      const pos = view.posAtCoords({ x: ev.clientX, y: ev.clientY }, false);
      const block = view.lineBlockAtHeight(ev.clientY - view.documentTop);
      const before = ev.clientY - view.documentTop < block.top + block.height / 2;
      drop = { pos, before };
      const line = view.state.doc.lineAt(pos);
      const box = view.scrollDOM.getBoundingClientRect();
      const lineBlock = view.lineBlockAt(line.from);
      const y = (before ? lineBlock.top : lineBlock.bottom) + view.documentTop - box.top + view.scrollDOM.scrollTop;
      marker.style.top = `${Math.round(y - 1)}px`;
      // Near the top or bottom edge: the page scrolls along.
      if (ev.clientY < box.top + 24) view.scrollDOM.scrollTop -= 12;
      else if (ev.clientY > box.bottom - 24) view.scrollDOM.scrollTop += 12;
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      marker.remove();
      el.classList.remove("dragging");
      if (!dragging) return;
      onDone();
      const p = current();
      if (drop) movePicture(view, p.from, p.to, drop.pos, drop.before);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  });
  return handle;
}
