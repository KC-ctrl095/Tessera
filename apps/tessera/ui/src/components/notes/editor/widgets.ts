// What the page draws in place of markdown syntax: bullets, numbers, task
// boxes, dividers, code block heads, callout icons and pictures.
import { StateEffect } from "@codemirror/state";
import { EditorView, WidgetType } from "@codemirror/view";
import { assetName, assetUrl } from "../../../lib/noteAssets";
import { copyText } from "../../../lib/util";
import { noteHooks, type Picture } from "./hooks";
import { pending } from "./paste";
import { pictureGestures } from "./pictures";

const svg = (body: string, size = 14) =>
  `<svg viewBox="0 0 24 24" width="${size}" height="${size}" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${body}</svg>`;

/** A list item's marker: a bullet (by depth), its number, or a task box. */
export class MarkWidget extends WidgetType {
  constructor(
    readonly kind: "bullet" | "ordered" | "task",
    readonly text: string,
    readonly checked: boolean,
    readonly depth: number,
  ) {
    super();
  }
  eq(o: MarkWidget) {
    return o.kind === this.kind && o.text === this.text && o.checked === this.checked && o.depth === this.depth;
  }
  toDOM(view: EditorView) {
    const el = document.createElement("span");
    el.className = `cm-mark cm-mark-${this.kind}`;
    if (this.kind === "bullet") {
      el.dataset.depth = String((this.depth - 1) % 3);
      el.setAttribute("aria-hidden", "true");
    } else if (this.kind === "ordered") {
      el.textContent = this.text;
    } else {
      const box = document.createElement("span");
      box.className = "cm-task";
      box.setAttribute("role", "checkbox");
      box.setAttribute("aria-checked", String(this.checked));
      box.setAttribute("aria-label", this.checked ? "Done" : "To do");
      el.append(box);
      // Ticked where it is, without moving the caret (or opening the keyboard).
      el.addEventListener("mousedown", (e) => e.preventDefault());
      el.addEventListener("click", (e) => {
        e.preventDefault();
        if (!view.state.facet(EditorView.editable)) return;
        toggleTaskAt(view, view.posAtDOM(el));
      });
    }
    return el;
  }
  ignoreEvent(e: Event) {
    return this.kind === "task" && (e.type === "mousedown" || e.type === "click" || e.type.startsWith("touch") || e.type.startsWith("pointer"));
  }
}

/** Tick or untick the task on the line at `pos`. */
export function toggleTaskAt(view: EditorView, pos: number): boolean {
  const line = view.state.doc.lineAt(pos);
  const m = /^([\s>]*(?:[-*+]|\d+[.)]) {1,4}\[)([ xX])\]/.exec(line.text);
  if (!m) return false;
  const at = line.from + m[1].length;
  view.dispatch({ changes: { from: at, to: at + 1, insert: m[2] === " " ? "x" : " " }, userEvent: "input.task" });
  return true;
}

/** A divider (`---`). */
export class RuleWidget extends WidgetType {
  eq() {
    return true;
  }
  toDOM() {
    const el = document.createElement("span");
    el.className = "cm-rule";
    el.setAttribute("aria-hidden", "true");
    return el;
  }
}

/** The opening line of a code block: its language, Copy, and (HTML) Preview. */
export class CodeHeadWidget extends WidgetType {
  constructor(
    readonly lang: string,
    readonly previewable: boolean,
    readonly previewing: boolean,
  ) {
    super();
  }
  eq(o: CodeHeadWidget) {
    return o.lang === this.lang && o.previewable === this.previewable && o.previewing === this.previewing;
  }
  toDOM(view: EditorView) {
    const el = document.createElement("span");
    el.className = "cm-code-head";
    const lang = document.createElement("span");
    lang.className = "cm-code-lang";
    lang.textContent = this.lang || "code";
    el.append(lang);
    const button = (label: string, icon: string, run: (b: HTMLButtonElement) => void, cls = "") => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = `cm-code-btn ${cls}`;
      b.innerHTML = `${icon}<span>${label}</span>`;
      b.addEventListener("mousedown", (e) => e.preventDefault());
      b.addEventListener("click", (e) => {
        e.preventDefault();
        e.stopPropagation();
        run(b);
      });
      el.append(b);
      return b;
    };
    if (this.previewable) {
      const b = button(
        this.previewing ? "Code only" : "Preview",
        this.previewing ? svg('<path d="m16 18 6-6-6-6M8 6l-6 6 6 6"/>', 13) : '<svg viewBox="0 0 24 24" width="13" height="13" fill="currentColor" aria-hidden="true"><path d="M7 5.5v13l11-6.5z"/></svg>',
        () => view.dispatch({ effects: togglePreviewEffect.of(view.posAtDOM(el)) }),
        "cm-code-run",
      );
      b.setAttribute("aria-pressed", String(this.previewing));
    }
    button("Copy", svg('<rect x="8.5" y="8.5" width="11" height="11" rx="1.5"/><path d="M15.5 8.5V6a1.5 1.5 0 0 0-1.5-1.5H6A1.5 1.5 0 0 0 4.5 6v8A1.5 1.5 0 0 0 6 15.5h2.5"/>', 13), (b) => {
      void copyText(codeAt(view, view.posAtDOM(el))).then((ok) => {
        if (!ok) return;
        b.classList.add("done");
        b.querySelector("span")!.textContent = "Copied";
        setTimeout(() => {
          b.classList.remove("done");
          b.querySelector("span")!.textContent = "Copy";
        }, 1400);
      });
    });
    return el;
  }
  ignoreEvent(e: Event) {
    return e.type === "mousedown" || e.type === "click";
  }
}

/** The code in the fenced block that starts on the line at `pos`. */
export function codeAt(view: EditorView, pos: number): string {
  const doc = view.state.doc;
  const open = doc.lineAt(pos);
  const fence = /^\s*(`{3,}|~{3,})/.exec(open.text)?.[1] ?? "```";
  const lines: string[] = [];
  for (let n = open.number + 1; n <= doc.lines; n++) {
    const t = doc.line(n).text;
    if (t.trim().startsWith(fence[0].repeat(fence.length)) && !t.trim().slice(fence.length).trim()) break;
    lines.push(t);
  }
  return lines.join("\n");
}

/** Preview (or stop previewing) the HTML code block that starts at a position. */
export const togglePreviewEffect = StateEffect.define<number>();

const CALLOUT_ICONS: Record<string, string> = {
  note: '<path d="M7 4.5h7.5l3.5 3.5v11.5H7z"/><path d="M14.5 4.5V8H18M9.5 12.5h6M9.5 16h4"/>',
  info: '<circle cx="12" cy="12" r="8"/><path d="M12 11v5M12 8v.01"/>',
  tip: '<path d="M9 18h6M10 21h4M12 3a6 6 0 0 0-3.5 10.9V16h7v-2.1A6 6 0 0 0 12 3z"/>',
  success: '<path d="M5.5 12.5l4 4L18.5 7.5"/>',
  question: '<circle cx="12" cy="12" r="8"/><path d="M9.6 9.5a2.5 2.5 0 1 1 3.4 2.3c-.6.3-1 .8-1 1.5v.5M12 17v.01"/>',
  warning: '<path d="M12 4 3.5 19h17z"/><path d="M12 10v4M12 16.5v.01"/>',
  danger: '<path d="M13 3 5 13.5h6L10 21l8-10.5h-6z"/>',
  bug: '<rect x="8" y="7" width="8" height="12" rx="4"/><path d="M12 7V4.5M8 11H4.5M19.5 11H16M8 15H4.5M19.5 15H16"/>',
  example: '<path d="M5 6.5h14M5 12h14M5 17.5h9"/>',
  quote: '<path d="M7 8.5h4v4H7c0 2 1 3 2.5 3.5M14 8.5h4v4h-4c0 2 1 3 2.5 3.5"/>',
};
const CALLOUT_ALIASES: Record<string, string> = {
  abstract: "note", summary: "note", tldr: "note", todo: "info", hint: "tip", important: "tip", check: "success", done: "success",
  help: "question", faq: "question", caution: "warning", attention: "warning", failure: "danger", fail: "danger", missing: "danger", error: "danger", cite: "quote",
};
/** A callout's look: one of the icons' names. */
export const calloutKind = (type: string) => {
  const t = type.toLowerCase();
  return CALLOUT_ICONS[t] ? t : (CALLOUT_ALIASES[t] ?? "note");
};

/** A callout's `[!type]`: its icon, and its name when it has no title of its own. */
export class CalloutWidget extends WidgetType {
  constructor(
    readonly type: string,
    readonly label: boolean,
  ) {
    super();
  }
  eq(o: CalloutWidget) {
    return o.type === this.type && o.label === this.label;
  }
  toDOM() {
    const el = document.createElement("span");
    el.className = "cm-callout-icon";
    el.innerHTML = svg(CALLOUT_ICONS[calloutKind(this.type)], 15);
    if (this.label) {
      const t = document.createElement("span");
      t.className = "cm-callout-label";
      t.textContent = this.type.charAt(0).toUpperCase() + this.type.slice(1).toLowerCase();
      el.append(t);
    }
    return el;
  }
}

/** Known sizes of pictures (so a picture drawn again takes its room at once). */
const sizes = new Map<string, [number, number]>();
/** The widget a picture's element shows now (it is updated in place). */
const shown = new WeakMap<HTMLElement, PictureWidget>();

/** A picture: `![alt](assets/…)` from the server, a data: URL, or (not loaded) one from the web. */
export class PictureWidget extends WidgetType {
  constructor(
    readonly p: Picture,
    readonly block: boolean,
    readonly selected: boolean,
  ) {
    super();
  }
  eq(o: PictureWidget) {
    return o.p.src === this.p.src && o.p.alt === this.p.alt && o.p.width === this.p.width && o.block === this.block && o.selected === this.selected;
  }
  updateDOM(dom: HTMLElement, _view: EditorView, from: PictureWidget) {
    // Only the selection changed: keep the picture (no flicker).
    if (from.p.src !== this.p.src) return false;
    shown.set(dom, this);
    dom.classList.toggle("selected", this.selected);
    dom.classList.toggle("block", this.block);
    const img = dom.querySelector("img");
    if (img) img.alt = this.p.alt;
    this.size(dom);
    return true;
  }
  private size(dom: HTMLElement) {
    dom.style.width = this.p.width ? `${this.p.width}px` : "";
    dom.classList.toggle("sized", !!this.p.width);
  }
  get estimatedHeight() {
    const s = sizes.get(this.p.src);
    return s ? Math.min(480, s[1]) : this.block ? 200 : -1;
  }
  toDOM(view: EditorView) {
    const el = document.createElement("span");
    shown.set(el, this);
    el.className = "cm-pic";
    el.classList.toggle("block", this.block);
    el.classList.toggle("selected", this.selected);
    this.size(el);
    const src = this.p.src.trim();
    const name = assetName(src);
    /** When a resize or a drag last ended (its click isn't a click on the picture). */
    let gestured = 0;
    const handle = pictureGestures(
      el,
      view,
      () => {
        const w = shown.get(el) ?? this;
        const from = view.posAtDOM(el);
        return { from, to: from + (w.p.to - w.p.from), alt: w.p.alt, width: w.p.width };
      },
      () => (gestured = Date.now()),
    );
    const put = (...nodes: Node[]) => el.replaceChildren(...nodes, handle);
    const show = (url: string) => {
      const img = document.createElement("img");
      img.alt = this.p.alt;
      img.draggable = false;
      img.decoding = "async";
      const known = sizes.get(src);
      if (known) img.style.aspectRatio = `${known[0]} / ${known[1]}`;
      img.onload = () => {
        sizes.set(src, [img.naturalWidth, img.naturalHeight]);
        img.style.aspectRatio = "";
        view.requestMeasure();
      };
      img.onerror = () => missing("This picture can't be shown.");
      img.src = url;
      put(img);
    };
    const missing = (why: string, link?: string) => {
      const box = document.createElement("span");
      box.className = "cm-pic-missing";
      box.textContent = this.p.alt ? `${this.p.alt} · ${why}` : why;
      if (link) box.title = link;
      put(box);
    };
    const local = src.startsWith("uploading-") ? pending.get(src) : undefined;
    if (src.startsWith("uploading-")) {
      // On its way to the server: the picture itself, faded, meanwhile.
      el.classList.add("uploading");
      if (local) show(local);
      else missing("Adding the picture…");
      const note = document.createElement("span");
      note.className = "cm-pic-note";
      note.textContent = "Adding…";
      el.insertBefore(note, handle);
    } else if (name) {
      const ph = document.createElement("span");
      ph.className = "cm-pic-wait";
      const known = sizes.get(src);
      if (known) ph.style.aspectRatio = `${known[0]} / ${known[1]}`;
      put(ph);
      assetUrl(name).then(show, () => missing("This picture is no longer on the server."));
    } else if (/^data:image\/(png|jpe?g|gif|webp);base64,/i.test(src)) show(src);
    else if (/^https?:\/\//i.test(src)) {
      // Pictures from the web aren't fetched (that would tell the site you're reading this note).
      let host = src;
      try {
        host = new URL(src).host;
      } catch {
        /* as it is */
      }
      missing(`a picture from ${host}, not loaded`, src);
    } else missing("This picture isn't in Tessera.");
    el.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
    });
    el.addEventListener("click", (e) => {
      e.preventDefault();
      if (Date.now() - gestured < 400) return;
      const w = shown.get(el) ?? this;
      const from = view.posAtDOM(el);
      const p = { ...w.p, from, to: from + (w.p.to - w.p.from) };
      view.dispatch({ selection: { anchor: p.from, head: p.to } });
      view.state.facet(noteHooks).pictureMenu(el, p);
    });
    return el;
  }
  ignoreEvent(e: Event) {
    return e.type === "mousedown" || e.type === "click";
  }
}

/** The alt text and asked-for width of `alt|300`. */
export function parseAlt(raw: string): { alt: string; width: number | null } {
  const m = /^(.*?)\|\s*(\d{2,4})(?:x\d{2,4})?\s*$/.exec(raw);
  return m ? { alt: m[1].trim(), width: Number(m[2]) } : { alt: raw.trim(), width: null };
}
