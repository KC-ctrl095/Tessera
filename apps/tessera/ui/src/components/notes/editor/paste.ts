// Pasting and dropping: rich text (from the web, Google Docs, Word) becomes
// markdown with its structure kept (headings, bold and italic however they
// were done, lists with their nesting, tasks, tables, links, code), and
// pictures go to the server and into the note where they were put (on every
// platform, see `pasting`). Plain markdown goes in as it is and is drawn like
// the rest.
import { syntaxTree } from "@codemirror/language";
import { EditorSelection, type Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { clipboardFiles, dropMayHide, droppedFiles, pasteMayHide, transferFiles } from "../../../lib/clipboard";
import { altFor, uploadPicture } from "../../../lib/noteAssets";
import { toast, toastError } from "../../../lib/ui";
import { noteHooks } from "./hooks";

// ── HTML → markdown ──────────────────────────────────────────────────────

const SKIP = new Set(["SCRIPT", "STYLE", "META", "LINK", "HEAD", "TITLE", "NOSCRIPT", "TEMPLATE", "BUTTON", "SELECT", "TEXTAREA", "IFRAME", "OBJECT", "EMBED", "CANVAS", "SVG", "MATH", "VIDEO", "AUDIO"]);
const BLOCK = new Set(["P", "DIV", "SECTION", "ARTICLE", "MAIN", "HEADER", "FOOTER", "ASIDE", "NAV", "FIGURE", "FIGCAPTION", "DETAILS", "SUMMARY", "ADDRESS", "CENTER", "FORM", "FIELDSET", "DL", "DT", "DD", "H1", "H2", "H3", "H4", "H5", "H6", "UL", "OL", "LI", "PRE", "BLOCKQUOTE", "TABLE", "HR", "BODY"]);

const BLOCK_SELECTOR = [...BLOCK].filter((t) => t !== "BODY").map((t) => t.toLowerCase()).join(",");

interface Style {
  bold: boolean;
  italic: boolean;
  strike: boolean;
  code: boolean;
}

/** A picture in pasted HTML that must go to the server first (`data:`), by placeholder. */
export interface PastedPicture {
  token: string;
  dataUrl: string;
  alt: string;
}

const MONO = /\b(mono|courier|consolas|menlo|monaco|source code|fira code|roboto mono|jetbrains)/i;

function styleOf(el: HTMLElement, inherited: Style): Style {
  const s = { ...inherited };
  const tag = el.tagName;
  const css = el.getAttribute("style") ?? "";
  const weight = /font-weight\s*:\s*(\w+)/i.exec(css)?.[1];
  if (tag === "B" || tag === "STRONG") s.bold = !(weight && (weight === "normal" || Number(weight) < 600));
  else if (weight) s.bold = weight === "bold" || weight === "bolder" || Number(weight) >= 600;
  if (tag === "I" || tag === "EM") s.italic = !/font-style\s*:\s*normal/i.test(css);
  else if (/font-style\s*:\s*italic/i.test(css)) s.italic = true;
  else if (/font-style\s*:\s*normal/i.test(css)) s.italic = false;
  if (tag === "S" || tag === "DEL" || tag === "STRIKE" || /text-decoration[^;]*line-through/i.test(css)) s.strike = true;
  if (tag === "CODE" || tag === "KBD" || tag === "SAMP" || tag === "TT" || MONO.test(/font-family\s*:([^;]+)/i.exec(css)?.[1] ?? "")) s.code = true;
  return s;
}

/** Text that must not turn into markdown syntax by accident. */
function escapeText(t: string): string {
  return t.replace(/([\\`*[\]])/g, "\\$1").replace(/(^|[^\w])_|_(?=[^\w]|$)/g, (m) => m.replace("_", "\\_")).replace(/<(?=[a-zA-Z/!])/g, "\\<").replace(/~~/g, "\\~\\~").replace(/==/g, "\\=\\=");
}

/** Escapes for the start of a block (it would be read as a heading, list, quote…). */
function escapeStart(t: string): string {
  return t.replace(/^(\s*)([#>+-]|\d+[.)])(?=\s)/, "$1\\$2").replace(/^(\s*)(-{3,}|={3,})\s*$/, "$1\\$2");
}

function wrap(text: string, mark: string): string {
  const m = /^(\s*)([\s\S]*?)(\s*)$/.exec(text)!;
  return m[2] ? `${m[1]}${mark}${m[2]}${mark}${m[3]}` : text;
}

function codeSpan(text: string): string {
  const longest = Math.max(0, ...(text.match(/`+/g) ?? []).map((r) => r.length));
  const fence = "`".repeat(longest + 1);
  return longest ? `${fence} ${text} ${fence}` : `${fence}${text}${fence}`;
}

const safeHref = (h: string | null) => (h && /^(https?:|mailto:)/i.test(h.trim()) ? h.trim().replace(/[()\s]/g, (c) => encodeURIComponent(c)) : null);

class Converter {
  pictures: PastedPicture[] = [];

  /** The inline markdown of a node's content. */
  inline(node: Node, st: Style): string {
    if (node.nodeType === Node.TEXT_NODE) {
      const raw = (node as Text).data.replace(/[\s\u00a0]+/g, " ");
      if (!raw) return "";
      if (st.code) return raw;
      return escapeText(raw);
    }
    if (node.nodeType !== Node.ELEMENT_NODE) return "";
    const el = node as HTMLElement;
    const tag = el.tagName;
    if (SKIP.has(tag)) return "";
    if (tag === "BR") return "\n";
    if (tag === "IMG") return this.img(el as HTMLImageElement);
    if (tag === "INPUT") return "";
    const s = styleOf(el, st);
    let inner = "";
    for (const c of Array.from(el.childNodes)) inner += this.inline(c, s);
    if (tag === "A") {
      const href = safeHref(el.getAttribute("href"));
      const text = inner.trim();
      if (!href || !text) return inner;
      if (text === href || text === escapeText(href)) return href;
      return `[${text.replace(/\n/g, " ")}](${href})`;
    }
    // Styles this element starts (not the ones it inherits).
    if (s.code && !st.code) return inner.trim() ? codeSpan(inner.replace(/\n/g, " ")) : inner;
    if (s.code) return inner;
    let out = inner;
    if (s.strike && !st.strike) out = wrap(out, "~~");
    if (s.italic && !st.italic) out = wrap(out, "*");
    if (s.bold && !st.bold) out = wrap(out, "**");
    return out;
  }

  img(el: HTMLImageElement): string {
    const src = el.getAttribute("src") ?? "";
    const alt = (el.getAttribute("alt") ?? "").replace(/[[\]\n]+/g, " ").trim();
    if (/^data:image\/(png|jpe?g|gif|webp);base64,/i.test(src)) {
      const token = `uploading-${Math.random().toString(36).slice(2, 10)}`;
      this.pictures.push({ token, dataUrl: src, alt });
      return `![${alt}](${token})`;
    }
    if (/^https?:\/\//i.test(src)) return `![${alt}](${src.replace(/[()\s]/g, (c) => encodeURIComponent(c))})`;
    return "";
  }

  /** Blocks (each one paragraph, heading, list, table…) of a container. */
  blocks(node: Node, depth = 0): string[] {
    const out: string[] = [];
    let line = "";
    const flush = () => {
      const t = line
        .replace(/[ \t]+\n/g, "\n")
        .replace(/\n[ \t]+/g, "\n")
        .replace(/^\s+|\s+$/g, "");
      if (t) out.push(escapeStart(t));
      line = "";
    };
    const base: Style = { bold: false, italic: false, strike: false, code: false };
    for (const c of Array.from(node.childNodes)) {
      if (c.nodeType === Node.ELEMENT_NODE && BLOCK.has((c as Element).tagName)) {
        flush();
        out.push(...this.block(c as HTMLElement, depth));
      } else if (c.nodeType === Node.ELEMENT_NODE && (c as Element).querySelector(BLOCK_SELECTOR)) {
        // An inline wrapper around blocks (Google Docs wraps everything in a <b>): its blocks.
        flush();
        out.push(...this.blocks(c, depth));
      } else line += this.inline(c, node.nodeType === Node.ELEMENT_NODE ? styleOf(node as HTMLElement, base) : base);
    }
    flush();
    return out;
  }

  block(el: HTMLElement, depth: number): string[] {
    const tag = el.tagName;
    const base: Style = { bold: false, italic: false, strike: false, code: false };
    if (/^H[1-6]$/.test(tag)) {
      const text = this.inline(el, { ...base, bold: true }).replace(/\s+/g, " ").trim();
      return text ? [`${"#".repeat(Number(tag[1]))} ${text}`] : [];
    }
    if (tag === "HR") return ["---"];
    if (tag === "PRE") {
      const code = el.textContent ?? "";
      const cls = `${el.className} ${el.querySelector("code")?.className ?? ""}`;
      const lang = /(?:language|lang)-([\w+#.-]+)/.exec(cls)?.[1] ?? "";
      const longest = Math.max(2, ...(code.match(/`+/g) ?? []).map((r) => r.length));
      const fence = "`".repeat(longest + 1);
      return [`${fence}${lang}\n${code.replace(/\n$/, "")}\n${fence}`];
    }
    if (tag === "BLOCKQUOTE") return [this.blocks(el, depth).map((b) => b.replace(/^/gm, "> ")).join("\n>\n")];
    if (tag === "UL" || tag === "OL") return [this.list(el, depth)];
    if (tag === "TABLE") return [this.table(el)];
    if (tag === "LI") return [this.list(el.parentElement ?? el, depth)];
    // Google Docs' and others' wrappers: what's in them.
    return this.blocks(el, depth);
  }

  list(listEl: HTMLElement, depth: number): string {
    const ordered = listEl.tagName === "OL";
    let n = Number(listEl.getAttribute("start") ?? 1) || 1;
    const lines: string[] = [];
    for (const li of Array.from(listEl.children)) {
      if (li.tagName === "UL" || li.tagName === "OL") {
        // A nested list straight inside a list (Google Docs does this).
        lines.push(this.list(li as HTMLElement, depth + 1));
        continue;
      }
      if (li.tagName !== "LI") continue;
      const level = Number(li.getAttribute("aria-level") ?? 0);
      const d = level > 0 ? level - 1 : depth;
      const box = li.querySelector(":scope > input[type=checkbox], :scope > p > input[type=checkbox]") as HTMLInputElement | null;
      const marker = ordered ? `${n++}.` : "-";
      const task = box ? (box.checked || box.hasAttribute("checked") ? " [x]" : " [ ]") : "";
      const indent = "  ".repeat(d) + (ordered ? " ".repeat(Math.max(0, d)) : "");
      const head = `${indent}${marker}${task} `;
      const parts: string[] = [];
      const nested: string[] = [];
      for (const c of Array.from(li.childNodes)) {
        if (c.nodeType === Node.ELEMENT_NODE && ((c as Element).tagName === "UL" || (c as Element).tagName === "OL")) nested.push(this.list(c as HTMLElement, d + 1));
        else if (c.nodeType === Node.ELEMENT_NODE && BLOCK.has((c as Element).tagName)) parts.push(...this.blocks(c.parentElement === li ? wrapOne(c) : c, d + 1));
        else parts.push(this.inline(c, { bold: false, italic: false, strike: false, code: false }));
      }
      const text = parts
        .join(" ")
        .replace(/[ \t]*\n[ \t]*/g, `\n${" ".repeat(head.length)}`)
        .replace(/ {2,}/g, " ")
        .trim();
      lines.push(head + text);
      lines.push(...nested);
    }
    return lines.filter((l) => l.trim()).join("\n");
  }

  table(t: HTMLElement): string {
    const rows = Array.from(t.querySelectorAll("tr")).filter((r) => r.closest("table") === t);
    if (!rows.length) return "";
    const grid = rows.map((r) =>
      Array.from(r.children)
        .filter((c) => c.tagName === "TD" || c.tagName === "TH")
        .map((c) =>
          this.blocks(c)
            .join("<br>")
            .replace(/\n/g, "<br>")
            .replace(/\|/g, "\\|")
            .trim(),
        ),
    );
    const cols = Math.max(...grid.map((r) => r.length));
    const row = (cells: string[]) => `| ${Array.from({ length: cols }, (_, i) => cells[i] ?? "").join(" | ")} |`;
    return [row(grid[0]), `|${" --- |".repeat(cols)}`, ...grid.slice(1).map(row)].join("\n");
  }
}

/** One node in a fragment of its own (a block inside a list item). */
function wrapOne(n: Node): Node {
  const d = document.createElement("div");
  d.append(n.cloneNode(true));
  return d;
}

/** Rich text as markdown, and the pictures in it that need uploading. */
export function htmlToMarkdown(html: string): { md: string; pictures: PastedPicture[] } {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const c = new Converter();
  const md = c
    .blocks(doc.body)
    .join("\n\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  return { md, pictures: c.pictures };
}

// ── putting things in ────────────────────────────────────────────────────

/** Pictures being uploaded, by their placeholder: what to show meanwhile. */
export const pending = new Map<string, string>();

/** Put pictures in at `at`: a placeholder at once (the picture itself, faded), the real one once it's on the server. */
export function addPictures(view: EditorView, files: File[], at: number) {
  const pics = files.filter((f) => f.type.startsWith("image/"));
  if (!pics.length) return;
  const items = pics.map((f) => ({ f, token: `uploading-${Math.random().toString(36).slice(2, 10)}`, alt: altFor(f, f.name ? "Picture" : "Pasted picture") }));
  for (const it of items) pending.set(it.token, URL.createObjectURL(it.f));
  const md = items.map((it) => `![${it.alt}](${it.token})`).join("\n\n");
  // Never in the middle of a word: after the line the caret is in (before it, at its start).
  const line = view.state.doc.lineAt(at);
  if (at > line.from && at < line.to) at = line.to;
  placeBlock(view, md, at);
  for (const it of items) void finishUpload(view, it.token, () => uploadPicture(it.f));
}

/** Text that becomes its own block(s) at `at`, kept apart from the text around it. */
function placeBlock(view: EditorView, md: string, at: number) {
  const { state } = view;
  const line = state.doc.lineAt(at);
  const before = at > line.from ? "\n\n" : line.number > 1 && state.doc.line(line.number - 1).text.trim() ? "\n" : "";
  // A blank line after it, unless the text after already starts with one.
  const next = line.number < state.doc.lines ? state.doc.line(line.number + 1) : null;
  const after = at < line.to ? "\n\n" : next && !next.text.trim() ? "" : "\n";
  const insert = before + md + after;
  view.dispatch({ changes: { from: at, insert }, selection: EditorSelection.cursor(at + insert.length), scrollIntoView: true, userEvent: "input.paste" });
}

async function finishUpload(view: EditorView, token: string, upload: () => Promise<{ name: string }>) {
  let name: string | null = null;
  try {
    name = (await upload()).name;
  } catch (e) {
    toastError(e, "The picture wasn't added");
  }
  const local = pending.get(token);
  pending.delete(token);
  if (local) setTimeout(() => URL.revokeObjectURL(local), 5000);
  if (!view.dom.isConnected) return;
  const text = view.state.doc.toString();
  const i = text.indexOf(`(${token})`);
  if (i < 0) return;
  if (name) {
    view.dispatch({ changes: { from: i + 1, to: i + 1 + token.length, insert: `assets/${name}` }, userEvent: "input.picture", annotations: [] });
  } else {
    // Gone again, with the blank line it came with.
    const start = text.lastIndexOf("![", i);
    let end = i + token.length + 2;
    if (text.slice(end, end + 2) === "\n\n") end += 2;
    else if (text[end] === "\n") end += 1;
    view.dispatch({ changes: { from: start, to: end }, userEvent: "delete.picture" });
  }
}

/** A data: URL as a file. */
function dataFile(url: string, name: string): File {
  const [head, data] = url.split(",", 2);
  const mime = /data:([^;]+)/.exec(head)?.[1] ?? "image/png";
  const bin = atob(data);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return new File([bytes], name, { type: mime });
}

const inCode = (view: EditorView, pos: number) => {
  for (let n: import("@lezer/common").SyntaxNode | null = syntaxTree(view.state).resolveInner(pos, -1); n; n = n.parent) {
    if (n.name === "FencedCode" || n.name === "CodeBlock" || n.name === "InlineCode") return true;
  }
  return false;
};

const plainish = (s: string) => s.replace(/\\([\\`*_[\]<~=#>+-])/g, "$1").replace(/\s+/g, " ").trim();

/** Paste rich text as markdown (else let plain text in as it is). */
function pasteRich(view: EditorView, html: string, plain: string): boolean {
  const { md, pictures } = htmlToMarkdown(html);
  if (!md.trim() || (!pictures.length && plainish(md) === plainish(plain))) return false;
  const r = view.state.selection.main;
  const blocky = /\n\n|^(#{1,6} |[-*+] |\d+[.)] |> |\||```|---)/m.test(md) || pictures.length > 0;
  if (blocky) {
    // Blocks of their own, apart from the text around (a list or table right under a line would join it).
    if (!r.empty) view.dispatch({ changes: { from: r.from, to: r.to }, userEvent: "input.paste" });
    placeBlock(view, md, r.from);
  } else view.dispatch({ changes: { from: r.from, to: r.to, insert: md }, selection: EditorSelection.cursor(r.from + md.length), scrollIntoView: true, userEvent: "input.paste" });
  for (const p of pictures) {
    const file = dataFile(p.dataUrl, p.alt || "Pasted picture");
    pending.set(p.token, URL.createObjectURL(file));
    void finishUpload(view, p.token, () => uploadPicture(file));
  }
  return true;
}

const isPicture = (f: File) => f.type.startsWith("image/");

/** Pictures in at the caret, over what's selected. */
function picturesAtCaret(view: EditorView, files: File[]) {
  const r = view.state.selection.main;
  if (!r.empty) view.dispatch({ changes: { from: r.from, to: r.to }, userEvent: "input.paste" });
  view.state.facet(noteHooks).addPictures(files, view.state.selection.main.from);
}

/**
 * Text from a paste: a web address over selected words links them, rich text
 * becomes markdown. False when it should go in as it is (the editor's own
 * paste does that, unless `byHand`: then it is put in here).
 */
function pasteText(view: EditorView, plain: string, html: string, types: readonly string[], byHand: boolean): boolean {
  const r = view.state.selection.main;
  const asIs = () => {
    if (!byHand || !plain) return byHand;
    view.dispatch(view.state.replaceSelection(plain), { scrollIntoView: true, userEvent: "input.paste" });
    return true;
  };
  if (inCode(view, r.from)) return asIs();
  // A web address over selected words: a link.
  if (!r.empty && /^https?:\/\/\S+$/.test(plain.trim()) && !view.state.sliceDoc(r.from, r.to).includes("\n")) {
    const text = view.state.sliceDoc(r.from, r.to);
    const insert = `[${text}](${plain.trim()})`;
    view.dispatch({ changes: { from: r.from, to: r.to, insert }, selection: EditorSelection.cursor(r.from + insert.length), userEvent: "input.paste" });
    return true;
  }
  // Code editors put coloured HTML on the clipboard too: their plain text is what's meant.
  if (!html || types.includes("vscode-editor-data")) return asIs();
  return pasteRich(view, html, plain) || asIs();
}

/**
 * Pasting and dropping, the same way the chat's message box does it on every
 * platform (lib/clipboard.ts): pictures carried as files or only as file
 * items; in the Linux desktop app, whose web view hands the page no picture
 * at all (or only its markup or link), the one the app reads from the system
 * clipboard, and files dropped from a file manager. Android keyboards'
 * pictures come as an event of their own (see NoteEditor).
 */
export const pasting: Extension = EditorView.domEventHandlers({
  paste(e, view) {
    if (view.state.readOnly) return false;
    const cd = e.clipboardData;
    const files = transferFiles(cd).filter(isPicture);
    if (files.length) {
      e.preventDefault();
      picturesAtCaret(view, files);
      return true;
    }
    const plain = cd?.getData("text/plain") ?? "";
    const html = cd?.getData("text/html") ?? "";
    const types = Array.from(cd?.types ?? []);
    if (pasteMayHide(cd, plain)) {
      // Asked while the paste is handled (that's when the app may read the clipboard); the text goes in after, if there was no picture.
      e.preventDefault();
      void clipboardFiles().then((found) => {
        if (!view.dom.isConnected || view.state.readOnly) return;
        const pics = (found ?? []).filter(isPicture);
        if (pics.length) return picturesAtCaret(view, pics);
        if (found?.length) toast({ level: "info", message: "Only pictures go into a note; the file's name was pasted instead." });
        pasteText(view, plain, html, types, true);
      });
      return true;
    }
    if (!cd) return false;
    if (pasteText(view, plain, html, types, false)) {
      e.preventDefault();
      return true;
    }
    return false;
  },
  dragover(e, view) {
    // WebKitGTK gives a file manager's files only as links: the drop must be taken here to get them.
    if (view.state.readOnly || !dropMayHide(e.dataTransfer)) return false;
    e.preventDefault();
    return false;
  },
  drop(e, view) {
    if (view.state.readOnly) return false;
    const files = transferFiles(e.dataTransfer).filter(isPicture);
    const at = view.posAtCoords({ x: e.clientX, y: e.clientY }) ?? view.state.selection.main.head;
    if (files.length) {
      e.preventDefault();
      view.state.facet(noteHooks).addPictures(files, at);
      return true;
    }
    if (!dropMayHide(e.dataTransfer)) return false;
    e.preventDefault();
    void droppedFiles().then((found) => {
      const pics = (found ?? []).filter(isPicture);
      if (!view.dom.isConnected || view.state.readOnly) return;
      if (pics.length) view.state.facet(noteHooks).addPictures(pics, Math.min(at, view.state.doc.length));
      else if (found?.length) toast({ level: "info", message: "Only pictures go into a note." });
    });
    return true;
  },
});
