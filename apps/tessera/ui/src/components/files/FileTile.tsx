// A text file from a chat, read in a tile (lib/fileOpen.ts opens it): the
// file as it is (TextView: find, line numbers for code and logs, wrap), or a
// Markdown file drawn as a page. Read only: the file stays as the chat has
// it; "Save as note" makes a copy to write in. The tile lives on this device
// only (other devices open the file from the chat themselves).
import { For, Match, Show, Suspense, Switch, createResource, createSignal, lazy } from "solid-js";
import { isDesktopApp } from "../../lib/native";
import { OPEN_LABEL, loadText, openFileWindow, openPlaces, openWithApp, saveAsNote, saveFile, type ViewFile } from "../../lib/fileOpen";
import { humanBytes } from "../../lib/images";
import { setDrawerOpen, toast } from "../../lib/ui";
import { copyText, isTauriMobile, touchUi } from "../../lib/util";
import { closePane, workspace } from "../../lib/workspace";
import { FileIcon } from "../chat/Messages";
import { Markdown } from "../chat/Markdown";
import { Icon, Tessellate } from "../icons";
import { usePane } from "../PaneContext";
import { RestoreButton } from "../RestoreButton";
import { openMenu, type MenuItem } from "../primitives";
import { ALT } from "../TopBar";
import type { TextViewHandle } from "./TextView";
import { NewTileButton, newInHeader } from "../NewMenu";

const TextView = lazy(() => import("./TextView"));

const MARKDOWN = /\.(md|markdown)$/i;
/** Tables and data read better unwrapped. */
const NO_WRAP = /\.(csv|tsv|json|jsonl)$/i;

export function FileTile(props: { paneId: string }) {
  const pane = usePane();
  const file = (): ViewFile => {
    const c = workspace.panes[props.paneId]?.content;
    return c?.type === "file" ? c.file : { name: "File" };
  };
  const [text] = createResource(
    () => file(),
    (f) => {
      // A private chat's file whose text wasn't kept: open it from the chat again.
      if (f.text === undefined && !f.id) return Promise.reject(new Error("This file isn't here any more. Open it again from its chat."));
      return loadText(f);
    },
  );
  const [wrap, setWrap] = createSignal(!NO_WRAP.test(file().name));
  const md = () => MARKDOWN.test(file().name);
  const [source, setSource] = createSignal(false);
  const [view, setView] = createSignal<TextViewHandle>();
  const touch = touchUi();
  const size = () => {
    const t = text();
    const n = file().size ?? (t !== undefined ? new TextEncoder().encode(t).length : undefined);
    return n !== undefined ? humanBytes(n) : "";
  };
  const lines = () => {
    const t = text();
    if (t === undefined) return "";
    let n = 1;
    for (let i = t.indexOf("\n"); i >= 0 && i < t.length - 1; i = t.indexOf("\n", i + 1)) n++;
    return `${n.toLocaleString()} ${n === 1 ? "line" : "lines"}`;
  };
  const shown = () => md() && !source();

  const copyAll = async () => {
    const t = text();
    if (t !== undefined && (await copyText(view()?.text() ?? t))) toast({ level: "success", message: "Copied" });
  };
  const find = () => {
    if (shown()) setSource(true);
    requestAnimationFrame(() => view()?.find());
  };
  const menu = (anchor: HTMLElement) => {
    const f = file();
    const can = openPlaces(f);
    const ready = text.state === "ready";
    const items: MenuItem[] = [];
    if (ready) {
      items.push({ label: "Find", icon: "search", hint: touch ? undefined : "Ctrl F", onSelect: find });
      if (md()) items.push({ label: "Show the Markdown source", icon: "variable", checked: source(), onSelect: () => setSource(!source()) });
      if (!shown()) items.push({ label: "Wrap long lines", checked: wrap(), onSelect: () => setWrap(!wrap()) });
      items.push({ separator: true });
      items.push({ label: "Copy all", icon: "copy", onSelect: () => void copyAll() });
      items.push({ label: "Save as note", icon: "note", description: "A copy to write in", onSelect: () => void saveAsNote(f, text()) });
    }
    if (can.window || can.app) items.push({ separator: true });
    if (can.window) items.push({ label: OPEN_LABEL.window, icon: "appWindow", onSelect: () => void openFileWindow(f) });
    if (can.app) items.push({ label: OPEN_LABEL.app, icon: "external", onSelect: () => void openWithApp(f) });
    if (f.id || f.text !== undefined) items.push({ label: isTauriMobile ? "Save to phone" : "Download", icon: "download", onSelect: () => void saveFile(f) });
    items.push({ separator: true });
    items.push({ label: "Close tile", icon: "x", hint: workspace.mode() === "tile" ? `${ALT} W` : undefined, onSelect: () => closePane(props.paneId) });
    openMenu(anchor, items, "bottom-end");
  };

  return (
    <div
      class="board file-tile"
      onKeyDown={(e) => {
        // Find works over the drawn Markdown too (it switches to the text).
        if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f" && shown()) {
          e.preventDefault();
          find();
        }
      }}
    >
      <header
        class="pane-head board-head"
        onPointerDown={(e) => {
          if ((e.target as HTMLElement).closest("button, input, a, [data-nodrag]")) return;
          pane.startDrag(e);
        }}
      >
        <Show when={pane.showMenuButton()}>
          <button type="button" class="icon-btn head-menu" data-keep-focus aria-label="Open sidebar" onClick={() => setDrawerOpen(true)}>
            <Icon name="menu" />
          </button>
        </Show>
        <Show when={(workspace.mode() === "mobile" && pane.count() > 1) || (workspace.mode() === "dual" && pane.count() > 2)}>
          <div class="page-dots" aria-label={`Tile ${pane.index() + 1} of ${pane.count()}`}>
            <For each={Array.from({ length: pane.count() })}>{(_, i) => <i classList={{ on: i() === pane.index() }} />}</For>
          </div>
        </Show>
        <div class="head-title">
          <span class="board-kind" aria-hidden="true">
            <FileIcon name={file().name} size={15} />
          </span>
          <div class="title-stack">
            <div class="title-line">
              <span class="title-static file-tile-name" title={file().path ? `${file().name} · on the server at ${file().path}` : file().name}>
                {file().name}
              </span>
            </div>
            <span class="head-sub board-sub">
              <span>File</span>
              <Show when={size()}>
                <span class="dot-sep" aria-hidden="true">
                  ·
                </span>
                <span>{size()}</span>
              </Show>
              <Show when={lines()}>
                <span class="dot-sep" aria-hidden="true">
                  ·
                </span>
                <span class="sub-text">{lines()}</span>
              </Show>
            </span>
          </div>
        </div>
        <div class="head-controls">
          <Show when={text.state === "ready"}>
            <button type="button" class="icon-btn board-act" aria-label="Find in file" data-tip="Find" onClick={find}>
              <Icon name="search" size={15} />
            </button>
          </Show>
          <button type="button" class="icon-btn" aria-label="File options" aria-haspopup="menu" data-tip="More" onClick={(e) => menu(e.currentTarget)}>
            <Icon name="more" size={17} />
          </button>
          <RestoreButton />
          {/* Phones: New at the top right; Close is in the ⋯ menu. */}
          <Show
            when={newInHeader() && pane.showMenuButton()}
            fallback={
              <button type="button" class="icon-btn head-close" aria-label="Close tile" data-tip="Close tile (the file stays in the chat)" data-kbd={`${ALT} W`} onClick={() => closePane(props.paneId)}>
                <Icon name="x" size={15} />
              </button>
            }
          >
            <NewTileButton />
          </Show>
        </div>
      </header>
      <div class="board-body file-body">
        <Switch>
          <Match when={text.state === "pending" || text.state === "unresolved"}>
            <div class="board-loading">
              <Tessellate size={10} />
            </div>
          </Match>
          <Match when={text.state === "errored"}>
            <div class="board-loading file-error">
              <Icon name="file" size={20} />
              <span>{String((text.error as { message?: string })?.message ?? text.error)}</span>
              <span class="file-error-actions">
                <Show when={file().id}>
                  <button type="button" class="btn btn-sm" onClick={() => void saveFile(file())}>
                    <Icon name="download" size={14} /> {isTauriMobile ? "Save to phone" : "Download"}
                  </button>
                </Show>
                <Show when={openPlaces(file()).app && file().id}>
                  <button type="button" class="btn btn-sm" onClick={() => void openWithApp(file())}>
                    <Icon name="external" size={14} /> {isDesktopApp ? "Open with its app" : "Open"}
                  </button>
                </Show>
              </span>
            </div>
          </Match>
          <Match when={shown()}>
            <div class="file-md" data-swipe="">
              <Markdown text={text()!} />
            </div>
          </Match>
          <Match when={text.state === "ready"}>
            <Suspense
              fallback={
                <div class="board-loading">
                  <Tessellate size={10} />
                </div>
              }
            >
              <TextView text={text()!} name={file().name} wrap={wrap()} touch={touch} handle={setView} />
            </Suspense>
          </Match>
        </Switch>
      </div>
    </div>
  );
}
