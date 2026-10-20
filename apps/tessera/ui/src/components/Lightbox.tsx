// The picture viewer (see lib/viewer.ts): full screen, centred and fitted with
// its shape kept. Zoom with the wheel, a pinch, a double click or double tap,
// or + and −; drag to look around once zoomed. More than one picture in the
// chat: the arrows, the arrow keys or a sideways swipe go to the next one. A
// swipe down, Escape, the back gesture, a click beside the picture or the ✕
// close it. Save puts it in Downloads on a phone, where you pick on a
// computer.
import { Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import { saveBlob } from "../lib/files";
import { isDesktopApp } from "../lib/native";
import { closeViewer, fitSize, imageFileName, showImage, viewer } from "../lib/viewer";
import { base64Blob, clamp, reducedMotion, touchUi } from "../lib/util";
import { toastError } from "../lib/ui";
import { Icon, Spinner } from "./icons";
import { useBackToClose } from "./primitives";
import "../styles/viewer.css";

/** Room kept around the picture where the controls sit (wide screens with a mouse). */
const PAD_WIDE = { x: 72, y: 60 };
const SWIPE = 0.18; // of the width: enough to go to the next picture
const FLICK = 0.35; // px/ms
/** Down this far (or flicked down), the viewer closes. */
const DISMISS = 110;
const ease = "260ms cubic-bezier(0.16, 0.84, 0.3, 1)";

export default function Lightbox() {
  let root!: HTMLDivElement;
  let stage!: HTMLDivElement;
  let img: HTMLImageElement | undefined;
  const v = () => viewer();
  const cur = createMemo(() => {
    const x = v();
    return x ? x.images[x.index] : undefined;
  });
  const count = () => v()?.images.length ?? 0;
  const index = () => v()?.index ?? 0;

  const [box, setBox] = createSignal({ w: window.innerWidth, h: window.innerHeight });
  const [nat, setNat] = createSignal({ w: 0, h: 0 });
  const [loaded, setLoaded] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  const [s, setS] = createSignal(1);
  const [t, setT] = createSignal({ x: 0, y: 0 });
  /** A swipe in progress: sideways (next/previous) or down (close). */
  const [drag, setDrag] = createSignal({ x: 0, y: 0 });
  const [anim, setAnim] = createSignal(false);
  const [chrome, setChrome] = createSignal(true);
  const [closing, setClosing] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const zoomed = () => s() > 1.01;

  const wide = () => box().w >= 640;
  /** The picture at fit size, centred in the room left for it. */
  const fit = createMemo(() => {
    const b = box();
    // A touch screen uses all of it (no arrows at the sides; the bar sits over the top).
    const pad = wide() && !touchUi() ? PAD_WIDE : { x: 0, y: 0 };
    const n = nat();
    // An image without a size of its own (some SVGs): most of the screen.
    const nw = n.w || b.w * 0.8;
    const nh = n.h || b.h * 0.8;
    return { ...fitSize(nw, nh, Math.max(1, b.w - pad.x * 2), Math.max(1, b.h - pad.y * 2), 1), bw: b.w, bh: b.h };
  });
  /** Zoomed this far, a picture shows three screen pixels per pixel of its own (at least 4×). */
  const maxS = () => clamp(3 / Math.max(0.001, fit().k), 4, 48);

  const clampPan = (x: number, y: number, sc: number) => {
    const f = fit();
    const mx = Math.max(0, (f.w * sc - f.bw) / 2);
    const my = Math.max(0, (f.h * sc - f.bh) / 2);
    return { x: clamp(x, -mx, mx), y: clamp(y, -my, my) };
  };
  /** Zoom to `next`, keeping the point (px, py) of the screen where it is. */
  const zoomAt = (next: number, px: number, py: number, smooth = false) => {
    next = clamp(next, 1, maxS());
    const f = fit();
    const cx = px - f.bw / 2;
    const cy = py - f.bh / 2;
    const k = next / s();
    const p = t();
    setAnim(smooth && !reducedMotion());
    setS(next);
    setT(next <= 1.001 ? { x: 0, y: 0 } : clampPan(cx - (cx - p.x) * k, cy - (cy - p.y) * k, next));
  };
  const reset = (smooth = true) => {
    setAnim(smooth && !reducedMotion());
    setS(1);
    setT({ x: 0, y: 0 });
  };
  /** Double click / double tap: in to fill the screen (a long screenshot: its width), at least 2.5×; again: back. */
  const toggleZoom = (px: number, py: number) => {
    if (zoomed()) return reset();
    const f = fit();
    const fill = Math.max(f.bw / Math.max(1, f.w), f.bh / Math.max(1, f.h));
    zoomAt(Math.max(2.5, Math.min(fill, maxS())), px, py, true);
  };

  // A new picture: fresh state.
  createEffect(
    on(
      () => cur()?.src,
      () => {
        setLoaded(false);
        setFailed(false);
        setNat({ w: 0, h: 0 });
        reset(false);
      },
    ),
  );
  const onLoad = (e: Event) => {
    const el = e.currentTarget as HTMLImageElement;
    setNat({ w: el.naturalWidth, h: el.naturalHeight });
    // Decoded off the main thread before it is shown: a big photo never stalls a frame.
    const done = () => {
      if (el === img) setLoaded(true);
    };
    if (typeof el.decode === "function") el.decode().then(done, done);
    else done();
  };

  const go = (dir: 1 | -1) => {
    const i = index() + dir;
    if (i < 0 || i >= count()) return false;
    showImage(i);
    // The next picture slides in from the side it comes from.
    setAnim(false);
    setDrag({ x: dir * Math.min(box().w * 0.5, 320), y: 0 });
    requestAnimationFrame(() =>
      requestAnimationFrame(() => {
        setAnim(!reducedMotion());
        setDrag({ x: 0, y: 0 });
      }),
    );
    return true;
  };

  let releaseBack = () => {};
  const close = () => {
    if (closing()) return;
    setClosing(true);
    releaseBack();
    closeViewer();
  };
  /** Swiped away: the picture leaves downward, then the viewer closes. */
  const dismiss = () => {
    if (reducedMotion()) return close();
    setAnim(true);
    setDrag({ x: 0, y: box().h });
    setTimeout(close, 180);
  };

  const save = async () => {
    const c = cur();
    if (!c || saving()) return;
    setSaving(true);
    try {
      const m = /^data:([^;,]+);base64,(.*)$/s.exec(c.src);
      const mime = m?.[1] ?? "image/png";
      const blob = m ? base64Blob(m[2], mime) : await (await fetch(c.src)).blob();
      await saveBlob(imageFileName(c.src), blob.type || mime, blob);
    } catch (e) {
      toastError(e, "Could not save the picture");
    } finally {
      setSaving(false);
    }
  };

  // ── pointers ────────────────────────────────────────────────────────────
  const pts = new Map<number, { x: number; y: number }>();
  type Gesture =
    | { kind: "tap"; x: number; y: number; t: number; onImage: boolean; touch: boolean }
    | { kind: "pan"; x: number; y: number; moved: boolean; onImage: boolean; touch: boolean; t: number }
    | { kind: "swipe"; x: number; y: number; t: number; lock: "x" | "y" | null; samples: { x: number; y: number; t: number }[] }
    | { kind: "pinch"; d: number; s: number; local: { x: number; y: number } };
  let g: Gesture | null = null;
  let lastTap = 0;
  let lastPointer = "mouse";
  let tapTimer: ReturnType<typeof setTimeout> | undefined;
  const local = (e: PointerEvent) => {
    const r = stage.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };
  const onImageAt = (p: { x: number; y: number }) => {
    const f = fit();
    const tt = t();
    const w = f.w * s();
    const h = f.h * s();
    const cx = f.bw / 2 + tt.x;
    const cy = f.bh / 2 + tt.y;
    return Math.abs(p.x - cx) <= w / 2 && Math.abs(p.y - cy) <= h / 2;
  };
  const twoFingers = () => {
    const [a, b] = [...pts.values()];
    return { d: Math.hypot(a.x - b.x, a.y - b.y), m: { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 } };
  };
  const startPinch = () => {
    const { d, m } = twoFingers();
    const f = fit();
    const tt = t();
    // The picture's own point under the fingers, relative to its centre at this zoom.
    g = { kind: "pinch", d: Math.max(1, d), s: s(), local: { x: (m.x - f.bw / 2 - tt.x) / s(), y: (m.y - f.bh / 2 - tt.y) / s() } };
    setDrag({ x: 0, y: 0 });
  };

  const onDown = (e: PointerEvent) => {
    if ((e.target as Element).closest(".lb-bar, .lb-nav")) return;
    if (e.pointerType === "mouse" && e.button !== 0) return;
    lastPointer = e.pointerType;
    e.preventDefault();
    // The first finger down: a finger whose release never arrived (the app went to the background
    // mid-touch) is forgotten, or it would turn every later tap into a pinch.
    if (e.pointerType !== "mouse" && e.isPrimary) {
      pts.clear();
      g = null;
    }
    const p = local(e);
    pts.set(e.pointerId, p);
    try {
      stage.setPointerCapture(e.pointerId);
    } catch {
      /* gone already */
    }
    if (pts.size === 2) return startPinch();
    if (pts.size > 2) return;
    const touch = e.pointerType !== "mouse";
    const onImage = onImageAt(p);
    if (zoomed()) g = { kind: "pan", x: p.x, y: p.y, moved: false, onImage, touch, t: e.timeStamp };
    else if (touch) g = { kind: "swipe", x: p.x, y: p.y, t: e.timeStamp, lock: null, samples: [{ ...p, t: e.timeStamp }] };
    else g = { kind: "tap", x: p.x, y: p.y, t: e.timeStamp, onImage, touch };
  };
  const onMove = (e: PointerEvent) => {
    const prev = pts.get(e.pointerId);
    if (!prev) return;
    const p = local(e);
    pts.set(e.pointerId, p);
    if (!g) return;
    if (g.kind === "pinch") {
      if (pts.size < 2) return;
      const { d, m } = twoFingers();
      const f = fit();
      const next = clamp(g.s * (d / g.d), 0.6, maxS() * 1.15);
      setAnim(false);
      setS(next);
      setT({ x: m.x - f.bw / 2 - g.local.x * next, y: m.y - f.bh / 2 - g.local.y * next });
      return;
    }
    if (g.kind === "pan") {
      if (Math.hypot(p.x - g.x, p.y - g.y) > 6) g.moved = true;
      setAnim(false);
      const cur = t();
      setT(clampPan(cur.x + (p.x - prev.x), cur.y + (p.y - prev.y), s()));
      return;
    }
    if (g.kind === "tap") {
      if (Math.hypot(p.x - g.x, p.y - g.y) > 6) g = null;
      return;
    }
    // swipe
    const dx = p.x - g.x;
    const dy = p.y - g.y;
    g.samples.push({ ...p, t: e.timeStamp });
    if (g.samples.length > 6) g.samples.shift();
    if (!g.lock) {
      if (Math.abs(dx) < 8 && Math.abs(dy) < 8) return;
      g.lock = Math.abs(dx) > Math.abs(dy) ? "x" : "y";
    }
    setAnim(false);
    if (g.lock === "x") {
      const atEnd = (dx > 0 && index() === 0) || (dx < 0 && index() === count() - 1);
      setDrag({ x: atEnd ? dx * 0.3 : dx, y: 0 });
    } else setDrag({ x: 0, y: dy > 0 ? dy : dy * 0.25 });
  };
  const onUp = (e: PointerEvent) => {
    const had = pts.delete(e.pointerId);
    if (!had) return;
    const p = local(e);
    if (g?.kind === "pinch") {
      if (pts.size === 1) {
        // One finger stays: it pans from here.
        const [q] = [...pts.values()];
        g = { kind: "pan", x: q.x, y: q.y, moved: true, onImage: true, touch: true, t: e.timeStamp };
      } else g = null;
      // Settle back inside the limits.
      if (s() < 1.01) reset();
      else if (s() > maxS()) zoomAt(maxS(), p.x, p.y, true);
      else {
        setAnim(!reducedMotion());
        setT((q) => clampPan(q.x, q.y, s()));
      }
      return;
    }
    if (pts.size) return;
    const gg = g;
    g = null;
    if (!gg) return;
    if (gg.kind === "swipe") {
      const first = gg.samples[0];
      const dt = Math.max(1, e.timeStamp - first.t);
      const vx = (p.x - first.x) / dt;
      const vy = (p.y - first.y) / dt;
      const dx = p.x - gg.x;
      const dy = p.y - gg.y;
      if (gg.lock === "x") {
        const dir = dx < -box().w * SWIPE || vx < -FLICK ? 1 : dx > box().w * SWIPE || vx > FLICK ? -1 : 0;
        if (dir && go(dir as 1 | -1)) return;
        setAnim(!reducedMotion());
        setDrag({ x: 0, y: 0 });
        return;
      }
      if (gg.lock === "y") {
        if (dy > DISMISS || (vy > 0.5 && dy > 30)) return dismiss();
        setAnim(!reducedMotion());
        setDrag({ x: 0, y: 0 });
        return;
      }
      return tap(p, onImageAt(p), true, e.timeStamp);
    }
    if (gg.kind === "pan") {
      if (!gg.moved && e.timeStamp - gg.t < 450) tap(p, gg.onImage, gg.touch, e.timeStamp);
      return;
    }
    if (gg.kind === "tap") tap(p, gg.onImage, gg.touch, e.timeStamp);
  };
  const onCancel = (e: PointerEvent) => {
    pts.delete(e.pointerId);
    if (pts.size) return;
    g = null;
    setAnim(!reducedMotion());
    setDrag({ x: 0, y: 0 });
    if (s() < 1.01) reset();
  };
  /**
   * A tap or click: once beside the picture closes; once on it (touch) shows or hides the controls;
   * two taps quickly zoom (a mouse's double click is the browser's own `dblclick`).
   */
  const tap = (p: { x: number; y: number }, onImage: boolean, touch: boolean, ts: number) => {
    if (touch && ts - lastTap < 320) {
      lastTap = 0;
      clearTimeout(tapTimer);
      toggleZoom(p.x, p.y);
      return;
    }
    lastTap = ts;
    if (!onImage && !zoomed()) {
      // Beside the picture: close (a mouse at once; a finger once it's clear this isn't a double tap).
      if (!touch) return close();
      tapTimer = setTimeout(close, 280);
      return;
    }
    if (touch) tapTimer = setTimeout(() => setChrome(!chrome()), 280);
  };

  // ── keys ────────────────────────────────────────────────────────────────
  const onKey = (e: KeyboardEvent) => {
    // The app under the viewer gets no keys while it is open.
    e.stopPropagation();
    const f = fit();
    const k = e.key;
    let handled = true;
    if (k === "Escape") close();
    else if (k === "ArrowRight" && !e.altKey) go(1);
    else if (k === "ArrowLeft" && !e.altKey) go(-1);
    else if (k === "+" || k === "=") zoomAt(s() * 1.6, f.bw / 2, f.bh / 2, true);
    else if (k === "-" || k === "_") zoomAt(s() / 1.6, f.bw / 2, f.bh / 2, true);
    else if (k === "0") reset();
    else if (k === "Tab") {
      // Focus stays in the viewer.
      const list = Array.from(root.querySelectorAll<HTMLElement>("button:not(:disabled)")).filter((b) => b.offsetParent !== null);
      if (list.length) {
        const i = list.indexOf(document.activeElement as HTMLElement);
        list[(i + (e.shiftKey ? -1 : 1) + list.length) % list.length].focus();
      }
    } else handled = false;
    if (handled) e.preventDefault();
  };

  onMount(() => {
    const prevFocus = document.activeElement as HTMLElement | null;
    releaseBack = useBackToClose(() => {
      setClosing(true);
      closeViewer();
    });
    // The Linux desktop app paints the app's scrollbars over anything above them.
    const cover = isDesktopApp && /Linux/.test(navigator.userAgent);
    if (cover) document.documentElement.classList.add("viewer-open");
    // Toasts (a phone shows them at the top) move out of the way of the viewer's controls.
    document.documentElement.classList.add("has-viewer");
    window.addEventListener("keydown", onKey, true);
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      if (g) return;
      const p = { x: e.clientX - stage.getBoundingClientRect().left, y: e.clientY - stage.getBoundingClientRect().top };
      const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? box().h : 1;
      const k = Math.exp(-e.deltaY * unit * (e.ctrlKey ? 0.01 : 0.0022));
      setAnim(false);
      zoomAt(s() * k, p.x, p.y);
    };
    stage.addEventListener("wheel", onWheel, { passive: false });
    const onDbl = (e: MouseEvent) => {
      // Touch screens send one too after a double tap: theirs is handled in `tap`.
      if (lastPointer !== "mouse") return;
      const r = stage.getBoundingClientRect();
      toggleZoom(e.clientX - r.left, e.clientY - r.top);
    };
    stage.addEventListener("dblclick", onDbl);
    // Pointer handlers are the stage's own (not delegated): nothing under the viewer sees its touches.
    stage.addEventListener("pointerdown", onDown);
    stage.addEventListener("pointermove", onMove);
    stage.addEventListener("pointerup", onUp);
    stage.addEventListener("pointercancel", onCancel);
    const ro = new ResizeObserver(() => {
      setBox({ w: root.clientWidth || window.innerWidth, h: root.clientHeight || window.innerHeight });
      setT((q) => clampPan(q.x, q.y, s()));
    });
    ro.observe(root);
    requestAnimationFrame(() => root.focus({ preventScroll: true }));
    onCleanup(() => {
      clearTimeout(tapTimer);
      ro.disconnect();
      window.removeEventListener("keydown", onKey, true);
      stage.removeEventListener("wheel", onWheel);
      stage.removeEventListener("dblclick", onDbl);
      stage.removeEventListener("pointerdown", onDown);
      stage.removeEventListener("pointermove", onMove);
      stage.removeEventListener("pointerup", onUp);
      stage.removeEventListener("pointercancel", onCancel);
      if (cover) document.documentElement.classList.remove("viewer-open");
      document.documentElement.classList.remove("has-viewer");
      releaseBack();
      prevFocus?.focus?.({ preventScroll: true });
    });
  });

  const imgStyle = () => {
    const f = fit();
    const d = drag();
    // Pulled down to close: the picture shrinks a little as it goes.
    const pull = d.y > 0 ? 1 - Math.min(0.25, d.y / (f.bh * 2.5)) : 1;
    return {
      left: `${(f.bw - f.w) / 2}px`,
      top: `${(f.bh - f.h) / 2}px`,
      width: `${f.w}px`,
      height: `${f.h}px`,
      transform: `translate(${t().x + d.x}px, ${t().y + d.y}px) scale(${s() * pull})`,
      transition: anim() ? `transform ${ease}, opacity 180ms` : "opacity 180ms",
    };
  };
  const backdrop = () => {
    const y = Math.max(0, drag().y);
    return y ? Math.max(0.15, 1 - y / (box().h * 0.7)) : 1;
  };

  return (
    <Portal>
      <div
        ref={root}
        class="lightbox-view"
        classList={{ "lb-zoomed": zoomed(), "lb-bare": !chrome(), "lb-touch": touchUi(), "lb-wide": wide() }}
        role="dialog"
        aria-modal="true"
        aria-label="Picture"
        tabIndex={-1}
        data-index={index()}
        data-scale={s().toFixed(3)}
      >
        <div class="lb-backdrop" style={{ opacity: backdrop() }} />
        <div ref={stage} class="lb-stage">
          <Show when={cur()} keyed>
            {(c) => (
              <Show when={!failed()} fallback={<div class="lb-error">This picture couldn't be shown. You can still save it.</div>}>
                <img
                  ref={(el) => (img = el)}
                  class="lb-img"
                  classList={{ ready: loaded() }}
                  src={c.src}
                  alt={c.alt}
                  draggable={false}
                  decoding="async"
                  style={imgStyle()}
                  onLoad={onLoad}
                  onError={() => setFailed(true)}
                  onTransitionEnd={() => setAnim(false)}
                />
              </Show>
            )}
          </Show>
          <Show when={!loaded() && !failed()}>
            <div class="lb-spin">
              <Spinner size={22} />
            </div>
          </Show>
        </div>
        <div class="lb-bar" onDblClick={(e) => e.stopPropagation()}>
          <span class="lb-count" aria-live="polite">
            {count() > 1 ? `${index() + 1} / ${count()}` : ""}
          </span>
          <span class="grow" />
          <Show when={!touchUi()}>
            <button type="button" class="lb-btn" aria-label="Zoom out" data-tip="Zoom out" data-kbd="−" disabled={!zoomed()} onClick={() => zoomAt(s() / 1.6, fit().bw / 2, fit().bh / 2, true)}>
              <Icon name="zoomOut" size={17} />
            </button>
            <button type="button" class="lb-btn lb-pct" aria-label="Fit to screen" data-tip="Fit to screen" data-kbd="0" disabled={!zoomed()} onClick={() => reset()}>
              {Math.round(s() * fit().k * 100)}%
            </button>
            <button type="button" class="lb-btn" aria-label="Zoom in" data-tip="Zoom in" data-kbd="+" disabled={s() >= maxS() - 0.01} onClick={() => zoomAt(s() * 1.6, fit().bw / 2, fit().bh / 2, true)}>
              <Icon name="zoomIn" size={17} />
            </button>
          </Show>
          <button type="button" class="lb-btn" aria-label="Save picture" data-tip="Save" disabled={saving()} onClick={() => void save()}>
            <Icon name="download" size={18} />
          </button>
          <button type="button" class="lb-btn lb-close" aria-label="Close" data-tip="Close" data-kbd="Esc" onClick={close}>
            <Icon name="x" size={20} />
          </button>
        </div>
        <Show when={count() > 1}>
          <button type="button" class="lb-nav lb-prev" aria-label="Previous picture" data-tip="Previous" data-kbd="←" disabled={index() === 0} onClick={() => go(-1)}>
            <Icon name="chevronLeft" size={22} />
          </button>
          <button type="button" class="lb-nav lb-next" aria-label="Next picture" data-tip="Next" data-kbd="→" disabled={index() === count() - 1} onClick={() => go(1)}>
            <Icon name="chevronRight" size={22} />
          </button>
        </Show>
      </div>
    </Portal>
  );
}
