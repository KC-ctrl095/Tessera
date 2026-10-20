// Writing helpers: the keys (Enter carries a list on, Tab nests it, ⌘B and
// friends), the format bar's and the insert menu's commands, and the little
// automatic things while typing (a `[ ] ` becomes a task, `[[` closes
// itself, a selection typed over with `*` is wrapped).
import { deleteMarkupBackward, insertNewlineContinueMarkupCommand } from "@codemirror/lang-markdown";
import { syntaxTree } from "@codemirror/language";
import { Annotation, EditorSelection, EditorState, type ChangeSpec, type Extension, Prec, Transaction } from "@codemirror/state";
import { isolateHistory } from "@codemirror/commands";
import { EditorView, type KeyBinding, keymap } from "@codemirror/view";
import { startCompletion } from "@codemirror/autocomplete";
import { lineShape } from "./shape";
import { toggleTaskAt } from "./widgets";

type Cmd = (view: EditorView) => boolean;

// ── inline styles ────────────────────────────────────────────────────────

const NODE_OF: Record<string, string> = { "**": "StrongEmphasis", "*": "Emphasis", _: "Emphasis", "~~": "Strikethrough", "`": "InlineCode", "==": "Highlight" };

/** The styled span of `node` around [from, to], if any. */
function around(state: EditorState, from: number, to: number, node: string) {
  for (let n = syntaxTree(state).resolveInner(from, 1) as import("@lezer/common").SyntaxNode | null; n; n = n.parent) {
    if (n.name === node && n.from <= from && n.to >= to) return n;
  }
  for (let n = syntaxTree(state).resolveInner(from, -1) as import("@lezer/common").SyntaxNode | null; n; n = n.parent) {
    if (n.name === node && n.from <= from && n.to >= to) return n;
  }
  return null;
}

/** The word around an empty selection (letters, digits, - and '). */
function wordAt(state: EditorState, pos: number): { from: number; to: number } | null {
  const line = state.doc.lineAt(pos);
  const t = line.text;
  let a = pos - line.from;
  let b = a;
  const w = /[\p{L}\p{N}_'’-]/u;
  while (a > 0 && w.test(t[a - 1])) a--;
  while (b < t.length && w.test(t[b])) b++;
  return b > a ? { from: line.from + a, to: line.from + b } : null;
}

/** Bold, italic, strike, code or highlight on (or off) for the selection, or the word at the caret. */
export function toggleInline(mark: string): Cmd {
  return (view) => {
    const { state } = view;
    if (state.readOnly) return false;
    const name = NODE_OF[mark];
    const tr = state.changeByRange((r) => {
      const span = around(state, r.from, r.to, name);
      if (span) {
        // Already styled: take the marks away.
        const marks: ChangeSpec[] = [];
        for (let c = span.firstChild; c; c = c.nextSibling) if (/Mark$/.test(c.name)) marks.push({ from: c.from, to: c.to });
        const cs = state.changes(marks);
        return { changes: cs, range: r.map(cs) };
      }
      let { from, to } = r;
      if (r.empty) {
        const w = wordAt(state, r.head);
        if (!w) return { changes: { from, insert: mark + mark }, range: EditorSelection.cursor(from + mark.length) };
        ({ from, to } = w);
      }
      // Spaces at the ends stay outside (`** bold**` isn't bold).
      const text = state.sliceDoc(from, to);
      from += text.length - text.trimStart().length;
      to -= text.length - text.trimEnd().length;
      return {
        changes: [
          { from, insert: mark },
          { from: to, insert: mark },
        ],
        range: r.empty ? EditorSelection.cursor(r.head + mark.length) : EditorSelection.range(from + mark.length, to + mark.length),
      };
    });
    view.dispatch(state.update(tr, { scrollIntoView: true, userEvent: "input.format", annotations: isolateHistory.of("full") }));
    return true;
  };
}

/** A link: the selection (or word) becomes `[text](url)` with the address picked to type over; in a link, its address is picked. */
export const toggleLink: Cmd = (view) => {
  const { state } = view;
  if (state.readOnly) return false;
  const r = state.selection.main;
  const link = around(state, r.from, r.to, "Link");
  if (link) {
    const url = link.getChild("URL");
    if (url) view.dispatch({ selection: EditorSelection.range(url.from, url.to), scrollIntoView: true });
    return true;
  }
  let { from, to } = r;
  if (r.empty) {
    const w = wordAt(state, r.head);
    if (w) ({ from, to } = w);
  }
  const text = state.sliceDoc(from, to);
  const isUrl = /^https?:\/\/\S+$/.test(text);
  const label = isUrl ? "link" : text || "link";
  const url = isUrl ? text : "https://";
  const insert = `[${label}](${url})`;
  const urlFrom = from + label.length + 3;
  view.dispatch({
    changes: { from, to, insert },
    selection: isUrl || !text ? EditorSelection.range(from + 1, from + 1 + label.length) : EditorSelection.range(urlFrom, urlFrom + url.length),
    scrollIntoView: true,
    userEvent: "input.format",
    annotations: isolateHistory.of("full"),
  });
  return true;
};

// ── block kinds ──────────────────────────────────────────────────────────

export type LineKind = "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "bullet" | "ordered" | "task" | "quote";

/** What the line at `pos` is now (for the format bar's state). */
export function lineKind(state: EditorState, pos: number): LineKind {
  const s = lineShape(state, state.doc.lineAt(pos));
  if (s.item) return s.item.kind;
  if (s.heading) return `h${Math.min(6, s.heading)}` as LineKind;
  if (s.quote) return "quote";
  return "p";
}

/** Make the selected lines `kind` (or plain text again, when they are already). */
export function setLineKind(kind: LineKind, toggle = true): Cmd {
  return (view) => {
    const { state } = view;
    if (state.readOnly) return false;
    const lines = new Set<number>();
    for (const r of state.selection.ranges) {
      for (let n = state.doc.lineAt(r.from).number; n <= state.doc.lineAt(r.to).number; n++) lines.add(n);
    }
    const all = [...lines].map((n) => state.doc.line(n));
    const same = toggle && all.every((l) => lineKind(state, l.from) === kind);
    const target: LineKind = same ? "p" : kind;
    const changes: ChangeSpec[] = [];
    let num = 1;
    for (const l of all) {
      const s = lineShape(state, l);
      if (s.kind !== "text") continue;
      // What's before the text on this line: quote marks stay unless it becomes / stops being a quote.
      const lead = /^\s*/.exec(l.text)![0];
      let prefix = "";
      switch (target) {
        case "bullet":
          prefix = "- ";
          break;
        case "ordered":
          prefix = `${num++}. `;
          break;
        case "task":
          prefix = "- [ ] ";
          break;
        case "quote":
          prefix = "> ";
          break;
        case "p":
          prefix = "";
          break;
        default:
          prefix = `${"#".repeat(Number(target.slice(1)))} `;
      }
      const keepIndent = (target === "bullet" || target === "ordered" || target === "task") && s.depth > 0 ? lead : "";
      changes.push({ from: l.from, to: s.start, insert: keepIndent + prefix });
    }
    // The caret stays with the text (on an empty line: after the new marker, ready to type).
    const cs = state.changes(changes);
    view.dispatch({ changes: cs, selection: state.selection.map(cs, 1), userEvent: "input.format", scrollIntoView: true, annotations: isolateHistory.of("full") });
    return true;
  };
}

/** Headings cycle: text → H1 → H2 → H3 → text (the phone's H button). */
export const cycleHeading: Cmd = (view) => {
  const k = lineKind(view.state, view.state.selection.main.head);
  const next: LineKind = k === "h1" ? "h2" : k === "h2" ? "h3" : k === "h3" ? "p" : "h1";
  return setLineKind(next, false)(view);
};

// ── Enter, Backspace, Tab ────────────────────────────────────────────────

/** Enter in a list or quote carries it on; on an empty item it steps out a level (never spreads the list out). */
const continueMarkup = insertNewlineContinueMarkupCommand({ nonTightLists: false });

/** Enter at the very start of a heading, quote or list item's text: a new line above, the text stays as it is. */
const enterAtTextStart: Cmd = (view) => {
  const { state } = view;
  const r = state.selection.main;
  if (!r.empty || state.selection.ranges.length > 1) return false;
  const line = state.doc.lineAt(r.head);
  const s = lineShape(state, line);
  if (s.kind !== "text" || s.start <= line.from || r.head !== s.start || r.head === line.to) return false;
  if (!s.heading || s.item) return false;
  view.dispatch({ changes: { from: line.from, insert: "\n" }, selection: EditorSelection.cursor(r.head + 1), scrollIntoView: true, userEvent: "input" });
  return true;
};

/** Enter on an empty quote line: out of the quote (one level), like an empty list item. */
const enterEmptyQuote: Cmd = (view) => {
  const { state } = view;
  const r = state.selection.main;
  if (!r.empty) return false;
  const line = state.doc.lineAt(r.head);
  const m = /^(\s*(?:>\s?)*)>\s?$/.exec(line.text);
  if (!m || r.head !== line.to) return false;
  view.dispatch({ changes: { from: line.from, to: line.to, insert: m[1].trimEnd() ? m[1] : "" }, scrollIntoView: true, userEvent: "delete" });
  return true;
};

/** Enter on `---`: a divider (with a blank line before it, so it isn't read as a heading's underline). */
const enterRule: Cmd = (view) => {
  const { state } = view;
  const r = state.selection.main;
  if (!r.empty) return false;
  const line = state.doc.lineAt(r.head);
  if (r.head !== line.to || !/^(-{3,}|\*{3,}|_{3,})\s*$/.test(line.text)) return false;
  const prev = line.number > 1 ? state.doc.line(line.number - 1) : null;
  const before = prev && prev.text.trim() ? "\n" : "";
  view.dispatch({ changes: [{ from: line.from, insert: before }, { from: line.to, insert: "\n" }], selection: EditorSelection.cursor(line.to + before.length + 1), scrollIntoView: true, userEvent: "input" });
  return true;
};

/** Enter after an opening ``` (with or without a language): the block is closed and the caret goes in. */
const enterFence: Cmd = (view) => {
  const { state } = view;
  const r = state.selection.main;
  if (!r.empty) return false;
  const line = state.doc.lineAt(r.head);
  const m = /^(\s*)(`{3,}|~{3,})([\w+#.-]*)\s*$/.exec(line.text);
  if (!m || r.head !== line.to) return false;
  // Already closed below (this is the closing one, or one that has its pair): normal Enter.
  const tree = syntaxTree(state);
  const node = tree.resolveInner(line.from + m[1].length, 1);
  for (let n: import("@lezer/common").SyntaxNode | null = node; n; n = n.parent) {
    if (n.name === "FencedCode") {
      if (state.doc.lineAt(n.from).number !== line.number) return false;
      const lastMark = n.lastChild;
      if (lastMark && lastMark.name === "CodeMark" && state.doc.lineAt(lastMark.from).number !== line.number) return false;
    }
  }
  view.dispatch({ changes: { from: line.to, insert: `\n${m[1]}\n${m[1]}${m[2]}` }, selection: EditorSelection.cursor(line.to + 1 + m[1].length), scrollIntoView: true, userEvent: "input" });
  return true;
};

/** Enter: the first of these that applies (else a plain new line). */
const enter: Cmd = (v) => enterAtTextStart(v) || enterEmptyQuote(v) || enterRule(v) || enterFence(v) || continueMarkup(v);

/**
 * A phone keyboard pressed Enter while composing a word: the browser hands
 * over the word and the line break as one piece of typing (sometimes as two
 * line breaks, an artefact of how it splits the line), not as an Enter key.
 * Taken apart again here: the word goes in, then Enter does what it does
 * (carry a list on, end it…), or there is just the one line break.
 */
export const enterAfterWord = EditorState.transactionFilter.of((tr) => {
  if (!tr.docChanged || !tr.isUserEvent("input.type") || tr.startState.selection.ranges.length > 1) return tr;
  let change: { from: number; to: number; text: string } | null = null;
  let count = 0;
  tr.changes.iterChanges((fa, ta, _fb, _tb, ins) => {
    count++;
    change = { from: fa, to: ta, text: ins.toString() };
  });
  const c = change as { from: number; to: number; text: string } | null;
  const m = c && /^([^\n]*)(\n{1,2})$/.exec(c.text);
  if (count !== 1 || !c || !m || (!m[1] && m[2].length === 1)) return tr;
  if (c.to !== tr.startState.selection.main.head) return tr;
  const word = m[1];
  const userEvent = tr.annotation(Transaction.userEvent);
  const mid = tr.startState.update({ changes: { from: c.from, to: c.to, insert: word }, selection: { anchor: c.from + word.length } }).state;
  let next: Transaction | null = null;
  const fake = { state: mid, dispatch: (t: Transaction | import("@codemirror/state").TransactionSpec) => (next = t instanceof Transaction ? t : mid.update(t)) } as unknown as EditorView;
  if (enter(fake) && next) {
    const after = next as Transaction;
    return [
      { changes: { from: c.from, to: c.to, insert: word }, userEvent },
      { changes: after.changes, selection: after.selection, sequential: true, scrollIntoView: true, userEvent, annotations: enterFromWord.of(true) },
    ];
  }
  if (m[2].length === 1) return tr;
  // One Enter is one line break.
  return { changes: { from: c.from, to: c.to, insert: `${word}\n` }, selection: { anchor: c.from + word.length + 1 }, scrollIntoView: true, userEvent, annotations: enterFromWord.of(true) };
});

/** Marks the Enter taken out of "word\n" (the editor drops the key it may still be holding for it: see index.ts). */
export const enterFromWord = Annotation.define<boolean>();

/** Backspace at the start of a heading's text: plain text again (one step, like Docs). */
const backspaceHeading: Cmd = (view) => {
  const { state } = view;
  const r = state.selection.main;
  if (!r.empty) return false;
  const line = state.doc.lineAt(r.head);
  const s = lineShape(state, line);
  if (!s.heading || s.item || s.quote || r.head !== s.start || s.start === line.from) return false;
  view.dispatch({ changes: { from: line.from, to: s.start }, userEvent: "delete" });
  return true;
};

/** Backspace at the start of the line under a divider, a table or a picture alone on its line: that goes (not the line joined into it). */
const backspaceAfterBlock: Cmd = (view) => {
  const { state } = view;
  const r = state.selection.main;
  if (!r.empty) return false;
  const line = state.doc.lineAt(r.head);
  if (r.head !== line.from || line.number === 1) return false;
  const prev = state.doc.line(line.number - 1);
  const ps = lineShape(state, prev);
  if (ps.kind === "table" && ps.block?.parent?.name === "Document") {
    // Joining would break the table: only an empty line goes.
    if (!line.text.trim()) view.dispatch({ changes: { from: prev.to, to: line.to }, userEvent: "delete" });
    return true;
  }
  if (ps.kind === "hr") {
    view.dispatch({ changes: { from: prev.from, to: line.from }, userEvent: "delete" });
    return true;
  }
  return false;
};

/** Tab / Shift+Tab on list items: nest under the item above, or come back out. */
function indentItems(out: boolean): Cmd {
  return (view) => {
    const { state } = view;
    if (state.readOnly) return false;
    const lines = new Set<number>();
    for (const r of state.selection.ranges) for (let n = state.doc.lineAt(r.from).number; n <= state.doc.lineAt(r.to).number; n++) lines.add(n);
    const shapes = [...lines].map((n) => [state.doc.line(n), lineShape(state, state.doc.line(n))] as const);
    if (!shapes.some(([, s]) => s.item || s.cont)) return false;
    const changes: ChangeSpec[] = [];
    for (const [l, s] of shapes) {
      if (!s.item && !s.cont) continue;
      const lead = /^ */.exec(l.text)![0].length;
      if (out) {
        if (!lead) continue;
        // Back to the parent's column.
        const step = parentIndent(state, l.number, lead);
        changes.push({ from: l.from, to: l.from + (lead - step) });
      } else {
        // Under the item above: as far in as its text starts.
        const want = childIndent(state, l.number, lead);
        if (want > lead) changes.push({ from: l.from, insert: " ".repeat(want - lead) });
      }
    }
    if (!changes.length) return true;
    view.dispatch({ changes, userEvent: out ? "delete.dedent" : "input.indent", scrollIntoView: true });
    return true;
  };
}

/** Where the text of the nearest list item above (less or equally indented) starts. */
function childIndent(state: EditorState, lineNo: number, lead: number): number {
  for (let n = lineNo - 1; n >= 1; n--) {
    const t = state.doc.line(n).text;
    if (!t.trim()) continue;
    const m = /^( *)([-*+]|\d+[.)])( +)/.exec(t);
    if (!m) {
      if (/^\S/.test(t)) break;
      continue;
    }
    if (m[1].length <= lead) return m[1].length + m[2].length + Math.min(m[3].length, 4);
  }
  return lead + 2;
}

/** The indentation of the list item this one is nested in. */
function parentIndent(state: EditorState, lineNo: number, lead: number): number {
  for (let n = lineNo - 1; n >= 1; n--) {
    const t = state.doc.line(n).text;
    if (!t.trim()) continue;
    const m = /^( *)([-*+]|\d+[.)])/.exec(t);
    if (m && m[1].length < lead) return m[1].length;
    if (/^\S/.test(t) && !m) break;
  }
  return 0;
}

const toggleTask: Cmd = (view) => {
  if (view.state.readOnly) return false;
  const pos = view.state.selection.main.head;
  const s = lineShape(view.state, view.state.doc.lineAt(pos));
  if (s.item?.kind === "task") return toggleTaskAt(view, pos);
  return setLineKind("task")(view);
};

const save = (onSave: () => void): Cmd => () => {
  onSave();
  return true;
};

export function noteKeymap(onSave: () => void): Extension {
  const bindings: KeyBinding[] = [
    { key: "Enter", run: enter },
    { key: "Backspace", run: (v) => backspaceHeading(v) || backspaceAfterBlock(v) || deleteMarkupBackward(v) },
    { key: "Tab", run: indentItems(false) },
    { key: "Shift-Tab", run: indentItems(true) },
    { key: "Mod-b", run: toggleInline("**") },
    { key: "Mod-i", run: toggleInline("*") },
    { key: "Mod-e", run: toggleInline("`") },
    { key: "Mod-Shift-x", run: toggleInline("~~") },
    { key: "Mod-Shift-h", run: toggleInline("==") },
    { key: "Mod-k", run: toggleLink },
    { key: "Mod-Alt-0", run: setLineKind("p", false) },
    ...[1, 2, 3, 4, 5, 6].map((n) => ({ key: `Mod-Alt-${n}`, run: setLineKind(`h${n}` as LineKind, false) })),
    { key: "Mod-Shift-7", run: setLineKind("ordered") },
    { key: "Mod-Shift-8", run: setLineKind("bullet") },
    { key: "Mod-Shift-9", run: setLineKind("task") },
    { key: "Mod-Enter", run: toggleTask },
    { key: "Mod-s", run: save(onSave), preventDefault: true },
  ];
  return Prec.high(keymap.of(bindings));
}

// ── while typing ─────────────────────────────────────────────────────────

const WRAP: Record<string, string> = { "*": "*", _: "_", "`": "`", "~": "~", "=": "=", "(": ")", '"': '"' };

/** Typing over a selection with a style mark wraps it; `[[` closes itself and suggests notes. */
export const typingRules = EditorView.inputHandler.of((view, from, to, text) => {
  if (view.state.readOnly || view.composing) return false;
  const { state } = view;
  // A selection typed over with * _ ` ~ = ( ": wrapped.
  if (from !== to && WRAP[text] && state.selection.ranges.length === 1) {
    const close = WRAP[text];
    view.dispatch({
      changes: [
        { from, insert: text },
        { from: to, insert: close },
      ],
      selection: EditorSelection.range(from + 1, to + 1),
      userEvent: "input.type",
    });
    return true;
  }
  if (from !== to) return false;
  if (text === "[" && state.sliceDoc(from - 1, from) === "[" && state.sliceDoc(from, from + 1) !== "]") {
    view.dispatch({ changes: { from, insert: "[]]" }, selection: EditorSelection.cursor(from + 1), userEvent: "input.type" });
    startCompletion(view);
    return true;
  }
  return false;
});

/**
 * After a space: `[] ` or `[ ] ` at the start of a line becomes a task
 * (`- [ ] `), and `* ` a bullet like `- `. Works on the resulting text,
 * so a phone keyboard's way of typing is fine.
 */
export const autoBlocks = EditorState.transactionFilter.of((tr) => {
  if (!tr.docChanged || !tr.isUserEvent("input.type") || tr.isUserEvent("input.type.compose")) return tr;
  let inserted = "";
  let at = -1;
  tr.changes.iterChanges((_fa, _ta, _fb, tb, t) => {
    inserted = t.toString();
    at = tb;
  });
  if (!inserted.endsWith(" ")) return tr;
  const doc = tr.newDoc;
  const line = doc.lineAt(at);
  const before = doc.sliceString(line.from, at);
  const m = /^(\s*)(?:([-*+]|\d+[.)]) )?\[([ xX]?)\] $/.exec(before);
  if (!m || (m[2] && m[3] !== "")) return tr;
  const insert = `${m[1]}${m[2] && /^\d/.test(m[2]) ? m[2] : "-"} [${m[3] && m[3] !== " " ? "x" : " "}] `;
  return [tr, { changes: { from: line.from, to: at, insert }, selection: EditorSelection.cursor(line.from + insert.length), sequential: true }];
});

/**
 * Text typed on the empty line right under a table would become one of its
 * rows: a blank line goes in first. (Not while a phone keyboard is still
 * composing a word: changing that under it would lose the word.)
 */
export const keepTablesApart = EditorState.transactionFilter.of((tr) => {
  if (!tr.docChanged || !tr.isUserEvent("input") || tr.isUserEvent("input.type.compose")) return tr;
  let at = -1;
  let text = "";
  let one = true;
  tr.changes.iterChanges((fa, ta, _fb, _tb, ins) => {
    if (at >= 0 || fa !== ta) one = false;
    at = fa;
    text = ins.toString();
  });
  if (!one || at < 0 || !text.trim() || text.startsWith("\n")) return tr;
  const state = tr.startState;
  const line = state.doc.lineAt(at);
  if (line.text.trim() || line.number === 1) return tr;
  const prev = lineShape(state, state.doc.line(line.number - 1));
  if (prev.kind !== "table" || prev.block?.parent?.name !== "Document") return tr;
  return { changes: { from: at, insert: `\n${text}` }, selection: EditorSelection.cursor(at + 1 + text.length), scrollIntoView: true, userEvent: tr.annotation(Transaction.userEvent) };
});

/** Commands for the format bar and menus, by name. */
export const commands = {
  bold: toggleInline("**"),
  italic: toggleInline("*"),
  strike: toggleInline("~~"),
  code: toggleInline("`"),
  highlight: toggleInline("=="),
  link: toggleLink,
  heading: cycleHeading,
  bullet: setLineKind("bullet"),
  ordered: setLineKind("ordered"),
  task: setLineKind("task"),
  quote: setLineKind("quote"),
  indent: indentItems(false),
  outdent: indentItems(true),
};
