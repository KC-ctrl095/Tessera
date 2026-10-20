import { For, Show, createEffect, onCleanup } from "solid-js";
import { dropHint } from "../lib/dnd";
import { liveDrag } from "../lib/drag";
import { setDrawerOpen, setToastSlot } from "../lib/ui";
import { firstFreeWorkspace, newWorkspace, paneStatus, switchWorkspace, usedWorkspaces, workspace, type LayoutKind } from "../lib/workspace";
import { touchUi } from "../lib/util";
import { Icon, type IconName } from "./icons";
import { confirmDeleteMosaic, mosaicMenuItems } from "../lib/actions";
import { openMosaics } from "../lib/mosaicsPage";
import { openMenu } from "./primitives";
import { spawnTile } from "../lib/spawn";
import { openNewMenu } from "./NewMenu";
import { NotificationBell } from "./NotificationBell";

export const LAYOUT_ICON: Record<LayoutKind, IconName> = {
  dwindle: "layoutDwindle",
  master: "layoutMaster",
  columns: "layoutColumns",
  rows: "layoutRows",
  grid: "layoutGrid",
  monocle: "layoutMonocle",
};

const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
export const MOD = isMac ? "⌘" : "Ctrl";
export const ALT = isMac ? "⌥" : "Alt";

/**
 * Mosaics in use (plus the active one). The top bar uses named tabs; the
 * phone drawer keeps compact numbers and a few tesserae (one per tile).
 */
export function WorkspaceStrip(props: { onPick?: () => void; always?: boolean; tabs?: boolean; /** The "+" for a new mosaic (default on). */ add?: boolean }) {
  let strip!: HTMLElement;
  createEffect(() => {
    const active = workspace.active();
    void usedWorkspaces().length;
    if (props.tabs) requestAnimationFrame(() => strip?.querySelector(`[data-ws="${active}"]`)?.scrollIntoView({ block: "nearest", inline: "nearest" }));
  });
  const used = () => usedWorkspaces();
  const hinted = (i: number) => {
    const h = dropHint();
    return !!h && "ws" in h && h.ws === i;
  };
  const free = () => firstFreeWorkspace();
  return (
    <nav ref={strip} class="ws-strip" classList={{ "mosaic-tabs": props.tabs }} aria-label="Mosaics">
      {/* A lone mosaic kept on this device still shows (its lock says so). */}
      <Show when={props.always || used().length > 1 || used().some((i) => workspace.localOf(i))}>
        <For each={used()}>
          {(i) => {
            const tiles = () => workspace.orderOf(i);
            const active = () => workspace.active() === i;
            const running = () => tiles().some((id) => paneStatus(id) === "running");
            const approval = () => tiles().some((id) => paneStatus(id) === "approval");
            const local = () => workspace.localOf(i);
            const label = () =>
              `Mosaic ${i + 1}${workspace.nameOf(i) ? ` (${workspace.nameOf(i)})` : ""}${local() ? ", only on this device" : ""}: ${tiles().length ? `${tiles().length} tile${tiles().length === 1 ? "" : "s"}` : "empty"}${running() ? ", working" : ""}${approval() ? ", needs approval" : ""}`;
            return (
              <button
                type="button"
                class="ws-pill"
                data-ws={i}
                classList={{ active: active(), empty: !tiles().length, running: running(), "ws-waiting": approval(), hint: hinted(i) }}
                aria-current={active() ? "true" : undefined}
                aria-label={label()}
                data-tip={`${workspace.nameOf(i) ? `${i + 1} · ${workspace.nameOf(i)}` : `Mosaic ${i + 1}`}${local() ? " · only on this device" : ""}`}
                data-kbd={`${ALT} ${i + 1}`}
                onClick={() => {
                  switchWorkspace(i);
                  props.onPick?.();
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  openMenu({ x: e.clientX, y: e.clientY }, mosaicMenuItems(i, () => switchWorkspace(i)));
                }}
              >
                <span class="ws-num" classList={{ "ws-title": props.tabs }}>{props.tabs ? workspace.titleOf(i) : i + 1}</span>
                <Show when={local()}>
                  <Icon name="lock" size={10} class="ws-lock" />
                </Show>
                <Show when={tiles().length > 1 || running() || approval()}>
                  <span class="ws-dots" aria-hidden="true">
                    <For each={tiles().slice(0, 5)}>
                      {(id) => <i data-st={paneStatus(id)} classList={{ f: workspace.focusedOf(i) === id }} />}
                    </For>
                  </span>
                </Show>
                <Show when={used().length > 1}>
                  <span
                    class="ws-del"
                    role="button"
                    tabIndex={-1}
                    aria-label={`Delete Mosaic ${i + 1}`}
                    data-tip="Delete mosaic (chats stay)"
                    onClick={(e) => {
                      e.stopPropagation();
                      void confirmDeleteMosaic(i);
                    }}
                  >
                    <Icon name="x" size={10} />
                  </span>
                </Show>
              </button>
            );
          }}
        </For>
      </Show>
      <Show when={props.add !== false && free() >= 0}>
        <button
          type="button"
          class="ws-add"
          data-ws={free()}
          classList={{ hint: hinted(free()) }}
          aria-label={`New mosaic (${free() + 1})`}
          data-tip="New mosaic"
          data-kbd={`${ALT} ${free() + 1}`}
          onClick={() => {
            newWorkspace();
            props.onPick?.();
          }}
        >
          <Icon name="plus" size={14} />
        </button>
      </Show>
    </nav>
  );
}

/**
 * The one way to start something new: a click opens a new chat; the chevron
 * (or a right-click) opens the New menu (components/NewMenu.tsx). Layouts are
 * on the keyboard (Alt+Space) and in the palette.
 */
export function NewButton() {
  const more = (anchor: HTMLElement) => openNewMenu(anchor);
  return (
    <div class="tb-new" role="group" aria-label="New">
      <button
        type="button"
        class="tb-new-main"
        onClick={() => spawnTile("chat")}
        onContextMenu={(e) => {
          e.preventDefault();
          more(e.currentTarget.parentElement!);
        }}
        aria-label="New chat"
        data-tip="New chat in a new tile"
        data-kbd={`${MOD} N`}
      >
        <Icon name="compose" size={16} />
        <span>New chat</span>
      </button>
      <button type="button" class="tb-new-more" aria-label="More new things" aria-haspopup="menu" data-tip="Private chat, note, web view…" onClick={(e) => more(e.currentTarget.parentElement!)}>
        <Icon name="chevronDown" size={13} />
      </button>
    </div>
  );
}

/** Slim bar above the tiles: named mosaic tabs and All mosaics at the left, New at the right. */
export function TopBar(props: { drawer: boolean }) {
  // Its free middle shows the toasts (primitives.tsx), out of the tiles' way.
  let slot!: HTMLDivElement;
  onCleanup(() => setToastSlot((s) => (s === slot ? null : s)));
  return (
    <header class="topbar" classList={{ "drag-live": !!liveDrag() }}>
      <Show when={props.drawer}>
        <button type="button" class="tb-icon" aria-label="Open sidebar" data-tip="Sidebar" data-kbd={`${MOD} B`} onClick={() => setDrawerOpen(true)}>
          <Icon name="sidebar" size={17} />
        </button>
      </Show>
      <WorkspaceStrip always tabs add={false} />
      <button
        type="button"
        class="tb-icon tb-mosaics"
        aria-label="All mosaics"
        data-tip="All mosaics"
        data-kbd={touchUi() ? undefined : `${MOD} Tab`}
        onClick={() => openMosaics()}
      >
        <Icon name="workspaces" size={16} />
        <span>All mosaics</span>
      </button>
      <div
        class="tb-toasts"
        ref={(el) => {
          slot = el;
          setToastSlot(el);
        }}
      />
      <NotificationBell />
      <NewButton />
    </header>
  );
}
