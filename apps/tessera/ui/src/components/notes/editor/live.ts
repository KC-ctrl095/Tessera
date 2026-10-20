// The live page: markdown drawn as what it means while it stays markdown
// underneath. The markers of a block (a heading's #, a bullet, a task box, a
// quote's >, a callout's [!type]) are always drawn, never shown; the marks
// of inline styles (**, *, `, ~~, ==, a link's brackets and address, a
// wikilink's [[ ]]) show only around the one span the caret is in, so you
// can see where it starts and stops. Pictures, dividers and code block heads
// are drawn. Only what's on screen is worked out.
//
// The caret never sits inside drawn markers: a click or arrow key that would
// put it there puts it at the text instead (keys.ts deals with Enter and
// Backspace there).
import { syntaxTree } from "@codemirror/language";
import { EditorSelection, EditorState, RangeSet, type Extension, type Range } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate } from "@codemirror/view";
import type { SyntaxNode, SyntaxNodeRef } from "@lezer/common";
import { codeMarks } from "./code";
import { noteHooks, redraw } from "./hooks";
import { previewField } from "./blocks";
import { lineShape, type LineShape } from "./shape";
import { CalloutWidget, CodeHeadWidget, MarkWidget, PictureWidget, RuleWidget, calloutKind, parseAlt } from "./widgets";

const hidden = Decoration.replace({});
const rule = new RuleWidget();
const STYLE: Record<string, [string, string]> = {
  Emphasis: ["cm-em", "EmphasisMark"],
  StrongEmphasis: ["cm-strong", "EmphasisMark"],
  Strikethrough: ["cm-strike", "StrikethroughMark"],
  Highlight: ["cm-hl", "HighlightMark"],
  InlineCode: ["cm-icode", "CodeMark"],
};
const CALLOUT = /^\s*(?:>\s?)+\[!([\w-]+)\]([+-]?)(?: |$)(.*)$/;

interface Built {
  deco: DecorationSet;
  atomic: DecorationSet;
}

function build(view: EditorView): Built {
  const { state } = view;
  const { doc } = state;
  const tree = syntaxTree(state);
  const sel = state.selection;
  // Being written in: the text has the focus (the window may not, while Android shows its
  // selection menu).
  const focused = view.root.activeElement === view.contentDOM;
  const hooks = state.facet(noteHooks);
  const previews = state.field(previewField, false) ?? [];
  /**
   * A span's syntax shows while you write in it: the caret is inside [from, to] (not at its
   * ends), or an end of a selection is inside one of its `hidden` parts (a link's address
   * picked to type over). Otherwise, and whenever the note isn't being written in, it is drawn.
   */
  const caretIn = (from: number, to: number, hidden: [number, number][] = []) =>
    focused &&
    sel.ranges.some((r) => (r.empty ? r.head > from && r.head < to : hidden.some(([a, b]) => (r.from > a && r.from < b) || (r.to > a && r.to < b))));
  /** The selection is on [from, to] while the note is being written in (a fence or divider shows as written). */
  const touched = (from: number, to: number) => focused && sel.ranges.some((r) => r.from <= to && r.to >= from);
  const out: Range<Decoration>[] = [];
  const atomic: Range<Decoration>[] = [];
  const hide = (from: number, to: number) => {
    if (to <= from) return;
    out.push(hidden.range(from, to));
    atomic.push(hidden.range(from, to));
  };
  const draw = (from: number, to: number, widget: import("@codemirror/view").WidgetType) => {
    const d = Decoration.replace({ widget });
    out.push(d.range(from, to));
    atomic.push(d.range(from, to));
  };
  const mark = (from: number, to: number, cls: string, attributes?: Record<string, string>) => {
    if (to > from) out.push(Decoration.mark({ class: cls, attributes }).range(from, to));
  };
  const lineClass = (at: number, cls: string, style?: string) => out.push(Decoration.line({ class: cls, attributes: style ? { style } : undefined }).range(at));

  /** The outermost block quote starting at `from` (a callout's extent). */
  const quoteAt = (from: number): SyntaxNode | null => {
    for (let n: SyntaxNode | null = tree.resolveInner(from, 1); n; n = n.parent) if (n.name === "Blockquote" && n.from === from) return n;
    return null;
  };

  const doneLines = new Set<number>();
  const line = (l: import("@codemirror/state").Line) => {
    if (doneLines.has(l.from)) return;
    doneLines.add(l.from);
    const s = lineShape(state, l, tree);
    switch (s.kind) {
      case "frontmatter":
        return lineClass(l.from, "cm-fm");
      case "table":
        // Drawn whole by the table widget (blocks.ts); one inside a list or quote stays as written.
        if (s.block?.parent?.name !== "Document") lineClass(l.from, "cm-raw");
        return;
      case "html":
        return lineClass(l.from, "cm-raw");
      case "code":
        return codeLine(l, s);
      case "hr":
        if (touched(l.from, l.to)) return lineClass(l.from, "cm-hr-src");
        lineClass(l.from, "cm-hr-line");
        return draw(l.from, l.to, rule);
    }
    // An empty line between blocks is the space between them, not a whole line of it (unless the caret is there).
    if (!l.text && !touched(l.from, l.to)) return lineClass(l.from, "cm-blank");
    // While a marker is still being typed ("#", "-", "1."), it's text: no flicker to a heading or bullet.
    const typing = /^\s*(#{1,6}|[-*+]|\d{1,9}[.)])$/.test(l.text) && touched(l.to, l.to);
    const cls: string[] = [];
    const style: string[] = [];
    if (s.heading && !typing) {
      const under = s.block && /^SetextHeading/.test(s.block.name) ? doc.lineAt(s.block.to) : null;
      if (!under || under.text.trim().length >= 3 || !touched(under.from, under.to)) cls.push(`cm-h cm-h${s.heading}`);
    }
    if (s.underline) cls.push(l.text.trim().length < 3 && touched(l.from, l.to) ? "" : "cm-setext");
    if (s.quote) {
      const first = s.quoteFrom >= 0 ? doc.lineAt(s.quoteFrom) : l;
      const m = CALLOUT.exec(first.text);
      if (m) {
        const q = quoteAt(s.quoteFrom);
        const last = q ? doc.lineAt(q.to) : l;
        cls.push("cm-callout", `cm-co-${calloutKind(m[1])}`);
        if (l.number === first.number) cls.push("cm-callout-first");
        if (l.number === last.number) cls.push("cm-callout-last");
        if (l.number === first.number && !typing) {
          const at = l.from + l.text.indexOf("[!");
          const end = at + m[1].length + 3 + m[2].length;
          const sp = doc.sliceString(end, end + 1) === " " ? 1 : 0;
          if (at >= s.start) {
            hide(l.from, s.start);
            draw(s.start, Math.min(l.to, end + sp), new CalloutWidget(m[1], !m[3].trim()));
            if (s.depth) style.push(`--li:${s.depth}`);
            lineClass(l.from, cls.join(" "), style.join(";") || undefined);
            return;
          }
        }
      } else {
        cls.push("cm-quote");
        style.push(`--q:${s.quote}`);
      }
    }
    if (s.item && !typing) {
      cls.push("cm-li");
      style.push(`--li:${s.depth}`);
      if (s.item.kind === "task" && s.item.checked) cls.push("cm-done");
      draw(l.from, s.start, new MarkWidget(s.item.kind, s.item.mark, s.item.checked, s.depth));
    } else if (s.cont) {
      cls.push("cm-li-cont");
      style.push(`--li:${s.depth}`);
      hide(l.from, s.start);
    } else if (s.start > l.from && !typing) hide(l.from, s.start);
    if (cls.length || style.length) lineClass(l.from, cls.filter(Boolean).join(" "), style.join(";") || undefined);
  };

  const codeLine = (l: import("@codemirror/state").Line, s: LineShape) => {
    const fenced = s.block?.name === "FencedCode";
    const cls = ["cm-codeline"];
    if (!fenced) cls.push("cm-code-indented");
    if (s.fence === "open") {
      cls.push("cm-code-first");
      if (touched(l.from, l.to)) cls.push("cm-code-fence");
      else {
        const open = previews.includes(l.from);
        draw(l.from, l.to, new CodeHeadWidget(s.lang, /^(html?|xhtml|svg)$/.test(s.lang), open));
      }
    } else if (s.fence === "close") {
      cls.push("cm-code-last");
      if (touched(l.from, l.to)) cls.push("cm-code-fence");
      else {
        cls.push("cm-code-close");
        hide(l.from, l.to);
      }
    }
    if (fenced && s.block && doc.lineAt(s.block.to).number === l.number && s.fence !== "close") cls.push("cm-code-last");
    lineClass(l.from, cls.join(" "));
  };

  const doneNodes = new Set<string>();
  const inline = (n: SyntaxNodeRef): boolean | void => {
    const name = n.name;
    switch (name) {
      case "FencedCode": {
        const key = `c${n.from}`;
        if (doneNodes.has(key)) return false;
        doneNodes.add(key);
        const node = n.node;
        const info = node.getChild("CodeInfo");
        const text = node.getChild("CodeText");
        const lang = info ? doc.sliceString(info.from, info.to).toLowerCase() : "";
        if (lang && text && text.to - text.from < 20_000) {
          for (const [a, b, cls] of codeMarks(view, lang, doc.sliceString(text.from, text.to))) mark(text.from + a, text.from + b, cls);
        }
        return false;
      }
      case "CodeBlock":
      case "HTMLBlock":
      case "CommentBlock":
      case "Table":
        return false;
      case "Emphasis":
      case "StrongEmphasis":
      case "Strikethrough":
      case "Highlight":
      case "InlineCode": {
        const [cls, markName] = STYLE[name];
        mark(n.from, n.to, cls);
        // Its marks: out of sight, or (the caret in it) shown faintly.
        const marks: [number, number][] = [];
        for (let c = n.node.firstChild; c; c = c.nextSibling) if (c.name === markName) marks.push([c.from, c.to]);
        const shown = caretIn(n.from, n.to, marks);
        for (const [a, b] of marks) (shown ? mark(a, b, "cm-syntax") : hide(a, b));
        return name === "InlineCode" ? false : undefined;
      }
      case "Link":
        link(n.node);
        return;
      case "Image":
        picture(n.node);
        return false;
      case "WikiLink":
        wiki(n.node);
        return false;
      case "Autolink": {
        const url = n.node.getChild("URL");
        if (!url) return false;
        const href = doc.sliceString(url.from, url.to);
        if (caretIn(n.from, n.to, [[n.from, url.from], [url.to, n.to]])) mark(url.from, url.to, "cm-link");
        else {
          hide(n.from, url.from);
          hide(url.to, n.to);
          mark(url.from, url.to, "cm-link", { "data-href": href });
        }
        return false;
      }
      case "URL": {
        const parent = n.node.parent?.name;
        if (parent === "Link" || parent === "Image" || parent === "Autolink" || parent === "LinkReference") return;
        const href = doc.sliceString(n.from, n.to);
        mark(n.from, n.to, "cm-link", caretIn(n.from, n.to) ? undefined : { "data-href": /^www\./i.test(href) ? `https://${href}` : href });
        return;
      }
      case "Escape":
        if (!caretIn(n.from, n.to) && !sel.ranges.some((r) => r.empty && r.head === n.to)) hide(n.from, n.from + 1);
        return;
      case "HTMLTag":
      case "Comment":
        mark(n.from, n.to, "cm-html-tag");
        return;
      case "HeaderMark": {
        // A heading's closing #s.
        const parent = n.node.parent;
        if (parent && /^ATXHeading/.test(parent.name) && n.to === parent.to && n.from > parent.from + 1) {
          let from = n.from;
          while (from > parent.from && doc.sliceString(from - 1, from) === " ") from--;
          const firstMark = parent.firstChild;
          if (firstMark && from >= firstMark.to) hide(from, n.to);
        }
        return;
      }
    }
  };

  const link = (node: SyntaxNode) => {
    const marks = node.getChildren("LinkMark");
    if (marks.length < 2) return;
    const [open, close] = marks;
    if (close.from <= open.to) return;
    const url = node.getChild("URL");
    const label = node.getChild("LinkLabel");
    const href = url ? doc.sliceString(url.from, url.to).replace(/^<|>$/g, "") : label ? "" : "";
    if (caretIn(node.from, node.to, [[node.from, open.to], [close.from, node.to]])) {
      mark(open.to, close.from, "cm-link");
      mark(node.from, open.to, "cm-link-src");
      mark(close.from, node.to, "cm-link-src");
    } else {
      hide(node.from, open.to);
      hide(close.from, node.to);
      mark(open.to, close.from, "cm-link", href ? { "data-href": href } : undefined);
    }
  };

  const picture = (node: SyntaxNode) => {
    const marks = node.getChildren("LinkMark");
    const url = node.getChild("URL");
    if (!url || marks.length < 2) return;
    const altFrom = marks[0].to;
    const altTo = marks[1].from;
    const { alt, width } = parseAlt(doc.sliceString(altFrom, altTo));
    const src = doc.sliceString(url.from, url.to).replace(/^<|>$/g, "");
    const l = doc.lineAt(node.from);
    const rest = l.text.replace(/^[\s>]*(?:(?:[-*+]|\d+[.)])\s+)?/, "").trim();
    const block = rest === doc.sliceString(node.from, node.to);
    const selected = sel.ranges.some((r) => !r.empty && r.from <= node.from && r.to >= node.to);
    draw(node.from, node.to, new PictureWidget({ from: node.from, to: node.to, alt, src, width }, block, selected));
  };

  const wiki = (node: SyntaxNode) => {
    const inner = doc.sliceString(node.from + 2, node.to - 2);
    const pipe = inner.indexOf("|");
    const target = (pipe >= 0 ? inner.slice(0, pipe) : inner).split("#")[0].trim();
    const cls = hooks.resolve(target) ? "cm-wiki" : "cm-wiki cm-wiki-new";
    const shown = pipe >= 0 ? node.from + 2 + pipe + 1 : node.from + 2;
    if (caretIn(node.from, node.to, [[node.from, shown], [node.to - 2, node.to]])) {
      mark(node.from + 2, node.to - 2, cls);
      mark(node.from, node.from + 2, "cm-link-src");
      mark(node.to - 2, node.to, "cm-link-src");
      return;
    }
    hide(node.from, shown);
    hide(node.to - 2, node.to);
    mark(shown, node.to - 2, cls, { "data-wiki": target });
  };

  for (const { from, to } of view.visibleRanges) {
    for (let pos = from; pos <= to; ) {
      const l = doc.lineAt(pos);
      line(l);
      pos = l.to + 1;
    }
    tree.iterate({ from, to, enter: inline });
  }
  return { deco: Decoration.set(out, true), atomic: Decoration.set(atomic, true) };
}

export const livePage = ViewPlugin.fromClass(
  class {
    deco: DecorationSet;
    atomic: DecorationSet;
    constructor(view: EditorView) {
      ({ deco: this.deco, atomic: this.atomic } = build(view));
    }
    update(u: ViewUpdate) {
      if (u.docChanged || u.viewportChanged || u.selectionSet || u.focusChanged || syntaxTree(u.startState) !== syntaxTree(u.state) || u.transactions.some((t) => t.effects.some((e) => e.is(redraw))) || u.startState.field(previewField, false) !== u.state.field(previewField, false)) {
        ({ deco: this.deco, atomic: this.atomic } = build(u.view));
      }
    }
  },
  {
    decorations: (v) => v.deco,
    provide: (p) => EditorView.atomicRanges.of((view) => view.plugin(p)?.atomic ?? RangeSet.empty),
  },
);

/**
 * The caret goes to a line's text, never into its drawn markers: a click
 * on a bullet, Home, an arrow key from the line above. Moving left from the
 * start of the text goes on to the line before.
 */
const caretToText = EditorState.transactionFilter.of((tr) => {
  if (!tr.selection || tr.docChanged || tr.selection.ranges.length !== 1 || !tr.selection.main.empty) return tr;
  const state = tr.state;
  const head = tr.selection.main.head;
  const l = state.doc.lineAt(head);
  const s = lineShape(state, l);
  if (s.kind !== "text" || s.start <= l.from || head >= s.start) return tr;
  const old = tr.startState.selection.main;
  let to = s.start;
  if (!tr.isUserEvent("select.pointer") && old.empty && old.head === s.start && l.number > 1) to = l.from - 1;
  return [tr, { selection: EditorSelection.cursor(to), sequential: true }];
});

export const live: Extension = [livePage, caretToText];
