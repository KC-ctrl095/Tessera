// An image, fitted to its tile. Click (or double-tap) zooms in at that point
// and back; drag pans; the wheel and a pinch zoom smoothly. At fit size a
// horizontal swipe still goes to the next tile on phones.
import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { clamp, reducedMotion } from "../../lib/util";
import { Icon } from "../icons";

const MAX = 8;

export function ImageViewer(props: { src: string; alt: string }) {
  let box!: HTMLDivElement;
  let img!: HTMLImageElement;
  const [s, setS] = createSignal(1);
  const [t, setT] = createSignal({ x: 0, y: 0 });
  const [anim, setAnim] = createSignal(false);
  const [loaded, setLoaded] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  const zoomed = () => s() > 1.01;

  /** The image at fit size (px), centred in the box. */
  const fit = () => {
    const bw = box?.clientWidth || 1;
    const bh = box?.clientHeight || 1;
    const nw = img?.naturalWidth || bw;
    const nh = img?.naturalHeight || bh;
    // Fit, but never blow a small image up past twice its size just to fill the tile.
    const k = Math.min(bw / nw, bh / nh, 2);
    return { w: nw * k, h: nh * k, bw, bh };
  };
  const clampPan = (x: number, y: number, sc: number) => {
    const f = fit();
    const mx = Math.max(0, (f.w * sc - f.bw) / 2);
    const my = Math.max(0, (f.h * sc - f.bh) / 2);
    return { x: clamp(x, -mx, mx), y: clamp(y, -my, my) };
  };
  /** Zoom to `next`, keeping the point under (cx, cy) (box px) where it is. */
  const zoomAt = (next: number, cx: number, cy: number, smooth = false) => {
    next = clamp(next, 1, MAX);
    const f = fit();
    const px = cx - f.bw / 2;
    const py = cy - f.bh / 2;
    const cur = s();
    const k = next / cur;
    const p = t();
    setAnim(smooth && !reducedMotion());
    setS(next);
    setT(next <= 1.001 ? { x: 0, y: 0 } : clampPan(px - (px - p.x) * k, py - (py - p.y) * k, next));
  };
  const reset = () => {
    setAnim(!reducedMotion());
    setS(1);
    setT({ x: 0, y: 0 });
  };

  // ── pointers: pan, pinch, click-to-zoom ────────────────────────────────
  const pts = new Map<number, { x: number; y: number }>();
  let down: { x: number; y: number; t: number; moved: boolean } | null = null;
  let pinch: { d: number; s: number } | null = null;
  let lastTap = 0;
  const local = (e: PointerEvent) => {
    const r = box.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };
  const dist = () => {
    const [a, b] = [...pts.values()];
    return a && b ? Math.hypot(a.x - b.x, a.y - b.y) : 0;
  };
  const mid = () => {
    const [a, b] = [...pts.values()];
    return { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
  };
  const onDown = (e: PointerEvent) => {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    pts.set(e.pointerId, local(e));
    if (pts.size === 2) {
      pinch = { d: dist(), s: s() };
      down = null;
      e.stopPropagation(); // a pinch here zooms the image, never the mosaic
    } else {
      down = { ...local(e), t: e.timeStamp, moved: false };
      if (zoomed()) e.stopPropagation();
    }
    try {
      box.setPointerCapture(e.pointerId);
    } catch {
      /* gone */
    }
  };
  const onMove = (e: PointerEvent) => {
    if (!pts.has(e.pointerId)) return;
    const prev = pts.get(e.pointerId)!;
    const p = local(e);
    pts.set(e.pointerId, p);
    if (pinch && pts.size === 2) {
      e.stopPropagation();
      const m = mid();
      setAnim(false);
      zoomAt(pinch.s * (dist() / Math.max(1, pinch.d)), m.x, m.y);
      return;
    }
    if (!down) return;
    if (Math.hypot(p.x - down.x, p.y - down.y) > 5) down.moved = true;
    if (zoomed()) {
      e.stopPropagation();
      setAnim(false);
      const cur = t();
      setT(clampPan(cur.x + (p.x - prev.x), cur.y + (p.y - prev.y), s()));
    }
  };
  const onUp = (e: PointerEvent) => {
    const had = pts.delete(e.pointerId);
    if (pts.size < 2) pinch = null;
    if (!had || !down) return;
    const d = down;
    down = null;
    if (d.moved || e.timeStamp - d.t > 450) return;
    // A tap: mouse clicks toggle zoom; touch needs a double tap (a single tap is just a tap).
    const p = local(e);
    if (e.pointerType === "touch") {
      if (e.timeStamp - lastTap < 320) {
        lastTap = 0;
        if (zoomed()) reset();
        else zoomAt(2.5, p.x, p.y, true);
      } else lastTap = e.timeStamp;
      return;
    }
    if (zoomed()) reset();
    else zoomAt(2.5, p.x, p.y, true);
  };

  onMount(() => {
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const p = { x: e.clientX - box.getBoundingClientRect().left, y: e.clientY - box.getBoundingClientRect().top };
      const k = Math.exp(-e.deltaY * (e.ctrlKey ? 0.01 : 0.0022));
      setAnim(false);
      zoomAt(s() * k, p.x, p.y);
    };
    box.addEventListener("wheel", onWheel, { passive: false });
    const ro = new ResizeObserver(() => setT((p) => clampPan(p.x, p.y, s())));
    ro.observe(box);
    onCleanup(() => {
      box.removeEventListener("wheel", onWheel);
      ro.disconnect();
    });
  });

  const imgStyle = () => {
    const f = fit();
    void loaded();
    return {
      width: `${f.w}px`,
      height: `${f.h}px`,
      transform: `translate(${t().x}px, ${t().y}px) scale(${s()})`,
      transition: anim() ? "transform 260ms cubic-bezier(0.16, 0.84, 0.3, 1)" : "none",
    };
  };

  return (
    <div
      ref={box}
      class="img-view"
      classList={{ zoomed: zoomed(), ready: loaded() }}
      data-noswipe={zoomed() ? "" : undefined}
      onPointerDown={onDown}
      onPointerMove={onMove}
      onPointerUp={onUp}
      onPointerCancel={onUp}
      onTransitionEnd={() => setAnim(false)}
    >
      <Show when={!failed()} fallback={<div class="board-note err">This image couldn't be shown.</div>}>
        <img
          ref={img}
          src={props.src}
          alt={props.alt}
          draggable={false}
          style={imgStyle()}
          onLoad={() => setLoaded(true)}
          onError={() => setFailed(true)}
        />
      </Show>
      <div class="img-tools" onPointerDown={(e) => e.stopPropagation()}>
        <button type="button" class="icon-btn sm" aria-label="Zoom out" disabled={!zoomed()} onClick={() => zoomAt(s() / 1.6, fit().bw / 2, fit().bh / 2, true)}>
          <Icon name="zoomOut" size={15} />
        </button>
        <span class="img-pct">{Math.round(s() * 100)}%</span>
        <button type="button" class="icon-btn sm" aria-label="Zoom in" disabled={s() >= MAX} onClick={() => zoomAt(s() * 1.6, fit().bw / 2, fit().bh / 2, true)}>
          <Icon name="zoomIn" size={15} />
        </button>
        <button type="button" class="icon-btn sm" aria-label="Fit to tile" disabled={!zoomed()} onClick={reset}>
          <Icon name="maximize" size={14} />
        </button>
      </div>
    </div>
  );
}
