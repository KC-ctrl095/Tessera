// Tables, edited where they are: a tap on a cell writes in it, Tab and Enter
// move on (Tab from the last cell adds a row), a cell's ⋯ adds or removes
// rows and columns and aligns. Every edit changes only the cells it touches
// in the markdown underneath, the rest stays exactly as written.
import type { EditorState } from "@codemirror/state";
import { EditorView, WidgetType } from "@codemirror/view";
import { renderMarkdownFragment } from "../../../lib/markdown";
import { noteHooks } from "./hooks";

/** A cell: its stretch between pipes, and its text in there (trimmed). */
interface Cell {
  a: number;
  b: number;
  text: string;
}
interface Row {
  from: number;
  to: number;
  cells: Cell[];
  /** It starts / ends with a pipe. */
  lead: boolean;
  trail: boolean;
}
export interface TableModel {
  from: number;
  to: number;
  head: Row;
  delim: Row;
  rows: Row[];
  align: ("left" | "center" | "right" | null)[];
  cols: number;
}

/** The cells of one table line (pipes in code spans and escaped ones don't count). */
function splitRow(text: string, from: number): Row {
  const pipes: number[] = [];
  let tick = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === "\\") {
      i++;
      continue;
    }
    if (c === "`") {
      let n = 1;
      while (text[i + n] === "`") n++;
      tick = tick === n ? 0 : tick || n;
      i += n - 1;
      continue;
    }
    if (c === "|" && !tick) pipes.push(i);
  }
  const lead = /^\s*\|/.test(text);
  const trail = /\|\s*$/.test(text) && pipes.length > (lead ? 1 : 0);
  const bounds = [lead ? pipes[0] : -1, ...pipes.slice(lead ? 1 : 0, trail ? -1 : undefined), trail ? pipes[pipes.length - 1] : text.length];
  const cells: Cell[] = [];
  for (let k = 0; k + 1 < bounds.length; k++) {
    const a = bounds[k] + 1;
    const b = bounds[k + 1];
    cells.push({ a: from + a, b: from + b, text: text.slice(a, b).trim() });
  }
  return { from, to: from + text.length, cells, lead, trail };
}

export function parseTable(state: EditorState, from: number, to: number): TableModel | null {
  const doc = state.doc;
  const first = doc.lineAt(from).number;
  const last = doc.lineAt(to).number;
  if (last - first < 1) return null;
  const rows: Row[] = [];
  for (let n = first; n <= last; n++) {
    const l = doc.line(n);
    rows.push(splitRow(l.text, l.from));
  }
  const [head, delim, ...body] = rows;
  const align = delim.cells.map((c) => {
    const t = c.text;
    const l = t.startsWith(":");
    const r = t.endsWith(":");
    return l && r ? "center" : r ? "right" : l ? "left" : null;
  });
  return { from: doc.line(first).from, to: doc.line(last).to, head, delim, rows: body, align, cols: head.cells.length };
}

/** Text written into a cell: pipes escaped, line breaks as <br>. */
const cellText = (t: string) => t.replace(/\\?\|/g, "\\|").replace(/\r?\n/g, "<br>");
/** A cell's text for editing: <br> back to line breaks, escaped pipes as pipes. */
const editText = (t: string) => t.replace(/<br\s*\/?>/gi, "\n").replace(/\\\|/g, "|");

type Change = { from: number; to?: number; insert?: string };

/** The changes that make cell (r, c) say `text` (r -1: the header). */
function setCell(t: TableModel, r: number, c: number, text: string): Change[] {
  const row = r < 0 ? t.head : t.rows[r];
  if (!row) return [];
  const cell = row.cells[c];
  if (cell) return [{ from: cell.a, to: cell.b, insert: ` ${cellText(text)} ` }];
  // A short row: add the missing cells.
  let add = "";
  for (let k = row.cells.length; k <= c; k++) add += `${k === c ? ` ${cellText(text)} ` : "  "}|`;
  return [{ from: row.to, insert: (row.trail ? "" : "|") + add }];
}

const blankRow = (t: TableModel) => `|${"  |".repeat(t.cols)}`;

function insertRow(t: TableModel, at: number): Change[] {
  // `at`: the body row it becomes (0: first under the header).
  const after = at === 0 ? t.delim : t.rows[at - 1];
  return [{ from: after.to, insert: `\n${blankRow(t)}` }];
}

function deleteRow(t: TableModel, r: number): Change[] {
  const row = t.rows[r];
  return row ? [{ from: row.from - 1, to: row.to }] : [];
}

function insertCol(t: TableModel, c: number): Change[] {
  const out: Change[] = [];
  const each = (row: Row, content: string) => {
    if (c < row.cells.length) out.push({ from: row.cells[c].a, insert: `${content}|` });
    else if (row.trail) out.push({ from: row.to, insert: `${content}|` });
    else out.push({ from: row.to, insert: `|${content}` });
  };
  each(t.head, `  `);
  each(t.delim, " --- ");
  for (const r of t.rows) each(r, "  ");
  return out;
}

function deleteCol(t: TableModel, c: number): Change[] {
  const out: Change[] = [];
  for (const row of [t.head, t.delim, ...t.rows]) {
    const cell = row.cells[c];
    if (!cell) continue;
    // The cell and the pipe before it (or after it, for the first without a leading pipe).
    if (c > 0 || row.lead) out.push({ from: cell.a - 1, to: cell.b });
    else out.push({ from: cell.a, to: Math.min(row.to, cell.b + 1) });
  }
  return out;
}

function setAlign(t: TableModel, c: number, a: TableModel["align"][number]): Change[] {
  const cell = t.delim.cells[c];
  if (!cell) return [];
  const d = a === "center" ? ":---:" : a === "right" ? "---:" : a === "left" ? ":---" : "---";
  return [{ from: cell.a, to: cell.b, insert: ` ${d} ` }];
}

/** A new table to put in the text. */
export const NEW_TABLE = "| Column 1 | Column 2 |\n| --- | --- |\n|  |  |";

/** Where to put the caret (which cell to open) after the table is drawn again. */
const pendingFocus = new WeakMap<EditorView, { r: number; c: number; select: boolean }>();
/** Open a cell of the table at `pos` once it is drawn. */
export function focusCellSoon(view: EditorView, r: number, c: number, select = false) {
  pendingFocus.set(view, { r, c, select });
}

export class TableWidget extends WidgetType {
  constructor(readonly t: TableModel) {
    super();
  }
  /** Same text, same table: nothing to draw. */
  eq(o: TableWidget) {
    return o.t.from === this.t.from && o.t.to === this.t.to && sameCells(o.t, this.t);
  }
  get estimatedHeight() {
    return 40 + 37 * (this.t.rows.length + 1);
  }
  toDOM(view: EditorView) {
    const wrap = document.createElement("div");
    wrap.className = "cm-table";
    const scroll = document.createElement("div");
    scroll.className = "cm-table-scroll";
    const table = document.createElement("table");
    scroll.append(table);
    wrap.append(scroll);
    if (view.state.facet(EditorView.editable)) {
      const addRow = button("cm-table-add cm-table-add-row", "Add a row", () => this.edit(view, wrap, (t) => insertRow(t, t.rows.length), (t) => ({ r: t.rows.length, c: 0 })));
      const addCol = button("cm-table-add cm-table-add-col", "Add a column", () => this.edit(view, wrap, (t) => insertCol(t, t.cols), (t) => ({ r: -1, c: t.cols })));
      wrap.append(addRow, addCol);
    }
    tables.set(wrap, this);
    this.fill(view, wrap);
    queueMicrotask(() => this.takeFocus(view, wrap));
    return wrap;
  }
  updateDOM(dom: HTMLElement, view: EditorView) {
    tables.set(dom, this);
    this.fill(view, dom);
    queueMicrotask(() => this.takeFocus(view, dom));
    return true;
  }
  ignoreEvent() {
    return true;
  }
  destroy(dom: HTMLElement) {
    tables.delete(dom);
  }

  /** The table as it is in the text now (the widget may be a step behind). */
  private now(view: EditorView, dom: HTMLElement): TableModel | null {
    const pos = view.posAtDOM(dom);
    const t = tables.get(dom)?.t ?? this.t;
    const line = view.state.doc.lineAt(pos);
    const lastLine = view.state.doc.lineAt(Math.min(view.state.doc.length, pos + (t.to - t.from)));
    return parseTable(view.state, line.from, lastLine.to);
  }

  private edit(view: EditorView, dom: HTMLElement, changes: (t: TableModel) => Change[], focus?: (t: TableModel) => { r: number; c: number }) {
    const t = this.now(view, dom);
    if (!t) return;
    const ch = changes(t);
    if (!ch.length) return;
    if (focus) focusCellSoon(view, focus(t).r, focus(t).c);
    view.dispatch({ changes: ch, userEvent: "input.table" });
  }

  private takeFocus(view: EditorView, dom: HTMLElement) {
    const want = pendingFocus.get(view);
    if (!want || !dom.isConnected) return;
    pendingFocus.delete(view);
    this.open(view, dom, want.r, want.c, want.select);
  }

  /** Draw the cells (all but the one being written in, which keeps its caret). */
  private fill(view: EditorView, dom: HTMLElement) {
    const t = this.t;
    const table = dom.querySelector("table")!;
    const active = document.activeElement;
    const editing = active instanceof HTMLTextAreaElement && dom.contains(active) ? { r: Number(active.dataset.r), c: Number(active.dataset.c) } : null;
    const fits = table.rows.length === t.rows.length + 1 && Array.from(table.rows).every((tr) => tr.cells.length === t.cols);
    if (!fits) {
      table.replaceChildren();
      const head = table.createTHead().insertRow();
      for (let c = 0; c < t.cols; c++) head.append(document.createElement("th"));
      const body = table.createTBody();
      for (let r = 0; r < t.rows.length; r++) {
        const tr = body.insertRow();
        for (let c = 0; c < t.cols; c++) tr.append(document.createElement("td"));
      }
    }
    [t.head, ...t.rows].forEach((row, i) => {
      const r = i - 1;
      for (let c = 0; c < t.cols; c++) {
        const td = table.rows[i].cells[c];
        td.dataset.r = String(r);
        td.dataset.c = String(c);
        td.style.textAlign = t.align[c] ?? "";
        if (fits && editing && editing.r === r && editing.c === c) continue;
        this.cell(view, dom, td, row.cells[c]?.text ?? "");
      }
    });
    if (!fits && editing) this.open(view, dom, editing.r, editing.c);
  }

  /** A cell's text, drawn (again only when it changed). */
  private cell(view: EditorView, dom: HTMLElement, td: HTMLTableCellElement, text: string) {
    if (td.dataset.text === text && td.firstElementChild?.classList.contains("cm-cell")) return;
    td.dataset.text = text;
    td.classList.remove("editing");
    const box = document.createElement("div");
    box.className = "cm-cell";
    box.append(cellFragment(text));
    td.replaceChildren(box);
    td.onmousedown = (e) => {
      if ((e.target as HTMLElement).closest("a, textarea, button")) return;
      e.preventDefault();
    };
    td.onclick = (e) => {
      const target = e.target as HTMLElement;
      if (target.closest("textarea, button")) return;
      const a = target.closest("a");
      if (a) {
        e.preventDefault();
        view.state.facet(noteHooks).openUrl(a.getAttribute("href") ?? "");
        return;
      }
      if (view.state.facet(EditorView.editable)) this.open(view, dom, Number(td.dataset.r), Number(td.dataset.c));
    };
  }

  /** Write in cell (r, c). */
  open(view: EditorView, dom: HTMLElement, r: number, c: number, select = false) {
    const t = (tables.get(dom) ?? this).t;
    r = Math.max(-1, Math.min(r, t.rows.length - 1));
    c = Math.max(0, Math.min(c, t.cols - 1));
    const td = dom.querySelector<HTMLTableCellElement>(`[data-r="${r}"][data-c="${c}"]`);
    if (!td) return;
    const row = r < 0 ? t.head : t.rows[r];
    const ta = document.createElement("textarea");
    ta.className = "cm-cell-edit";
    ta.rows = 1;
    ta.dataset.r = String(r);
    ta.dataset.c = String(c);
    ta.setAttribute("data-swipe", "");
    ta.setAttribute("aria-label", r < 0 ? `Column ${c + 1} heading` : `Row ${r + 1}, column ${c + 1}`);
    ta.spellcheck = true;
    ta.value = editText(row.cells[c]?.text ?? "");
    const menu = document.createElement("button");
    menu.type = "button";
    menu.className = "cm-cell-menu";
    menu.setAttribute("aria-label", "Table options");
    menu.innerHTML = '<svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor" aria-hidden="true"><circle cx="6" cy="12" r="1.6"/><circle cx="12" cy="12" r="1.6"/><circle cx="18" cy="12" r="1.6"/></svg>';
    menu.addEventListener("mousedown", (e) => e.preventDefault());
    menu.addEventListener("click", (e) => {
      e.preventDefault();
      this.menu(view, dom, menu, Number(ta.dataset.r), Number(ta.dataset.c));
    });
    td.replaceChildren(ta, menu);
    td.classList.add("editing");
    delete td.dataset.text;
    const fit = () => {
      ta.style.height = "auto";
      ta.style.height = `${ta.scrollHeight}px`;
    };
    fit();
    ta.addEventListener("input", () => {
      fit();
      const now = this.now(view, dom);
      if (now) view.dispatch({ changes: setCell(now, Number(ta.dataset.r), Number(ta.dataset.c), ta.value), userEvent: "input.type" });
    });
    ta.addEventListener("keydown", (e) => this.key(view, dom, ta, e));
    ta.addEventListener("blur", () => {
      setTimeout(() => {
        if (document.activeElement === ta || document.activeElement === menu || !td.isConnected || !td.contains(ta)) return;
        // Drawn again from the text as it is now.
        const w = tables.get(dom) ?? this;
        const r2 = Number(ta.dataset.r);
        const c2 = Number(ta.dataset.c);
        const row = r2 < 0 ? w.t.head : w.t.rows[r2];
        delete td.dataset.text;
        w.cell(view, dom, td, row?.cells[c2]?.text ?? "");
      }, 0);
    });
    ta.focus({ preventScroll: true });
    td.scrollIntoView({ block: "nearest", inline: "nearest" });
    if (select) ta.select();
    else ta.setSelectionRange(ta.value.length, ta.value.length);
  }

  private key(view: EditorView, dom: HTMLElement, ta: HTMLTextAreaElement, e: KeyboardEvent) {
    if (e.isComposing) return;
    const t = (tables.get(dom) ?? this).t;
    const r = Number(ta.dataset.r);
    const c = Number(ta.dataset.c);
    const go = (r2: number, c2: number, select = false) => {
      e.preventDefault();
      ta.blur();
      this.open(view, dom, r2, c2, select);
    };
    const leave = (after: boolean) => {
      e.preventDefault();
      view.focus();
      leaveTable(view, this.now(view, dom), view.posAtDOM(dom), after);
    };
    const atStart = ta.selectionStart === 0 && ta.selectionEnd === 0;
    const atEnd = ta.selectionStart === ta.value.length;
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") return;
    if (e.key === "Tab") {
      // Like Docs: the next cell's text is picked, to type over.
      if (e.shiftKey) {
        if (c > 0) go(r, c - 1, true);
        else if (r > -1) go(r - 1, t.cols - 1, true);
        else e.preventDefault();
      } else if (c < t.cols - 1) go(r, c + 1, true);
      else if (r < t.rows.length - 1) go(r + 1, 0, true);
      else {
        e.preventDefault();
        this.edit(view, dom, (now) => insertRow(now, now.rows.length), (now) => ({ r: now.rows.length, c: 0 }));
      }
    } else if (e.key === "Enter" && !e.shiftKey) {
      if (r < t.rows.length - 1) go(r + 1, c);
      else leave(true);
    } else if (e.key === "Escape") leave(true);
    else if (e.key === "ArrowUp" && atStart) {
      if (r > -1) go(r - 1, c);
      else leave(false);
    } else if (e.key === "ArrowDown" && atEnd) {
      if (r < t.rows.length - 1) go(r + 1, c);
      else leave(true);
    } else if (e.key === "ArrowLeft" && atStart && c > 0) go(r, c - 1);
    else if (e.key === "ArrowRight" && atEnd && c < t.cols - 1) go(r, c + 1);
  }

  private menu(view: EditorView, dom: HTMLElement, anchor: HTMLElement, r: number, c: number) {
    const t = (tables.get(dom) ?? this).t;
    const items: { label: string; icon?: string; danger?: boolean; run: () => void }[] = [];
    const ed = (changes: (t: TableModel) => Change[], focus?: (t: TableModel) => { r: number; c: number }) => () => this.edit(view, dom, changes, focus);
    items.push({ label: "Row above", icon: "arrowUp", run: ed((t) => insertRow(t, Math.max(0, r)), () => ({ r: Math.max(0, r), c })) });
    items.push({ label: "Row below", icon: "arrowDown", run: ed((t) => insertRow(t, r + 1), () => ({ r: r + 1, c })) });
    items.push({ label: "Column left", icon: "arrowLeft", run: ed((t) => insertCol(t, c), () => ({ r, c })) });
    items.push({ label: "Column right", icon: "arrowRight", run: ed((t) => insertCol(t, c + 1), () => ({ r, c: c + 1 })) });
    const al = t.align[c];
    items.push({ label: al === "center" ? "Align right" : al === "right" ? "Align left" : "Align center", icon: "columns", run: ed((t) => setAlign(t, c, al === "center" ? "right" : al === "right" ? null : "center"), () => ({ r, c })) });
    if (r >= 0) items.push({ label: "Delete row", icon: "minus", danger: true, run: ed((t) => deleteRow(t, r), (t) => ({ r: Math.min(r, t.rows.length - 2), c })) });
    if (t.cols > 1) items.push({ label: "Delete column", icon: "minus", danger: true, run: ed((t) => deleteCol(t, c), () => ({ r, c: Math.max(0, c - 1) })) });
    items.push({
      label: "Delete table",
      icon: "trash",
      danger: true,
      run: () => {
        const now = this.now(view, dom);
        if (!now) return;
        const doc = view.state.doc;
        const to = now.to < doc.length ? now.to + 1 : now.to;
        view.dispatch({ changes: { from: now.from, to }, selection: { anchor: now.from }, userEvent: "delete.table" });
        view.focus();
      },
    });
    view.state.facet(noteHooks).tableMenu(anchor, items);
  }
}

const tables = new WeakMap<HTMLElement, TableWidget>();

/**
 * The caret out of a table, onto a line of its own: above it, or below it
 * after a blank line (text right under a table would become one of its rows).
 */
export function leaveTable(view: EditorView, t: TableModel | null, pos: number, after: boolean) {
  const doc = view.state.doc;
  if (!after) {
    view.dispatch({ selection: { anchor: Math.max(0, pos - 1) }, scrollIntoView: true });
    return;
  }
  const end = t ? t.to : pos;
  const next = end < doc.length ? doc.lineAt(end + 1) : null;
  if (next && !next.text.trim() && next.to < doc.length) {
    view.dispatch({ selection: { anchor: next.to + 1 }, scrollIntoView: true });
    return;
  }
  // Make the blank line (and the line to write on).
  const insert = next && !next.text.trim() ? "\n" : next ? "\n\n" : "\n\n";
  const at = next && !next.text.trim() ? next.to : end;
  view.dispatch({ changes: { from: at, insert }, selection: { anchor: at + insert.length }, scrollIntoView: true, userEvent: "input" });
}

function sameCells(a: TableModel, b: TableModel) {
  const rows = (t: TableModel) => [t.head, t.delim, ...t.rows];
  const ra = rows(a);
  const rb = rows(b);
  return ra.length === rb.length && ra.every((r, i) => r.cells.length === rb[i].cells.length && r.cells.every((c, k) => c.text === rb[i].cells[k].text));
}

function button(cls: string, label: string, run: () => void) {
  const b = document.createElement("button");
  b.type = "button";
  b.className = cls;
  b.setAttribute("aria-label", label);
  b.title = label;
  b.innerHTML = '<svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true"><path d="M12 5.5v13M5.5 12h13"/></svg>';
  b.addEventListener("mousedown", (e) => e.preventDefault());
  b.addEventListener("click", (e) => {
    e.preventDefault();
    run();
  });
  return b;
}

/** A cell's markdown, drawn (inline only). */
function cellFragment(text: string): DocumentFragment | Text {
  if (!text) return document.createTextNode("");
  if (!/[*_`~[<\\&]/.test(text)) return document.createTextNode(text);
  const f = renderMarkdownFragment(text);
  const p = f.firstElementChild;
  if (f.childElementCount === 1 && p?.tagName === "P") {
    const out = document.createDocumentFragment();
    out.append(...Array.from(p.childNodes));
    return out;
  }
  return f;
}
