// Suggestions while typing: `[[` lists notes to link to; `/` at the start
// of a line (or after a space) lists blocks to insert.
import { autocompletion, type Completion, type CompletionContext, type CompletionResult } from "@codemirror/autocomplete";
import { syntaxTree } from "@codemirror/language";
import type { Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { noteHooks } from "./hooks";
import { INSERTS } from "./insert";

const inCode = (ctx: CompletionContext) => {
  for (let n: import("@lezer/common").SyntaxNode | null = syntaxTree(ctx.state).resolveInner(ctx.pos, -1); n; n = n.parent) {
    if (n.name === "FencedCode" || n.name === "CodeBlock" || n.name === "InlineCode" || n.name === "HTMLBlock") return true;
  }
  return false;
};

function wikiLinks(ctx: CompletionContext): CompletionResult | null {
  const m = ctx.matchBefore(/\[\[[^\]\n|#[]*$/);
  if (!m || inCode(ctx)) return null;
  const hooks = ctx.state.facet(noteHooks);
  const from = m.from + 2;
  const closed = ctx.state.sliceDoc(ctx.pos, ctx.pos + 2) === "]]";
  const options: Completion[] = hooks
    .notes()
    .filter((n) => n.id !== hooks.noteId && n.title.trim())
    .slice(0, 400)
    .map((n, i) => ({
      label: n.title,
      type: "note",
      boost: -Math.min(99, i / 4),
      apply: (view: EditorView, _c: Completion, a: number, b: number) => {
        const end = closed ? b + 2 : b;
        const insert = `${n.title.replace(/[[\]|#\n]/g, " ").trim()}]]`;
        view.dispatch({ changes: { from: a, to: end, insert }, selection: { anchor: a + insert.length }, userEvent: "input.complete" });
      },
    }));
  return { from, options, validFor: /^[^\]\n|#[]*$/ };
}

function slashMenu(ctx: CompletionContext): CompletionResult | null {
  const m = ctx.matchBefore(/(?:^|\s)\/[\p{L}\d ]{0,24}$/u);
  if (!m || inCode(ctx)) return null;
  const slash = m.from + m.text.indexOf("/");
  // Only at a line's start or after a space, and not halfway into a path ("a/b").
  const query = ctx.state.sliceDoc(slash + 1, ctx.pos);
  if (/\s{2}|^\s/.test(query)) return null;
  const options: Completion[] = INSERTS.map((it) => ({
    label: it.label,
    type: `insert-${it.id}`,
    apply: (view: EditorView, _c: Completion, _a: number, b: number) => {
      view.dispatch({ changes: { from: slash, to: b }, selection: { anchor: slash }, userEvent: "input.complete" });
      it.run(view);
    },
  }));
  // Asked again on every key (no `validFor`): the list is filtered here, synonyms included.
  return { from: slash + 1, options, filter: false };
}

/** The insert menu's items for what follows the `/`, best first. */
function slashFiltered(ctx: CompletionContext): CompletionResult | null {
  const r = slashMenu(ctx);
  if (!r) return null;
  const q = ctx.state.sliceDoc(r.from, ctx.pos).trim().toLowerCase();
  if (!q) return r;
  const words = q.split(/\s+/);
  const hit = INSERTS.map((it, i) => {
    const hay = `${it.label} ${it.also}`.toLowerCase();
    const score = words.every((w) => hay.includes(w)) ? (it.label.toLowerCase().startsWith(q) ? 2 : 1) : 0;
    return { opt: r.options[i], score };
  })
    .filter((x) => x.score)
    .sort((a, b) => b.score - a.score)
    .map((x) => x.opt);
  return hit.length ? { ...r, options: hit } : null;
}

const ICONS: Record<string, string> = Object.fromEntries(INSERTS.map((it) => [`insert-${it.id}`, it.icon]));

export const suggestions: Extension = [
  autocompletion({
    override: [wikiLinks, slashFiltered],
    icons: false,
    closeOnBlur: true,
    activateOnTyping: true,
    defaultKeymap: true,
    maxRenderedOptions: 60,
    addToOptions: [
      {
        position: 20,
        render: (c) => {
          const el = document.createElement("span");
          el.className = "cm-sug-icon";
          el.setAttribute("aria-hidden", "true");
          const t = typeof c.type === "string" ? c.type : "";
          el.textContent = ICONS[t] ?? (t === "note" ? "▤" : "");
          return el;
        },
      },
    ],
    tooltipClass: () => "cm-sug",
  }),
];
