// Whole blocks the page draws in place of lines: tables (table.ts), the
// sandboxed preview under an HTML code block, and what follows the text
// (the notes that link here). These change the page's height, so they live
// in the editor state rather than in the on-screen pass (live.ts).
import { syntaxTree } from "@codemirror/language";
import { StateField, type EditorState, type Extension, type Range } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView, WidgetType } from "@codemirror/view";
import { noteHooks } from "./hooks";
import { TableWidget, parseTable } from "./table";
import { codeAt, togglePreviewEffect } from "./widgets";

/** Where the HTML code blocks being previewed start (they move with the text). */
export const previewField = StateField.define<number[]>({
  create: () => [],
  update(list, tr) {
    let next = tr.docChanged ? list.map((p) => tr.changes.mapPos(p, 1)) : list;
    for (const e of tr.effects) {
      if (!e.is(togglePreviewEffect)) continue;
      const at = tr.state.doc.lineAt(e.value).from;
      next = next.includes(at) ? next.filter((p) => p !== at) : [...next, at];
    }
    return next;
  },
});

class PreviewWidget extends WidgetType {
  constructor(readonly html: string) {
    super();
  }
  eq(o: PreviewWidget) {
    return o.html === this.html;
  }
  get estimatedHeight() {
    return 340;
  }
  toDOM(view: EditorView) {
    const el = document.createElement("div");
    el.className = "cm-preview";
    stops.set(el, view.state.facet(noteHooks).preview(el, this.html));
    return el;
  }
  destroy(dom: HTMLElement) {
    stops.get(dom)?.();
    stops.delete(dom);
  }
  ignoreEvent() {
    return true;
  }
}

class FooterWidget extends WidgetType {
  eq() {
    return true;
  }
  toDOM(view: EditorView) {
    const el = document.createElement("div");
    el.className = "cm-note-foot";
    stops.set(el, view.state.facet(noteHooks).footer(el));
    return el;
  }
  destroy(dom: HTMLElement) {
    stops.get(dom)?.();
    stops.delete(dom);
  }
  ignoreEvent() {
    return true;
  }
}
const footer = new FooterWidget();
const stops = new WeakMap<HTMLElement, () => void>();

function build(state: EditorState): DecorationSet {
  const out: Range<Decoration>[] = [];
  const previews = state.field(previewField, false) ?? [];
  const tree = syntaxTree(state);
  for (let n = tree.topNode.firstChild; n; n = n.nextSibling) {
    if (n.name === "Table") {
      const t = parseTable(state, n.from, n.to);
      if (t && t.cols > 0) out.push(Decoration.replace({ widget: new TableWidget(t), block: true }).range(t.from, t.to));
    } else if (n.name === "FencedCode" && previews.includes(state.doc.lineAt(n.from).from)) {
      const view = { state } as EditorView;
      out.push(Decoration.widget({ widget: new PreviewWidget(codeAt(view, n.from)), block: true, side: 1 }).range(n.to));
    }
  }
  out.push(Decoration.widget({ widget: footer, block: true, side: 1 }).range(state.doc.length));
  return Decoration.set(out, true);
}

export const blocksField = StateField.define<DecorationSet>({
  create: build,
  update(deco, tr) {
    if (tr.docChanged || syntaxTree(tr.startState) !== syntaxTree(tr.state) || tr.startState.field(previewField, false) !== tr.state.field(previewField, false)) return build(tr.state);
    return deco;
  },
  provide: (f) => [EditorView.decorations.from(f), EditorView.atomicRanges.of((v) => v.state.field(f))],
});

export const blocks: Extension = [previewField, blocksField];
