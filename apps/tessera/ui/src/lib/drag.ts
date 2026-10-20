// Pointer-event drags: tiles lifted by their header, and chats/bots dragged
// from the sidebar. One engine so both look and feel the same.
//
// · A drag starts after a few pixels (so a click on the title still renames),
//   or after a long-press on touch (so a swipe still swipes).
// · The dragged thing follows the pointer through a rAF-batched transform:
//   the tile itself lifts like a card picked up by the grab point (about
//   half size, flat — 2D only, at most a 1.5° lean into the motion — with a
//   soft shadow); sidebar items get a small card. Nothing else re-renders per
//   move.
// · Hit testing runs once per frame; the drop hint only updates on change.
// · Esc cancels and the tile glides back; a successful drop hands over to the
//   workspace's FLIP (see `landing`), so the tile settles from where it was.
import { createSignal } from "solid-js";
import { dropHint, dropPayload, hitTestAt, setDropHint, type DragPayload, type DropHint } from "./dnd";
import { reducedMotion } from "./util";

export interface LiveDrag {
  kind: "tile" | "payload";
  /** The lifted tile. */
  tileId?: string;
  payload?: DragPayload;
  label: string;
}

export const [liveDrag, setLiveDrag] = createSignal<LiveDrag | null>(null);

/** Where the dragged thing was on screen when it was dropped (client px). */
export interface Landing {
  /** Visual centre. */
  cx: number;
  cy: number;
  /** Visual size. */
  w: number;
  h: number;
  rot: number;
}

export interface DragSpec {
  kind: "tile" | "payload";
  label: string;
  /** kind "tile": the tile element that lifts. */
  tile?: HTMLElement;
  tileId?: string;
  payload?: DragPayload;
  /** Resolve what is under the pointer. */
  hitTest: (x: number, y: number) => DropHint | null;
  /** Commit the drop; false means nothing happened (the tile glides back). */
  drop: (hint: DropHint, landing: Landing) => boolean;
  /** Touch: hold this long to pick up (ms). Without it touch drags like a mouse. */
  longPress?: number;
  /** Pixels to move before a mouse drag starts. */
  threshold?: number;
  /** Each frame of a live drag, with the pointer (e.g. turning pages at the screen's sides). */
  onFrame?: (x: number, y: number) => void;
  /** How far the lifted tile's own container moved since the drag began (its page turned): kept under the pointer. */
  offset?: () => { x: number; y: number };
}

const EASE_OUT = "cubic-bezier(0.16, 0.84, 0.3, 1)";
const LIFT_MS = 170;

const sameHint = (a: DropHint | null, b: DropHint | null) => {
  if (!a || !b) return a === b;
  if ("ws" in a) return "ws" in b && a.ws === b.ws;
  if ("launcher" in a) return "launcher" in b;
  if ("snap" in a) return "snap" in b && a.snap === b.snap;
  return "paneId" in b && "paneId" in a && a.paneId === b.paneId && a.zone === b.zone;
};

/** The press being tracked; a new press replaces one that never became a drag. */
let current: { live: () => boolean; stop: () => void; poke: () => void } | null = null;

/** Run a frame of the live drag now (what's under the pointer changed without it moving). */
export const pokeDrag = () => current?.poke();

/** Swallow the click that follows a drag (e.g. on the title button or a sidebar row). */
function swallowNextClick() {
  const swallow = (ev: MouseEvent) => {
    ev.stopPropagation();
    ev.preventDefault();
  };
  window.addEventListener("click", swallow, { capture: true, once: true });
  setTimeout(() => window.removeEventListener("click", swallow, { capture: true }), 0);
}

/** Start tracking a possible drag from a pointerdown. */
export function pointerDrag(e: PointerEvent, spec: DragSpec) {
  if (e.pointerType === "mouse" && e.button !== 0) return;
  if (current) {
    if (current.live()) return;
    current.stop();
  }
  const handle = e.currentTarget as HTMLElement;
  const pid = e.pointerId;
  const sx = e.clientX;
  const sy = e.clientY;
  const touch = e.pointerType === "touch";
  const hold = touch ? spec.longPress : undefined;
  const threshold = spec.threshold ?? 5;
  let px = sx;
  let py = sy;
  let live = false;
  let done = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let raf = 0;
  let t0 = 0;
  let lastX = sx;
  let vx = 0;
  let el: HTMLElement | null = null;
  let r0: DOMRect | null = null;
  let scaleTo = 1;
  let scale = 1;
  let rot = 0;

  const transformNow = () => {
    const o = spec.offset?.() ?? { x: 0, y: 0 };
    return spec.kind === "tile"
      ? `translate(${px - sx + o.x}px, ${py - sy + o.y}px) scale(${scale}) rotate(${rot}deg)`
      : `translate(${px + 14}px, ${py + 12}px) scale(${scale}) rotate(${rot}deg)`;
  };

  const frame = (t: number) => {
    raf = 0;
    if (!live || !el) return;
    const k = reducedMotion() ? 1 : Math.min(1, (t - t0) / LIFT_MS);
    const ease = 1 - Math.pow(1 - k, 3);
    scale = (spec.kind === "tile" ? 1 : 0.85) + (scaleTo - (spec.kind === "tile" ? 1 : 0.85)) * ease;
    // Lean a touch into the direction of travel (never more than 1.5°).
    vx = vx * 0.75 + (px - lastX) * 0.25;
    lastX = px;
    rot = reducedMotion() ? 0 : Math.max(-1.5, Math.min(1.5, (spec.kind === "tile" ? 0.4 : 0.8) + vx * 0.12));
    el.style.transform = transformNow();
    spec.onFrame?.(px, py);
    const h = spec.hitTest(px, py);
    if (!sameHint(h, dropHint())) setDropHint(h);
    if (k < 1 || Math.abs(vx) > 0.05) schedule();
  };
  const schedule = () => {
    if (!raf) raf = requestAnimationFrame(frame);
  };

  const activate = () => {
    if (live || done) return;
    live = true;
    if (touch) navigator.vibrate?.(12);
    try {
      handle.setPointerCapture(pid);
    } catch {
      /* the pointer is gone */
    }
    document.documentElement.classList.add("dragging-now");
    if (spec.kind === "tile" && spec.tile) {
      el = spec.tile;
      r0 = el.getBoundingClientRect();
      // A card about half the tile's size, so what's under it stays visible.
      scaleTo = Math.max(0.5, Math.min(0.6, 560 / Math.max(1, r0.width), 520 / Math.max(1, r0.height)));
      el.style.transformOrigin = `${sx - r0.left}px ${sy - r0.top}px`;
      el.classList.add("lifted");
    } else {
      el = document.createElement("div");
      el.className = "drag-card";
      const mark = document.createElement("span");
      mark.className = "drag-card-mark";
      const text = document.createElement("span");
      text.textContent = spec.label;
      el.append(mark, text);
      el.style.transform = `translate(${px + 14}px, ${py + 12}px) scale(0.85)`;
      document.body.append(el);
      scaleTo = 0.96;
    }
    setLiveDrag({ kind: spec.kind, tileId: spec.tileId, payload: spec.payload, label: spec.label });
    t0 = performance.now();
    lastX = px;
    schedule();
  };

  const landing = (): Landing => {
    if (spec.kind === "tile" && r0) {
      // The grab point stays under the pointer; the tile scales around it.
      const gx = sx - r0.left;
      const gy = sy - r0.top;
      return {
        cx: px - gx * scale + (r0.width * scale) / 2,
        cy: py - gy * scale + (r0.height * scale) / 2,
        w: r0.width * scale,
        h: r0.height * scale,
        rot,
      };
    }
    return { cx: px, cy: py, w: 0, h: 0, rot };
  };

  const teardown = () => {
    done = true;
    if (current === me) current = null;
    clearTimeout(timer);
    cancelAnimationFrame(raf);
    raf = 0;
    window.removeEventListener("pointermove", onMove, true);
    window.removeEventListener("pointerup", onUp, true);
    window.removeEventListener("pointercancel", onCancel, true);
    window.removeEventListener("pointerdown", onDownAgain, true);
    document.removeEventListener("visibilitychange", onHidden);
    window.removeEventListener("keydown", onKey, true);
    window.removeEventListener("touchmove", noScroll);
    window.removeEventListener("contextmenu", noMenu, true);
    document.documentElement.classList.remove("dragging-now");
    setDropHint(null);
    setLiveDrag(null);
  };

  /** The tile glides back to its place; a card fades where it started. */
  const flyBack = () => {
    const node = el;
    if (!node) return;
    const from = node.style.transform;
    if (spec.kind === "tile") {
      node.style.transform = "";
      if (reducedMotion()) {
        node.classList.remove("lifted");
        node.style.transformOrigin = "";
        return;
      }
      const a = node.animate([{ transform: from }, { transform: "none" }], { duration: 240, easing: EASE_OUT });
      a.onfinish = a.oncancel = () => {
        node.classList.remove("lifted");
        node.style.transformOrigin = "";
      };
    } else {
      const a = node.animate(
        [
          { transform: from, opacity: 1 },
          { transform: `translate(${sx + 14}px, ${sy + 12}px) scale(0.85)`, opacity: 0 },
        ],
        { duration: reducedMotion() ? 1 : 220, easing: EASE_OUT, fill: "forwards" },
      );
      a.onfinish = a.oncancel = () => node.remove();
    }
  };

  /** Dropped: the workspace FLIP takes over for tiles; a card sinks into its target. */
  const settle = () => {
    const node = el;
    if (!node) return;
    if (spec.kind === "tile") {
      node.style.transform = "";
      node.style.transformOrigin = "";
      node.classList.remove("lifted");
    } else {
      const a = node.animate(
        [
          { transform: node.style.transform, opacity: 1 },
          { transform: `translate(${px + 14}px, ${py + 12}px) scale(0.7)`, opacity: 0 },
        ],
        { duration: reducedMotion() ? 1 : 160, easing: EASE_OUT, fill: "forwards" },
      );
      a.onfinish = a.oncancel = () => node.remove();
    }
  };

  const onMove = (ev: PointerEvent) => {
    if (ev.pointerId !== pid) return;
    px = ev.clientX;
    py = ev.clientY;
    if (!live) {
      const dist = Math.hypot(px - sx, py - sy);
      if (hold !== undefined) {
        // Moving before the long-press fires is a swipe or a scroll, not a drag.
        if (dist > 8) {
          teardown();
        }
        return;
      }
      if (dist < threshold) return;
      activate();
    }
    ev.preventDefault();
    schedule();
  };
  const onUp = (ev: PointerEvent) => {
    if (ev.pointerId !== pid) return;
    const wasLive = live;
    const hint = dropHint();
    const land = wasLive ? landing() : null;
    teardown();
    if (!wasLive) return;
    swallowNextClick();
    let ok = false;
    if (hint && land) {
      try {
        ok = spec.drop(hint, land);
      } catch (err) {
        console.error(err);
      }
    }
    if (ok) settle();
    else flyBack();
  };
  const onCancel = (ev: PointerEvent) => {
    if (ev.pointerId !== pid) return;
    const wasLive = live;
    teardown();
    if (wasLive) flyBack();
  };
  /**
   * The press can't still be down: a new first press of the same kind came, or the app went
   * to the background. Its release never reached the page (a system gesture took the touch,
   * the screen folded): the tile goes back instead of staying lifted, and nothing else
   * waits on a drag that will never end.
   */
  const abandon = () => {
    const wasLive = live;
    teardown();
    if (wasLive) flyBack();
  };
  const onDownAgain = (ev: PointerEvent) => {
    if (ev.isPrimary && ev.pointerType === e.pointerType) abandon();
  };
  const onHidden = () => {
    if (document.visibilityState === "hidden") abandon();
  };
  const onKey = (ev: KeyboardEvent) => {
    if (ev.key !== "Escape" || !live) return;
    ev.preventDefault();
    ev.stopPropagation();
    teardown();
    flyBack();
    // The button is still down: its click must not rename or open anything.
    window.addEventListener("pointerup", () => swallowNextClick(), { capture: true, once: true });
  };
  // Once picked up, the finger moves the tile, not the page.
  const noScroll = (ev: TouchEvent) => {
    if (live && ev.cancelable) ev.preventDefault();
  };
  const noMenu = (ev: Event) => {
    if (live || timer) ev.preventDefault();
  };

  const me = { live: () => live, stop: teardown, poke: () => live && schedule() };
  current = me;
  window.addEventListener("pointermove", onMove, true);
  window.addEventListener("pointerup", onUp, true);
  window.addEventListener("pointercancel", onCancel, true);
  window.addEventListener("pointerdown", onDownAgain, true);
  document.addEventListener("visibilitychange", onHidden);
  window.addEventListener("keydown", onKey, true);
  window.addEventListener("touchmove", noScroll, { passive: false });
  window.addEventListener("contextmenu", noMenu, true);
  if (hold !== undefined) timer = setTimeout(activate, hold);
}

/** Drag a chat or bot out of the sidebar (mouse and pen; touch rows scroll and long-press for a menu). */
export function dragItem(e: PointerEvent, payload: DragPayload, label: string) {
  if (e.pointerType === "touch") return;
  if ((e.target as HTMLElement).closest("input, textarea, [data-nodrag]")) return;
  pointerDrag(e, {
    kind: "payload",
    label,
    payload,
    hitTest: (x, y) => hitTestAt(x, y),
    drop: (hint) => dropPayload(payload, hint),
  });
}
