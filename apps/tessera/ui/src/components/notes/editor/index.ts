// The note editor (CodeMirror 6 with a live page, see live.ts): built here,
// loaded as its own chunk the first time a note opens. The text is the
// note's markdown, always; nothing is converted on the way in or out.
import { defaultKeymap, history, historyKeymap, redo, undo } from "@codemirror/commands";
import { closeSearchPanel, openSearchPanel, search, searchKeymap } from "@codemirror/search";
import { Annotation, Compartment, EditorSelection, EditorState, type Extension, MapMode, StateEffect, StateField, Transaction } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView, type Rect, type Tooltip, keymap, placeholder, showTooltip, tooltips } from "@codemirror/view";
import { ensureStyles } from "../../../lib/cmStyles";
import { diffText } from "../../../lib/textDiff";
import { blocks } from "./blocks";
import { suggestions } from "./complete";
import { type NoteHooks, noteHooks, redraw } from "./hooks";
import { INSERTS, type InsertItem } from "./insert";
import { type LineKind, autoBlocks, commands, enterAfterWord, enterFromWord, keepTablesApart, lineKind, noteKeymap, typingRules } from "./keys";
import { noteLanguage, noteParser } from "./language";
import { live } from "./live";
import { addPictures, pasting } from "./paste";

export type { NoteHooks, InsertItem, LineKind };
export { INSERTS, addPictures, noteParser };

/** A change that came from outside (not typed here): not reported back as an edit. */
const fromOutside = Annotation.define<boolean>();

/** Ranges of the text an agent just changed, tinted while its changes are offered. */
const setTint = StateEffect.define<{ from: number; to: number }[] | null>();
const tint = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    deco = deco.map(tr.changes);
    for (const e of tr.effects) {
      if (!e.is(setTint)) continue;
      deco = e.value ? Decoration.set(e.value.filter((r) => r.to > r.from).map((r) => Decoration.mark({ class: "cm-agent" }).range(r.from, r.to)), true) : Decoration.none;
    }
    return deco;
  },
  provide: (f) => EditorView.decorations.from(f),
});

/** The little bar over selected text (computers; phones have the format bar). */
function bubble(view: EditorView): HTMLElement {
  const el = document.createElement("div");
  el.className = "cm-bubble";
  el.setAttribute("role", "toolbar");
  el.setAttribute("aria-label", "Format");
  const items: [string, string, keyof typeof commands, string][] = [
    ["B", "Bold", "bold", "Ctrl+B"],
    ["I", "Italic", "italic", "Ctrl+I"],
    ["S", "Strikethrough", "strike", "Ctrl+Shift+X"],
    ["</>", "Code", "code", "Ctrl+E"],
    ["H", "Highlight", "highlight", "Ctrl+Shift+H"],
    ["🔗", "Link", "link", "Ctrl+K"],
  ];
  for (const [text, label, cmd, keys] of items) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = `cm-bubble-btn cm-bubble-${cmd}`;
    b.textContent = text;
    b.title = `${label} (${keys.replace("Ctrl", /Mac/.test(navigator.platform) ? "⌘" : "Ctrl")})`;
    b.setAttribute("aria-label", label);
    b.addEventListener("mousedown", (e) => e.preventDefault());
    b.addEventListener("click", () => {
      commands[cmd](view);
      view.focus();
    });
    el.append(b);
  }
  return el;
}

/**
 * The whole selection's box, so the bubble goes above its first row or below
 * its last one and never over it. Rows are taller than their characters'
 * boxes (the line spacing): the box takes the rows' full height. A selection
 * that runs past what's drawn reaches the bottom of the page.
 */
function selectionBox(view: EditorView): Rect {
  const row = (pos: number, side: 1 | -1): Rect | null => {
    const c = view.coordsAtPos(pos, side);
    if (!c) return null;
    let lh = view.defaultLineHeight;
    try {
      const n = view.domAtPos(pos).node;
      const line = (n.nodeType === 1 ? (n as HTMLElement) : n.parentElement)?.closest(".cm-line");
      const v = line ? parseFloat(getComputedStyle(line).lineHeight) : NaN;
      if (v > 0) lh = v;
    } catch {
      /* not drawn: the default spacing */
    }
    const pad = Math.max(1, (lh - (c.bottom - c.top)) / 2 + 1);
    return { left: c.left, right: c.right, top: c.top - pad, bottom: c.bottom + pad };
  };
  const r = view.state.selection.main;
  const a = row(r.from, 1);
  const b = row(r.to, -1);
  const bottom = b?.bottom ?? view.scrollDOM.getBoundingClientRect().bottom;
  if (!a) return b ?? { left: 0, right: 0, top: bottom, bottom };
  return { left: a.left, right: Math.max(a.right, a.left + 1), top: Math.min(a.top, b?.top ?? a.top), bottom: Math.max(a.bottom, bottom) };
}

/**
 * Where the editor's floating bits (the format bubble, suggestions) may go:
 * the page's visible part, under the tile's header and inside the tile. So a
 * selection on the first line gets its bubble below it instead of over the
 * header (or, where the tile clips, over the line itself).
 */
const inPage = tooltips({
  tooltipSpace: (view) => {
    const r = view.scrollDOM.getBoundingClientRect();
    const d = view.dom.ownerDocument.documentElement;
    return { top: Math.max(0, r.top), left: Math.max(0, r.left), bottom: Math.min(d.clientHeight, r.bottom), right: Math.min(d.clientWidth, r.right) };
  },
});

function bubbleField(touch: boolean): Extension {
  if (touch) return [];
  const make = (state: EditorState): readonly Tooltip[] => {
    const r = state.selection.main;
    if (r.empty || state.readOnly || state.selection.ranges.length > 1) return [];
    // Not over a picture or table someone just clicked.
    const text = state.sliceDoc(r.from, Math.min(r.to, r.from + 200));
    if (/^!\[[^\]\n]*\]\([^)\n]*\)$/.test(state.sliceDoc(r.from, r.to))) return [];
    if (!text.trim()) return [];
    return [{ pos: r.from, end: r.to, above: true, strictSide: false, arrow: false, create: (view) => ({ dom: bubble(view), resize: false, getCoords: () => selectionBox(view) }) }];
  };
  return StateField.define<readonly Tooltip[]>({
    create: make,
    update: (v, tr) => (tr.docChanged || tr.selection || tr.reconfigured ? make(tr.state) : v),
    provide: (f) => showTooltip.computeN([f], (s) => s.field(f)),
  });
}

/** Opening links: a click on a drawn link or wikilink (not while its syntax shows). */
const linkClicks = EditorView.domEventHandlers({
  mousedown(e, view) {
    if (e.button !== 0 || e.shiftKey || e.altKey) return false;
    const t = e.target as HTMLElement;
    const a = t.closest<HTMLElement>("[data-href], [data-wiki]");
    if (!a || !view.contentDOM.contains(a)) return false;
    e.preventDefault();
    const hooks = view.state.facet(noteHooks);
    if (a.dataset.wiki !== undefined) hooks.openLink(a.dataset.wiki, a);
    else if (a.dataset.href) hooks.openUrl(a.dataset.href);
    return true;
  },
});

/**
 * A space typed on a phone can reach the page as a non-breaking one (the
 * browser's way of keeping a space visible next to a drawn marker). In the
 * note it is a space: the note gets one at once, the page once the keyboard
 * is away (changing it under a keyboard while typing confuses it). Only the
 * ones typed here: a note's own non-breaking spaces stay.
 */
const typedNbsp = StateField.define<number[]>({
  create: () => [],
  update(list, tr) {
    if (!tr.docChanged) return list;
    const next = list.map((p) => tr.changes.mapPos(p, 1, MapMode.TrackDel)).filter((p): p is number => p !== null && tr.newDoc.sliceString(p, p + 1) === "\u00a0");
    if (tr.isUserEvent("input.type")) {
      tr.changes.iterChanges((_fa, _ta, fb, _tb, ins) => {
        const t = ins.toString();
        for (let i = t.indexOf("\u00a0"); i >= 0; i = t.indexOf("\u00a0", i + 1)) next.push(fb + i);
      });
    }
    return next;
  },
});

/** The text, with the non-breaking spaces typed here as plain ones. */
function withSpaces(state: EditorState): string {
  const text = state.doc.toString();
  const at = state.field(typedNbsp, false);
  if (!at?.length) return text;
  let out = "";
  let last = 0;
  for (const p of [...at].sort((a, b) => a - b)) {
    out += text.slice(last, p) + " ";
    last = p + 1;
  }
  return out + text.slice(last);
}

function plainSpaces(view: EditorView) {
  const at = view.state.field(typedNbsp, false);
  if (!at?.length || view.composing) return;
  view.dispatch({ changes: at.map((p) => ({ from: p, to: p + 1, insert: " " })), annotations: [fromOutside.of(true), Transaction.addToHistory.of(false)] });
}

export interface NoteEditorOptions {
  parent: HTMLElement;
  text: string;
  readOnly: boolean;
  touch: boolean;
  hooks: NoteHooks;
  onChange: (text: string) => void;
  onSave: () => void;
  /** The caret moved or the text changed (the format bar's state). */
  onCursor?: (kind: LineKind) => void;
  onFocus?: (focused: boolean) => void;
}

export interface NoteEditor {
  view: EditorView;
  /** Take text from elsewhere (another device, an agent, Undo, Restore): changed as little as possible, the caret kept. */
  setText(text: string, undoable: boolean): void;
  setReadOnly(on: boolean): void;
  focus(where?: "start" | "end"): void;
  blur(): void;
  run(cmd: keyof typeof commands): void;
  insert(item: InsertItem): void;
  find(): void;
  undo(): void;
  redo(): void;
  /** Tint what an agent changed (ranges in the current text), or stop. */
  tint(ranges: { from: number; to: number }[] | null): void;
  /** Show the notes' titles again (the library changed). */
  redraw(): void;
  goTo(pos: number): void;
  addPictures(files: File[]): void;
  destroy(): void;
}

export function createNoteEditor(o: NoteEditorOptions): NoteEditor {
  const ro = new Compartment();
  const roExt = (on: boolean) => [EditorState.readOnly.of(on), EditorView.editable.of(!on)];
  // A note written with Windows line ends keeps them (else any edit would change every line):
  // the editor works with plain line breaks, the note gets its own back.
  const crlf = o.text.includes("\r\n") && !/\r(?!\n)|(?<!\r)\n/.test(o.text);
  const inn = (t: string) => (crlf ? t.replace(/\r\n/g, "\n") : t);
  const out = (t: string) => (crlf ? t.replace(/\n/g, "\r\n") : t);
  const state = EditorState.create({
    doc: inn(o.text),
    extensions: [
      noteLanguage,
      typedNbsp,
      noteHooks.of(o.hooks),
      history(),
      EditorView.lineWrapping,
      ro.of(roExt(o.readOnly)),
      live,
      blocks,
      tint,
      noteKeymap(o.onSave),
      typingRules,
      autoBlocks,
      keepTablesApart,
      enterAfterWord,
      suggestions,
      pasting,
      linkClicks,
      search({ top: true }),
      keymap.of([...searchKeymap, ...historyKeymap, ...defaultKeymap]),
      inPage,
      bubbleField(o.touch),
      placeholder(o.touch ? "Write something…" : "Write something… or type / for headings, lists, tables and pictures"),
      EditorView.contentAttributes.of({ "data-swipe": "", spellcheck: "true", autocorrect: "on", autocapitalize: "sentences", "aria-label": "Note text" }),
      EditorView.updateListener.of((u) => {
        // Enter came with a word (enterAfterWord) and was done: Android's Enter key, which the
        // editor may still be holding back to replay, is not done twice.
        if (u.transactions.some((t) => t.annotation(enterFromWord))) (u.view as unknown as { observer?: { clearDelayedAndroidKey?: () => void } }).observer?.clearDelayedAndroidKey?.();
        if (u.docChanged && u.transactions.some((t) => !t.annotation(fromOutside))) o.onChange(out(withSpaces(u.state)));
        if (u.docChanged || u.selectionSet) o.onCursor?.(lineKind(u.state, u.state.selection.main.head));
        if (u.focusChanged) {
          const writing = u.view.root.activeElement === u.view.contentDOM;
          o.onFocus?.(writing);
          if (!writing) queueMicrotask(() => plainSpaces(u.view));
        }
      }),
    ],
  });
  const view = new EditorView({ state, parent: o.parent });
  ensureStyles(view);

  return {
    view,
    setText(text, undoable) {
      text = inn(text);
      const cur = view.state.doc.toString();
      if (cur === text) return;
      const changes: { from: number; to?: number; insert?: string }[] = [];
      let pos = 0;
      for (const r of diffText(cur, text)) {
        if (r.op === 0) pos += r.text.length;
        else if (r.op === -1) {
          changes.push({ from: pos, to: pos + r.text.length });
          pos += r.text.length;
        } else changes.push({ from: pos, insert: r.text });
      }
      view.dispatch({ changes, annotations: [fromOutside.of(true), Transaction.addToHistory.of(undoable), Transaction.userEvent.of("input.outside")] });
    },
    setReadOnly(on) {
      view.dispatch({ effects: ro.reconfigure(roExt(on)) });
      if (on) closeSearchPanel(view);
    },
    focus(where) {
      view.focus();
      if (where === "start") view.dispatch({ selection: EditorSelection.cursor(0), scrollIntoView: true });
      else if (where === "end") view.dispatch({ selection: EditorSelection.cursor(view.state.doc.length), scrollIntoView: true });
    },
    blur() {
      view.contentDOM.blur();
    },
    run(cmd) {
      commands[cmd](view);
    },
    insert(item) {
      item.run(view);
    },
    find() {
      openSearchPanel(view);
    },
    undo() {
      undo(view);
    },
    redo() {
      redo(view);
    },
    tint(ranges) {
      view.dispatch({ effects: setTint.of(ranges) });
    },
    redraw() {
      view.dispatch({ effects: redraw.of(null) });
    },
    goTo(pos) {
      view.focus();
      view.dispatch({ selection: EditorSelection.cursor(Math.min(pos, view.state.doc.length)), effects: EditorView.scrollIntoView(pos, { y: "start", yMargin: 24 }) });
    },
    addPictures(files) {
      addPictures(view, files, view.state.selection.main.head);
    },
    destroy() {
      view.destroy();
    },
  };
}

/** The headings of a note's text, for its outline. */
export function headings(text: string): { level: number; text: string; pos: number }[] {
  const out: { level: number; text: string; pos: number }[] = [];
  const tree = noteParser.parse(text);
  tree.iterate({
    enter: (n) => {
      const m = /^(?:ATX|Setext)Heading(\d)$/.exec(n.name);
      if (!m) return n.name === "Document" || n.name === "Blockquote" || n.name === "BulletList" || n.name === "OrderedList" || n.name === "ListItem" ? undefined : false;
      const raw = text.slice(n.from, n.to).split("\n")[0];
      const t = raw.replace(/^\s*#{1,6}\s*/, "").replace(/\s+#+\s*$/, "").replace(/[*_`~=]|\[\[|\]\]/g, "").trim();
      if (t) out.push({ level: Number(m[1]), text: t, pos: n.from });
      return false;
    },
  });
  return out;
}
