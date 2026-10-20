import { For, Match, Show, Switch, batch, createEffect, createMemo, createSignal, lazy, on, onCleanup, onMount } from "solid-js";
import { dropHint, hitTestAt, MOVE_LABEL, SNAP_LABEL, snapRegionAt, ZONE_LABEL, type DropHint } from "../lib/dnd";
import { liveDrag, pointerDrag, pokeDrag, type Landing } from "../lib/drag";
import { requestClosePane } from "../lib/actions";
import { prefs } from "../lib/prefs";
import { botById, isBusy, state } from "../lib/store";
import { overviewOpen, setDrawerOpen, setOverviewOpen } from "../lib/ui";
import { clamp, reducedMotion } from "../lib/util";
import {
  equalizeDivider,
  focusPane,
  ghosts,
  isPrivatePane,
  moveToWorkspace,
  movePane,
  newChat,
  paneStatus,
  regionRect,
  setDividerRatio,
  setDualRatio,
  snapToRegion,
  toggleMaximize,
  workspace,
  type DividerGeom,
  type Rect,
  type SnapRegion,
} from "../lib/workspace";
import { BotAvatar } from "./bots";
import { ChatPane } from "./chat/ChatPane";
import { BoardSlot } from "./board/BoardSlot";
import { AssistTile } from "./board/AssistTile";
import { WebViewSlot } from "./board/WebViewSlot";
import { paneCard } from "./paneCard";
import { Icon, Tessellate } from "./icons";
import { Launcher } from "./Launcher";
import { PaneContext } from "./PaneContext";
import { onDemand } from "../lib/onDemand";

/** A note tile (and its styles): loaded with the first note shown, or soon after start when there are notes. */
export const noteTile = onDemand(() => import("./notes/NoteTile"));
/** A chat file being read: loaded the first time one opens. */
const FileTile = lazy(() => import("./files/FileTile").then((m) => ({ default: m.FileTile })));
const TerminalTile = lazy(() => import("./terminal/TerminalTile").then((m) => ({ default: m.TerminalTile })));

const MIN_PANE_PX = 240;
const TABS_H = 34;
const EPS = 1e-4;

export function Workspace() {
  let area!: HTMLDivElement;
  const size = workspace.areaSize;
  const paneIds = createMemo(() => Object.keys(workspace.panes), undefined, {
    equals: (a, b) => a.length === b.length && a.every((v, i) => v === b[i]),
  });
  const dividerIds = createMemo(() => workspace.geometry().dividers.map((d) => d.id), undefined, {
    equals: (a, b) => a.length === b.length && a.every((v, i) => v === b[i]),
  });
  const dividerMap = createMemo(() => new Map(workspace.geometry().dividers.map((d) => [d.id, d])));
  const mode = workspace.mode;
  const order = workspace.order;
  const count = () => order().length;
  const multi = () => count() > 1;
  const tiled = () => mode() === "tile";
  const monocle = () => tiled() && workspace.layoutKind() === "monocle" && multi() && !workspace.maximized();
  const visibleCount = () => (mode() === "mobile" ? 1 : mode() === "dual" ? Math.max(1, Math.min(2, count())) : count());
  const gap = () => (mode() === "mobile" ? 0 : prefs.gaps);

  // ── carousel state (mobile / dual) ─────────────────────────────────────
  const [start, setStart] = createSignal(0);
  const [dragX, setDragX] = createSignal(0);
  const [swiping, setSwiping] = createSignal(false);
  const [snapMs, setSnapMs] = createSignal(260);
  const [resizing, setResizing] = createSignal(false);

  // Keep the focused tile inside the visible window (not while one is carried across pages).
  createEffect(() => {
    if (tiled() || liveDrag()) return;
    const n = count();
    const vis = visibleCount();
    const idx = Math.max(0, order().indexOf(workspace.focused() ?? ""));
    let s = start();
    if (idx < s) s = idx;
    if (idx >= s + vis) s = idx - vis + 1;
    setStart(clamp(s, 0, Math.max(0, n - vis)));
  });

  onMount(() => {
    const ro = new ResizeObserver(() => {
      workspace.setAreaSize({ w: area.clientWidth, h: area.clientHeight });
    });
    ro.observe(area);
    onCleanup(() => ro.disconnect());
  });

  // ── tile geometry ──────────────────────────────────────────────────────
  const tileRect = (id: string): Rect => {
    const max = workspace.maximized();
    if (max === id) return { x: 0, y: 0, w: 1, h: 1 };
    return workspace.geometry().panes.get(id) ?? { x: 0, y: 0, w: 1, h: 1 };
  };

  /** Inset a fractional rect by the gap: full gap on outer edges, half between tiles. */
  const insetStyle = (r: Rect) => {
    const g = gap();
    const h = g / 2;
    const l = r.x <= EPS ? g : h;
    const t = r.y <= EPS ? g : h;
    const rr = r.x + r.w >= 1 - EPS ? g : h;
    const b = r.y + r.h >= 1 - EPS ? g : h;
    return {
      left: `calc(${r.x * 100}% + ${l}px)`,
      top: `calc(${r.y * 100}% + ${t}px)`,
      width: `calc(${r.w * 100}% - ${l + rr}px)`,
      height: `calc(${r.h * 100}% - ${t + b}px)`,
    };
  };

  const onActive = (id: string) => workspace.paneWorkspace(id) === workspace.active();

  // ── motion: tiles glide to their new places (FLIP), mosaics slide ─────
  /** Pixel rect of a tile as laid out (same maths as insetStyle). */
  const pxRect = (id: string): Rect => fracPx(tileRect(id));
  /** A part of the mosaic (fractions) in pixels, with the gaps tiles there would have. */
  const fracPx = (r: Rect): Rect => {
    const a = size();
    const g = gap();
    const h = g / 2;
    const l = r.x <= EPS ? g : h;
    const t = r.y <= EPS ? g : h;
    const rr = r.x + r.w >= 1 - EPS ? g : h;
    const b = r.y + r.h >= 1 - EPS ? g : h;
    return { x: r.x * a.w + l, y: r.y * a.h + t, w: r.w * a.w - l - rr, h: r.h * a.h - t - b };
  };
  const EASE_OUT = "cubic-bezier(0.16, 0.84, 0.3, 1)";
  /** A dropped tile settles from where its lifted self was, not from its old slot. */
  let landing: { id: string; at: Landing } | null = null;
  // ── two-up (foldables): the pair on screen splits by a ratio the user drags ──
  /** Width of tile `j` in the row, as a fraction of the area: the pair on screen shares it by the ratio. */
  const rowFrac = (j: number) => {
    if (visibleCount() < 2) return 1 / visibleCount();
    const s0 = start();
    const r = workspace.dualRatio();
    return j === s0 ? r : j === s0 + 1 ? 1 - r : 0.5;
  };
  /** Left edge of tile `j` in the row (fraction of the area). */
  const rowLeft = (j: number) => {
    let x = 0;
    for (let k = 0; k < j; k++) x += rowFrac(k);
    return x;
  };
  /** Pixel rect of a tile in the two-up row (foldables). */
  const rowRect = (id: string): Rect => {
    const i = order().indexOf(id);
    const W = size().w;
    return { x: rowLeft(i) * W, y: 0, w: rowFrac(i) * W, h: size().h };
  };
  let last = new Map<string, Rect>();
  let lastKey = "";
  let lastActive = workspace.active();
  createEffect(() => {
    // Everything that moves tiles: the tree/layout, maximize, the mosaic, the area.
    const ids = order();
    const act = workspace.active();
    const m = mode();
    const a = size();
    const max = workspace.maximized();
    void workspace.geometry();
    void max;
    const next = new Map<string, Rect>();
    if (m === "tile") for (const id of ids) next.set(id, pxRect(id));
    else if (m === "dual") for (const id of ids) next.set(id, rowRect(id));
    const key = `${act}|${m}|${Math.round(a.w)}x${Math.round(a.h)}`;
    const prev = last;
    const sameFrame = key === lastKey;
    const switched = act !== lastActive;
    const dir = Math.sign(act - lastActive);
    last = next;
    lastKey = key;
    lastActive = act;
    const land = landing;
    landing = null;
    if (reducedMotion() || resizing() || swiping()) return;
    // Let the DOM take the new geometry first, then animate from the old one.
    queueMicrotask(() => {
      if (switched) {
        for (const id of ids) {
          const el = area.querySelector<HTMLElement>(`[data-pane-id="${id}"]`);
          el?.animate(
            [
              { transform: `translateX(${dir * 36}px)`, opacity: 0 },
              { transform: "none", opacity: 1 },
            ],
            { duration: 240, easing: EASE_OUT },
          );
        }
        return;
      }
      if (!sameFrame || m === "mobile") return;
      for (const [id, r] of next) {
        const el = area.querySelector<HTMLElement>(`[data-pane-id="${id}"]`);
        if (!el) continue;
        if (land?.id === id) {
          // From the lifted tile under the pointer into its new bed.
          const tr = el.getBoundingClientRect();
          const cx = tr.left + tr.width / 2;
          const cy = tr.top + tr.height / 2;
          const sc = Math.min(1.2, Math.max(0.3, land.at.w / Math.max(1, r.w)));
          el.animate(
            [
              { transform: `translate(${land.at.cx - cx}px, ${land.at.cy - cy}px) scale(${sc}) rotate(${land.at.rot}deg)` },
              { transform: "none" },
            ],
            { duration: 280, easing: EASE_OUT },
          );
          continue;
        }
        const o = prev.get(id);
        if (!o) {
          if (workspace.panes[id]?.content.type === "whiteboard") {
            // A whiteboard the agent puts up rises into place beside its chat.
            el.animate(
              [
                { transform: "translateY(14px) scale(0.96)", opacity: 0 },
                { transform: "none", opacity: 1 },
              ],
              { duration: 380, easing: "cubic-bezier(0.34, 1.25, 0.64, 1)" },
            );
            continue;
          }
          // A new tile is pressed into the grout.
          el.animate([{ transform: "scale(0.965)", opacity: 0 }, { transform: "none", opacity: 1 }], { duration: 220, easing: EASE_OUT });
          continue;
        }
        const dx = o.x + o.w / 2 - (r.x + r.w / 2);
        const dy = o.y + o.h / 2 - (r.y + r.h / 2);
        const resized = Math.abs(o.w - r.w) > 2 || Math.abs(o.h - r.h) > 2;
        if (Math.abs(dx) < 1 && Math.abs(dy) < 1 && !resized) continue;
        // Uniform scale only (text never stretches), kept close to 1.
        const sc = resized ? Math.min(1.06, Math.max(0.94, Math.sqrt((o.w * o.h) / Math.max(1, r.w * r.h)))) : 1;
        el.animate(
          [
            { transform: `translate(${dx}px, ${dy}px) scale(${sc})`, opacity: resized ? 0.82 : 1 },
            { transform: "none", opacity: 1 },
          ],
          { duration: 240, easing: EASE_OUT },
        );
      }
    });
  });

  const paneStyle = (id: string) => {
    if (!onActive(id)) return { display: "none" };
    if (tiled()) {
      const max = workspace.maximized();
      const hidden = (!!max && max !== id) || (monocle() && workspace.focused() !== id);
      return {
        ...insetStyle(tileRect(id)),
        visibility: hidden ? ("hidden" as const) : undefined,
        "pointer-events": hidden ? ("none" as const) : undefined,
        "z-index": max === id ? 4 : undefined,
      };
    }
    const i = order().indexOf(id);
    const g = mode() === "dual" && visibleCount() > 1 ? gap() / 2 : 0;
    const outer = mode() === "dual" ? gap() : 0;
    const x = rowLeft(i) * 100;
    const w = rowFrac(i) * 100;
    return {
      left: `calc(${x}% + ${g + (i === 0 ? outer / 2 : 0)}px)`,
      top: `${outer}px`,
      width: `calc(${w}% - ${g * 2 + outer / 2}px)`,
      height: `calc(100% - ${outer * 2}px)`,
    };
  };

  const trackStyle = () => {
    const top = monocle() ? TABS_H : 0;
    if (tiled()) return { top: `${top}px` };
    const x = -rowLeft(start()) * size().w + dragX();
    return {
      transform: `translate3d(${x}px,0,0)`,
      transition: swiping() || reducedMotion() ? "none" : `transform ${snapMs()}ms cubic-bezier(.2,.8,.2,1)`,
      // A compositor layer only while the finger is down or the snap runs.
      "will-change": swiping() || snapping() ? "transform" : undefined,
    };
  };
  const [snapping, setSnapping] = createSignal(false);
  let snapTimer: ReturnType<typeof setTimeout> | undefined;
  // Pointer events can arrive faster than frames: apply the latest position once per frame.
  let dragRaf = 0;
  let dragPending = 0;
  const queueDrag = (x: number) => {
    dragPending = x;
    if (!dragRaf)
      dragRaf = requestAnimationFrame(() => {
        dragRaf = 0;
        setDragX(dragPending);
      });
  };
  /** The pages stop following the finger (a frame still queued would leave them askew). */
  const dropDrag = () => {
    cancelAnimationFrame(dragRaf);
    dragRaf = 0;
    setSwiping(false);
    setDragX(0);
  };

  // ── swipe (touch) ──────────────────────────────────────────────────────
  /**
   * A horizontal gesture inside a tile either scrolls something wide (a code
   * block, a table, a diff) that can still move that way, or swipes to the
   * next tile. Those wide things leave horizontal panning to us (see
   * `.workspace[data-mode] … touch-action` in chat.css), so the browser never
   * grabs the gesture and swallows the swipe: we scroll them ourselves.
   */
  let sw: {
    id: number;
    x: number;
    y: number;
    lock: "x" | "y" | "inner" | null;
    samples: { x: number; t: number }[];
    scroller?: HTMLElement;
    lastX?: number;
  } | null = null;

  const scrollerX = (el: HTMLElement | null, dx: number): HTMLElement | null => {
    while (el && el !== area) {
      if (el.scrollWidth > el.clientWidth + 1) {
        const ox = getComputedStyle(el).overflowX;
        if (ox === "auto" || ox === "scroll") {
          if (dx > 0 && el.scrollLeft > 0) return el;
          if (dx < 0 && el.scrollLeft + el.clientWidth < el.scrollWidth - 1) return el;
        }
      }
      el = el.parentElement;
    }
    return null;
  };
  /** A flick on something wide keeps it gliding for a moment. */
  let glide = 0;
  const glideScroll = (el: HTMLElement, v: number) => {
    cancelAnimationFrame(glide);
    let speed = v * 16; // px per frame
    const step = () => {
      if (Math.abs(speed) < 0.5) return;
      el.scrollLeft -= speed;
      speed *= 0.92;
      glide = requestAnimationFrame(step);
    };
    if (!reducedMotion()) glide = requestAnimationFrame(step);
  };

  // Two fingers pinching in (phones, foldables) open the Mosaic overview.
  const touches = new Map<number, { x: number; y: number }>();
  let pinch0 = 0;
  const pinchDist = () => {
    const [a, b] = [...touches.values()];
    return a && b ? Math.hypot(a.x - b.x, a.y - b.y) : 0;
  };

  /**
   * Forget the gesture in progress. A finger whose release never reached the page (the app
   * went to the background, a system gesture took the touch, the screen folded) would
   * otherwise stay "down" here, and every later touch would count as a second finger: no
   * swipe would start again until the app restarted.
   */
  const resetSwipe = () => {
    if (sw) {
      try {
        if (area.hasPointerCapture(sw.id)) area.releasePointerCapture(sw.id);
      } catch {
        /* the pointer is gone */
      }
    }
    sw = null;
    touches.clear();
    pinch0 = 0;
    cancelAnimationFrame(glide);
    dropDrag();
    setResizing(false);
  };
  // The app going to the background, folding or unfolding, turning the phone: no release comes.
  const onHidden = () => {
    if (document.visibilityState === "hidden") resetSwipe();
  };
  document.addEventListener("visibilitychange", onHidden);
  onCleanup(() => document.removeEventListener("visibilitychange", onHidden));
  createEffect(on([mode, () => Math.round(size().w)], () => resetSwipe(), { defer: true }));

  const onPointerDown = (e: PointerEvent) => {
    // The first finger down: no other finger is on the screen, so whatever an earlier
    // gesture left behind goes now.
    if (e.pointerType === "touch" && e.isPrimary) resetSwipe();
    if (e.pointerType === "touch" && !tiled()) {
      touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (touches.size === 2) {
        pinch0 = pinchDist();
        // A second finger is never a swipe.
        sw = null;
        dropDrag();
      }
    }
    if (e.pointerType !== "touch" || tiled() || touches.size > 1) return;
    sw = { id: e.pointerId, x: e.clientX, y: e.clientY, lock: null, samples: [{ x: e.clientX, t: e.timeStamp }] };
  };
  const onPointerMove = (e: PointerEvent) => {
    if (touches.has(e.pointerId)) {
      touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (touches.size === 2 && pinch0 > 0 && pinchDist() < pinch0 * 0.62) {
        pinch0 = 0;
        setOverviewOpen(true);
      }
    }
    if (!sw || e.pointerId !== sw.id) return;
    // A tile picked up (long-press) moves; the carousel stays put.
    if (liveDrag()) {
      if (sw.lock === "x") dropDrag();
      sw = null;
      return;
    }
    const dx = e.clientX - sw.x;
    const dy = e.clientY - sw.y;
    if (sw.lock === "inner" && sw.scroller) {
      sw.scroller.scrollLeft -= e.clientX - (sw.lastX ?? e.clientX);
      sw.lastX = e.clientX;
      sw.samples.push({ x: e.clientX, t: e.timeStamp });
      if (sw.samples.length > 6) sw.samples.shift();
      return;
    }
    if (!sw.lock) {
      if (Math.abs(dx) < 8 && Math.abs(dy) < 8) return;
      const t = e.target as HTMLElement;
      // (A web view's address swipes like the rest of its bar until it's being edited. A note's
      // text swipes too, but only a quick stroke: a finger that rests first is selecting text.)
      const field = t.closest('textarea, input, [contenteditable="true"], [data-noswipe]');
      const swipes =
        !field ||
        (field.classList.contains("wv-input") && document.activeElement !== field) ||
        (field.hasAttribute("data-swipe") && e.timeStamp - sw.samples[0].t < 350);
      if (Math.abs(dx) > Math.abs(dy) * 1.15 && swipes) {
        const inner = scrollerX(t, dx);
        if (inner) {
          sw.lock = "inner";
          sw.scroller = inner;
          sw.lastX = e.clientX;
          cancelAnimationFrame(glide);
          return;
        }
        sw.lock = "x";
        setSwiping(true);
        try {
          area.setPointerCapture(e.pointerId);
        } catch {
          /* ignore */
        }
      } else {
        sw.lock = "y";
        return;
      }
    }
    if (sw.lock !== "x") return;
    const maxStart = Math.max(0, count() - visibleCount());
    let off = dx;
    // Rubber-band at the ends (a right-pull on the first tile reveals the drawer).
    if ((start() === 0 && dx > 0) || (start() >= maxStart && dx < 0)) off = dx * 0.3;
    queueDrag(off);
    sw.samples.push({ x: e.clientX, t: e.timeStamp });
    if (sw.samples.length > 6) sw.samples.shift();
  };
  const onPointerUp = (e: PointerEvent) => {
    touches.delete(e.pointerId);
    if (touches.size < 2) pinch0 = 0;
    if (!sw || e.pointerId !== sw.id) return;
    const s = sw;
    sw = null;
    if (s.lock === "inner" && s.scroller) {
      const f = s.samples[0];
      glideScroll(s.scroller, (e.clientX - f.x) / Math.max(1, e.timeStamp - f.t));
      return;
    }
    if (s.lock !== "x") return;
    const dx = e.clientX - s.x;
    const first = s.samples[0];
    const v = (e.clientX - first.x) / Math.max(1, e.timeStamp - first.t); // px/ms
    const paneW = size().w / visibleCount();
    const maxStart = Math.max(0, count() - visibleCount());
    let next = start();
    // A fifth of a tile, or a flick, is enough.
    if (dx < -paneW * 0.2 || v < -0.32) next = start() + 1;
    else if (dx > paneW * 0.2 || v > 0.32) next = start() - 1;
    if (next < 0 && dx > 60) setDrawerOpen(true);
    next = clamp(next, 0, maxStart);
    // Duration from remaining distance and release velocity ("momentum").
    const remaining = Math.abs((start() - next) * paneW + dragX());
    setSnapMs(clamp(Math.abs(v) > 0.05 ? remaining / Math.abs(v) : 280, 140, 340));
    cancelAnimationFrame(dragRaf);
    dragRaf = 0;
    setSnapping(true);
    clearTimeout(snapTimer);
    snapTimer = setTimeout(() => setSnapping(false), snapMs() + 60);
    setSwiping(false);
    setDragX(0);
    // Focus follows the swipe: a focused tile the pages leave behind hands it to the nearest one
    // still on screen. Both in one batch: the effect that keeps the focused tile on screen would
    // otherwise see the new page with the old focus and turn straight back (on an unfolded Fold,
    // every swipe onward that started with the left tile focused sprang back).
    const idx = order().indexOf(workspace.focused() ?? "");
    const vis = visibleCount();
    batch(() => {
      setStart(next);
      if (idx < next || idx >= next + vis) focusPane(order()[clamp(idx, next, next + vis - 1)]);
    });
  };
  // The pager lost the finger it held (its release went elsewhere): spring back, as for a cancel.
  // (Only its own capture: the one it takes over from the touched element bubbles up here too.)
  const onLostCapture = (e: PointerEvent) => {
    if (e.target === area && sw && e.pointerId === sw.id && sw.lock === "x") {
      sw = null;
      dropDrag();
    }
  };
  const onPointerCancel = (e: PointerEvent) => {
    touches.delete(e.pointerId);
    if (touches.size < 2) pinch0 = 0;
    if (sw && e.pointerId === sw.id) {
      sw = null;
      dropDrag();
    }
  };

  // ── tile header drag → move / swap / other mosaic ─────────────────────
  // Desktop: drag the header (a click still renames). Two-up on foldables:
  // press and hold the header, then drag. See lib/drag.ts.
  // Phones and foldables: hold the header, then drag; held at the screen's
  // left or right side, the pages turn under the tile (see `edgeFrame`).
  const beginHeaderDrag = (id: string) => (e: PointerEvent) => {
    const m = mode();
    // A lone tile has nowhere to go: its header just works as a header.
    if (!multi()) return;
    const tile = area.querySelector<HTMLElement>(`[data-pane-id="${id}"]`);
    if (!tile) return;
    let x0: number | null = null;
    // The sides and corners snap only once the pointer has been away from them
    // (a header grabbed near the mosaic's edge doesn't snap at once).
    let armed = false;
    pointerDrag(e, {
      kind: "tile",
      tileId: id,
      tile,
      label: "Move tile",
      longPress: m !== "tile" || e.pointerType === "touch" ? 350 : undefined,
      onFrame: (x) => {
        if (!tiled()) edgeFrame(x);
      },
      // The pages turned under the lifted tile: it stays under the finger.
      offset: () => {
        if (tiled()) return { x: 0, y: 0 };
        const x = trackX();
        if (x0 === null) x0 = x;
        return { x: x0 - x, y: 0 };
      },
      hitTest: (x, y) => {
        const h = hitTestAt(x, y, id);
        if (h && "ws" in h) return h;
        if (mode() === "tile") {
          const region = snapRegionAt(area.getBoundingClientRect(), x, y);
          if (!region) armed = true;
          else if (armed) return { snap: region };
        }
        if (h && "launcher" in h) return null;
        if (h && "paneId" in h && !multi()) return null;
        return h;
      },
      drop: (hint, at) => {
        if ("ws" in hint) {
          if (hint.ws === workspace.active()) return false;
          moveToWorkspace(id, hint.ws, false);
          return true;
        }
        if ("snap" in hint) {
          landing = { id, at };
          snapToRegion(id, hint.snap);
          return true;
        }
        if (!("paneId" in hint)) return false;
        landing = { id, at };
        movePane(id, hint.paneId, hint.zone);
        return true;
      },
    });
  };

  // ── carrying a tile across pages (phones, foldables) ───────────────────
  const EDGE_PX = 44;
  const TURN_MS = 420;
  const AGAIN_MS = 800;
  let track!: HTMLDivElement;
  const [edgeSide, setEdgeSide] = createSignal(0);
  let turnTimer: ReturnType<typeof setTimeout> | undefined;
  /** Where the track is right now, mid-glide included. */
  const trackX = () => {
    const t = getComputedStyle(track).transform;
    return t && t !== "none" ? new DOMMatrixReadOnly(t).m41 : 0;
  };
  const canTurn = (dir: number) => {
    const next = start() + dir;
    return next >= 0 && next <= Math.max(0, count() - visibleCount());
  };
  /** Turn one page under the carried tile; the drag follows the glide frame by frame. */
  const turnPage = (dir: number) => {
    if (!canTurn(dir)) return false;
    setSnapMs(300);
    setSnapping(true);
    clearTimeout(snapTimer);
    snapTimer = setTimeout(() => setSnapping(false), 360);
    setStart(start() + dir);
    navigator.vibrate?.(6);
    const until = performance.now() + 360;
    const tick = () => {
      pokeDrag();
      if (liveDrag() && performance.now() < until) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    return true;
  };
  /** Each frame of a tile drag: resting at a side turns the page (again and again while held). */
  const edgeFrame = (x: number) => {
    const r = area.getBoundingClientRect();
    const raw = x < r.left + EDGE_PX ? -1 : x > r.right - EDGE_PX ? 1 : 0;
    const side = raw && canTurn(raw) ? raw : 0;
    if (side === edgeSide()) return;
    setEdgeSide(side);
    clearTimeout(turnTimer);
    if (!side) return;
    const turn = () => {
      if (edgeSide() !== side || !liveDrag()) return;
      if (turnPage(side) && canTurn(side)) turnTimer = setTimeout(turn, AGAIN_MS);
      else setEdgeSide(0);
    };
    turnTimer = setTimeout(turn, TURN_MS);
  };
  createEffect(() => {
    if (!liveDrag()) {
      clearTimeout(turnTimer);
      setEdgeSide(0);
    }
  });
  onCleanup(() => clearTimeout(turnTimer));

  /** The empty bed a lifted tile leaves behind. */
  const socketStyle = () => {
    const id = liveDrag()?.tileId;
    if (!id) return {};
    const st = paneStyle(id) as Record<string, string | undefined>;
    return { left: st.left, top: st.top, width: st.width, height: st.height };
  };


  /** Whether a tile is one of the pages on screen (phones, foldables). */
  const inWindow = (id: string) => {
    const i = order().indexOf(id);
    return i >= start() && i < start() + visibleCount();
  };

  /** Whether a tile can be seen now (hidden ones pause what they render, e.g. whiteboards). */
  const onScreen = (id: string) => {
    if (!onActive(id) || overviewOpen()) return false;
    if (tiled()) {
      const max = workspace.maximized();
      if (max && max !== id) return false;
      return !(monocle() && workspace.focused() !== id);
    }
    const i = order().indexOf(id);
    const s = start();
    // While a swipe is under way, the neighbours get ready too.
    const pad = swiping() ? 1 : 0;
    return i >= s - pad && i < s + visibleCount() + pad;
  };

  // Only phones need the drawer button in a tile header (elsewhere the top bar has one).
  const menuPane = () => (mode() === "mobile" ? order()[start()] ?? null : null);

  return (
    <div
      ref={area}
      class="workspace"
      data-mode={mode()}
      data-layout={workspace.layoutKind()}
      classList={{
        multi: multi(),
        framed: mode() !== "mobile",
        resizing: resizing(),
        dragging: !!liveDrag(),
        swiping: swiping(),
        maximized: !!workspace.maximized(),
      }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerCancel}
      onLostPointerCapture={onLostCapture}
      // The track moves the tiles; the area itself never scrolls (focusing something in a
      // tile that is still sliding in would otherwise scroll it and leave the pages askew).
      onScroll={(e) => {
        const el = e.currentTarget;
        if (el.scrollLeft || el.scrollTop) el.scrollTo(0, 0);
      }}
    >
      <Show when={monocle()}>
        <MonocleTabs />
      </Show>

      <div class="ws-track" ref={track} style={trackStyle()}>
        <For each={paneIds()}>
          {(id) => {
            const ctx = {
              paneId: id,
              startDrag: beginHeaderDrag(id),
              showMenuButton: () => menuPane() === id,
              index: () => order().indexOf(id),
              count,
              visible: () => onScreen(id),
            };
            // Restoring a phone's saved mosaics must not mount every transcript, composer and
            // observer at launch. Once visited, keep the body so drafts and scroll state survive.
            const [visited, setVisited] = createSignal(false);
            createEffect(() => {
              if (ctx.visible()) setVisited(true);
            });
            return (
              <PaneContext.Provider value={ctx}>
                <section
                  class="pane"
                  data-pane-id={id}
                  classList={{
                    focused: workspace.focused() === id,
                    "drag-source": liveDrag()?.tileId === id,
                    "is-max": workspace.maximized() === id,
                    private: isPrivatePane(id),
                  }}
                  style={paneStyle(id)}
                  aria-label="Tile"
                  onPointerDown={(e) => {
                    if ((e.target as HTMLElement).closest("[data-keep-focus]")) return;
                    // Only a page on screen: the edge of the next one, showing while a stroke
                    // springs back, isn't a place to go (the touch would turn two pages).
                    if (!tiled() && !inWindow(id)) return;
                    if (workspace.focused() !== id) focusPane(id);
                  }}
                  onFocusIn={(e) => {
                    if ((e.target as HTMLElement).closest("[data-keep-focus]")) return;
                    if (workspace.focused() !== id && onActive(id)) focusPane(id);
                  }}
                  onDblClick={(e) => {
                    // Like a window's title bar: double-clicking a tile's header maximizes or restores it.
                    const t = e.target as HTMLElement;
                    if (!tiled() || !t.closest(".pane-head") || t.closest("button, input, textarea, a, [role=button], [data-nodrag]")) return;
                    if (count() > 1 || workspace.maximized() === id) toggleMaximize(id);
                  }}
                >
                  <Show when={visited() || workspace.panes[id]?.content.type !== "chat"}><PaneBody id={id} /></Show>
                </section>
              </PaneContext.Provider>
            );
          }}
        </For>
        <Show when={tiled()}>
          <For each={ghosts()}>{(g) => <div class="tile-ghost" style={insetStyle(g.rect)} aria-hidden="true" />}</For>
        </Show>
        <Show when={liveDrag()?.tileId && onActive(liveDrag()!.tileId!)}>
          <div class="drag-socket" style={socketStyle()} aria-hidden="true" />
        </Show>
        <DropPreview rectOf={(id) => (tiled() ? pxRect(id) : rowRect(id))} regionOf={(r) => fracPx(regionRect(r))} />
      </div>

      <Show when={!count()}>
        <Launcher />
      </Show>

      <Show when={tiled() && multi() && !workspace.maximized() && !monocle()}>
        <For each={dividerIds()}>
          {(id) => <Divider geom={() => dividerMap().get(id)} area={() => area} onResizing={setResizing} />}
        </For>
      </Show>

      {/* Where you are among the pages: one row for the whole workspace, at the top edge (in the
          grout on a foldable), so it never sits on a tile's title. */}
      <Show when={!tiled() && count() > visibleCount()}>
        <div
          class="ws-dots"
          role="img"
          aria-label={
            visibleCount() > 1
              ? `Tiles ${start() + 1} and ${start() + 2} of ${count()}`
              : `Tile ${start() + 1} of ${count()}`
          }
        >
          <For each={order()}>{(_, i) => <i classList={{ on: i() >= start() && i() < start() + visibleCount() }} />}</For>
        </div>
      </Show>

      {/* A carried tile resting at a side: the next page is that way. */}
      <Show when={liveDrag() && edgeSide()}>
        <div class="page-edge" data-side={edgeSide() < 0 ? "left" : "right"} aria-hidden="true">
          <Icon name={edgeSide() < 0 ? "chevronLeft" : "chevronRight"} size={18} />
        </div>
      </Show>

      {/* Gone while the pages slide (a swipe and its settling), so it never stands still over moving tiles. */}
      <Show when={mode() === "dual" && visibleCount() > 1 && !liveDrag()}>
        <DualHandle ratio={workspace.dualRatio} area={() => area} onResizing={setResizing} away={swiping() || snapping()} />
      </Show>

    </div>
  );
}

const ZONE_GAP = 4;

/**
 * Where a drop will land, drawn once for the whole workspace: half a tile for
 * an edge, the whole tile to swap or replace. It glides between zones (FLIP,
 * transform only) instead of blinking.
 */
function DropPreview(props: { rectOf: (id: string) => Rect; regionOf: (r: SnapRegion) => Rect }) {
  let box: HTMLDivElement | undefined;
  let prev: Rect | null = null;
  const target = (): { r: Rect; label: string; snap?: boolean } | null => {
    const h: DropHint | null = dropHint();
    const d = liveDrag();
    if (h && d && "snap" in h) return { r: props.regionOf(h.snap), label: SNAP_LABEL[h.snap], snap: true };
    if (!h || !d || !("paneId" in h)) return null;
    const t = props.rectOf(h.paneId);
    const half = (v: number) => v / 2 - ZONE_GAP / 2;
    const r: Rect =
      h.zone === "left"
        ? { x: t.x, y: t.y, w: half(t.w), h: t.h }
        : h.zone === "right"
          ? { x: t.x + t.w / 2 + ZONE_GAP / 2, y: t.y, w: half(t.w), h: t.h }
          : h.zone === "top"
            ? { x: t.x, y: t.y, w: t.w, h: half(t.h) }
            : h.zone === "bottom"
              ? { x: t.x, y: t.y + t.h / 2 + ZONE_GAP / 2, w: t.w, h: half(t.h) }
              : t;
    return { r, label: d.kind === "tile" ? MOVE_LABEL[h.zone] : ZONE_LABEL[h.zone] };
  };
  createEffect(() => {
    const t = target();
    if (!t) {
      prev = null;
      return;
    }
    const r = t.r;
    const o = prev;
    prev = r;
    queueMicrotask(() => {
      if (!box || !o || reducedMotion()) return;
      const dx = o.x + o.w / 2 - (r.x + r.w / 2);
      const dy = o.y + o.h / 2 - (r.y + r.h / 2);
      box.animate(
        [{ transform: `translate(${dx}px, ${dy}px) scale(${o.w / Math.max(1, r.w)}, ${o.h / Math.max(1, r.h)})` }, { transform: "none" }],
        { duration: 150, easing: "cubic-bezier(0.16, 0.84, 0.3, 1)" },
      );
    });
  });
  return (
    <Show when={target()}>
      {(t) => (
        <>
          <div
            ref={box}
            class="drop-preview"
            classList={{ "snap-zone": !!t().snap }}
            aria-hidden="true"
            style={{ left: `${t().r.x}px`, top: `${t().r.y}px`, width: `${t().r.w}px`, height: `${t().r.h}px` }}
          />
          <div class="drop-chip" aria-hidden="true" style={{ transform: `translate(${t().r.x + t().r.w / 2}px, ${t().r.y + Math.min(t().r.h / 2, 34)}px) translate(-50%, -50%)` }}>
            {t().label}
          </div>
        </>
      )}
    </Show>
  );
}

function PaneBody(props: { id: string }) {
  const content = () => workspace.panes[props.id]?.content;
  return (
    <Switch>
      <Match when={content()?.type === "chat"}>
        <ChatPane paneId={props.id} />
      </Match>
      <Match when={content()?.type === "whiteboard"}>
        <BoardSlot paneId={props.id} />
      </Match>
      <Match when={content()?.type === "note"}>
        <NoteSlot paneId={props.id} />
      </Match>
      <Match when={content()?.type === "file"}>
        <FileTile paneId={props.id} />
      </Match>
      <Match when={content()?.type === "terminal"}>
        <TerminalTile paneId={props.id} />
      </Match>
      <Match when={content()?.type === "draft" && (content() as { kind: string }).kind === "assist"}>
        <AssistTile paneId={props.id} />
      </Match>
      <Match when={content()?.type === "draft"}>
        <WebViewSlot paneId={props.id} />
      </Match>
      <Match when={content()?.type === "other"}>
        <OtherPane id={props.id} />
      </Match>
    </Switch>
  );
}

/** A tile another client made that this version can't show: kept as it is. */
function OtherPane(props: { id: string }) {
  return (
    <div class="placeholder-pane">
      <Icon name="mosaic" size={26} />
      <p>This tile was made by a newer Tessera and can't be shown here.</p>
      <button type="button" class="btn btn-sm" onClick={() => void requestClosePane(props.id)}>
        Close tile
      </button>
    </div>
  );
}

/** Tabs across the top of the workspace in the monocle layout. */
function MonocleTabs() {
  return (
    <div class="ws-tabs" role="tablist" aria-label="Tiles" style={{ height: `${TABS_H}px` }}>
      <For each={workspace.order()}>
        {(id) => {
          const c = () => workspace.panes[id]?.content;
          const sid = () => (c()?.type === "chat" ? (c() as { sessionId: string | null }).sessionId : null);
          const info = () => (sid() ? state.sessions[sid()!]?.info : undefined);
          const draftBot = () => {
            const x = c();
            return x?.type === "chat" ? botById(x.draft?.botId) : undefined;
          };
          const bot = () => botById(info()?.kind === "chat" ? info()?.bot_id : undefined) ?? draftBot();
          const card = () => paneCard(id);
          const label = () => card().title;
          const active = () => workspace.focused() === id;
          return (
            <div class="ws-tab" classList={{ active: active() }} data-pane-tab={id}>
              <button
                type="button"
                role="tab"
                aria-selected={active()}
                class="ws-tab-main"
                title={label()}
                onClick={() => focusPane(id)}
                onAuxClick={(e) => e.button === 1 && void requestClosePane(id)}
              >
                <Show
                  when={bot()}
                  fallback={
                    <Show
                      when={info() && isBusy(info())}
                      fallback={<Icon name={card().icon} size={13} />}
                    >
                      <Tessellate size={8} />
                    </Show>
                  }
                >
                  <BotAvatar bot={bot()} size={16} />
                </Show>
                <span class="ws-tab-label">{label()}</span>
                <Show when={paneStatus(id) === "running" && bot()}>
                  <Tessellate size={7} label="Working" />
                </Show>
                <Show when={paneStatus(id) === "approval"}>
                  <span class="tab-warn" aria-label="Needs approval" />
                </Show>
              </button>
              <button type="button" class="ws-tab-x" aria-label={`Close ${label()}`} onClick={() => void requestClosePane(id)}>
                <Icon name="x" size={12} />
              </button>
            </div>
          );
        }}
      </For>
      <button type="button" class="ws-tab-add icon-btn sm" aria-label="New tile" title="New tile (Alt+Enter)" onClick={() => newChat({ newPane: true })}>
        <Icon name="plus" size={14} />
      </button>
    </div>
  );
}

/**
 * The handle between the two tiles on a foldable (like split view): drag it
 * to share the width differently, double-tap for half and half. It sits in
 * the middle of the screen, clear of the edges the system's back gesture uses.
 */
function DualHandle(props: { ratio: () => number; area: () => HTMLDivElement; onResizing: (v: boolean) => void; away?: boolean }) {
  const [active, setActive] = createSignal(false);
  let lastTap = 0;
  let tapX = 0;
  const bounds = () => {
    const w = props.area().clientWidth || 1;
    const min = clamp(200 / w, 0.2, 0.4);
    return [min, 1 - min] as const;
  };
  const reset = () => setDualRatio(0.5);
  const onDown = (e: PointerEvent) => {
    if (e.button !== 0) return;
    // Not a swipe, not a pinch: the workspace never sees this finger.
    e.stopPropagation();
    e.preventDefault();
    const el = e.currentTarget as HTMLElement;
    try {
      el.setPointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }
    const rect = props.area().getBoundingClientRect();
    const x0 = e.clientX;
    let moved = false;
    setActive(true);
    props.onResizing(true);
    const move = (ev: PointerEvent) => {
      if (!moved && Math.abs(ev.clientX - x0) < 4) return;
      moved = true;
      const [lo, hi] = bounds();
      setDualRatio(clamp((ev.clientX - rect.left) / Math.max(1, rect.width), lo, hi));
    };
    const up = (ev: PointerEvent) => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
      setActive(false);
      props.onResizing(false);
      if (moved || ev.type === "pointercancel") return;
      // A double tap evens it out.
      if (ev.timeStamp - lastTap < 320 && Math.abs(ev.clientX - tapX) < 24) {
        lastTap = 0;
        reset();
      } else {
        lastTap = ev.timeStamp;
        tapX = ev.clientX;
      }
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  };
  const onKey = (e: KeyboardEvent) => {
    const [lo, hi] = bounds();
    if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      setDualRatio(clamp(props.ratio() + (e.key === "ArrowRight" ? 0.04 : -0.04), lo, hi));
    } else if (e.key === "Enter" || e.key === "=") {
      e.preventDefault();
      reset();
    }
  };
  return (
    <div
      class="dual-handle"
      classList={{ active: active(), away: !!props.away }}
      style={{ left: `${props.ratio() * 100}%` }}
      role="separator"
      aria-orientation="vertical"
      aria-valuenow={Math.round(props.ratio() * 100)}
      aria-valuemin={20}
      aria-valuemax={80}
      aria-label="Resize the two tiles"
      tabIndex={0}
      data-noswipe
      onPointerDown={onDown}
      onKeyDown={onKey}
      title="Drag to resize · double-tap for half and half"
    >
      <span class="dual-grip" aria-hidden="true" />
    </div>
  );
}

function Divider(props: { geom: () => DividerGeom | undefined; area: () => HTMLDivElement; onResizing: (v: boolean) => void }) {
  const style = () => {
    const g = props.geom();
    if (!g) return { display: "none" };
    const p = g.parent;
    // 12px to grab: the grout between the tiles and a sliver of each edge.
    if (g.dir === "h") {
      const x = p.x + p.w * g.ratio;
      return { left: `calc(${x * 100}% - 6px)`, top: `${p.y * 100}%`, height: `${p.h * 100}%`, width: "12px" };
    }
    const y = p.y + p.h * g.ratio;
    return { top: `calc(${y * 100}% - 6px)`, left: `${p.x * 100}%`, width: `${p.w * 100}%`, height: "12px" };
  };

  const onDown = (e: PointerEvent) => {
    const g = props.geom();
    if (!g || e.button !== 0) return;
    e.preventDefault();
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    props.onResizing(true);
    const rect = props.area().getBoundingClientRect();
    const move = (ev: PointerEvent) => {
      const gg = props.geom();
      if (!gg) return;
      const p = gg.parent;
      const total = gg.dir === "h" ? p.w * rect.width : p.h * rect.height;
      const origin = gg.dir === "h" ? rect.left + p.x * rect.width : rect.top + p.y * rect.height;
      const pos = (gg.dir === "h" ? ev.clientX : ev.clientY) - origin;
      const minFrac = Math.min(0.45, (gg.dir === "h" ? MIN_PANE_PX : 150) / Math.max(1, total));
      setDividerRatio(gg.target, clamp(pos / total, minFrac, 1 - minFrac));
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
      props.onResizing(false);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  };

  const onKey = (e: KeyboardEvent) => {
    const g = props.geom();
    if (!g) return;
    const dec = g.dir === "h" ? "ArrowLeft" : "ArrowUp";
    const inc = g.dir === "h" ? "ArrowRight" : "ArrowDown";
    if (e.key === dec || e.key === inc) {
      e.preventDefault();
      setDividerRatio(g.target, clamp(g.ratio + (e.key === inc ? 0.03 : -0.03), 0.1, 0.9));
    } else if (e.key === "Enter" || e.key === "=") {
      e.preventDefault();
      equalizeDivider(g.target);
    }
  };

  return (
    <div
      class="divider"
      classList={{ h: props.geom()?.dir === "h", v: props.geom()?.dir === "v" }}
      style={style()}
      role="separator"
      aria-orientation={props.geom()?.dir === "h" ? "vertical" : "horizontal"}
      aria-valuenow={Math.round((props.geom()?.ratio ?? 0.5) * 100)}
      aria-valuemin={10}
      aria-valuemax={90}
      aria-label="Resize tiles"
      tabIndex={0}
      onPointerDown={onDown}
      onKeyDown={onKey}
      onDblClick={() => {
        const g = props.geom();
        if (g) equalizeDivider(g.target);
      }}
      title="Drag to resize · double-click for equal sizes"
    >
      <span class="divider-grip" />
    </div>
  );
}

/** Where a note tile goes: an empty tile for the moment its code takes to arrive. */
function NoteSlot(props: { paneId: string }) {
  void noteTile.load();
  return (
    <Show when={noteTile.get()} fallback={<div class="board" />}>
      {(m) => {
        const Tile = m().NoteTile;
        return <Tile paneId={props.paneId} />;
      }}
    </Show>
  );
}
