// Markdown → sanitized HTML. Code is highlighted by highlight.js (core + a
// handful of languages), loaded with the first code block (lib/highlight.ts).
import { Marked, type Token, type TokensList } from "marked";
import DOMPurify from "dompurify";

type Hl = typeof import("./highlight");
let hl: Hl | null = null;
let hlLoading: Promise<Hl> | null = null;
const loadHl = () => (hlLoading ??= import("./highlight").then((m) => (hl = m)));
const marked = new Marked({
  gfm: true,
  breaks: false,
  async: false,
  renderer: {
    // Task lists: plain ballot boxes instead of form inputs.
    checkbox({ checked }) {
      return checked ? "\u2611 " : "\u2610 ";
    },
  },
});

// Model output is untrusted. A tight allowlist: no styling hooks (style, id,
// data-*, arbitrary classes) so a reply can't dress itself up as part of the
// UI (e.g. a fake approval card), no forms, no remote resources.
const ALLOWED_TAGS = [
  "p", "br", "strong", "em", "b", "i", "code", "pre", "blockquote", "ul", "ol", "li", "a",
  "h1", "h2", "h3", "h4", "h5", "h6",
  "table", "thead", "tbody", "tfoot", "tr", "th", "td", "caption",
  "hr", "del", "img", "span", "div", "sup", "sub", "details", "summary", "kbd",
];
const ALLOWED_ATTR = ["href", "title", "alt", "src", "class", "colspan", "rowspan", "align"];
/** Classes the renderer and highlighter need; everything else is dropped. */
const SAFE_CLASS = /^(?:language-[\w+#.-]+|hljs(?:-[\w-]+)?)$/;
/** Inline images only (the app never loads remote content). */
const SAFE_IMG = /^data:image\/(?:png|jpeg|gif|webp);base64,[a-z0-9+/=\s]+$/i;

DOMPurify.addHook("uponSanitizeAttribute", (node, data) => {
  if (data.attrName === "class") {
    const keep = data.attrValue.split(/\s+/).filter((c) => SAFE_CLASS.test(c));
    if (keep.length) data.attrValue = keep.join(" ");
    else data.keepAttr = false;
  } else if (data.attrName === "src") {
    if (node.nodeName !== "IMG" || !SAFE_IMG.test(data.attrValue)) data.keepAttr = false;
  }
});
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName === "A") {
    node.setAttribute("target", "_blank");
    node.setAttribute("rel", "noopener noreferrer");
  }
});

const PURIFY = {
  ALLOWED_TAGS,
  ALLOWED_ATTR,
  FORBID_TAGS: ["style", "form", "area", "map", "input", "button", "textarea", "select", "iframe", "svg", "math"],
  FORBID_ATTR: ["style", "id", "name", "usemap"],
  ALLOW_DATA_ATTR: false,
  ALLOW_ARIA_ATTR: false,
  ALLOWED_URI_REGEXP: /^(?:https?:|mailto:|#)/i,
  // With a strict URI pattern DOMPurify checks every attribute value against
  // it; these plain-value attributes are not URLs.
  ADD_URI_SAFE_ATTR: ["align", "colspan", "rowspan"],
  ADD_DATA_URI_TAGS: ["img"],
  RETURN_DOM_FRAGMENT: true as const,
};

/** Sanitized markdown as a DOM fragment. */
export function renderMarkdownFragment(src: string): DocumentFragment {
  return renderTokens(marked.lexer(src));
}

/** Markdown's top-level blocks (with their inline tokens), as the renderer sees them. */
export function lexMarkdown(src: string): TokensList {
  return marked.lexer(src);
}

/** Top-level blocks only, their inline content not lexed: cheap, for finding where blocks begin and end. */
export function lexBlocks(src: string): TokensList {
  const lexer = new marked.Lexer(marked.defaults);
  lexer.blockTokens(src, lexer.tokens);
  return lexer.tokens;
}

/** Inline tokens of a run of text (a paragraph's, say). */
export function lexInline(src: string): Token[] {
  return marked.Lexer.lexInline(src, marked.defaults);
}

/**
 * Sanitized DOM for lexed blocks. Rendering blocks one run at a time gives
 * the same DOM as rendering them together (lib/mdstream.ts relies on it).
 */
export function renderTokens(tokens: Token[]): DocumentFragment {
  return sanitized(marked.parser(tokens) as string);
}

/** Sanitized DOM for inline tokens (no enclosing block). */
export function renderInline(tokens: Token[]): DocumentFragment {
  return sanitized(marked.Parser.parseInline(tokens, marked.defaults) as string);
}

function sanitized(html: string): DocumentFragment {
  const frag = DOMPurify.sanitize(html, PURIFY);
  // Remote (or otherwise unsafe) images lost their src: show their alt text instead of a broken image.
  for (const img of Array.from(frag.querySelectorAll("img"))) {
    if (img.getAttribute("src")) continue;
    const alt = img.getAttribute("alt")?.trim();
    img.replaceWith(document.createTextNode(alt ? `[${alt}]` : ""));
  }
  return frag;
}

export function renderMarkdown(src: string): string {
  const box = document.createElement("div");
  box.append(renderMarkdownFragment(src));
  return box.innerHTML;
}

/** Highlighted HTML for `code` (escaped text until highlight.js has loaded). */
export function highlightCode(code: string, lang?: string): string {
  if (hl && lang && hl.hasLanguage(lang)) {
    try {
      return hl.highlight(code, lang);
    } catch {
      /* fall through */
    }
  }
  return escapeHtml(code);
}

/** Highlight the code blocks under `root` (now, or once highlight.js is here). */
function highlightIn(root: HTMLElement) {
  highlightCodes(Array.from(root.querySelectorAll<HTMLElement>(".code-block code:not(.hljs)")));
}

/** Highlight these `<code>` elements of code blocks (now, or once highlight.js is here, if still shown). */
export function highlightCodes(codes: HTMLElement[]) {
  const blocks = codes.filter((c) => !c.classList.contains("hljs") && /language-[\w+#-]+/.test(c.className));
  if (!blocks.length) return;
  if (!hl) {
    void loadHl().then(() => highlightCodes(blocks.filter((c) => c.isConnected)));
    return;
  }
  for (const code of blocks) {
    const lang = (code.className.match(/language-([\w+#-]+)/)?.[1] ?? "").toLowerCase();
    if (!hl.hasLanguage(lang)) continue;
    code.innerHTML = highlightCode(code.textContent ?? "", lang);
    code.classList.add("hljs");
  }
}

/** Code put in per frame by `highlightInSteps` (about 45 lines). */
const STEP_CHARS = 2500;
const steps: HTMLElement[] = [];
let stepping = false;

/**
 * Highlight these code blocks without a long task: one block per idle moment,
 * and a big block's coloured spans go in over several frames, replacing its
 * plain text from the top (the end result is the same DOM as `highlightCodes`).
 */
export function highlightInSteps(codes: HTMLElement[]) {
  steps.push(...codes.filter((c) => /language-[\w+#-]+/.test(c.className)));
  if (!stepping) {
    stepping = true;
    whenIdle(nextStep);
  }
}

function nextStep() {
  if (!hl) return void loadHl().then(() => whenIdle(nextStep));
  const code = steps.shift();
  if (!code) return void (stepping = false);
  const lang = (code.className.match(/language-([\w+#-]+)/)?.[1] ?? "").toLowerCase();
  const plain = code.firstChild;
  if (!code.isConnected || code.classList.contains("hljs") || !hl.hasLanguage(lang)) return nextStep();
  if (!(plain instanceof Text) || plain.nextSibling) {
    highlightCodes([code]);
    return whenIdle(nextStep);
  }
  const text = plain.data;
  const tpl = document.createElement("template");
  tpl.innerHTML = highlightCode(text, lang);
  const parts = Array.from(tpl.content.childNodes);
  let i = 0;
  const step = () => {
    if (!code.isConnected || plain.parentNode !== code) return whenIdle(nextStep);
    // Offsets in the plain text shift as it shrinks: keep a selection in the block where it was.
    const keep = saveSelectionIn(code);
    const frag = document.createDocumentFragment();
    let n = 0;
    while (i < parts.length && n < STEP_CHARS) {
      n += parts[i].textContent?.length ?? 0;
      frag.append(parts[i++]);
    }
    code.insertBefore(frag, plain);
    plain.deleteData(0, n);
    if (i >= parts.length) {
      plain.remove();
      code.classList.add("hljs");
    }
    if (keep) keep();
    if (i < parts.length) requestAnimationFrame(step);
    else whenIdle(nextStep);
  };
  step();
}

/** When the selection ends in `el`, a function that puts it back at the same text offsets. */
function saveSelectionIn(el: HTMLElement): (() => void) | null {
  const sel = document.getSelection();
  if (!sel || !sel.rangeCount || sel.isCollapsed) return null;
  const a = sel.anchorNode && el.contains(sel.anchorNode);
  const f = sel.focusNode && el.contains(sel.focusNode);
  if (!a && !f) return null;
  const at = (node: Node, off: number) => {
    const r = document.createRange();
    r.setStart(el, 0);
    r.setEnd(node, off);
    return r.toString().length;
  };
  const ends = [a ? at(sel.anchorNode!, sel.anchorOffset) : ([sel.anchorNode!, sel.anchorOffset] as const), f ? at(sel.focusNode!, sel.focusOffset) : ([sel.focusNode!, sel.focusOffset] as const)];
  return () => {
    const find = (e: number | readonly [Node, number]): [Node, number] => {
      if (typeof e !== "number") return [e[0], e[1]];
      const w = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
      let n = e;
      let last: [Node, number] = [el, 0];
      for (let t = w.nextNode() as Text | null; t; t = w.nextNode() as Text | null) {
        if (n <= t.data.length) return [t, n];
        n -= t.data.length;
        last = [t, t.data.length];
      }
      return last;
    };
    const [an, ao] = find(ends[0]);
    const [fn, fo] = find(ends[1]);
    document.getSelection()?.setBaseAndExtent(an, ao, fn, fo);
  };
}

/** Run `fn` when the main thread is idle (soon, at the latest). */
export function whenIdle(fn: () => void) {
  if (typeof requestIdleCallback === "function") requestIdleCallback(fn, { timeout: 250 });
  else setTimeout(fn, 30);
}

export function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}

const COPY_SVG =
  '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="8.5" y="8.5" width="11" height="11" rx="2.5"/><path d="M15.5 8.5V6.5a2 2 0 0 0-2-2h-7a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h2"/></svg>';

/**
 * Post-process rendered markdown inside `root`: code block chrome + copy
 * buttons, highlighting (skipped while streaming), scrollable tables.
 */
export function enhance(root: HTMLElement, highlight: boolean) {
  for (const pre of Array.from(root.querySelectorAll("pre"))) {
    if (pre.parentElement?.classList.contains("code-block")) continue;
    const code = pre.querySelector("code");
    const lang = (code?.className.match(/language-([\w+#-]+)/)?.[1] ?? "").toLowerCase();
    const wrap = document.createElement("div");
    wrap.className = "code-block";
    const head = document.createElement("div");
    head.className = "code-head";
    const label = document.createElement("span");
    label.textContent = lang || "text";
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "code-copy";
    btn.setAttribute("aria-label", "Copy code");
    btn.innerHTML = COPY_SVG + "<span>Copy</span>";
    head.append(label, btn);
    pre.replaceWith(wrap);
    wrap.append(head, pre);
  }
  if (highlight) highlightIn(root);
  for (const table of Array.from(root.querySelectorAll("table"))) {
    if (table.parentElement?.classList.contains("table-wrap")) continue;
    const wrap = document.createElement("div");
    wrap.className = "table-wrap";
    table.replaceWith(wrap);
    wrap.append(table);
  }
}
