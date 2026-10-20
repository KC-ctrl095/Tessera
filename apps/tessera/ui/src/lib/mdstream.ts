// Markdown for replies that are still being written (components/chat/Markdown.tsx).
//
// A streamed reply only ever grows at its end, so its blocks settle one by
// one: once a block is followed by another, appending text can't change it.
// Settled blocks are rendered once (code highlighted, as in a finished reply)
// and never touched again; only the block being written, the tail, is redone,
// and only from its own source. The cost of a frame follows the size of that
// tail, not of the reply, and a long reply streams as smoothly as a short one.
//
// The tail itself is cheap too: an open code fence grows by appending to its
// text node; a list or table re-renders only its last item or row. Text being
// written gets its unfinished **bold**, `code` and [links]( closed for display,
// so markup never flashes as raw characters. A selection inside the tail is
// carried over each time it is redone.
//
// When the reply is complete, the tail is rendered as final and the DOM is the
// same as rendering the whole text at once (the committed message adopts it,
// see `park` / `adopt`, so nothing is rendered twice and nothing jumps).
import type { Token, Tokens } from "marked";
import { enhance, highlightInSteps, lexBlocks, lexInline, lexMarkdown, renderInline, renderMarkdownFragment, renderTokens } from "./markdown";
import { reducedMotion } from "./util";

/** How long freshly streamed words take to fade in (see `.fresh`). */
const FADE_MS = 220;

interface Tail {
  /** Where the tail block starts in the source. */
  start: number;
  kind: "code" | "list" | "table" | "para" | "block";
  /** Its top-level nodes in the root, in order. */
  nodes: Node[];
  /** code: the language, the text node that grows and the code it shows (without the final line break). */
  lang?: string;
  text?: Text;
  code?: string;
  /** list/table: what must not change for the incremental path, the item/row box, and its last item/row. */
  sig?: string;
  box?: HTMLElement;
  last?: HTMLElement;
  /** list/table: items/rows kept as they are (all but the last). */
  kept?: number;
  /** para: how much of its text is rendered for good, and the nodes after that (redone every time). */
  fixed?: number;
  fixedSrc?: string;
  /** para: the length of the kept part's rendered text. */
  fixedText?: number;
  /** para: its text when that is all of its source (so words can be added without lexing). */
  ptext?: string;
  live?: Node[];
}

export class StreamRenderer {
  root: HTMLElement;
  /** The text rendered so far. */
  src = "";
  /** Everything is rendered as final (the text is complete). */
  complete = false;
  /** It was ever updated as a stream (only those are handed over on unmount). */
  streamed = false;
  /** Where the settled blocks end: everything before is rendered for good. */
  private done = 0;
  private tail: Tail | null = null;
  /** Reference-style link definitions were seen: the final render resolves them across blocks. */
  private defs = false;
  private fade = new Fade();

  constructor(root: HTMLElement) {
    this.root = root;
  }

  /** Show `text`; `final` once it is complete. */
  update(text: string, final: boolean) {
    if (text.includes("\r")) text = text.replace(/\r\n?/g, "\n");
    if (text === this.src && final === this.complete) return;
    if (this.complete || !text.startsWith(this.src)) this.reset();
    // Nothing shown yet and the text is complete (a finished reply): one pass.
    if (final && !this.src) return this.renderAll(text);
    if (!final) this.streamed = true;
    this.step(text, final);
    this.src = text;
    this.complete = final;
    // Link definitions apply to the whole text: render it whole, once.
    if (final && this.defs) this.renderAll(text);
  }

  /** Move what is shown to another (empty) root. */
  moveTo(root: HTMLElement) {
    if (root === this.root) return;
    root.replaceChildren(...Array.from(this.root.childNodes));
    this.root = root;
  }

  private reset() {
    this.root.replaceChildren();
    this.src = "";
    this.done = 0;
    this.tail = null;
    this.defs = false;
    this.complete = false;
  }

  /** The whole text in one pass (a finished reply), keeping a selection inside it. */
  private renderAll(text: string) {
    const keep = saveSelection(Array.from(this.root.childNodes));
    this.root.replaceChildren(renderMarkdownFragment(text));
    // Saved history needs the same bounded highlighting work as a settled stream.
    // Mounting a page of old replies must not colour every code block in one task.
    enhance(this.root, false);
    highlightInSteps(Array.from(this.root.querySelectorAll<HTMLElement>(".code-block code")));
    if (keep) restoreSelection(keep, Array.from(this.root.childNodes));
    this.src = text;
    this.done = text.length;
    this.tail = null;
    this.complete = true;
  }

  private step(text: string, final: boolean) {
    // Words added to a paragraph's current line (no line break): its block structure can't change, so
    // there is nothing to lex but the words. Only for a line that already has letters or digits and doesn't
    // start with "<": those can't turn into a heading underline, a rule, a table row or an HTML block.
    const tail = this.tail;
    if (!final && tail?.kind === "para" && tail.ptext !== undefined) {
      const add = text.slice(this.src.length);
      const line = this.src.slice(this.src.lastIndexOf("\n") + 1);
      if (!add.includes("\n") && line.length >= 4 && /[\p{L}\p{N}]/u.test(line) && !line.trimStart().startsWith("<")) {
        return this.paraTail({ type: "paragraph", raw: tail.ptext + add, text: tail.ptext + add, tokens: [] }, tail.start, tail);
      }
    }
    // Block boundaries only (inline content is lexed for what gets rendered, below).
    const toks = lexBlocks(text.slice(this.done));
    if (Object.keys(toks.links).length) this.defs = true;
    let last = toks.length - 1;
    while (last >= 0 && toks[last].type === "space") last--;
    // Blocks [0, settled) can't change any more: all of them once the text is complete.
    let settled = final ? toks.length : Math.max(0, last);
    if (!final && last >= 0 && closedFence(toks[last])) settled = last + 1;
    let at = this.done;
    const starts = toks.map((t) => ((at += t.raw.length), at - t.raw.length));
    if (settled > 0) {
      const end = settled < toks.length ? starts[settled] : at;
      const blocks = lexMarkdown(text.slice(starts[0], end)).filter((t) => t.type !== "space");
      if (blocks.length) this.settle(blocks, starts[0]);
      this.done = end;
    }
    if (!final && last >= settled) this.renderTail(toks[last], starts[last]);
    else if (this.tail) this.dropTail();
  }

  /** Render blocks that are done (one pass for all of them) in place of the tail, if it was one of them. */
  private settle(blocks: Token[], start: number) {
    const old = this.tail;
    const codes: HTMLElement[] = [];
    // A code fence that was being written is already what it will be: keep it (with its final
    // text) rather than render it again; it only lacks its colours.
    const first = blocks[0] as Tokens.Code;
    if (old && old.start === start && old.kind === "code" && old.text?.isConnected && first.type === "code" && (first.lang ?? "") === old.lang) {
      // The rest of its text goes in, nothing is replaced: a selection in it keeps its place.
      grow(old, first.text);
      if (old.text.parentElement) codes.push(old.text.parentElement);
      this.tail = null;
      blocks = blocks.slice(1);
    }
    if (blocks.length) {
      const box = document.createElement("div");
      box.append(renderTokens(blocks));
      enhance(box, false);
      codes.push(...box.querySelectorAll<HTMLElement>(".code-block code"));
      const nodes = Array.from(box.childNodes);
      const tail = this.tail;
      if (tail && tail.start === start) {
        swap(this.root, tail.nodes, nodes);
        this.tail = null;
      } else {
        if (tail) this.dropTail();
        this.root.append(...nodes);
      }
    }
    // Colours come in idle moments, off the frame that is drawing the stream (a big block over a few frames).
    if (codes.length) highlightInSteps(codes);
  }

  private dropTail() {
    for (const n of this.tail?.nodes ?? []) n.parentNode?.removeChild(n);
    this.tail = null;
  }

  /** The block being written (`t` has its block structure only; inline content is lexed here, as needed). */
  private renderTail(t: Token, start: number) {
    const same = this.tail && this.tail.start === start ? this.tail : null;
    if (t.type === "code" && !(t as Tokens.Code).codeBlockStyle) return this.codeTail(t as Tokens.Code, start, same);
    if (t.type === "paragraph") return this.paraTail(t as Tokens.Paragraph, start, same);
    if (t.type === "list") return this.listTail(t as Tokens.List, start, same);
    if (t.type === "table") return this.tableTail(t as Tokens.Table, start, same);
    const raw = t.type === "heading" || t.type === "blockquote" ? closeTail(t.raw) : t.raw;
    const nodes = blockNodes(lexMarkdown(raw).filter((x) => x.type !== "space"));
    // Faded before it goes in, so a selection is restored over the final nodes.
    const el = nodes.find((n): n is HTMLElement => n instanceof HTMLElement);
    if (el) this.fade.apply(el, `${start}`);
    this.place({ start, kind: "block", nodes });
  }

  /** Put a new rendering of the tail in place of the old one. */
  private place(tail: Tail) {
    if (this.tail) swap(this.root, this.tail.nodes, tail.nodes);
    else this.root.append(...tail.nodes);
    this.tail = tail;
  }

  /**
   * A paragraph: its text up to a point where nothing is left open (no
   * unclosed emphasis, code span or link) is rendered once and kept; only the
   * rest is redone. One very long paragraph (a long string of text) costs per
   * frame what its last few hundred characters do.
   */
  private paraTail(t: Tokens.Paragraph, start: number, same: Tail | null) {
    const text = t.text;
    let tail = same?.kind === "para" && same.box?.isConnected && text.startsWith(same.fixedSrc!) ? same : null;
    // The fast path in `step` needs the text to be the whole source (no line break at its end).
    if (!tail) {
      const p = document.createElement("p");
      tail = { start, kind: "para", nodes: [p, document.createTextNode("\n")], box: p, fixed: 0, fixedSrc: "", live: [] };
      this.place(tail);
    }
    const box = tail.box!;
    // The paragraph's text is the same up to the new end, so a selection in it maps back by offset.
    const keep = saveSelection([box]);
    // Keep more of it for good, a segment at a time, once enough has arrived after a safe point.
    if (text.length - tail.fixed! > FIX_AFTER + FIX_LAG) {
      const s = text.lastIndexOf(" ", text.length - FIX_LAG) + 1;
      if (s > tail.fixed! + FIX_AFTER / 2 && /\S/.test(text[s - 2] ?? "") && /\S/.test(text[s] ?? "")) {
        const seg = text.slice(tail.fixed, s);
        const toks = lexInline(seg);
        if (closedInline(seg, toks)) {
          const frag = renderInline(toks);
          tail.fixedText = (tail.fixedText ?? 0) + (frag.textContent?.length ?? 0);
          box.insertBefore(frag, tail.live![0]?.parentNode === box ? tail.live![0] : null);
          tail.fixed = s;
          tail.fixedSrc = text.slice(0, s);
        }
      }
    }
    const rest = text.slice(tail.fixed);
    const nl = rest.lastIndexOf("\n");
    const live = document.createElement("span");
    live.append(renderInline(lexInline(rest.slice(0, nl + 1) + closeLine(rest.slice(nl + 1)))));
    this.fade.apply(live, `${start}`, tail.fixedText ?? 0);
    const nodes = Array.from(live.childNodes);
    const at = tail.live!.find((n) => n.parentNode === box) ?? null;
    for (const n of nodes) box.insertBefore(n, at);
    for (const n of tail.live!) n.parentNode?.removeChild(n);
    tail.live = nodes;
    tail.ptext = t.raw === text ? text : undefined;
    if (keep) restoreSelection(keep, [box]);
  }

  /** An open code fence: plain text that grows; highlighted once it closes. */
  private codeTail(t: Tokens.Code, start: number, same: Tail | null) {
    // The fence's first line (and its language) isn't complete yet: show nothing until it is.
    if (!t.raw.includes("\n")) {
      if (this.tail) this.dropTail();
      return;
    }
    const lang = t.lang ?? "";
    if (same?.kind === "code" && same.lang === lang && same.text && same.text.isConnected) return grow(same, t.text);
    const nodes = blockNodes([t]);
    const el = nodes.find((n): n is HTMLElement => n instanceof HTMLElement)?.querySelector("code");
    if (!el) return this.place({ start, kind: "block", nodes });
    const text = el.firstChild instanceof Text ? el.firstChild : el.appendChild(document.createTextNode(""));
    this.place({ start, kind: "code", nodes, lang, text, code: t.text.replace(/\n$/, "") });
  }

  /** A list: items before the last are kept; only the last one is redone (and lexed). */
  private listTail(t: Tokens.List, start: number, same: Tail | null) {
    const sig = `${t.ordered}|${t.start}|${t.loose}`;
    const n = t.items.length;
    // Items of a tight list lex the same on their own; a loose one is lexed whole.
    if (!t.loose && same?.kind === "list" && same.sig === sig && same.box && same.last && n >= same.kept! + 1) {
      const fresh: HTMLElement[] = [];
      for (let i = same.kept!; i < n; i++) {
        const raw = i === n - 1 ? closeTail(t.items[i].raw) : t.items[i].raw;
        const one = lexMarkdown(raw).filter((x) => x.type !== "space");
        const item = one.length === 1 && one[0].type === "list" ? (one[0] as Tokens.List).items : [];
        const li = item.length === 1 ? blockNodes([{ ...t, items: item }])[0]?.firstChild?.nextSibling : null;
        if (!(li instanceof HTMLElement)) return this.wholeList(t, start, sig);
        fresh.push(li);
      }
      this.fade.apply(fresh[fresh.length - 1], `${start}:${n}`);
      swap(same.box, [same.last], [fresh[0]]);
      for (const li of fresh.slice(1)) same.box.append(li, "\n");
      same.last = fresh[fresh.length - 1];
      same.kept = n - 1;
      return;
    }
    this.wholeList(t, start, sig);
  }

  private wholeList(block: Tokens.List, start: number, sig: string) {
    const t = (lexMarkdown(closeTail(block.raw)).find((x) => x.type === "list") as Tokens.List | undefined) ?? block;
    const nodes = blockNodes([t]);
    const box = nodes.find((x): x is HTMLElement => x instanceof HTMLElement);
    const last = box?.lastElementChild;
    if (last instanceof HTMLElement) this.fade.apply(last, `${start}:${t.items.length}`);
    this.place({ start, kind: "list", nodes, sig, box, last: last instanceof HTMLElement ? last : undefined, kept: t.items.length - 1 });
  }

  /** A table: rows before the last are kept; only the last one is redone (lexed under the header). */
  private tableTail(t: Tokens.Table, start: number, same: Tail | null) {
    const sig = JSON.stringify([t.header.map((c) => c.text), t.align]);
    const n = t.rows.length;
    const lines = t.raw.replace(/\n+$/, "").split("\n");
    if (same?.kind === "table" && same.sig === sig && same.box && same.last && n >= same.kept! + 1 && lines.length === n + 2) {
      const head = lines[0] + "\n" + lines[1] + "\n";
      const fresh: HTMLElement[] = [];
      for (let i = same.kept!; i < n; i++) {
        const one = lexMarkdown(head + (i === n - 1 ? closeTail(lines[i + 2]) : lines[i + 2])).filter((x) => x.type !== "space");
        const tr = one.length === 1 && one[0].type === "table" && (one[0] as Tokens.Table).rows.length === 1 ? blockNodes(one)[0] : null;
        const row = tr instanceof HTMLElement ? tr.querySelector("tbody > tr") : null;
        if (!(row instanceof HTMLElement)) return this.wholeTable(t, start, sig);
        fresh.push(row);
      }
      this.fade.apply(fresh[fresh.length - 1], `${start}:${n}`);
      swap(same.box, [same.last], [fresh[0]]);
      for (const tr of fresh.slice(1)) same.box.append(tr, "\n");
      same.last = fresh[fresh.length - 1];
      same.kept = n - 1;
      return;
    }
    this.wholeTable(t, start, sig);
  }

  private wholeTable(block: Tokens.Table, start: number, sig: string) {
    const t = (lexMarkdown(closeTail(block.raw)).find((x) => x.type === "table") as Tokens.Table | undefined) ?? block;
    const nodes = blockNodes([t]);
    const wrap = nodes.find((x): x is HTMLElement => x instanceof HTMLElement);
    const box = wrap?.querySelector("tbody") ?? undefined;
    const last = box?.lastElementChild;
    if (last instanceof HTMLElement) this.fade.apply(last, `${start}:${t.rows.length}`);
    this.place({ start, kind: "table", nodes, sig, box, last: last instanceof HTMLElement ? last : undefined, kept: t.rows.length - 1 });
  }
}

/** A paragraph keeps a segment for good once this much follows its last kept point… */
const FIX_AFTER = 800;
/** …leaving at least this much of its end to be redone (where markup may still be open). */
const FIX_LAG = 300;

/**
 * Whether a segment of a paragraph (ending in a space) has nothing left open
 * that later text could close: every emphasis, code span and link in it is
 * complete, and what is left as plain text can't start one.
 */
function closedInline(seg: string, toks: Token[]): boolean {
  let at = 0;
  for (const t of toks) {
    const from = at;
    at += t.raw.length;
    if (t.type === "html") return false;
    if (t.type !== "text") continue;
    for (let i = from; i < at; i++) {
      const c = seg[i];
      if (c === "`" || c === "[" || c === "]" || c === "<") return false;
      if (c !== "*" && c !== "_" && c !== "~") continue;
      // A delimiter can open or close only next to non-space; `_` between letters is just a letter.
      const prev = seg[i - 1] ?? " ";
      let j = i;
      while (seg[j] === c) j++;
      const next = seg[j] ?? " ";
      const blankAround = /\s/.test(prev) && /\s/.test(next);
      const intraword = c === "_" && /[\p{L}\p{N}]/u.test(prev) && /[\p{L}\p{N}]/u.test(next);
      if (!blankAround && !intraword) return false;
      i = j - 1;
    }
  }
  return at === seg.length;
}

/**
 * A code block's text node, from the code it shows (`tail.code`, without the
 * line break the renderer adds at the end) to `text`: what was added goes in
 * before that line break, so nothing already there moves.
 */
function grow(tail: Tail, text: string) {
  const body = text.replace(/\n$/, "");
  const old = tail.code ?? "";
  if (old && body.startsWith(old)) {
    if (body.length > old.length) tail.text!.insertData(old.length, body.slice(old.length));
  } else tail.text!.data = body ? body + "\n" : "";
  tail.code = body;
}

/** Rendered blocks with their code-block and table chrome (not highlighted: they're still being written). */
function blockNodes(tokens: Token[]): Node[] {
  const box = document.createElement("div");
  box.append(renderTokens(tokens));
  enhance(box, false);
  return Array.from(box.childNodes);
}

/** A fenced code block whose closing fence (and the line break after it) has arrived. */
function closedFence(t: Token): boolean {
  if (t.type !== "code" || (t as Tokens.Code).codeBlockStyle) return false;
  const m = /^ {0,3}(`{3,}|~{3,})/.exec(t.raw);
  if (!m) return false;
  const close = new RegExp(`\\n {0,3}${m[1][0] === "`" ? "`" : "~"}{${m[1].length},}[ \\t]*\\n\\s*$`);
  return close.test(t.raw);
}

// ── text that is still being written ─────────────────────────────────────

/**
 * The source of the block being written with the unfinished markup on its
 * last line closed (or, when nothing follows it yet, left out), so `**bold`
 * reads as bold rather than flashing its asterisks. Only for display: the next
 * frame starts again from the real text.
 */
function closeTail(src: string): string {
  const raw = src.replace(/\s+$/, "");
  // Inside a fence (a code block in a list item, say) every character is meant literally.
  if ((raw.match(/^[ \t>]*(?:`{3,}|~{3,})/gm)?.length ?? 0) % 2) return src;
  const nl = raw.lastIndexOf("\n");
  const line = raw.slice(nl + 1);
  if (/^[ \t>]*(?:`{3,}|~{3,})/.test(line)) return src;
  const fixed = closeLine(line);
  return fixed === line ? src : raw.slice(0, nl + 1) + fixed;
}

/**
 * One line of text being written, with its unfinished `code span`, **strong**,
 * ~~strike~~ or [link](url closed. A marker with nothing after it yet is left
 * out instead.
 */
export function closeLine(s: string): string {
  // Code spans first: inside one, nothing else counts.
  let open = 0;
  let openAt = -1;
  let from = 0;
  const outside: [number, number][] = [];
  for (let i = 0; i < s.length; ) {
    if (!open && s[i] === "\\") {
      i += 2;
      continue;
    }
    if (s[i] !== "`") {
      i++;
      continue;
    }
    let j = i;
    while (s[j] === "`") j++;
    if (!open) {
      outside.push([from, i]);
      open = j - i;
      openAt = i;
    } else if (j - i === open) {
      open = 0;
      from = j;
    }
    i = j;
  }
  let close = "";
  if (open) {
    if (/\S/.test(s.slice(openAt + open))) close = "`".repeat(open);
    else s = s.slice(0, openAt);
  } else outside.push([from, s.length]);
  // Strong and strike outside code spans (escapes blanked out, segments kept apart).
  const plain = outside.map(([a, b]) => s.slice(a, b).replace(/\\[\s\S]/g, "  ")).join("\u0000");
  const opens: [number, string][] = [];
  let cut = s.length;
  for (const mark of ["**", "~~"]) {
    const at = unmatched(plain, mark);
    if (at < 0) continue;
    const pos = mapBack(outside, at);
    const after = s.slice(pos + 2) + close;
    if (!/\S/.test(after)) cut = Math.min(cut, pos);
    else if (/^\S/.test(after)) opens.push([pos, mark]);
  }
  if (cut < s.length) return s.slice(0, cut);
  // Inner markers close first.
  for (const [, mark] of opens.sort((a, b) => b[0] - a[0])) close += mark;
  if (close) return s + close;
  // [label](partial-url: a link until the rest of it arrives.
  const link = /\[[^\]\n]*\]\([^)\s]*$/.exec(s);
  if (link) return s + ")";
  return s;
}

/** Index of the last unmatched `mark` in `s`, or -1. */
function unmatched(s: string, mark: string): number {
  let count = 0;
  let last = -1;
  for (let i = s.indexOf(mark); i >= 0; i = s.indexOf(mark, i + mark.length)) {
    count++;
    last = i;
  }
  return count % 2 ? last : -1;
}

/** A position in the joined outside-code text, back in the line. */
function mapBack(outside: [number, number][], at: number): number {
  let p = 0;
  for (const [a, b] of outside) {
    const len = b - a;
    if (at < p + len) return a + (at - p);
    p += len + 1;
  }
  return outside.length ? outside[outside.length - 1][1] : 0;
}

// ── fading in what just arrived ──────────────────────────────────────────

/**
 * Wraps the characters that arrived in the last FADE_MS in a span that fades
 * in, `age` ms into its fade (the tail is redone every frame, so a fade must
 * continue rather than restart).
 */
class Fade {
  private key = "";
  private hist: { len: number; t: number }[] = [];

  /** `before`: rendered text ahead of `el` that belongs to the same block (a paragraph's kept part). */
  apply(el: HTMLElement, key: string, before = 0) {
    if (reducedMotion()) return;
    const now = performance.now();
    const len = before + (el.textContent?.length ?? 0);
    if (key !== this.key) {
      this.key = key;
      this.hist = [{ len: 0, t: now - 16 }];
    }
    this.hist.push({ len, t: now });
    while (this.hist.length > 2 && this.hist[1].t <= now - FADE_MS) this.hist.shift();
    const fresh = len - this.hist[0].len;
    if (fresh > 0 && fresh < 600) fadeTail(el, fresh, Math.min(FADE_MS, now - (this.hist[1]?.t ?? now)));
  }
}

function fadeTail(root: Node, n: number, age: number) {
  // The last non-blank text node, found from the end.
  let last: Text | null = null;
  for (let node: Node | null = root.lastChild; node && !last; ) {
    if (node instanceof Text) {
      if (node.data.trim()) last = node;
      else node = prevNode(node, root);
    } else node = node.lastChild ?? prevNode(node, root);
  }
  if (!last || last.parentElement?.closest("pre, code")) return;
  const len = last.data.length;
  const k = Math.min(n, len);
  if (k <= 0) return;
  const fresh = k < len ? last.splitText(len - k) : last;
  const span = document.createElement("span");
  span.className = "fresh";
  span.style.animationDelay = `-${Math.round(age)}ms`;
  fresh.replaceWith(span);
  span.append(fresh);
}

/** The node before `n` in document order (its previous sibling's deepest last descendant comes later). */
function prevNode(n: Node, root: Node): Node | null {
  for (let x: Node | null = n; x && x !== root; x = x.parentNode) if (x.previousSibling) return x.previousSibling;
  return null;
}

// ── keeping a selection while nodes are replaced ─────────────────────────

type End = { at: number } | { node: Node; offset: number };
export interface SavedSelection {
  anchor: End;
  focus: End;
}

const inside = (nodes: Node[], n: Node | null) => !!n && nodes.some((o) => o === n || o.contains(n));

/** Where a (non-empty) selection that touches `nodes` is, as text offsets into them. */
export function saveSelection(nodes: Node[]): SavedSelection | null {
  // An empty/new block cannot contain a selection. Asking the browser for its
  // selection first can flush pending layout once per reply while mounting history.
  if (!nodes[0]?.parentNode) return null;
  const sel = document.getSelection();
  if (!sel || !sel.rangeCount || sel.isCollapsed) return null;
  const a = inside(nodes, sel.anchorNode);
  const f = inside(nodes, sel.focusNode);
  if (!a && !f) return null;
  const offset = (node: Node, off: number) => {
    const r = document.createRange();
    r.setStartBefore(nodes[0]);
    r.setEnd(node, off);
    return r.toString().length;
  };
  return {
    anchor: a ? { at: offset(sel.anchorNode!, sel.anchorOffset) } : { node: sel.anchorNode!, offset: sel.anchorOffset },
    focus: f ? { at: offset(sel.focusNode!, sel.focusOffset) } : { node: sel.focusNode!, offset: sel.focusOffset },
  };
}

/** Put a saved selection back, now over `nodes` (the same text, rendered again). */
export function restoreSelection(s: SavedSelection, nodes: Node[]) {
  const find = (e: End): [Node, number] | null => {
    if ("node" in e) return e.node.isConnected ? [e.node, e.offset] : null;
    let n = e.at;
    let end: [Node, number] | null = null;
    for (const top of nodes) {
      const w = document.createTreeWalker(top, NodeFilter.SHOW_TEXT);
      for (let t = top instanceof Text ? top : (w.nextNode() as Text | null); t; t = top instanceof Text ? null : (w.nextNode() as Text | null)) {
        if (n <= t.data.length) return [t, n];
        n -= t.data.length;
        end = [t, t.data.length];
      }
    }
    return end;
  };
  const a = find(s.anchor);
  const f = find(s.focus);
  if (a && f) document.getSelection()?.setBaseAndExtent(a[0], a[1], f[0], f[1]);
}

/** Replace `old` with `fresh` under `parent`, keeping the selection and a code block's or table's horizontal scroll. */
function swap(parent: Node, old: Node[], fresh: Node[]) {
  const keep = saveSelection(old);
  const first = old.find((n) => n.parentNode === parent);
  const scroll = first instanceof HTMLElement ? scrollOf(first) : 0;
  if (first) {
    for (const n of fresh) parent.insertBefore(n, first);
    for (const n of old) n.parentNode?.removeChild(n);
  } else for (const n of fresh) parent.appendChild(n);
  if (scroll && fresh[0] instanceof HTMLElement) {
    const el = scroller(fresh[0]);
    if (el) el.scrollLeft = scroll;
  }
  if (keep) restoreSelection(keep, fresh);
}

const scroller = (el: HTMLElement): HTMLElement | null => (el.classList.contains("code-block") ? el.querySelector("pre") : el.classList.contains("table-wrap") ? el : null);
const scrollOf = (el: HTMLElement) => scroller(el)?.scrollLeft ?? 0;

// ── handing a streamed reply to its committed message ────────────────────

/**
 * When a reply is committed, its live copy unmounts and the message mounts in
 * the transcript with the same text. The live renderer is parked for a moment
 * so the message can take its DOM as it is: no second render, no jump, and a
 * selection inside it survives.
 */
const parked: { r: StreamRenderer; text: string; at: number; sel: SavedSelection | null }[] = [];
let sweep: ReturnType<typeof setTimeout> | undefined;

export function park(r: StreamRenderer, text: string) {
  if (!text) return;
  const now = performance.now();
  parked.push({ r, text, at: now, sel: saveSelection(Array.from(r.root.childNodes)) });
  while (parked.length > 6 || (parked.length && parked[0].at < now - 3000)) parked.shift();
  clearTimeout(sweep);
  sweep = setTimeout(() => (parked.length = 0), 3000);
}

/** A parked renderer showing `text`, moved into `root` (or null). */
export function adopt(text: string, root: HTMLElement): StreamRenderer | null {
  const i = parked.findIndex((p) => p.text === text);
  if (i < 0) return null;
  const [p] = parked.splice(i, 1);
  p.r.moveTo(root);
  if (p.sel) restoreSelection(p.sel, Array.from(root.childNodes));
  return p.r;
}
