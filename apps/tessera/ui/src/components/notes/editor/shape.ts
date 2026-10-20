// What one line of a note is, from the markdown parse: where its text starts
// (the markers before it are drawn, not shown), whether it starts a list
// item or a heading, how deep in lists and quotes it sits, and lines that
// are shown as they are (code, HTML, front matter).
import { syntaxTree } from "@codemirror/language";
import type { EditorState, Line, Text } from "@codemirror/state";
import type { SyntaxNode, Tree } from "@lezer/common";

export type ItemKind = "bullet" | "ordered" | "task";

export interface LineShape {
  /** Where the line's text starts: before it, the page draws (a bullet, a quote's bar) instead. */
  start: number;
  /** "text" lines get all of it; the rest are shown as written. */
  kind: "text" | "code" | "hr" | "table" | "html" | "frontmatter";
  /** ATX heading level, or a setext heading's text line's. */
  heading: number;
  /** A setext heading's underline (`===`, `---`). */
  underline: boolean;
  /** Block quotes this line is in. */
  quote: number;
  /** Where the outermost quote starts (a callout's first line), or -1. */
  quoteFrom: number;
  /** The list item this line begins. */
  item: null | { kind: ItemKind; mark: string; checked: boolean; box: number };
  /** List items around the line. */
  depth: number;
  /** Indented text under a list item (not its first line). */
  cont: boolean;
  /** Fenced code: its opening or closing line. */
  fence: "open" | "close" | null;
  /** Fenced code's language (on its opening line). */
  lang: string;
  /** The fenced code / heading / quote node, for its range. */
  block: SyntaxNode | null;
}

const frontMatter = new WeakMap<Text, number>();

/** Where front matter (`---` … `---` at the very top) ends, or -1. */
export function frontMatterEnd(doc: Text): number {
  const hit = frontMatter.get(doc);
  if (hit !== undefined) return hit;
  let end = -1;
  if (doc.lines > 2 && /^---\s*$/.test(doc.line(1).text)) {
    for (let n = 2; n <= Math.min(doc.lines, 200); n++) {
      const t = doc.line(n).text;
      if (/^(---|\.\.\.)\s*$/.test(t)) {
        end = doc.line(n).to;
        break;
      }
      if (!/^([\w-]+\s*:|\s+|-\s|#)/.test(t) && t.trim()) break;
    }
  }
  frontMatter.set(doc, end);
  return end;
}

const ITEM_TASK = /^( {1,4})\[([ xX])\](?= |$)/;

export function lineShape(state: EditorState, line: Line, tree: Tree = syntaxTree(state)): LineShape {
  const s: LineShape = {
    start: line.from,
    kind: "text",
    heading: 0,
    underline: false,
    quote: 0,
    quoteFrom: -1,
    item: null,
    depth: 0,
    cont: false,
    fence: null,
    lang: "",
    block: null,
  };
  const fm = frontMatterEnd(state.doc);
  if (fm >= 0 && line.from <= fm) {
    s.kind = "frontmatter";
    return s;
  }
  const text = line.text;
  const first = line.from + (text.length - text.trimStart().length);
  // What the line is in.
  for (let n: SyntaxNode | null = tree.resolveInner(Math.min(first, line.to), 1); n; n = n.parent) {
    const name = n.name;
    if (name === "ListItem") s.depth++;
    else if (name === "Blockquote") s.quoteFrom = n.from;
    else if (name === "FencedCode") {
      s.kind = "code";
      s.block = n;
      const open = state.doc.lineAt(n.from);
      if (open.number === line.number) {
        s.fence = "open";
        const info = n.getChild("CodeInfo");
        s.lang = info ? state.doc.sliceString(info.from, info.to).toLowerCase() : "";
      } else if (n.lastChild?.name === "CodeMark" && state.doc.lineAt(n.lastChild.from).number === line.number) s.fence = "close";
    } else if (name === "CodeBlock" || name === "HTMLBlock" || name === "CommentBlock" || name === "ProcessingInstructionBlock" || name === "LinkReference") {
      s.kind = name === "CodeBlock" ? "code" : "html";
    } else if (name === "Table") {
      s.kind = "table";
      s.block = n;
    } else if (name === "HorizontalRule") s.kind = "hr";
    else if (/^SetextHeading\d$/.test(name)) {
      s.block = n;
      if (state.doc.lineAt(n.to).number === line.number && state.doc.lineAt(n.from).number !== line.number) s.underline = true;
      else s.heading = Number(name.slice(-1));
    } else if (/^ATXHeading\d$/.test(name)) s.block = n;
  }
  if (s.kind !== "text" && s.kind !== "hr") return s;
  // The markers on the line.
  let markEnd = line.from;
  tree.iterate({
    from: line.from,
    to: line.to,
    enter: (n) => {
      if (n.from < line.from || n.from > line.to) return;
      switch (n.name) {
        case "QuoteMark":
          s.quote++;
          markEnd = Math.max(markEnd, n.to + (state.doc.sliceString(n.to, n.to + 1) === " " ? 1 : 0));
          return;
        case "ListMark": {
          const list = n.node.parent?.parent?.name;
          const after = /^ {0,4}/.exec(state.doc.sliceString(n.to, Math.min(line.to, n.to + 5)))![0].length;
          s.item = { kind: list === "OrderedList" ? "ordered" : "bullet", mark: state.doc.sliceString(n.from, n.to), checked: false, box: -1 };
          markEnd = Math.max(markEnd, n.to + Math.max(after > 4 ? 1 : after, 0));
          // A task with nothing written yet isn't one for the parser: it is here.
          const t = ITEM_TASK.exec(state.doc.sliceString(n.to, line.to));
          if (t && s.item.kind === "bullet") {
            s.item.kind = "task";
            s.item.box = n.to + t[1].length;
            s.item.checked = t[2] !== " ";
            markEnd = Math.min(line.to, s.item.box + 4);
          }
          return;
        }
        case "TaskMarker":
          if (s.item) {
            s.item.kind = "task";
            s.item.box = n.from;
            s.item.checked = /x/i.test(state.doc.sliceString(n.from + 1, n.from + 2));
            markEnd = Math.min(line.to, n.to + 1);
          }
          return;
        case "HeaderMark": {
          const parent = n.node.parent?.name ?? "";
          if (!/^ATXHeading\d$/.test(parent)) return;
          if (n.from === first || n.from === markEnd) {
            s.heading = Number(parent.slice(-1));
            const sp = state.doc.sliceString(n.to, n.to + 1) === " " ? 1 : 0;
            markEnd = Math.max(markEnd, n.to + sp);
          }
          return;
        }
        case "Paragraph":
        case "Table":
          return false;
      }
    },
  });
  if (s.item) s.start = markEnd;
  else if (s.heading && !s.underline) s.start = markEnd;
  else if (s.quote) s.start = Math.max(markEnd, s.depth ? first : markEnd);
  else if (s.depth && first > line.from) {
    // Text under a list item: its indentation becomes the page's.
    s.cont = true;
    s.start = first;
  }
  if (s.kind === "hr") s.start = line.from;
  return s;
}

/** A list item's marker text for a new item after this one ("-", "2.", "- [ ]"). */
export function nextMark(item: NonNullable<LineShape["item"]>): string {
  if (item.kind === "ordered") {
    const m = /^(\d+)([.)])$/.exec(item.mark);
    return m ? `${Number(m[1]) + 1}${m[2]}` : item.mark;
  }
  return item.kind === "task" ? `${item.mark} [ ]` : item.mark;
}
