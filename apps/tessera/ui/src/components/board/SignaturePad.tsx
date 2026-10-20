// A signature pad: smooth, pressure-aware ink for pen, finger and mouse.
// Exports a trimmed PNG with a transparent background (base64, no data: prefix).
import { Show, createSignal, onCleanup, onMount, type JSX } from "solid-js";
import { Icon } from "../icons";

interface Pt {
  x: number;
  y: number;
  /** Stroke width at this point (CSS px). */
  w: number;
}

const MIN_W = 1.1;
const MAX_W = 3.6;
/** The ink of the exported signature, whatever the theme (it goes on documents). */
const EXPORT_INK = "#1b1e28";

export interface SignatureHandle {
  /** Trimmed transparent PNG (base64), or null when nothing is drawn. */
  png: () => string | null;
  clear: () => void;
}

export function SignaturePad(props: { onChange?: (hasInk: boolean) => void; handle?: (h: SignatureHandle) => void; label?: string; children?: JSX.Element }) {
  let wrap!: HTMLDivElement;
  let canvas!: HTMLCanvasElement;
  const strokes: Pt[][] = [];
  let cur: Pt[] | null = null;
  let last: { x: number; y: number; t: number } | null = null;
  let pointer = -1;
  const [ink, setInk] = createSignal(false);
  const [active, setActive] = createSignal(false);

  const inkColor = () => getComputedStyle(wrap).getPropertyValue("--sig-ink").trim() || "#ece6dc";

  const size = () => {
    const r = wrap.getBoundingClientRect();
    return { w: Math.max(1, r.width), h: Math.max(1, r.height) };
  };

  const fit = () => {
    const { w, h } = size();
    const dpr = Math.min(3, window.devicePixelRatio || 1);
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;
    const ctx = canvas.getContext("2d")!;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    redraw(ctx, inkColor());
  };

  /** One stroke as a filled ribbon of quadratic segments (width varies smoothly). */
  const drawStroke = (ctx: CanvasRenderingContext2D, s: Pt[], dx = 0, dy = 0, k = 1) => {
    if (!s.length) return;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    if (s.length === 1) {
      ctx.beginPath();
      ctx.arc((s[0].x + dx) * k, (s[0].y + dy) * k, (s[0].w / 2) * k, 0, Math.PI * 2);
      ctx.fill();
      return;
    }
    for (let i = 1; i < s.length; i++) {
      const p0 = s[i - 1];
      const p1 = s[i];
      const m0 = i === 1 ? p0 : { x: (s[i - 2].x + p0.x) / 2, y: (s[i - 2].y + p0.y) / 2 };
      const m1 = { x: (p0.x + p1.x) / 2, y: (p0.y + p1.y) / 2 };
      ctx.lineWidth = ((p0.w + p1.w) / 2) * k;
      ctx.beginPath();
      ctx.moveTo((m0.x + dx) * k, (m0.y + dy) * k);
      ctx.quadraticCurveTo((p0.x + dx) * k, (p0.y + dy) * k, (m1.x + dx) * k, (m1.y + dy) * k);
      ctx.stroke();
    }
    const a = s[s.length - 2];
    const b = s[s.length - 1];
    ctx.lineWidth = b.w * k;
    ctx.beginPath();
    ctx.moveTo(((a.x + b.x) / 2 + dx) * k, ((a.y + b.y) / 2 + dy) * k);
    ctx.lineTo((b.x + dx) * k, (b.y + dy) * k);
    ctx.stroke();
  };

  const redraw = (ctx: CanvasRenderingContext2D, color: string) => {
    const { w, h } = size();
    ctx.clearRect(0, 0, w, h);
    ctx.strokeStyle = color;
    ctx.fillStyle = color;
    for (const s of strokes) drawStroke(ctx, s);
    if (cur) drawStroke(ctx, cur);
  };

  const pointAt = (e: PointerEvent) => {
    const r = canvas.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top, t: e.timeStamp };
  };

  /** Pen pressure when there is any; otherwise speed: fast strokes thin out like real ink. */
  const widthFor = (e: PointerEvent, x: number, y: number, t: number) => {
    if (e.pointerType === "pen" && e.pressure > 0) return MIN_W + (MAX_W - MIN_W) * Math.min(1, e.pressure * 1.25);
    if (!last) return (MIN_W + MAX_W) / 2;
    const v = Math.hypot(x - last.x, y - last.y) / Math.max(1, t - last.t); // px/ms
    return Math.max(MIN_W, Math.min(MAX_W, MAX_W - v * 1.1));
  };

  const add = (e: PointerEvent) => {
    if (!cur) return;
    const { x, y, t } = pointAt(e);
    const prev = cur[cur.length - 1];
    if (prev && Math.hypot(x - prev.x, y - prev.y) < 0.8) return;
    const target = widthFor(e, x, y, t);
    // Ease the width so it never jumps between samples.
    const w = prev ? prev.w + (target - prev.w) * 0.35 : target;
    cur.push({ x, y, w });
    last = { x, y, t };
  };

  let raf = 0;
  const paint = () => {
    raf = 0;
    const ctx = canvas.getContext("2d")!;
    redraw(ctx, inkColor());
  };
  const schedule = () => {
    if (!raf) raf = requestAnimationFrame(paint);
  };

  const onDown = (e: PointerEvent) => {
    if (pointer !== -1 || (e.pointerType === "mouse" && e.button !== 0)) return;
    e.preventDefault();
    pointer = e.pointerId;
    canvas.setPointerCapture(e.pointerId);
    setActive(true);
    last = null;
    cur = [];
    add(e);
    schedule();
  };
  const onMove = (e: PointerEvent) => {
    if (e.pointerId !== pointer || !cur) return;
    e.preventDefault();
    const list = typeof e.getCoalescedEvents === "function" ? e.getCoalescedEvents() : [];
    if (list.length) for (const c of list) add(c);
    else add(e);
    schedule();
  };
  const onUp = (e: PointerEvent) => {
    if (e.pointerId !== pointer) return;
    pointer = -1;
    setActive(false);
    if (cur && cur.length) strokes.push(cur);
    cur = null;
    last = null;
    schedule();
    const has = strokes.some((s) => s.length > 2) || strokes.length > 1;
    setInk(has);
    props.onChange?.(has);
  };

  const clear = () => {
    strokes.length = 0;
    cur = null;
    setInk(false);
    props.onChange?.(false);
    schedule();
  };
  const undo = () => {
    strokes.pop();
    const has = strokes.length > 0;
    setInk(has);
    props.onChange?.(has);
    schedule();
  };

  const png = (): string | null => {
    const all = strokes.flat();
    if (!all.length) return null;
    let x0 = Infinity;
    let y0 = Infinity;
    let x1 = -Infinity;
    let y1 = -Infinity;
    for (const p of all) {
      x0 = Math.min(x0, p.x - p.w);
      y0 = Math.min(y0, p.y - p.w);
      x1 = Math.max(x1, p.x + p.w);
      y1 = Math.max(y1, p.y + p.w);
    }
    const pad = 6;
    const k = 2; // export at 2x for crisp print
    const out = document.createElement("canvas");
    out.width = Math.ceil((x1 - x0 + pad * 2) * k);
    out.height = Math.ceil((y1 - y0 + pad * 2) * k);
    const ctx = out.getContext("2d")!;
    ctx.strokeStyle = EXPORT_INK;
    ctx.fillStyle = EXPORT_INK;
    for (const s of strokes) drawStroke(ctx, s, pad - x0, pad - y0, k);
    return out.toDataURL("image/png").replace(/^data:image\/png;base64,/, "");
  };

  onMount(() => {
    fit();
    const ro = new ResizeObserver(() => fit());
    ro.observe(wrap);
    props.handle?.({ png, clear });
    onCleanup(() => {
      ro.disconnect();
      cancelAnimationFrame(raf);
    });
  });

  return (
    <div class="sig" classList={{ inked: ink(), active: active() }}>
      <div class="sig-pad" ref={wrap} data-noswipe>
        <canvas
          ref={canvas}
          class="sig-canvas"
          role="img"
          aria-label={props.label ?? "Signature pad: draw your signature"}
          onPointerDown={onDown}
          onPointerMove={onMove}
          onPointerUp={onUp}
          onPointerCancel={onUp}
          onContextMenu={(e) => e.preventDefault()}
        />
        <div class="sig-guide" aria-hidden="true">
          <span class="sig-x">×</span>
          <span class="sig-line" />
        </div>
        <Show when={!ink() && !active()}>
          <span class="sig-hint" aria-hidden="true">
            <Icon name="pen" size={15} /> Sign here
          </span>
        </Show>
      </div>
      <div class="sig-tools">
        <button type="button" class="btn btn-sm btn-ghost" disabled={!ink()} onClick={undo} aria-label="Undo the last stroke">
          Undo
        </button>
        <button type="button" class="btn btn-sm btn-ghost" disabled={!ink()} onClick={clear}>
          Clear
        </button>
        <span class="spacer" />
        {props.children}
      </div>
    </div>
  );
}
