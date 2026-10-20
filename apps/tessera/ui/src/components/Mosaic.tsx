// The Tesserae motif: a static, generated mosaic of hand-cut stones laid in
// gently waving rows (opus vermiculatum), a few gold and lapis pieces among
// them, fading out toward the edges. Rendered once as a handful of paths —
// no animation, no filters.
import { createMemo } from "solid-js";

function rng(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export type MosaicTone = "stone" | "veiled";

interface Piece {
  d: string;
  cls: string;
}

/**
 * @param focus where the mosaic is densest (fractions of width/height); it
 *   thins out and fades with distance from there.
 */
export function MosaicArt(props: {
  seed?: number;
  width?: number;
  height?: number;
  cell?: number;
  focus?: [number, number];
  tone?: MosaicTone;
  class?: string;
}) {
  const pieces = createMemo(() => {
    const W = props.width ?? 560;
    const H = props.height ?? 140;
    const cell = props.cell ?? 13;
    const gap = Math.max(1.6, cell * 0.17);
    const [fx, fy] = props.focus ?? [0.5, 0.55];
    const r = rng(props.seed ?? 7);
    const veiled = props.tone === "veiled";
    // Buckets: tint class × opacity step, merged into one path each.
    const buckets = new Map<string, string[]>();
    const add = (cls: string, d: string) => {
      const l = buckets.get(cls);
      if (l) l.push(d);
      else buckets.set(cls, [d]);
    };
    const rowH = cell;
    const rows = Math.ceil(H / rowH) + 1;
    const amp = cell * 0.55;
    const period = W / (1.6 + r() * 0.8);
    const phase = r() * Math.PI * 2;
    const j = (v: number) => (r() - 0.5) * v;
    for (let row = 0; row < rows; row++) {
      let x = -r() * cell;
      const y0 = row * rowH;
      while (x < W) {
        const w = cell * (0.7 + r() * 0.65);
        const x1 = x;
        const x2 = x + w - gap;
        // Rows wave gently: each stone follows the curve at its left and right edge.
        const wave = (xx: number) => Math.sin(xx / period * Math.PI * 2 + phase + row * 0.18) * amp;
        const ya = y0 + wave(x1);
        const yb = y0 + wave(x2);
        const h = rowH - gap;
        // An ellipse around the focus fills the box; stones thin out and fade toward its rim.
        const dx = ((x1 + x2) / 2 - fx * W) / (W * 0.5);
        const dy = (y0 + h / 2 - fy * H) / (H * 0.6);
        const keep = 1 - Math.hypot(dx, dy);
        x += w;
        if (keep <= 0 || r() > keep + 0.25) continue;
        const k = 0.9 + j(0.1);
        const pts = [
          [x1 + j(1.4), ya + j(1.2)],
          [x2 + j(1.4), yb + j(1.2)],
          [x2 + j(1.4), yb + h * k + j(1.2)],
          [x1 + j(1.4), ya + h * k + j(1.2)],
        ];
        const d = "M" + pts.map(([a, b]) => `${a.toFixed(1)} ${b.toFixed(1)}`).join("L") + "Z";
        const roll = r();
        const step = Math.max(1, Math.min(5, Math.round(keep * 5 + j(1.2))));
        let tint: string;
        if (veiled) tint = roll < 0.05 ? "p" : `s${Math.floor(r() * 3)}`;
        else tint = roll < 0.022 ? "g" : roll < 0.034 ? "l" : `s${Math.floor(r() * 4)}`;
        add(`${tint} o${step}`, d);
      }
    }
    const out: Piece[] = [];
    for (const [cls, ds] of buckets) out.push({ cls, d: ds.join("") });
    return { W, H, out };
  });
  return (
    <svg
      class={"mosaic-art" + (props.tone === "veiled" ? " veiled" : "") + (props.class ? " " + props.class : "")}
      viewBox={`0 0 ${pieces().W} ${pieces().H}`}
      width={pieces().W}
      height={pieces().H}
      aria-hidden="true"
      preserveAspectRatio="xMidYMid slice"
    >
      {pieces().out.map((p) => (
        <path class={p.cls} d={p.d} />
      ))}
    </svg>
  );
}

/** A hairline of tiny squares, as a divider. */
export function TesseraRule(props: { class?: string }) {
  return <div class={"tessera-rule" + (props.class ? " " + props.class : "")} role="separator" aria-hidden="true" />;
}

/**
 * A small set of tesserae laid in one by one — the "set in place" moment for
 * pairing and setup. The centre stone is gold.
 */
export function SettleTiles(props: { size?: number; class?: string; count?: 4 | 9 }) {
  const n = () => props.count ?? 9;
  return (
    <span class={"settle-tiles" + (props.class ? " " + props.class : "")} style={{ "--st": `${props.size ?? 9}px`, "grid-template-columns": `repeat(${n() === 4 ? 2 : 3}, var(--st))` }} aria-hidden="true">
      {Array.from({ length: n() }, (_, i) => (
        <i class={(n() === 9 && i === 4) || (n() === 4 && i === 1) ? "g" : i === 6 ? "l" : ""} style={{ "--i": String([0, 1, 3, 2, 4, 5, 7, 6, 8][i] ?? i) }} />
      ))}
    </span>
  );
}
