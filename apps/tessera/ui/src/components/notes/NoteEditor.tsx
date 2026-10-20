// A note's page: the editor (./editor, CodeMirror with a live page) and
// what goes with it here: links to other notes, pictures (paste, drop, pick,
// or the phone's camera), the menus of pictures and tables, the HTML
// preview, the notes that link here, and on a phone the format bar above
// the keyboard. Loaded as its own chunk the first time a note opens.
import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { render } from "solid-js/web";
import type { NoteInfo } from "../../proto/NoteInfo";
import { onReceivedFiles } from "../../lib/clipboard";
import { saveBlob } from "../../lib/files";
import { assetBytes, assetName, assetUrl } from "../../lib/noteAssets";
import { findNote, openLinkedNote, openNote } from "../../lib/notes";
import { state } from "../../lib/store";
import { diffText } from "../../lib/textDiff";
import { promptDialog, toastError } from "../../lib/ui";
import { openExternal, relTime, touchUi } from "../../lib/util";
import { openViewer } from "../../lib/viewer";
import { openMenu, type MenuItem } from "../primitives";
import { Icon, type IconName } from "../icons";
import { PreviewFrame } from "./PreviewFrame";
import { INSERTS, addPictures, createNoteEditor, headings, type LineKind, type NoteEditor as Editor, type NoteHooks } from "./editor";
import type { Picture } from "./editor/hooks";
import { movePictureBy, setPicture } from "./editor/pictures";

export interface NoteEditorHandle {
  focus: (where?: "start" | "end") => void;
  find: () => void;
  /** Pick a picture to put in (a phone asks: camera or photos). */
  pickPicture: (anchor?: HTMLElement) => void;
  headings: () => { level: number; text: string; pos: number }[];
  goTo: (pos: number) => void;
  /** Tint what was added since `before` (an agent's changes), or stop (null). */
  tint: (before: string | null) => void;
  /** Text from an Undo or Restore here: it can be undone again. */
  replace: (text: string) => void;
  editing: () => boolean;
}

/** Sizes offered for a picture (its width in the page). */
const SIZES: [string, number | null][] = [
  ["Small", 240],
  ["Medium", 480],
  ["Large", 720],
  ["Full width", null],
];

export default function NoteEditor(props: {
  noteId: string;
  text: string;
  readOnly: boolean;
  /** The tile is on screen. */
  visible: () => boolean;
  /** Notes linking here (null: still asking). */
  backlinks: () => NoteInfo[] | null;
  onChange: (text: string) => void;
  onSave: () => void;
  handle?: (h: NoteEditorHandle) => void;
}) {
  let host!: HTMLDivElement;
  let camera!: HTMLInputElement;
  let gallery!: HTMLInputElement;
  let ed: Editor | undefined;
  const touch = touchUi();
  const [focused, setFocused] = createSignal(false);
  const [kind, setKind] = createSignal<LineKind>("p");
  /** The text as the editor has it (anything else came from outside). */
  let mine = props.text;

  const notesList = createMemo(() =>
    Object.values(state.notes)
      .sort((a, b) => b.updated_at - a.updated_at)
      .map((n) => ({ id: n.id, title: n.title })),
  );

  // ── pictures ───────────────────────────────────────────────────────────
  /**
   * What a picker or the camera gave: read into memory first (on Android a picked file can stop
   * being readable once the input lets go of it), then put in. The value is cleared so choosing
   * again fires `change` again.
   */
  const picked = async (input: HTMLInputElement) => {
    const files = Array.from(input.files ?? []);
    if (!files.length) return;
    const read = (f: File) => f.arrayBuffer().then((b) => new File([b], f.name, { type: f.type, lastModified: f.lastModified }));
    let copies: File[];
    try {
      copies = await Promise.all(files.map((f) => read(f).catch(() => new Promise<File>((r) => setTimeout(r, 400)).then(() => read(f)))));
    } catch (e) {
      toastError(e, "Couldn't read the picture");
      return;
    } finally {
      input.value = "";
    }
    ed?.addPictures(copies);
  };
  const pickPicture = (anchor?: HTMLElement) => {
    if (props.readOnly) return;
    if (!touch) return gallery.click();
    const at = anchor ?? caretAnchor();
    openMenu(
      at,
      [
        { label: "Take photo", icon: "camera", onSelect: () => camera.click() },
        { label: "Photos", icon: "image", onSelect: () => gallery.click() },
      ],
      "top-start",
      { sheet: false },
    );
  };
  const caretAnchor = () => {
    const v = ed?.view;
    const c = v?.coordsAtPos(v.state.selection.main.head);
    return c ? { x: c.left, y: c.bottom } : host;
  };

  /** The picture's markdown again, with `alt` / `width` changed. */
  const rewritePicture = (p: Picture, alt: string, width: number | null) => {
    if (ed) setPicture(ed.view, p.from, p.to, alt, width);
  };
  const pictureMenu = (anchor: HTMLElement, p: Picture) => {
    const name = assetName(p.src);
    const items: MenuItem[] = [];
    if (name || p.src.startsWith("data:")) {
      items.push({
        label: "View",
        icon: "maximize",
        onSelect: () => void (name ? assetUrl(name) : Promise.resolve(p.src)).then((src) => openViewer([{ src, alt: p.alt || "Picture" }])),
      });
    }
    if (!props.readOnly) {
      items.push({ separator: true });
      for (const [label, w] of SIZES) items.push({ label, checked: p.width === w, onSelect: () => rewritePicture(p, p.alt, w) });
      if (p.width !== null && !SIZES.some(([, w]) => w === p.width)) items.push({ label: `Custom (${p.width} px)`, checked: true, disabled: true, onSelect: () => {} });
      // Phones can't drag it (a finger scrolls the page); computers can do both.
      items.push({ separator: true });
      items.push({ label: "Move up", icon: "arrowUp", onSelect: () => ed && movePictureBy(ed.view, p.from, p.to, -1) });
      items.push({ label: "Move down", icon: "arrowDown", onSelect: () => ed && movePictureBy(ed.view, p.from, p.to, 1) });
      items.push({
        label: "Description…",
        icon: "pencil",
        description: p.alt || "What the picture shows (for search, agents and screen readers)",
        onSelect: async () => {
          const alt = await promptDialog({ title: "Describe the picture", value: p.alt, placeholder: "A photo of…", confirmLabel: "Save" });
          if (alt !== null) rewritePicture(p, alt, p.width);
        },
      });
    }
    if (name) {
      items.push({ separator: true });
      items.push({
        label: "Save picture",
        icon: "download",
        onSelect: async () => {
          try {
            const b = await assetBytes(name);
            await saveBlob(`${(p.alt || "picture").replace(/[\\/:*?"<>|]+/g, " ").slice(0, 60)}.${name.split(".")[1]}`, b.type, b);
          } catch (e) {
            toastError(e, "Couldn't save the picture");
          }
        },
      });
    }
    if (!props.readOnly) {
      items.push({
        label: "Remove",
        icon: "trash",
        danger: true,
        onSelect: () => {
          const v = ed?.view;
          if (!v) return;
          const doc = v.state.doc;
          const line = doc.lineAt(p.from);
          // Alone on its line: the line goes too.
          const whole = line.text.trim() === doc.sliceString(p.from, p.to);
          const from = whole ? line.from : p.from;
          const to = whole ? Math.min(doc.length, line.to + 1) : p.to;
          v.dispatch({ changes: { from, to }, selection: { anchor: from }, userEvent: "delete.picture" });
          v.focus();
        },
      });
    }
    openMenu(anchor, items, "bottom-start");
  };

  // ── links ──────────────────────────────────────────────────────────────
  const openLink = (target: string, anchor: HTMLElement) => {
    const hit = findNote(target);
    if (hit) return void openNote(hit.id);
    openMenu(anchor, [{ label: `Make the note “${target}”`, icon: "note", description: "Nothing is called that yet", onSelect: () => void openLinkedNote(target) }], "bottom-start", { sheet: false });
  };

  const hooks: NoteHooks = {
    noteId: props.noteId,
    notes: notesList,
    resolve: (t) => findNote(t)?.id ?? null,
    openLink,
    openUrl: (url) => void openExternal(url),
    pictureMenu,
    pickPicture: () => pickPicture(),
    addPictures: (files, at) => ed && addPictures(ed.view, files, at),
    tableMenu: (anchor, items) =>
      openMenu(
        anchor,
        items.map((i) => ({ label: i.label, icon: i.icon as IconName | undefined, danger: i.danger, onSelect: i.run })),
        "bottom-end",
      ),
    preview: (dom, html) => render(() => <PreviewFrame html={html} visible={props.visible} />, dom),
    footer: (dom) => render(() => <Backlinks list={props.backlinks} />, dom),
  };

  onMount(() => {
    // Pictures an Android keyboard inserted (its clipboard, GIFs, stickers) while writing here: the
    // app's activity hands them over as an event of their own (lib/clipboard.ts).
    onCleanup(
      onReceivedFiles((files) => {
        const pics = files.filter((f) => f.type.startsWith("image/"));
        if (!ed || props.readOnly || !pics.length || ed.view.root.activeElement !== ed.view.contentDOM) return false;
        ed.addPictures(pics);
        return true;
      }),
    );
    ed = createNoteEditor({
      parent: host,
      text: props.text,
      readOnly: props.readOnly,
      touch,
      hooks,
      onChange: (t) => {
        mine = t;
        props.onChange(t);
      },
      onSave: props.onSave,
      onCursor: setKind,
      // Hidden a moment after the text loses the focus, and only if it is still gone (a menu, a
      // picker or Android's selection toolbar can take it for a moment).
      onFocus: (f) => {
        if (f) return setFocused(true);
        setTimeout(() => setFocused(!!ed && ed.view.root.activeElement === ed.view.contentDOM), 250);
      },
    });
    props.handle?.({
      focus: (w) => ed?.focus(w),
      find: () => ed?.find(),
      pickPicture,
      headings: () => (ed ? headings(ed.view.state.doc.toString()) : []),
      goTo: (pos) => ed?.goTo(pos),
      // After the text from outside is in (it may come in the same step).
      tint: (before) =>
        requestAnimationFrame(() => {
          if (!ed) return;
          if (before === null) return ed.tint(null);
          const out: { from: number; to: number }[] = [];
          let pos = 0;
          for (const r of diffText(before, ed.view.state.doc.toString())) {
            if (r.op === 1) out.push({ from: pos, to: (pos += r.text.length) });
            else if (r.op === 0) pos += r.text.length;
          }
          ed.tint(out);
        }),
      replace: (t) => {
        mine = t;
        ed?.setText(t, true);
      },
      editing: () => !!ed?.view.hasFocus,
    });
  });
  onCleanup(() => ed?.destroy());

  // Text from elsewhere (another device, an agent): the least change, the caret kept.
  createEffect(
    on(
      () => props.text,
      (t) => {
        if (t === mine || !ed) return;
        mine = t;
        ed.setText(t, false);
      },
      { defer: true },
    ),
  );
  createEffect(on(() => props.readOnly, (ro) => ed?.setReadOnly(ro), { defer: true }));
  // Off screen (swiped away, another mosaic): stop writing, so the keyboard goes.
  createEffect(on(props.visible, (v) => !v && ed?.blur(), { defer: true }));
  // Notes made, renamed or removed: links are drawn again (found or not).
  createEffect(
    on(
      () => notesList()
        .map((n) => n.title)
        .join("\n"),
      () => ed?.redraw(),
      { defer: true },
    ),
  );

  const bar = () => focused() && touch && !props.readOnly;
  const tool = (label: string, icon: () => import("solid-js").JSX.Element, run: (el: HTMLElement) => void, on?: () => boolean) => (
    <button
      type="button"
      class="nb-tool"
      aria-label={label}
      data-tool={label}
      aria-pressed={on ? on() : undefined}
      onClick={(e) => {
        run(e.currentTarget);
      }}
    >
      {icon()}
    </button>
  );
  /** The phone's ＋: a link, and the blocks `/` offers. */
  const insertMenu = (anchor: HTMLElement) =>
    openMenu(
      anchor,
      [{ label: "Link", icon: "link", onSelect: () => ed?.run("link") } as MenuItem, { separator: true } as MenuItem, ...INSERTS.map((it) => ({ label: it.label, onSelect: () => ed?.insert(it) }))],
      "top-start",
    );

  return (
    <div class="note-ed-wrap">
      <div ref={host} class="note-ed" classList={{ "read-only": props.readOnly }} />
      <input ref={camera} type="file" accept="image/*" capture="environment" hidden onChange={(e) => void picked(e.currentTarget)} />
      <input ref={gallery} type="file" accept="image/*" multiple hidden onChange={(e) => void picked(e.currentTarget)} />
      <Show when={bar()}>
        {/* Pressing these never takes the focus (and the keyboard) away from the text. */}
        <div class="nb-tools" role="toolbar" aria-label="Format" onPointerDown={(e) => e.preventDefault()} onMouseDown={(e) => e.preventDefault()}>
          <div class="nb-tools-row">
            {tool("Heading", () => <span class="nb-h">{/^h\d$/.test(kind()) ? kind().toUpperCase() : "H"}</span>, () => ed?.run("heading"), () => /^h\d$/.test(kind()))}
            {tool("Bold", () => <b>B</b>, () => ed?.run("bold"))}
            {tool("Italic", () => <i class="nb-i">I</i>, () => ed?.run("italic"))}
            {tool(
              "List",
              () => (
                <svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" aria-hidden="true">
                  <path d="M9.5 6.5h10M9.5 12h10M9.5 17.5h10" />
                  <circle cx="4.8" cy="6.5" r="1.1" fill="currentColor" />
                  <circle cx="4.8" cy="12" r="1.1" fill="currentColor" />
                  <circle cx="4.8" cy="17.5" r="1.1" fill="currentColor" />
                </svg>
              ),
              () => ed?.run("bullet"),
              () => kind() === "bullet",
            )}
            {tool("Task", () => <Icon name="listChecks" size={17} />, () => ed?.run("task"), () => kind() === "task")}
            {tool("Picture", () => <Icon name="image" size={17} />, (el) => pickPicture(el))}
            {tool("Undo", () => <Icon name="rewind" size={16} />, () => ed?.undo())}
            {tool("Insert", () => <Icon name="plus" size={17} />, (el) => insertMenu(el))}
          </div>
          <button type="button" class="nb-tool done" onClick={() => ed?.blur()}>
            Done
          </button>
        </div>
      </Show>
    </div>
  );
}

/** "Linked from": the notes whose text links here. */
function Backlinks(props: { list: () => NoteInfo[] | null }) {
  return (
    <Show when={props.list()?.length}>
      <div class="note-links">
        <div class="note-links-title">Linked from</div>
        <For each={props.list()!}>
          {(n) => (
            <button type="button" class="note-link" onClick={() => openNote(n.id)}>
              <Icon name="note" size={13} />
              <span class="note-link-title">{n.title}</span>
              <span class="note-link-when">{relTime(n.updated_at)}</span>
            </button>
          )}
        </For>
      </div>
    </Show>
  );
}
