// Phones: every tile of the current mosaic at a glance, as cards. Tap to
// open, long-press and drag to rearrange, ⋯ to move to another mosaic or
// close. Swiping between tiles in the workspace itself still works.
import { For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { confirmDeleteMosaic, mosaicMenuItems, renameMosaic, requestClosePane, toggleMosaicLocal } from "../lib/actions";
import { openMosaics } from "../lib/mosaicsPage";
import { dismissHint, hintSeen } from "../lib/prefs";
import { isBusy, modelLabel } from "../lib/store";
import { setOverviewOpen } from "../lib/ui";
import { reducedMotion } from "../lib/util";
import {
  WORKSPACES,
  firstFreeWorkspace,
  focusPane,
  isPrivatePane,
  moveToWorkspace,
  newChat,
  paneStatus,
  reorderPane,
  switchWorkspace,
  usedWorkspaces,
  workspace,
} from "../lib/workspace";
import { BotAvatar } from "./bots";
import { Icon, Tessellate } from "./icons";
import { paneCard } from "./paneCard";
import { longPress, openMenu, useBackToClose, type MenuItem } from "./primitives";

const LONG_PRESS_MS = 350;
const EASE_OUT = "cubic-bezier(0.16, 0.84, 0.3, 1)";

export function MosaicOverview() {
  const order = () => workspace.order();
  // Only `over` is reactive; the card itself follows the finger through a rAF transform.
  const [drag, setDrag] = createSignal<{ id: string; over: number } | null>(null);
  let grid!: HTMLDivElement;
  const close = () => setOverviewOpen(false);
  /** Up a level: every mosaic (pinching in again does the same). */
  const allMosaics = () => {
    close();
    openMosaics();
  };
  // Two fingers pinching in: zoom out from the tiles to the mosaics.
  const touches = new Map<number, { x: number; y: number }>();
  let pinch0 = 0;
  const dist = () => {
    const [a, b] = [...touches.values()];
    return a && b ? Math.hypot(a.x - b.x, a.y - b.y) : 0;
  };
  const onPinchDown = (e: PointerEvent) => {
    if (e.pointerType !== "touch") return;
    touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (touches.size === 2) pinch0 = dist();
  };
  const onPinchMove = (e: PointerEvent) => {
    if (!touches.has(e.pointerId)) return;
    touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (touches.size === 2 && pinch0 > 0 && dist() < pinch0 * 0.62) {
      pinch0 = 0;
      allMosaics();
    }
  };
  const onPinchEnd = (e: PointerEvent) => {
    touches.delete(e.pointerId);
    if (touches.size < 2) pinch0 = 0;
  };

  onMount(() => {
    onCleanup(useBackToClose(() => setOverviewOpen(false)));
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    // Once a card is picked up, the finger moves the card, not the page.
    const noScroll = (e: TouchEvent) => {
      if (drag()) e.preventDefault();
    };
    document.addEventListener("keydown", onKey);
    grid.addEventListener("touchmove", noScroll, { passive: false });
    onCleanup(() => {
      document.removeEventListener("keydown", onKey);
      grid.removeEventListener("touchmove", noScroll);
    });
  });

  const open = (id: string) => {
    focusPane(id);
    close();
  };

  const moveMenu = (anchor: HTMLElement, id: string) => {
    const items: MenuItem[] = [{ heading: "Move to mosaic" }];
    for (const i of usedWorkspaces()) {
      if (i === workspace.active()) continue;
      items.push({ label: `Mosaic ${i + 1}`, icon: "mosaic", description: `${workspace.orderOf(i).length} tiles`, onSelect: () => moveToWorkspace(id, i, false) });
    }
    const free = firstFreeWorkspace();
    if (free >= 0) items.push({ label: `New mosaic (${free + 1})`, icon: "plus", onSelect: () => moveToWorkspace(id, free, false) });
    openMenu(anchor, items, "bottom-end");
  };

  const cardMenu = (anchor: HTMLElement, id: string) => {
    openMenu(
      anchor,
      [
        { label: "Open", icon: "arrowRight", onSelect: () => open(id) },
        { label: "Move to mosaic…", icon: "mosaic", onSelect: () => requestAnimationFrame(() => moveMenu(anchor, id)) },
        { separator: true },
        { label: isPrivatePane(id) ? "Close and delete" : "Close", icon: "x", danger: true, onSelect: () => void requestClosePane(id) },
      ],
      "bottom-end",
    );
  };

  // ── long-press + drag to reorder ───────────────────────────────────────
  let press: { id: string; x: number; y: number; pid: number; timer: ReturnType<typeof setTimeout>; el: HTMLElement } | null = null;
  let lift: { el: HTMLElement; x: number; y: number; px: number; py: number; vx: number; lastX: number; raf: number; moved: boolean } | null = null;
  const indexAt = (x: number, y: number): number => {
    const cards = Array.from(grid.querySelectorAll<HTMLElement>("[data-card]"));
    let best = 0;
    let bestD = Infinity;
    cards.forEach((c, i) => {
      // The lifted card counts at its slot, not where it floats.
      const r = c === lift?.el ? slotRect : c.getBoundingClientRect();
      const d = Math.hypot(r.left + r.width / 2 - x, r.top + r.height / 2 - y);
      if (d < bestD) {
        bestD = d;
        best = i;
      }
    });
    return best;
  };
  let slotRect = new DOMRect();
  const frame = () => {
    if (!lift) return;
    lift.raf = 0;
    const l = lift;
    l.vx = l.vx * 0.75 + (l.px - l.lastX) * 0.25;
    l.lastX = l.px;
    const rot = Math.max(-3, Math.min(3.5, 1.2 + l.vx * 0.3));
    l.el.style.transform = `translate(${l.px - l.x}px, ${l.py - l.y}px) scale(1.04) rotate(${rot}deg)`;
    const over = indexAt(l.px, l.py);
    const d = drag();
    if (d && d.over !== over) setDrag({ ...d, over });
    if (Math.abs(l.vx) > 0.05) lift.raf = requestAnimationFrame(frame);
  };
  const onDown = (id: string) => (e: PointerEvent) => {
    if ((e.target as HTMLElement).closest("button.card-more")) return;
    // The grip picks the card up at once; elsewhere it takes a long-press.
    const grip = !!(e.target as HTMLElement).closest(".ov-grip");
    if (grip) e.preventDefault();
    const el = e.currentTarget as HTMLElement;
    const x = e.clientX;
    const y = e.clientY;
    const pid = e.pointerId;
    const begin = () => {
      navigator.vibrate?.(12);
      try {
        el.setPointerCapture(pid);
      } catch {
        /* ignore */
      }
      slotRect = el.getBoundingClientRect();
      el.style.transformOrigin = `${x - slotRect.left}px ${y - slotRect.top}px`;
      lift = { el, x, y, px: x, py: y, vx: 0, lastX: x, raf: 0, moved: false };
      setDrag({ id, over: order().indexOf(id) });
      // Picked up: the card rises toward the finger.
      if (!reducedMotion()) el.animate([{ transform: "none" }, { transform: "scale(1.04) rotate(1.2deg)" }], { duration: 140, easing: EASE_OUT });
      el.style.transform = "scale(1.04) rotate(1.2deg)";
    };
    press = { id, x, y, pid, el, timer: setTimeout(begin, grip ? 0 : e.pointerType === "mouse" ? 220 : LONG_PRESS_MS) };
    if (grip) {
      clearTimeout(press.timer);
      begin();
    }
  };
  const onMove = (e: PointerEvent) => {
    if (lift && e.pointerId === press?.pid) {
      e.preventDefault();
      lift.px = e.clientX;
      lift.py = e.clientY;
      if (Math.hypot(lift.px - lift.x, lift.py - lift.y) > 8) lift.moved = true;
      if (!lift.raf) lift.raf = requestAnimationFrame(frame);
      return;
    }
    // Moving before the long-press fires is a scroll, not a drag.
    if (press && Math.hypot(e.clientX - press.x, e.clientY - press.y) > 8) {
      clearTimeout(press.timer);
      press = null;
    }
  };
  /** Reorder, then let every card glide to its new place (FLIP). */
  const settle = (id: string, over: number) => {
    const l = lift;
    lift = null;
    if (l) cancelAnimationFrame(l.raf);
    const cards = () => Array.from(grid.querySelectorAll<HTMLElement>("[data-card]"));
    const before = new Map(cards().map((c) => [c.dataset.card!, c.getBoundingClientRect()]));
    if (l) {
      l.el.style.transform = "";
      l.el.style.transformOrigin = "";
    }
    setDrag(null);
    reorderPane(id, over);
    if (reducedMotion()) return;
    requestAnimationFrame(() => {
      for (const c of cards()) {
        const o = before.get(c.dataset.card!);
        if (!o) continue;
        const n = c.getBoundingClientRect();
        const dx = o.left - n.left;
        const dy = o.top - n.top;
        const lifted = c === l?.el;
        if (!lifted && Math.abs(dx) < 1 && Math.abs(dy) < 1) continue;
        c.animate([{ transform: `translate(${dx}px, ${dy}px)${lifted ? " scale(1.04) rotate(1.2deg)" : ""}` }, { transform: "none" }], {
          duration: lifted ? 260 : 220,
          easing: EASE_OUT,
        });
      }
    });
  };
  const onUp = (e: PointerEvent) => {
    const d = drag();
    if (press) clearTimeout(press.timer);
    const p = press;
    press = null;
    if (d) {
      const moved = lift?.moved;
      const el = lift?.el;
      settle(d.id, d.over);
      // Held without moving: the card's menu, like a long-press anywhere else.
      if (!moved && el) cardMenu(el.querySelector<HTMLElement>(".card-more") ?? el, d.id);
      return;
    }
    if (p && e.pointerId === p.pid && Math.hypot(e.clientX - p.x, e.clientY - p.y) < 8) open(p.id);
  };
  const onCancel = () => {
    if (press) clearTimeout(press.timer);
    press = null;
    const d = drag();
    if (d) settle(d.id, order().indexOf(d.id));
  };

  return (
    <div
      class="overview"
      role="dialog"
      aria-modal="true"
      aria-label={`Mosaic ${workspace.active() + 1}`}
      onPointerDown={onPinchDown}
      onPointerMove={onPinchMove}
      onPointerUp={onPinchEnd}
      onPointerCancel={onPinchEnd}
    >
      <header class="overview-head">
        <button type="button" class="icon-btn" aria-label="Back to the tile" onClick={close}>
          <Icon name="arrowLeft" size={18} />
        </button>
        <div class="overview-title">
          <span class="inscription">Mosaic</span>
          <span class="overview-num">{workspace.active() + 1}</span>
          <Show when={workspace.nameOf(workspace.active())}>
            <span class="overview-name">{workspace.nameOf(workspace.active())}</span>
          </Show>
          <Show when={workspace.localOf(workspace.active())}>
            <span class="overview-local" aria-label="Only on this device">
              <Icon name="lock" size={12} />
              <span>This device</span>
            </span>
          </Show>
        </div>
        <span class="spacer" />
        <button
          type="button"
          class="icon-btn"
          aria-label={`Options for Mosaic ${workspace.active() + 1}`}
          aria-haspopup="menu"
          onClick={(e) => {
            const i = workspace.active();
            openMenu(
              e.currentTarget,
              [
                { label: "Rename…", icon: "pencil", onSelect: () => void renameMosaic(i) },
                {
                  label: "Only on this device",
                  icon: "lock",
                  checked: workspace.localOf(i),
                  description: workspace.localOf(i) ? "Your other devices don't show it" : "Your other devices stop showing it",
                  onSelect: () => toggleMosaicLocal(i),
                },
                { label: "All mosaics", icon: "workspaces", onSelect: allMosaics },
                ...(usedWorkspaces().length > 1
                  ? [{ separator: true as const }, { label: "Delete mosaic…", icon: "trash" as const, danger: true, onSelect: () => void confirmDeleteMosaic(i) }]
                  : []),
              ],
              "bottom-end",
            );
          }}
        >
          <Icon name="more" size={18} />
        </button>
        <button
          type="button"
          class="btn btn-sm"
          onClick={() => {
            newChat({ newPane: true });
            close();
          }}
        >
          <Icon name="plus" size={14} /> Tile
        </button>
      </header>

      <Show when={usedWorkspaces().length > 1 || firstFreeWorkspace() >= 0}>
        <nav class="overview-mosaics" aria-label="Mosaics">
          {/* Up a level: every mosaic, on a page of its own. */}
          <button type="button" class="mosaic-chip all" aria-label="All mosaics" onClick={allMosaics}>
            <Icon name="workspaces" size={14} />
            <span class="mosaic-chip-count">All</span>
          </button>
          <For each={usedWorkspaces()}>
            {(i) => (
              <button
                type="button"
                class="mosaic-chip"
                classList={{ on: i === workspace.active() }}
                aria-current={i === workspace.active()}
                onClick={() => switchWorkspace(i)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  openMenu(e.currentTarget, mosaicMenuItems(i, () => switchWorkspace(i)));
                }}
                {...longPress((x, y) => openMenu({ x, y }, mosaicMenuItems(i, () => switchWorkspace(i))))}
              >
                <span class="mosaic-chip-n">{i + 1}</span>
                <Show when={workspace.localOf(i)}>
                  <Icon name="lock" size={10} class="mosaic-chip-lock" />
                </Show>
                <span class="mosaic-chip-count">{workspace.orderOf(i).length}</span>
              </button>
            )}
          </For>
          <Show when={usedWorkspaces().length < WORKSPACES && firstFreeWorkspace() >= 0}>
            <button type="button" class="mosaic-chip add" aria-label="New mosaic" onClick={() => switchWorkspace(firstFreeWorkspace())}>
              <Icon name="plus" size={13} />
            </button>
          </Show>
        </nav>
      </Show>

      <Show when={!hintSeen("overview")}>
        <div class="hint-line" role="note">
          <Icon name="mosaic" size={14} />
          <span>Your tiles live in a mosaic. Tap one to open it, drag its grip to rearrange. In a tile, swipe sideways for the next one, or hold its header and carry it to a side of the screen.</span>
          <button type="button" class="icon-btn sm" aria-label="Got it" onClick={() => dismissHint("overview")}>
            <Icon name="x" size={12} />
          </button>
        </div>
      </Show>

      <div class="overview-grid" ref={grid} onPointerMove={onMove} onPointerUp={onUp} onPointerCancel={onCancel} classList={{ dragging: !!drag() }}>
        <Show
          when={order().length}
          fallback={
            <div class="empty-state">
              <p>This mosaic is empty.</p>
              <button
                type="button"
                class="btn btn-primary"
                onClick={() => {
                  newChat({ newPane: true });
                  close();
                }}
              >
                New chat
              </button>
            </div>
          }
        >
          <For each={order()}>
            {(id) => {
              const card = createMemo(() => paneCard(id));
              const status = () => paneStatus(id);
              const dragging = () => drag()?.id === id;
              return (
                <div
                  class="ov-card"
                  data-card={id}
                  classList={{
                    focused: workspace.focused() === id,
                    private: card().priv,
                    dragging: dragging(),
                    "drop-before": !!drag() && !dragging() && drag()!.over === order().indexOf(id) && order().indexOf(id) < order().indexOf(drag()!.id),
                    "drop-after": !!drag() && !dragging() && drag()!.over === order().indexOf(id) && order().indexOf(id) > order().indexOf(drag()!.id),
                  }}
                  onPointerDown={onDown(id)}
                  onContextMenu={(e) => e.preventDefault()}
                >
                  <div class="ov-top">
                    <Show when={order().length > 1}>
                      <span class="ov-grip" aria-hidden="true" title="Drag to rearrange">
                        <Icon name="grip" size={14} />
                      </span>
                    </Show>
                    <Show
                      when={card().bot}
                      fallback={<Icon name={card().icon} size={14} class="ov-icon" />}
                    >
                      <BotAvatar bot={card().bot} size={18} />
                    </Show>
                    <span class="ov-state">
                      <Show when={status() === "running"}>
                        <Tessellate size={8} label="Working" />
                      </Show>
                      <Show when={status() === "approval"}>
                        <span class="sq sq-approval" aria-label="Needs approval" />
                      </Show>
                      <Show when={status() === "error"}>
                        <span class="sq sq-error" aria-label="Error" />
                      </Show>
                    </span>
                    <button type="button" class="icon-btn sm card-more" aria-label={`Options for ${card().title}`} onClick={(e) => cardMenu(e.currentTarget, id)}>
                      <Icon name="more" size={15} />
                    </button>
                  </div>
                  <div class="ov-title">{card().title}</div>
                  <div class="ov-preview">{card().preview}</div>
                  <div class="ov-foot">
                    <Show when={card().priv} fallback={<span>{card().info ? modelLabel(card().info!.model) : ""}</span>}>
                      <span class="ov-private">Private · not saved</span>
                    </Show>
                    <Show when={card().info && isBusy(card().info)}>
                      <span class="ov-live">working</span>
                    </Show>
                  </div>
                </div>
              );
            }}
          </For>
        </Show>
      </div>
      <Show when={order().length > 1 && hintSeen("overview")}>
        <p class="ov-reorder-hint">
          <Icon name="grip" size={12} /> Drag a card by its grip to rearrange · long-press for more
        </p>
      </Show>
    </div>
  );
}
