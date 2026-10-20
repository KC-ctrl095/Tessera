// All mosaics, as a page of their own on every screen: each mosaic a big
// card drawn as its tiles (titles, avatars, board kinds, what's working), to
// click or tap and go into. Right-click or long-press a card (or its ⋯) for
// its options: rename, keep it on this device, delete (and on phones and
// foldables, its tiles as cards). The level above a mosaic's tile overview
// (MosaicOverview.tsx); keys in lib/mosaicsPage.ts.
import { For, Show, createEffect, createMemo, onCleanup, onMount } from "solid-js";
import { mosaicMenuItems } from "../lib/actions";
import { setOverviewOpen } from "../lib/ui";
import { closeMosaics, mosaicsOrder, pickMosaic, pointerHeld, selected, setSelected } from "../lib/mosaicsPage";
import { touchUi } from "../lib/util";
import { firstFreeWorkspace, newWorkspace, paneStatus, workspace, type Rect } from "../lib/workspace";
import { BotAvatar } from "./bots";
import { Icon, Tessellate } from "./icons";
import { paneCard } from "./paneCard";
import { longPress, openMenu, useBackToClose } from "./primitives";
import { ALT, MOD } from "./TopBar";

export function MosaicsPage() {
  const order = mosaicsOrder;
  let grid!: HTMLDivElement;
  // Previews take the shape of the tiles' own area on wide screens; a steady 4:3 on phones and foldables.
  const aspect = createMemo(() => {
    if (workspace.mode() !== "tile") return 4 / 3;
    const a = workspace.areaSize();
    return Math.min(1.9, Math.max(1.25, a.w / Math.max(1, a.h)));
  });

  onMount(() => {
    if (touchUi()) onCleanup(useBackToClose(closeMosaics));
    // Keys belong to the page now (and on phones the keyboard goes away).
    (document.activeElement as HTMLElement | null)?.blur?.();
  });
  // The selected card has the focus (and is scrolled into view).
  createEffect(() => {
    const k = selected();
    const el = grid?.querySelectorAll<HTMLElement>(".mz-card")[k];
    if (!el) return;
    if (!touchUi()) el.focus({ preventScroll: true });
    el.scrollIntoView({ block: "nearest" });
  });

  const cardMenu = (i: number, at: HTMLElement | { x: number; y: number }) => {
    const items = mosaicMenuItems(i, () => pickMosaic(i));
    // Phones and foldables: its tiles as cards, to rearrange, move or close (the overview).
    if (workspace.mode() !== "tile")
      items.splice(2, 0, {
        label: "Tiles…",
        icon: "mosaic",
        description: "Rearrange, move or close them",
        onSelect: () => {
          pickMosaic(i);
          setOverviewOpen(true);
        },
      });
    openMenu(at, items, "bottom-end");
  };
  // Opened by a long-press, the finger lifting would "click" whatever is now
  // under it (a card, the + button): only clicks that started on the page count.
  let armed = !pointerHeld();
  let root!: HTMLDivElement;
  onMount(() => {
    const guard = (e: MouseEvent) => {
      if (armed) return;
      e.preventDefault();
      e.stopPropagation();
    };
    root.addEventListener("click", guard, true);
    onCleanup(() => root.removeEventListener("click", guard, true));
  });
  const newMosaic = () => {
    closeMosaics();
    newWorkspace();
  };

  return (
    <div ref={root} class="overview mosaics-page" role="dialog" aria-modal="true" aria-label="All mosaics" onPointerDown={() => (armed = true)}>
      <header class="overview-head">
        <button type="button" class="icon-btn" aria-label="Back" data-tip="Back" data-kbd={touchUi() ? undefined : "Esc"} onClick={closeMosaics}>
          <Icon name="arrowLeft" size={18} />
        </button>
        <div class="overview-title">
          <span class="inscription">All mosaics</span>
          <span class="overview-num">{order().length}</span>
        </div>
        <span class="spacer" />
        <Show when={firstFreeWorkspace() >= 0}>
          <button type="button" class="btn btn-sm" onClick={newMosaic}>
            <Icon name="plus" size={14} /> Mosaic
          </button>
        </Show>
      </header>

      <div class="mosaics-grid" ref={grid} role="listbox" aria-label="Mosaics">
        <For each={order()}>
          {(i, k) => {
            const tiles = () => workspace.orderOf(i);
            const current = () => i === workspace.active();
            const running = () => tiles().filter((id) => paneStatus(id) === "running").length;
            const approval = () => tiles().some((id) => paneStatus(id) === "approval");
            const title = () => workspace.titleOf(i);
            return (
              <div
                class="mz-card"
                role="option"
                tabIndex={selected() === k() ? 0 : -1}
                data-ws={i}
                aria-selected={selected() === k()}
                aria-label={`${title()}${workspace.localOf(i) ? ", only on this device" : ""}: ${tiles().length} tile${tiles().length === 1 ? "" : "s"}${current() ? ", on screen" : ""}`}
                classList={{ sel: selected() === k(), current: current(), "mz-asking": approval() }}
                onPointerEnter={(e) => {
                  if (e.pointerType === "mouse") setSelected(k());
                }}
                onClick={(e) => {
                  if ((e.target as HTMLElement).closest(".mz-more")) return;
                  pickMosaic(i);
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  cardMenu(i, { x: e.clientX, y: e.clientY });
                }}
                {...longPress((x, y) => cardMenu(i, { x, y }))}
              >
                <div class="mz-head">
                  <span class="mz-num">{i + 1}</span>
                  <Show when={workspace.nameOf(i)}>
                    <span class="mz-name">{workspace.nameOf(i)}</span>
                  </Show>
                  <Show when={workspace.localOf(i)}>
                    <span class="mz-lock" data-tip="Only on this device">
                      <Icon name="lock" size={12} />
                    </span>
                  </Show>
                  <span class="mz-meta">
                    <Show when={running()}>
                      <Tessellate size={7} label="Working" />
                    </Show>
                    <Show when={approval()}>
                      <span class="sq sq-approval" aria-label="Needs you" />
                    </Show>
                    <Show when={current()}>
                      <span class="mz-here">On screen</span>
                    </Show>
                    <span>{tiles().length ? `${tiles().length} tile${tiles().length === 1 ? "" : "s"}` : "Empty"}</span>
                  </span>
                  <button
                    type="button"
                    class="icon-btn sm mz-more"
                    tabIndex={-1}
                    aria-label={`Options for ${title()}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      cardMenu(i, e.currentTarget);
                    }}
                  >
                    <Icon name="more" size={16} />
                  </button>
                </div>
                <MosaicPreview index={i} aspect={aspect()} />
              </div>
            );
          }}
        </For>
        <Show when={firstFreeWorkspace() >= 0}>
          <div class="mz-card mz-new" role="button" tabIndex={-1} aria-label="New mosaic" onClick={newMosaic}>
            <div class="mz-head">
              <span class="mz-num">{firstFreeWorkspace() + 1}</span>
              <span class="mz-name">New mosaic</span>
            </div>
            <div class="mz-new-body" style={{ "aspect-ratio": String(aspect()) }}>
              <Icon name="plus" size={22} />
            </div>
          </div>
        </Show>
      </div>

      <p class="mosaics-foot">
        <Show
          when={!touchUi()}
          fallback={<>Tap a mosaic to go into it. Long-press for its options.</>}
        >
          <kbd>←</kbd>
          <kbd>→</kbd>
          <kbd>↑</kbd>
          <kbd>↓</kbd> choose · <kbd>↵</kbd> open · <kbd>1</kbd>–<kbd>9</kbd> go straight there · <kbd>Esc</kbd> back · right-click a mosaic for its options ·{" "}
          <kbd>{MOD}</kbd> <kbd>Tab</kbd> or <kbd>{ALT}</kbd> <kbd>`</kbd> opens this page
        </Show>
      </p>
    </div>
  );
}

/** A mosaic drawn small: its tiles where they sit, each with its avatar or kind, title and latest words. */
export function MosaicPreview(props: { index: number; aspect: number }) {
  const geo = createMemo(() => workspace.geometryOf(props.index, { w: 1000 * props.aspect, h: 1000 }));
  const monocle = () => workspace.layoutOf(props.index) === "monocle" && workspace.orderOf(props.index).length > 1;
  const rects = createMemo((): [string, Rect][] => {
    // Only tiles that exist are drawn (and only they took room in `geo`).
    const order = workspace.orderOf(props.index).filter((id) => geo().panes.has(id));
    const f = workspace.focusedOf(props.index);
    // One tile at a time (monocle): the one in front stands for them all.
    if (monocle()) return [[f && order.includes(f) ? f : order[0], { x: 0, y: 0, w: 1, h: 1 }]];
    return order.map((id) => [id, geo().panes.get(id) ?? { x: 0, y: 0, w: 1, h: 1 }] as [string, Rect]);
  });
  return (
    <div class="mz-preview" style={{ "aspect-ratio": String(props.aspect) }}>
      <Show when={workspace.orderOf(props.index).length} fallback={<span class="mz-empty">Empty</span>}>
        <For each={rects()}>{([id, r]) => <PreviewTile id={id} rect={r} focused={workspace.focusedOf(props.index) === id} />}</For>
        <Show when={monocle()}>
          <span class="mz-more-tiles">+{workspace.orderOf(props.index).length - 1}</span>
        </Show>
      </Show>
    </div>
  );
}

function PreviewTile(props: { id: string; rect: Rect; focused: boolean }) {
  const card = createMemo(() => paneCard(props.id));
  const st = () => paneStatus(props.id);
  const g = 2; // px of grout between tiles
  return (
    <div
      class="mz-tile"
      data-st={st()}
      classList={{ "is-focused": props.focused, "is-private": card().priv, "is-board": card().shown }}
      style={{
        left: `calc(${props.rect.x * 100}% + ${g}px)`,
        top: `calc(${props.rect.y * 100}% + ${g}px)`,
        width: `calc(${props.rect.w * 100}% - ${g * 2}px)`,
        height: `calc(${props.rect.h * 100}% - ${g * 2}px)`,
      }}
    >
      <div class="mz-tile-top">
        <Show
          when={card().bot}
          fallback={<Icon name={card().icon} size={12} />}
        >
          <BotAvatar bot={card().bot} size={14} />
        </Show>
        <Show when={st() === "running"}>
          <Tessellate size={6} />
        </Show>
      </div>
      <div class="mz-tile-title">{card().title}</div>
      <Show when={card().preview}>
        <div class="mz-tile-text">{card().preview}</div>
      </Show>
    </div>
  );
}
