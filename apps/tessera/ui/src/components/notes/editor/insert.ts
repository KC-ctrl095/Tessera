// The insert menu: what `/` offers at the start of a line (or after a
// space), and the phone's ＋ button. Each puts its block where the caret is.
import { EditorSelection } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";
import { noteHooks } from "./hooks";
import { commands, setLineKind } from "./keys";
import { NEW_TABLE, focusCellSoon } from "./table";

export interface InsertItem {
  id: string;
  label: string;
  /** Words it is found by, besides its label. */
  also: string;
  icon: string;
  run: (view: EditorView) => void;
}

/**
 * Put a block of lines where the caret's line is: on it when it's empty,
 * else after it. Blank lines keep it apart from text around it (a table
 * or divider right under text would be read as something else).
 * `caret`: where in `text` the caret goes.
 */
export function insertBlock(view: EditorView, text: string, caret = text.length) {
  const { state } = view;
  const line = state.doc.lineAt(state.selection.main.head);
  const empty = !line.text.trim();
  const at = empty ? line.from : line.to;
  const prevText = empty ? (line.number > 1 ? state.doc.line(line.number - 1).text : "") : line.text;
  const before = empty ? (prevText.trim() ? "\n" : "") : "\n\n";
  const nextLine = line.number < state.doc.lines ? state.doc.line(line.number + 1) : null;
  // The line break after the caret's line stays; this adds what's missing for a blank line after.
  const after = !nextLine || nextLine.text.trim() ? "\n" : "";
  const insert = before + text + after;
  view.dispatch({
    changes: { from: at, to: line.to, insert },
    selection: EditorSelection.cursor(at + before.length + caret),
    scrollIntoView: true,
    userEvent: "input.insert",
  });
  view.focus();
}

const lineKind = (k: Parameters<typeof setLineKind>[0]) => (view: EditorView) => void setLineKind(k, false)(view);

export const INSERTS: InsertItem[] = [
  { id: "h1", label: "Heading 1", also: "title h1 big", icon: "H1", run: lineKind("h1") },
  { id: "h2", label: "Heading 2", also: "subtitle h2", icon: "H2", run: lineKind("h2") },
  { id: "h3", label: "Heading 3", also: "h3 small", icon: "H3", run: lineKind("h3") },
  { id: "bullet", label: "Bulleted list", also: "list bullets ul", icon: "•", run: lineKind("bullet") },
  { id: "ordered", label: "Numbered list", also: "list numbers ol", icon: "1.", run: lineKind("ordered") },
  { id: "task", label: "To-do list", also: "task checkbox todo check", icon: "☐", run: lineKind("task") },
  {
    id: "table",
    label: "Table",
    also: "grid columns rows",
    icon: "▦",
    run: (view) => {
      focusCellSoon(view, -1, 0, true);
      insertBlock(view, NEW_TABLE);
    },
  },
  { id: "image", label: "Image", also: "picture photo camera upload", icon: "🖼", run: (view) => view.state.facet(noteHooks).pickPicture() },
  { id: "code", label: "Code block", also: "code snippet pre", icon: "</>", run: (view) => insertBlock(view, "```\n\n```", 4) },
  { id: "callout", label: "Callout", also: "note tip warning box admonition", icon: "ⓘ", run: (view) => insertBlock(view, "> [!note] ", 10) },
  { id: "quote", label: "Quote", also: "blockquote citation", icon: "❝", run: (view) => void commands.quote(view) },
  { id: "divider", label: "Divider", also: "line rule separator hr", icon: "—", run: (view) => insertBlock(view, "---", 4) },
];
