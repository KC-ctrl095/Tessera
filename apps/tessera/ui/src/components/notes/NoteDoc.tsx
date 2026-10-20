// A note's page: its markdown shown rendered, and edited where you tap. The
// tapped block (a paragraph, a list, a table, a code block…) turns into its
// markdown source, with the caret where you tapped; everything else stays
// rendered and untouched, so editing is one tap and the page never jumps to
// a wall of source. Below the last block a tap starts a new one.
//
// Rendered blocks get a little more than chat markdown: task boxes you can
// tick, and HTML code that previews in a sandboxed frame (lib/notes.ts
// previewSrc). Read-only notes do neither (except previews).
import { Index, Show, batch, createEffect, createMemo, createResource, createSignal, on, onCleanup, untrack, type JSX } from "solid-js";
import { Marked } from "marked";
import { enhance, renderMarkdownFragment } from "../../lib/markdown";
import { previewSrc } from "../../lib/notes";
import { resolvedTheme } from "../../lib/prefs";
import { copyText, openExternal, touchUi } from "../../lib/util";
import { noteKey } from "../../lib/noteSync";
import { diffText, offsetMap } from "../../lib/textDiff";
import { Icon, Tessellate } from "../icons";

// ── blocks ───────────────────────────────────────────────────────────────

type Block = { start: number; end: number; kind: string; lang: string };

const lexer = new Marked({ gfm: true });

/** The top-level blocks of `text` (blank lines between them left out), with their offsets. */
export function splitBlocks(text: string): Block[] {
  if (!text.trim()) return [];
  // marked reads "\r\n" as "\n": lex that, then map offsets back.
  let src = text;
  let back: ((i: number) => number) | null = null;
  if (text.includes("\r")) {
    const map: number[] = [];
    let out = "";
    for (let i = 0; i < text.length; i++) {
      if (text[i] === "\r") {
        if (text[i + 1] === "\n") continue;
        map.push(i);
        out += "\n";
        continue;
      }
      map.push(i);
      out += text[i];
    }
    map.push(text.length);
    src = out;
    back = (i) => map[i] ?? text.length;
  }
  const out: Block[] = [];
  let pos = 0;
  try {
    for (const t of lexer.lexer(src)) {
      const len = t.raw.length;
      if (t.type !== "space" && t.raw.trim()) {
        // A block's own text, without the line breaks after it.
        const own = t.raw.replace(/\s+$/, "").length;
        const s = back ? back(pos) : pos;
        const e = back ? back(pos + own) : pos + own;
        out.push({ start: s, end: e, kind: t.type, lang: t.type === "code" ? String((t as { lang?: string }).lang ?? "").toLowerCase() : "" });
      }
      pos += len;
    }
  } catch {
    return [{ start: 0, end: text.length, kind: "paragraph", lang: "" }];
  }
  return out;
}

/** Text areas that grow with their text by themselves (Chromium; not WebKitGTK yet). */
const SIZES_ITSELF = typeof CSS !== "undefined" && CSS.supports("field-sizing", "content");

const isHtmlCode = (b: Pick<Block, "kind" | "lang">) => b.kind === "code" && /^(html?|xhtml)$/.test(b.lang);
const TASK = /^(\s*(?:[-*+]|\d+[.)])\s+\[)([ xX])(\])/gm;

// Rendering the same markdown again (a block scrolled back, editing ends) reuses it.
const cache = new Map<string, DocumentFragment>();
function rendered(raw: string): DocumentFragment {
  let f = cache.get(raw);
  if (!f) {
    f = renderMarkdownFragment(raw);
    cache.set(raw, f);
    if (cache.size > 400) cache.delete(cache.keys().next().value!);
  }
  return f.cloneNode(true) as DocumentFragment;
}

/** The code in an HTML code block's source. */
const codeOf = (raw: string) => raw.replace(/^\s*(`{3,}|~{3,})[^\n]*\n?/, "").replace(/\n?\s*(`{3,}|~{3,})\s*$/, "");

// ── caret: from a tap on rendered text to the markdown under it ──────────

function caretAt(x: number, y: number): { node: Node; offset: number } | null {
  const d = document as Document & {
    caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null;
    caretRangeFromPoint?: (x: number, y: number) => Range | null;
  };
  if (d.caretPositionFromPoint) {
    const p = d.caretPositionFromPoint(x, y);
    return p ? { node: p.offsetNode, offset: p.offset } : null;
  }
  const r = d.caretRangeFromPoint?.(x, y);
  return r ? { node: r.startContainer, offset: r.startOffset } : null;
}

/**
 * Where in `raw` (a block's markdown) the caret at `hit` in its rendering
 * (`root`) belongs: each text node is found in the source in order (most are
 * there word for word), else its characters are matched one by one.
 */
function sourceOffset(root: HTMLElement, raw: string, hit: { node: Node; offset: number } | null): number {
  if (!hit || !root.contains(hit.node)) return raw.length;
  const walk = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => ((n.parentElement?.closest(".code-head, .nb-task, .nb-run") ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT)),
  });
  let r = 0;
  for (let n = walk.nextNode(); n; n = walk.nextNode()) {
    const data = (n as Text).data;
    const target = n === hit.node ? hit.offset : data.length;
    const at = data.trim() ? raw.indexOf(data, r) : -1;
    if (at >= 0 && at - r < 400) {
      if (n === hit.node) return at + target;
      r = at + data.length;
      continue;
    }
    for (let i = 0; i < target; i++) {
      const ch = data[i];
      if (/\s/.test(ch)) continue;
      const k = raw.indexOf(ch, r);
      if (k >= 0 && k - r < 24) r = k + 1;
    }
    if (n === hit.node) return r;
  }
  return Math.min(r, raw.length);
}

// ── lists: Enter carries the bullet on ───────────────────────────────────

const LIST_LINE = /^(\s*)([-*+]|(\d+)([.)]))(\s+)(\[[ xX]\]\s+)?/;

/** Enter at the end of a list line: the next line starts with the same bullet (an empty item ends the list). */
function listEnter(ta: HTMLTextAreaElement): boolean {
  const { value, selectionStart: s, selectionEnd: e } = ta;
  if (s !== e) return false;
  const lineStart = value.lastIndexOf("\n", s - 1) + 1;
  const lineEnd = value.indexOf("\n", s) < 0 ? value.length : value.indexOf("\n", s);
  if (s !== lineEnd) return false;
  const line = value.slice(lineStart, lineEnd);
  const m = LIST_LINE.exec(line);
  if (!m) return false;
  if (line.slice(m[0].length).trim() === "") {
    // An empty item: end the list here.
    ta.setRangeText("", lineStart, lineEnd, "end");
    return true;
  }
  const bullet = m[3] ? `${Number(m[3]) + 1}${m[4]}` : m[2];
  ta.setRangeText(`\n${m[1]}${bullet}${m[5]}${m[6] ? "[ ] " : ""}`, s, s, "end");
  return true;
}

// ── the page ─────────────────────────────────────────────────────────────

export interface NoteDocHandle {
  /** Start writing at the end (a new block). */
  writeAtEnd: () => void;
  stopEditing: () => void;
  editing: () => boolean;
}

/**
 * The block being edited: [start, end) of the text. `sep`: a new block at the
 * end gets its blank line with its first character. `preview`: an HTML block
 * that was previewed (its preview comes back with its new code).
 */
type Editing = { start: number; end: number; sep: string; kind: string; lang: string; preview: boolean };

export function NoteDoc(props: {
  text: string;
  onChange: (text: string) => void;
  readOnly: boolean;
  /** The tile is on screen (previews pause off screen). */
  visible: () => boolean;
  onSave?: () => void;
  handle?: (h: NoteDocHandle) => void;
}) {
  let root!: HTMLDivElement;
  let ta: HTMLTextAreaElement | undefined;
  const [editing, setEditing] = createSignal<Editing | null>(null);
  /** HTML code blocks being previewed, by their code. */
  const [previewing, setPreviewing] = createSignal<ReadonlySet<number>>(new Set());
  /** The text as this page last wrote it (anything else came from outside). */
  let mine = props.text;

  const emit = (text: string) => {
    mine = text;
    props.onChange(text);
  };

  // Rendered around the block being edited: each part is lexed once per edit, not per keystroke.
  const before = createMemo(() => {
    const ed = editing();
    return ed ? props.text.slice(0, ed.start) : props.text;
  });
  const after = createMemo(() => {
    const ed = editing();
    return ed ? props.text.slice(ed.end) : "";
  });
  const beforeBlocks = createMemo(() => splitBlocks(before()));
  const afterBlocks = createMemo(() => splitBlocks(after()));
  const editedText = () => {
    const ed = editing();
    return ed ? props.text.slice(ed.start, ed.end) : "";
  };

  // The block's text area grows with its text: by itself where the browser can (field-sizing),
  // else measured once per frame (measuring forces a layout of the whole note).
  let fitRaf = 0;
  const fit = () => {
    if (!ta || SIZES_ITSELF || fitRaf) return;
    fitRaf = requestAnimationFrame(() => {
      fitRaf = 0;
      if (!ta) return;
      ta.style.height = "auto";
      ta.style.height = `${ta.scrollHeight + 2}px`;
    });
  };
  onCleanup(() => cancelAnimationFrame(fitRaf));

  /**
   * Where a rendered block is in the text. Blocks carry offsets within their part (before or
   * after the block being edited), so typing never has to touch them.
   */
  const rangeOf = (el: HTMLElement): [number, number] => {
    const base = el.dataset.part === "after" ? (untrack(editing)?.end ?? 0) : 0;
    return [base + Number(el.dataset.s), base + Number(el.dataset.e)];
  };

  /** Edit [start, end) of the text, caret at `caret` (an offset in the text); `from`: the block tapped. */
  const startEditing = (start: number, end: number, caret: number, kind = "paragraph", lang = "", sep = "", from?: HTMLElement) => {
    if (props.readOnly) return;
    // Keep the spot under the finger where it is.
    const top0 = from?.getBoundingClientRect().top;
    const was = untrack(editing);
    const raw = props.text.slice(start, end);
    const preview = isHtmlCode({ kind, lang }) && untrack(previewing).has(noteKey("", codeOf(raw)));
    if (was) carryPreview(was);
    setEditing({ start, end, sep, kind, lang, preview });
    if (!ta) return;
    ta.value = props.text.slice(start, end);
    fit();
    const at = Math.max(0, Math.min(ta.value.length, caret - start));
    ta.focus({ preventScroll: true });
    ta.setSelectionRange(at, at);
    if (top0 !== undefined) {
      const top1 = ta.getBoundingClientRect().top;
      root.scrollTop += top1 - top0;
    } else if (!was) ta.scrollIntoView({ block: "nearest" });
  };

  /** A previewed HTML block, edited: its new code is previewed too. */
  const carryPreview = (ed: Editing) => {
    if (!ed.preview) return;
    const raw = props.text.slice(ed.start, ed.end);
    const keys = new Set(untrack(previewing));
    for (const b of splitBlocks(raw)) if (isHtmlCode(b)) keys.add(noteKey("", codeOf(raw.slice(b.start, b.end))));
    setPreviewing(keys);
  };

  /** Done with the block: it renders again. */
  const stopEditing = () => {
    const ed = untrack(editing);
    if (!ed) return;
    carryPreview(ed);
    setEditing(null);
  };

  // Text that changed elsewhere (combined with another device's or an agent's change) while a
  // block is open: the block, the caret and the selection move with the text around them, and
  // what changed inside the block shows in it.
  createEffect(
    on(
      () => props.text,
      (text, prev) => {
        if (text === mine) return;
        const old = prev ?? mine;
        mine = text;
        const ed = untrack(editing);
        if (!ed || !ta) return;
        const map = offsetMap(diffText(old, text));
        const start = map(ed.start, 1);
        const end = Math.max(start, map(ed.end, -1));
        if (end === start && ed.end > ed.start) return setEditing(null);
        const [s0, s1] = [ta.selectionStart, ta.selectionEnd];
        ta.value = text.slice(start, end);
        setEditing({ ...ed, start, end, sep: "" });
        ta.setSelectionRange(map(ed.start + s0, -1) - start, map(ed.start + s1, -1) - start);
        fit();
      },
      { defer: true },
    ),
  );

  // Off screen (swiped away, another mosaic): stop editing, so the keyboard goes.
  createEffect(on(props.visible, (v) => !v && untrack(editing) && ta?.blur(), { defer: true }));
  createEffect(on(() => props.readOnly, (ro) => ro && stopEditing(), { defer: true }));

  const onInput = () => {
    const ed = editing();
    if (!ed || !ta) return;
    // A new block at the end: the blank line before it comes with its first character.
    const head = props.text.slice(0, ed.start) + (ta.value ? ed.sep : "");
    const next = head + ta.value + props.text.slice(ed.end);
    setEditing({ ...ed, start: head.length, end: head.length + ta.value.length, sep: ta.value ? "" : ed.sep });
    fit();
    emit(next);
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (!ta) return;
    const ed = editing();
    if (!ed) return;
    const { selectionStart: s, selectionEnd: en, value } = ta;
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
      e.preventDefault();
      props.onSave?.();
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      ta.blur();
      return;
    }
    if (e.key === "Enter" && !e.shiftKey && !e.ctrlKey && !e.metaKey && !e.altKey && !e.isComposing) {
      if (listEnter(ta)) {
        e.preventDefault();
        onInput();
      }
      return;
    }
    const prev = beforeBlocks().at(-1);
    const next = afterBlocks()[0];
    const nextAt = (b: Block) => ed.end + b.start;
    if (e.key === "ArrowUp" && s === 0 && en === 0 && prev) {
      e.preventDefault();
      startEditing(prev.start, prev.end, prev.end, prev.kind, prev.lang);
    } else if (e.key === "ArrowDown" && s === value.length && en === s && next) {
      e.preventDefault();
      startEditing(nextAt(next), ed.end + next.end, nextAt(next), next.kind, next.lang);
    } else if (e.key === "Backspace" && s === 0 && en === 0 && prev && !ed.sep) {
      // Join with the block above (one line break at a time).
      e.preventDefault();
      const text = props.text.slice(0, ed.start - 1) + props.text.slice(ed.start);
      batch(() => {
        emit(text);
        startEditing(prev.start, ed.end - 1, ed.start - 1, prev.kind, prev.lang);
      });
    } else if (e.key === "Delete" && s === value.length && en === s && next) {
      e.preventDefault();
      const cut = ed.end;
      const text = props.text.slice(0, cut) + props.text.slice(cut + 1);
      batch(() => {
        emit(text);
        startEditing(ed.start, ed.end + next.end - 1, cut, ed.kind, ed.lang);
      });
    }
  };

  const onBlur = () => {
    // Focus moving to another block re-opens editing there (the format bar keeps it); anything else ends it.
    setTimeout(() => {
      if (!ta || document.activeElement === ta) return;
      if (document.activeElement?.closest(".nb-tools")) return void ta.focus({ preventScroll: true });
      stopEditing();
    }, 0);
  };

  // ── the format bar (touch screens, while writing) ─────────────────────
  /** The current line starts with `mark` (a bullet, a task box, a heading), or with none of them again. */
  const lineMark = (mark: string) => {
    if (!ta) return;
    const { value, selectionStart: s } = ta;
    const start = value.lastIndexOf("\n", s - 1) + 1;
    const line = value.slice(start);
    const had = /^(\s*)(?:[-*+] \[[ xX]\] |[-*+] |\d+[.)] |#{1,6} )?/.exec(line)!;
    const next = had[0].slice(had[1].length) === mark ? "" : mark;
    ta.setRangeText(had[1] + next, start, start + had[0].length, "end");
    const at = Math.max(start, s + (had[1] + next).length - had[0].length);
    ta.setSelectionRange(at, at);
    onInput();
  };
  /** The selection between `mark`s (or a pair to type into). */
  const wrap = (mark: string) => {
    if (!ta) return;
    const { selectionStart: s, selectionEnd: e, value } = ta;
    const inner = value.slice(s, e);
    ta.setRangeText(mark + inner + mark, s, e, "select");
    ta.setSelectionRange(s + mark.length, s + mark.length + inner.length);
    onInput();
  };
  const tools: { label: string; text: JSX.Element; run: () => void }[] = [
    { label: "Task", text: <Icon name="listChecks" size={17} />, run: () => lineMark("- [ ] ") },
    {
      label: "List",
      text: (
        <svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" aria-hidden="true">
          <path d="M9.5 6.5h10M9.5 12h10M9.5 17.5h10" />
          <circle cx="4.8" cy="6.5" r="1.1" fill="currentColor" />
          <circle cx="4.8" cy="12" r="1.1" fill="currentColor" />
          <circle cx="4.8" cy="17.5" r="1.1" fill="currentColor" />
        </svg>
      ),
      run: () => lineMark("- "),
    },
    { label: "Heading", text: "H", run: () => lineMark("## ") },
    { label: "Bold", text: "B", run: () => wrap("**") },
    { label: "Code", text: "</>", run: () => wrap("`") },
  ];

  /** Write at the end: a new block (or the empty note's first). */
  const writeAtEnd = () => {
    const text = props.text;
    const L = text.length;
    if (!text.trim()) return startEditing(0, L, L);
    startEditing(L, L, L, "paragraph", "", text.endsWith("\n\n") ? "" : text.endsWith("\n") ? "\n" : "\n\n");
  };
  props.handle?.({ writeAtEnd, stopEditing, editing: () => !!editing() });

  // ── taps ───────────────────────────────────────────────────────────────

  const toggleTask = (blockEl: HTMLElement, box: HTMLElement) => {
    const [s, e] = rangeOf(blockEl);
    const boxes = [...blockEl.querySelectorAll(".nb-task")];
    const i = boxes.indexOf(box);
    const raw = props.text.slice(s, e);
    let k = 0;
    const next = raw.replace(TASK, (m, a: string, mark: string, b: string) => (k++ === i ? `${a}${mark === " " ? "x" : " "}${b}` : m));
    if (next === raw) return;
    emit(props.text.slice(0, s) + next + props.text.slice(e));
  };

  const onClick = (e: MouseEvent) => {
    const t = e.target as HTMLElement;
    const run = t.closest<HTMLButtonElement>(".nb-run");
    if (run) {
      const key = Number(run.dataset.key);
      const keys = new Set(previewing());
      if (keys.has(key)) keys.delete(key);
      else keys.add(key);
      setPreviewing(keys);
      return;
    }
    const copy = t.closest<HTMLButtonElement>(".code-copy");
    if (copy) {
      const code = copy.closest(".code-block")?.querySelector("pre")?.textContent ?? "";
      void copyText(code).then((ok) => {
        if (!ok) return;
        copy.classList.add("done");
        const label = copy.querySelector("span");
        if (label) label.textContent = "Copied";
        setTimeout(() => {
          copy.classList.remove("done");
          if (label) label.textContent = "Copy";
        }, 1400);
      });
      return;
    }
    const block = t.closest<HTMLElement>(".nb");
    const box = t.closest<HTMLElement>(".nb-task");
    if (box && block) {
      e.preventDefault();
      if (!props.readOnly) toggleTask(block, box);
      return;
    }
    const a = t.closest<HTMLAnchorElement>("a[href]");
    if (a) {
      e.preventDefault();
      const href = a.getAttribute("href") ?? "";
      if (!href.startsWith("#")) void openExternal(a.href);
      return;
    }
    if (props.readOnly || t.closest("button, summary, iframe, .nb-frame, .nb-edit")) return;
    // Selecting text isn't a tap.
    const sel = getSelection();
    if (sel && !sel.isCollapsed && root.contains(sel.anchorNode)) return;
    if (block) {
      const [s, en] = rangeOf(block);
      const md = block.querySelector<HTMLElement>(".nb-md") ?? block;
      const caret = s + sourceOffset(md, props.text.slice(s, en), caretAt(e.clientX, e.clientY));
      startEditing(s, en, caret, block.dataset.kind ?? "paragraph", block.dataset.lang ?? "", "", block);
      return;
    }
    // Below the last block (or an empty note): write on.
    if (t.closest(".nb-tail") || !props.text.trim()) writeAtEnd();
  };

  const blockAt = (part: () => string, name: "before" | "after") => (b: () => Block) => (
    <BlockView
      raw={part().slice(b().start, b().end)}
      part={name}
      start={b().start}
      end={b().end}
      kind={b().kind}
      lang={b().lang}
      readOnly={props.readOnly}
      previewing={previewing()}
      visible={props.visible}
    />
  );

  onCleanup(() => ta?.blur());

  return (
    <div class="note-doc-wrap">
      <div ref={root} class="note-doc" classList={{ "read-only": props.readOnly, editing: !!editing() }} onClick={onClick}>
        <div class="note-page md">
          <Show when={props.text.trim() || editing()} fallback={<p class="nb-empty">{props.readOnly ? "This note is empty." : "Write something… Markdown works: # headings, - lists, - [ ] tasks, **bold**, ``` code."}</p>}>
            <Index each={beforeBlocks()}>{blockAt(before, "before")}</Index>
            <Show when={editing()}>
              {(ed) => (
                <textarea
                  ref={(el) => {
                    ta = el;
                    el.value = editedText();
                    fit();
                  }}
                  class="nb-edit"
                  data-kind={ed().kind}
                  data-swipe=""
                  rows={1}
                  spellcheck
                  aria-label="Note text"
                  placeholder={ed().sep || !props.text.trim() ? "Write here…" : ""}
                  onInput={onInput}
                  onKeyDown={onKeyDown}
                  onBlur={onBlur}
                />
              )}
            </Show>
            <Index each={afterBlocks()}>{blockAt(after, "after")}</Index>
          </Show>
          <Show when={!props.readOnly}>
            <div class="nb-tail" aria-hidden="true" />
          </Show>
        </div>
      </div>
      <Show when={editing() && touchUi()}>
        {/* Pressing these never takes the focus (and the keyboard) away from the text. */}
        <div class="nb-tools" role="toolbar" aria-label="Format" onPointerDown={(e) => e.preventDefault()} onMouseDown={(e) => e.preventDefault()}>
          {tools.map((t) => (
            <button type="button" class="nb-tool" aria-label={t.label} data-tool={t.label} onClick={t.run}>
              {t.text}
            </button>
          ))}
          <button type="button" class="nb-tool done" onClick={() => ta?.blur()}>
            Done
          </button>
        </div>
      </Show>
    </div>
  );
}

// ── one rendered block ───────────────────────────────────────────────────

function BlockView(props: { raw: string; part: "before" | "after"; start: number; end: number; kind: string; lang: string; readOnly: boolean; previewing: ReadonlySet<number>; visible: () => boolean }) {
  let md!: HTMLDivElement;
  // Memos: the props are recomputed on every keystroke elsewhere, but rarely change.
  const raw = createMemo(() => props.raw);
  const html = createMemo(() => isHtmlCode(props));
  const code = createMemo(() => (html() ? codeOf(raw()) : ""));
  const key = createMemo(() => (html() ? noteKey("", code()) : 0));
  const showing = () => html() && props.previewing.has(key());

  createEffect(
    on(
      raw,
      (raw) => {
        md.replaceChildren(rendered(raw));
        enhance(md, true);
        // Task boxes: real buttons, ticked in the markdown.
        for (const li of md.querySelectorAll("li")) {
          const p = li.querySelector(":scope > p");
          const first = li.firstChild?.nodeType === Node.TEXT_NODE ? (li.firstChild as Text) : p?.firstChild?.nodeType === Node.TEXT_NODE ? (p.firstChild as Text) : null;
          const m = first && /^([☐☑]) ?/.exec(first.data);
          if (!first || !m) continue;
          first.data = first.data.slice(m[0].length);
          const box = document.createElement("button");
          box.type = "button";
          box.className = "nb-task";
          box.setAttribute("role", "checkbox");
          box.setAttribute("aria-checked", String(m[1] === "☑"));
          box.setAttribute("aria-label", m[1] === "☑" ? "Done" : "To do");
          first.before(box);
          li.classList.add("task");
          if (m[1] === "☑") li.classList.add("done");
        }
        // HTML code: a Preview button beside Copy.
        if (html()) {
          const head = md.querySelector(".code-head");
          const copy = head?.querySelector(".code-copy");
          if (head && copy) {
            const run = document.createElement("button");
            run.type = "button";
            run.className = "code-copy nb-run";
            run.dataset.key = String(key());
            head.insertBefore(run, copy);
            label(run);
          }
        }
      },
    ),
  );
  const label = (run: HTMLButtonElement) => {
    const on = showing();
    run.innerHTML = on
      ? '<svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m16 18 6-6-6-6M8 6l-6 6 6 6"/></svg><span>Code only</span>'
      : '<svg viewBox="0 0 24 24" width="13" height="13" fill="currentColor" aria-hidden="true"><path d="M7 5.5v13l11-6.5z"/></svg><span>Preview</span>';
    run.setAttribute("aria-pressed", String(on));
  };
  createEffect(() => {
    void showing();
    const run = md.querySelector<HTMLButtonElement>(".nb-run");
    if (run) label(run);
  });

  return (
    <div class="nb" data-part={props.part} data-s={props.start} data-e={props.end} data-kind={props.kind} data-lang={props.lang || undefined}>
      <div ref={md} class="nb-md" />
      <Show when={showing()}>
        <PreviewFrame html={code()} visible={props.visible} />
      </Show>
    </div>
  );
}

/** HTML from a note, run like an HTML whiteboard: another origin, no network, sandboxed. */
function PreviewFrame(props: { html: string; visible: () => boolean }) {
  const [tall, setTall] = createSignal(false);
  const [live, setLive] = createSignal(props.visible());
  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(() => {
    clearTimeout(timer);
    if (props.visible()) setLive(true);
    else timer = setTimeout(() => setLive(false), 900);
  });
  onCleanup(() => clearTimeout(timer));
  const [tick, setTick] = createSignal(0);
  const [src] = createResource(
    () => (live() ? { html: props.html, theme: resolvedTheme(), tick: tick() } : null),
    (k) => previewSrc(k.html),
  );
  const [loaded, setLoaded] = createSignal(false);
  createEffect(on(() => src(), () => setLoaded(false)));
  return (
    <div class="nb-frame" classList={{ tall: tall(), ready: loaded() }}>
      <div class="nb-frame-bar">
        <span class="nb-frame-label">
          <Icon name="play" size={11} /> Preview
          <span class="nb-frame-note">· runs sandboxed, no network</span>
        </span>
        <button type="button" class="icon-btn sm" aria-label="Run again" data-tip="Run again" onClick={() => setTick((n) => n + 1)}>
          <Icon name="refresh" size={13} />
        </button>
        <button type="button" class="icon-btn sm" aria-label={tall() ? "Smaller" : "Taller"} data-tip={tall() ? "Smaller" : "Taller"} onClick={() => setTall(!tall())}>
          <Icon name={tall() ? "minimize" : "maximize"} size={13} />
        </button>
      </div>
      <Show
        when={!src.error}
        fallback={
          <p class="nb-frame-msg">
            <Icon name="alertCircle" size={14} /> {String((src.error as Error)?.message ?? src.error)}
          </p>
        }
      >
        <Show when={live() && src()} keyed fallback={<div class="nb-frame-msg">{live() ? <Tessellate size={8} /> : "Paused while hidden"}</div>}>
          {(url) => (
            <iframe src={url} title="Preview" sandbox="allow-scripts" referrerPolicy="no-referrer" allow="" onLoad={() => setLoaded(true)} />
          )}
        </Show>
      </Show>
    </div>
  );
}
