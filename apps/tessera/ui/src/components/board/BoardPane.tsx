// A whiteboard tile: what an agent shows the user (a visual explanation, notes,
// an image, a web page or remote desktop, a web app it is building) plus the
// question it may be asking. Content is untrusted and rendered isolated; see
// lib/boards.ts. Frames are unloaded while the tile is off screen and come
// back when it is shown again. Closing the tile never deletes the board.
import { For, Match, Show, Switch, createEffect, createMemo, createResource, createSignal, on, onCleanup, onMount, type JSX } from "solid-js";
import type { Whiteboard } from "../../proto/Whiteboard";
import {
  BOARD_KIND,
  askAboutBoard,
  boardChat,
  confirmDeleteBoard,
  hasExternal,
  openBoardExternally,
  htmlFrameSrc,
  kindOf,
  shownBy,
} from "../../lib/boards";
import { dismissHint, hintSeen, resolvedTheme } from "../../lib/prefs";
import { botById, state } from "../../lib/store";
import { setDrawerOpen, toast } from "../../lib/ui";
import { base64Blob, copyText, touchUi } from "../../lib/util";
import { closePane, focusPane, openSession, workspace } from "../../lib/workspace";
import { BotAvatar } from "../bots";
import { Icon, Tessellate } from "../icons";
import { usePane } from "../PaneContext";
import { RestoreButton } from "../RestoreButton";
import { openMenu, type MenuItem } from "../primitives";
import { ALT } from "../TopBar";
import { Markdown } from "../chat/Markdown";
import { AskPanel } from "./AskPanel";
import { AnsweredLine } from "./AnsweredLine";
import { lookedAt } from "../../lib/snapshot";
import { ImageViewer } from "./ImageViewer";

import { WebViewSlot } from "./WebViewSlot";
import { NewTileButton, newInHeader } from "../NewMenu";

export function BoardPane(props: { paneId: string }) {
  const pane = usePane();
  const boardId = () => {
    const c = workspace.panes[props.paneId]?.content;
    return c?.type === "whiteboard" ? c.boardId : "";
  };
  const board = (): Whiteboard | undefined => state.boards[boardId()];
  const [tick, setTick] = createSignal(0);

  // Phones show one tile at a time: say so when the agent just put this up elsewhere.
  onMount(() => {
    const b = board();
    // Something the user put up themselves needs no announcement.
    if (!b || b.by_user || !state.ready || Date.now() - b.updated_at > 20_000 || pane.visible()) return;
    if (workspace.mode() !== "mobile" && workspace.paneWorkspace(props.paneId) === workspace.active()) return;
    toast({
      level: "info",
      title: b.ask
        ? `${shownBy(b)} needs your input`
        : `${shownBy(b) === "the agent" ? "The agent" : shownBy(b)} ${b.content.type === "url" || b.content.type === "local" ? "opened a page for you" : "put up a whiteboard"}`,
      message: b.title,
      action: { label: "Show", run: () => focusPane(props.paneId) },
      timeout: 6000,
    });
  });
  const reload = () => setTick((n) => n + 1);

  // An ask answered from here collapses to one line for a moment.
  const [done, setDone] = createSignal<{ label: string; prompt: string } | null>(null);
  let doneTimer: ReturnType<typeof setTimeout> | undefined;
  createEffect(
    on(
      () => board()?.ask?.id,
      (id, prev) => {
        if (id || !prev) return;
        const a = state.answered[prev];
        if (!a) return;
        setDone({ label: a.label, prompt: a.prompt });
        clearTimeout(doneTimer);
        doneTimer = setTimeout(() => setDone(null), 4200);
      },
    ),
  );
  onCleanup(() => clearTimeout(doneTimer));

  return (
    <div class="board" classList={{ asking: !!board()?.ask, [`kind-${kindOf(board())}`]: true }}>
      <Show
        when={board()}
        fallback={
          <>
            <BoardHead paneId={props.paneId} />
            <div class="board-body">
              <Show
                when={state.ready}
                fallback={
                  <div class="board-loading">
                    <Tessellate size={10} />
                  </div>
                }
              >
                <BoardNote icon="board" title="This whiteboard is gone" body="It was deleted, or it belongs to another server.">
                  <button type="button" class="btn btn-sm" onClick={() => closePane(props.paneId)}>
                    Close tile
                  </button>
                </BoardNote>
              </Show>
            </div>
          </>
        }
      >
        {(b) => (
          <>
            <Show
              when={b().content.type !== "url" && b().content.type !== "local"}
              fallback={
                <WebViewSlot paneId={props.paneId} board={b()} fallback={<BoardHead paneId={props.paneId} board={b()} />} />
              }
            >
            <BoardHead paneId={props.paneId} board={b()} onReload={reload} />
            <div class="board-body">
              <Switch>
                <Match when={b().content.type === "html"}>
                  <Frame
                    key={`${b().id}:${b().version}:${tick()}:${resolvedTheme()}`}
                    src={() => htmlFrameSrc(b())}
                    sandbox="allow-scripts"
                    title={b().title}
                    visible={pane.visible}
                  />
                </Match>
                <Match when={b().content.type === "markdown"}>
                  <div class="board-prose">
                    <Markdown text={(b().content as { markdown: string }).markdown} />
                  </div>
                </Match>
                <Match when={b().content.type === "image"}>
                  <ImageView board={b()} />
                </Match>
              </Switch>
            </div>
            {/* Touches inside a frame never reach the app, and a swipe from the
                screen edge is the system's back gesture: on phones and foldables
                a board swipes to the next tile from its header or this bar. */}
            <Show when={touchUi() && workspace.mode() !== "tile" && !b().ask && b().content.type === "html"}>
              <div class="board-grip" aria-hidden="true">
                <i />
              </div>
            </Show>
            </Show>
            <Show when={b().ask} keyed>
              {(ask) => (
                <div class="board-ask" data-keep-focus>
                  <AskPanel board={{ ...b(), ask }} where="tile" />
                </div>
              )}
            </Show>
            <Show when={!b().ask && done()}>
              {(d) => (
                <div class="board-ask done">
                  <AnsweredLine label={d().label} prompt={d().prompt} />
                </div>
              )}
            </Show>
          </>
        )}
      </Show>
    </div>
  );
}

/** Title, kind and who showed it; reload, open, copy, more, close. */
function BoardHead(props: { paneId: string; board?: Whiteboard; onReload?: () => void }) {
  const pane = usePane();
  const b = () => props.board;
  const kind = () => BOARD_KIND[kindOf(b())];
  const tiled = () => workspace.mode() === "tile";
  const chatInfo = () => (b() ? state.sessions[boardChat(b()!)]?.info : undefined);
  const bot = () => (chatInfo()?.kind === "chat" ? botById(chatInfo()?.bot_id) : undefined);
  const ext = () => !!b() && hasExternal(b()!);
  const [copied, setCopied] = createSignal(false);

  const copyWhat = (): { label: string; run: () => Promise<boolean> } | null => {
    const x = b();
    if (!x) return null;
    const c = x.content;
    if (c.type === "url") return { label: "Copy address", run: () => copyText(c.url) };
    if (c.type === "local") return { label: "Copy address", run: () => copyText(`http://127.0.0.1:${c.port}${c.path}`) };
    if (c.type === "markdown") return { label: "Copy text", run: () => copyText(c.markdown) };
    if (c.type === "html") return { label: "Copy HTML", run: () => copyText(c.html) };
    return {
      label: "Copy image",
      run: async () => {
        try {
          const blob = base64Blob(c.data, c.mime);
          await navigator.clipboard.write([new ClipboardItem({ [blob.type]: blob })]);
          return true;
        } catch {
          return false;
        }
      },
    };
  };
  const copy = async () => {
    const w = copyWhat();
    if (w && (await w.run())) {
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    }
  };

  const menu = (anchor: HTMLElement) => {
    const x = b();
    const items: MenuItem[] = [];
    if (x) {
      items.push({ label: "Ask about this", icon: "chat", description: "Mention it in the chat's message box", onSelect: () => void askAboutBoard(x, props.paneId) });
      items.push({ label: "Go to the chat", icon: "arrowRight", onSelect: () => openSession(boardChat(x), { newPane: true }) });
      items.push({ label: "Reload", icon: "refresh", onSelect: () => props.onReload?.() });
      if (ext()) items.push({ label: "Open in browser", icon: "external", onSelect: () => void openBoardExternally(x) });
      const w = copyWhat();
      if (w) items.push({ label: w.label, icon: "copy", onSelect: () => void copy() });
    }
    items.push({ label: "Close tile", icon: "x", hint: tiled() ? `${ALT} W` : undefined, onSelect: () => closePane(props.paneId) });
    if (x) {
      items.push({ separator: true });
      items.push({ label: "Delete whiteboard…", icon: "trash", danger: true, description: "Removes it everywhere", onSelect: () => void confirmDeleteBoard(x) });
    }
    openMenu(anchor, items, "bottom-end");
  };

  const onHeadDown = (e: PointerEvent) => {
    if ((e.target as HTMLElement).closest("button, input, a, [data-nodrag]")) return;
    pane.startDrag(e);
  };

  return (
    <header class="pane-head board-head" onPointerDown={onHeadDown}>
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
          <Icon name={kind().icon} size={15} />
        </span>
        <div class="title-stack">
          <div class="title-line">
            <span class="title-static" title={b()?.title}>
              {b()?.title ?? "Whiteboard"}
            </span>
            <Show when={b()?.ask}>
              <span class="needs-you" role="status">
                <i aria-hidden="true" />
                Needs you
              </span>
            </Show>
          </div>
          {/* For a few seconds after the agent looked, that's said in place of who showed it (narrow tiles have room for one). */}
          <span class="head-sub board-sub">
            <Show
              when={b() && lookedAt(b()!.id)}
              fallback={
                <>
                  <span class="board-sub-kind">{kind().label}</span>
                  <Show when={b()}>
                    <span class="dot-sep" aria-hidden="true">
                      ·
                    </span>
                    <span class="shown-by">
                      <Show when={bot() && !b()!.by_user}>
                        <BotAvatar bot={bot()} size={13} />
                      </Show>
                      <span class="sub-text">Shown by {shownBy(b()!)}</span>
                    </span>
                  </Show>
                </>
              }
            >
              <span class="looked" role="status">
                <Icon name="eye" size={12} />
                <span class="sub-text">The agent looked at this</span>
              </span>
            </Show>
          </span>
        </div>
      </div>
      <div class="head-controls">
        <Show when={b()}>
          <button type="button" class="icon-btn board-act" aria-label="Ask about this" data-tip="Ask about this in the chat" onClick={() => void askAboutBoard(b()!, props.paneId)}>
            <Icon name="chat" size={15} />
          </button>
          <button type="button" class="icon-btn board-act" aria-label="Reload" data-tip="Reload" onClick={() => props.onReload?.()}>
            <Icon name="refresh" size={15} />
          </button>
          <Show when={ext()}>
            <button type="button" class="icon-btn board-act" aria-label="Open in browser" data-tip="Open in browser" onClick={() => void openBoardExternally(b()!)}>
              <Icon name="external" size={15} />
            </button>
          </Show>
          <Show when={copyWhat()}>
            <button type="button" class="icon-btn board-act" aria-label={copyWhat()!.label} data-tip={copied() ? "Copied" : copyWhat()!.label} onClick={() => void copy()}>
              <Icon name={copied() ? "check" : "copy"} size={15} />
            </button>
          </Show>
        </Show>
        <button type="button" class="icon-btn" aria-label="Whiteboard options" aria-haspopup="menu" data-tip="More" onClick={(e) => menu(e.currentTarget)}>
          <Icon name="more" size={17} />
        </button>
        <RestoreButton />
        {/* Phones: New at the top right; Close is in the ⋯ menu. */}
        <Show
          when={newInHeader() && pane.showMenuButton()}
          fallback={
            <button type="button" class="icon-btn head-close" aria-label="Close tile" data-tip="Close tile (the whiteboard stays)" data-kbd={`${ALT} W`} onClick={() => closePane(props.paneId)}>
              <Icon name="x" size={15} />
            </button>
          }
        >
          <NewTileButton />
        </Show>
      </div>
    </header>
  );
}

function ImageView(props: { board: Whiteboard }) {
  const src = () => {
    const c = props.board.content;
    return c.type === "image" ? `data:${c.mime};base64,${c.data}` : "";
  };
  return <ImageViewer src={src()} alt={props.board.title} />;
}

/**
 * A sandboxed frame that only exists while its tile is on screen (a hidden
 * tile stops rendering; it reloads when shown again). Nothing is ever read
 * from or posted to the frame.
 */
function Frame(props: {
  key: string;
  src: () => Promise<string>;
  sandbox: string;
  title: string;
  visible: () => boolean;
}) {
  const [live, setLive] = createSignal(props.visible());
  let hideTimer: ReturnType<typeof setTimeout> | undefined;
  createEffect(() => {
    if (props.visible()) {
      clearTimeout(hideTimer);
      setLive(true);
    } else {
      // A short grace period: swiping past a tile shouldn't reload it.
      clearTimeout(hideTimer);
      hideTimer = setTimeout(() => setLive(false), 900);
    }
  });
  onCleanup(() => clearTimeout(hideTimer));

  const [retry, setRetry] = createSignal(0);
  const [src] = createResource(
    () => (live() ? `${props.key}#${retry()}` : null),
    async () => props.src(),
  );
  const [loaded, setLoaded] = createSignal(false);
  createEffect(on(() => [props.key, retry(), live()], () => setLoaded(false)));
  /** A new object (a new frame) for every reload, even when the address is the same. */
  const frame = createMemo(() => {
    const url = live() ? src() : undefined;
    return url ? { url, key: `${props.key}#${retry()}` } : null;
  }, null, { equals: (a, b) => a?.url === b?.url && a?.key === b?.key });

  return (
    <div class="board-frame" classList={{ ready: loaded() }}>
      <Show
        when={!src.error}
        fallback={
          <BoardNote icon="alertCircle" title="This whiteboard couldn't be shown" body={String((src.error as Error)?.message ?? src.error)}>
            <button type="button" class="btn btn-sm" onClick={() => setRetry((n) => n + 1)}>
              Try again
            </button>
          </BoardNote>
        }
      >
        <Show when={frame()} keyed>
          {(f) => (
            <iframe
              class="board-iframe"
              src={f.url}
              title={props.title}
              sandbox={props.sandbox}
              referrerPolicy="no-referrer"
              allow=""
              onLoad={() => setLoaded(true)}
            />
          )}
        </Show>
        <Show when={!loaded()}>
          <div class="board-loading" classList={{ paused: !live() }} aria-hidden={live() ? undefined : "true"}>
            <Show when={live()} fallback={<Icon name="board" size={20} />}>
              <Tessellate size={10} />
            </Show>
            <span>{live() ? "Loading…" : "Paused while hidden"}</span>
          </div>
        </Show>
      </Show>
    </div>
  );
}

/** A one-paragraph note under a frame (icons stay inline with the words); dismissed for good once closed. */
export function BoardFoot(props: { hint: string; children: JSX.Element }) {
  return (
    <Show when={!hintSeen(props.hint)}>
      <div class="board-foot" role="note">
        <Icon name="info" size={13} class="board-foot-icon" />
        <p class="board-foot-text">{props.children}</p>
        <button type="button" class="icon-btn sm board-foot-x" aria-label="Dismiss" data-tip="Got it" onClick={() => dismissHint(props.hint)}>
          <Icon name="x" size={12} />
        </button>
      </div>
    </Show>
  );
}

function BoardNote(props: { icon: "board" | "globe" | "shieldAlert" | "alertCircle"; title: string; body: string; children?: JSX.Element }) {
  return (
    <div class="board-note" role="note">
      <span class="board-note-icon">
        <Icon name={props.icon} size={22} />
      </span>
      <h3>{props.title}</h3>
      <p>{props.body}</p>
      <Show when={props.children}>
        <div class="board-note-actions">{props.children}</div>
      </Show>
    </div>
  );
}
