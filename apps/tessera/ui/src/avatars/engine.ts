// The mosaic engine: turns a small vector design into an SVG made of tesserae.
//
// A design is a painter's-order list of shapes on a 100×100 canvas:
//   fills  (circles, ellipses, polygons, smooth blobs, paths) are coloured areas;
//   lines  (polylines, smooth curves, arcs) become a chain of tiles along their centreline;
//   dots   are single tesserae (eyes, gems, highlights).
//
// Tiling ("tesserizing") works like a mosaicist would:
//   1. the whole field is laid in straight courses of slightly irregular tiles (opus tessellatum),
//      each tile taking the colour under its centre;
//   2. every visible outline gets rows of tiles that hug it on both sides (andamento): one or more
//      rows inside the shape, and a halo of rows in the background around the figure;
//   3. lines and dots are set on top as their own tiles; a border band follows the rounded frame.
// Each tile is drawn with a grout-coloured stroke, so later tiles look cut into earlier ones.
// Everything is seeded by the design id, so the output never changes between renders.

import { PALETTE, GROUT } from "./palette";

export type Key = string;

export interface FillOpts {
  /** Colour of the first row of tiles inside the outline (a contour line of darker stone). */
  rim?: Key;
  /** Rows that follow the outline inside the shape (default 1). */
  rows?: number;
  /** Rows that follow the outline outside it (default: the design's halo into the background, else 1). */
  out?: number;
  /** Tile size for this shape's rows. */
  t?: number;
}

export interface LineOpts {
  /** Rows of the surrounding colour that follow the line on each side (default 0). */
  halo?: number;
  /** Tile length along the line. */
  t?: number;
}

export interface Design {
  id: string;
  /** Background stone. */
  bg: Key;
  /** Border band colour ("" for none). */
  frame?: Key;
  /** Rows of background that hug the figure (default 2). */
  halo?: number;
  /** Base tile size (default 4.4, about 23 tiles across). */
  t?: number;
  draw: (c: Canvas) => void;
}

type BB = [number, number, number, number];
type Mat = [number, number, number, number, number, number];

interface FillShape {
  kind: 0;
  col: Key;
  polys: number[][];
  bb: BB;
  test: (x: number, y: number) => boolean;
  o: FillOpts;
}
interface LineShape {
  kind: 1;
  col: Key;
  pts: number[];
  w: number;
  closed: boolean;
  o: LineOpts;
}
interface DotShape {
  kind: 2;
  col: Key;
  x: number;
  y: number;
  w: number;
  h: number;
  a: number;
}
type Shape = FillShape | LineShape | DotShape;

const ID: Mat = [1, 0, 0, 1, 0, 0];
const mul = (m: Mat, n: Mat): Mat => [
  m[0] * n[0] + m[2] * n[1],
  m[1] * n[0] + m[3] * n[1],
  m[0] * n[2] + m[2] * n[3],
  m[1] * n[2] + m[3] * n[3],
  m[0] * n[4] + m[2] * n[5] + m[4],
  m[1] * n[4] + m[3] * n[5] + m[5],
];
const inv = (m: Mat): Mat => {
  const det = m[0] * m[3] - m[1] * m[2];
  const a = m[3] / det,
    b = -m[1] / det,
    c = -m[2] / det,
    d = m[0] / det;
  return [a, b, c, d, -(a * m[4] + c * m[5]), -(b * m[4] + d * m[5])];
};
const DEG = Math.PI / 180;

// ── geometry helpers ────────────────────────────────────────────────────

/** Catmull-Rom spline through points (flat array), sampled about every `step` units. */
export function spline(p: number[], closed: boolean, step = 1): number[] {
  const n = p.length / 2;
  if (n < 3) return p.slice();
  const out: number[] = [];
  const at = (i: number) => {
    if (closed) i = ((i % n) + n) % n;
    else i = Math.max(0, Math.min(n - 1, i));
    return [p[2 * i], p[2 * i + 1]];
  };
  const segs = closed ? n : n - 1;
  for (let i = 0; i < segs; i++) {
    const [x0, y0] = at(i - 1),
      [x1, y1] = at(i),
      [x2, y2] = at(i + 1),
      [x3, y3] = at(i + 2);
    const m = Math.max(2, Math.ceil(Math.hypot(x2 - x1, y2 - y1) / step));
    for (let j = 0; j < m; j++) {
      const t = j / m,
        t2 = t * t,
        t3 = t2 * t;
      out.push(
        0.5 * (2 * x1 + (-x0 + x2) * t + (2 * x0 - 5 * x1 + 4 * x2 - x3) * t2 + (-x0 + 3 * x1 - 3 * x2 + x3) * t3),
        0.5 * (2 * y1 + (-y0 + y2) * t + (2 * y0 - 5 * y1 + 4 * y2 - y3) * t2 + (-y0 + 3 * y1 - 3 * y2 + y3) * t3),
      );
    }
  }
  if (!closed) out.push(p[2 * n - 2], p[2 * n - 1]);
  return out;
}

/** Parse a subset of SVG path syntax (M L H V C S Q T Z, absolute and relative) into flattened polygons. */
export function parsePath(d: string): number[][] {
  const toks = d.match(/[a-zA-Z]|-?(?:\d+\.?\d*|\.\d+)(?:e-?\d+)?/g) ?? [];
  const polys: number[][] = [];
  let cur: number[] = [];
  let x = 0,
    y = 0,
    sx = 0,
    sy = 0,
    cx = 0,
    cy = 0,
    cmd = "",
    i = 0;
  const num = () => parseFloat(toks[i++]);
  const bez = (pts: number[][]) => {
    const [p0, p1, p2, p3] = pts;
    const len = Math.hypot(p1[0] - p0[0], p1[1] - p0[1]) + Math.hypot(p2[0] - p1[0], p2[1] - p1[1]) + (p3 ? Math.hypot(p3[0] - p2[0], p3[1] - p2[1]) : 0);
    const m = Math.max(3, Math.ceil(len / 1.2));
    for (let j = 1; j <= m; j++) {
      const t = j / m,
        u = 1 - t;
      if (p3) cur.push(u * u * u * p0[0] + 3 * u * u * t * p1[0] + 3 * u * t * t * p2[0] + t * t * t * p3[0], u * u * u * p0[1] + 3 * u * u * t * p1[1] + 3 * u * t * t * p2[1] + t * t * t * p3[1]);
      else cur.push(u * u * p0[0] + 2 * u * t * p1[0] + t * t * p2[0], u * u * p0[1] + 2 * u * t * p1[1] + t * t * p2[1]);
    }
  };
  while (i < toks.length) {
    if (/[a-zA-Z]/.test(toks[i])) cmd = toks[i++];
    const rel = cmd === cmd.toLowerCase();
    const C = cmd.toUpperCase();
    const ox = rel ? x : 0,
      oy = rel ? y : 0;
    if (C === "Z") {
      if (cur.length) polys.push(cur);
      cur = [];
      x = sx;
      y = sy;
      continue;
    }
    if (C === "M") {
      if (cur.length > 2) polys.push(cur);
      x = ox + num();
      y = oy + num();
      sx = x;
      sy = y;
      cur = [x, y];
      cmd = rel ? "l" : "L";
      cx = x;
      cy = y;
    } else if (C === "L") {
      x = ox + num();
      y = oy + num();
      cur.push(x, y);
      cx = x;
      cy = y;
    } else if (C === "H") {
      x = ox + num();
      cur.push(x, y);
      cx = x;
      cy = y;
    } else if (C === "V") {
      y = (rel ? y : 0) + num();
      cur.push(x, y);
      cx = x;
      cy = y;
    } else if (C === "C" || C === "S") {
      let x1: number, y1: number;
      if (C === "C") {
        x1 = ox + num();
        y1 = oy + num();
      } else {
        x1 = 2 * x - cx;
        y1 = 2 * y - cy;
      }
      const x2 = ox + num(),
        y2 = oy + num(),
        x3 = ox + num(),
        y3 = oy + num();
      bez([[x, y], [x1, y1], [x2, y2], [x3, y3]]);
      cx = x2;
      cy = y2;
      x = x3;
      y = y3;
    } else if (C === "Q" || C === "T") {
      let x1: number, y1: number;
      if (C === "Q") {
        x1 = ox + num();
        y1 = oy + num();
      } else {
        x1 = 2 * x - cx;
        y1 = 2 * y - cy;
      }
      const x2 = ox + num(),
        y2 = oy + num();
      bez([[x, y], [x1, y1], [x2, y2]]);
      cx = x1;
      cy = y1;
      x = x2;
      y = y2;
    } else {
      i++; // unsupported: skip
    }
  }
  if (cur.length > 2) polys.push(cur);
  return polys;
}

/**
 * Even-odd point-in-polygon test with edges bucketed by row, so each query only looks at the few
 * edges that cross its scanline instead of every vertex of a long spline outline.
 */
function polyTester(polys: number[][]): (x: number, y: number) => boolean {
  const b = bbox(polys);
  const y0 = b[1],
    rows = Math.max(1, Math.ceil(b[3] - b[1]) + 1);
  const buckets: number[][] = Array.from({ length: rows }, () => []);
  // Flat edge list: x1, y1, x2, y2.
  const E: number[] = [];
  for (const p of polys) {
    const n = p.length;
    for (let i = 0, j = n - 2; i < n; j = i, i += 2) {
      const e = E.length;
      E.push(p[j], p[j + 1], p[i], p[i + 1]);
      const lo = Math.max(0, Math.floor(Math.min(p[j + 1], p[i + 1]) - y0)),
        hi = Math.min(rows - 1, Math.floor(Math.max(p[j + 1], p[i + 1]) - y0));
      for (let r = lo; r <= hi; r++) buckets[r].push(e);
    }
  }
  return (x, y) => {
    const r = Math.floor(y - y0);
    if (r < 0 || r >= rows) return false;
    let inside = false;
    for (const e of buckets[r]) {
      const xj = E[e],
        yj = E[e + 1],
        xi = E[e + 2],
        yi = E[e + 3];
      if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) inside = !inside;
    }
    return inside;
  };
}

function bbox(polys: number[][], pad = 0): BB {
  const b: BB = [Infinity, Infinity, -Infinity, -Infinity];
  for (const p of polys)
    for (let i = 0; i < p.length; i += 2) {
      b[0] = Math.min(b[0], p[i]);
      b[1] = Math.min(b[1], p[i + 1]);
      b[2] = Math.max(b[2], p[i]);
      b[3] = Math.max(b[3], p[i + 1]);
    }
  b[0] -= pad;
  b[1] -= pad;
  b[2] += pad;
  b[3] += pad;
  return b;
}

/** Points evenly spaced along a polyline. Closed: n points (no repeat). Open: includes both ends. */
function resample(p: number[], closed: boolean, step: number): number[] {
  const n = p.length / 2;
  if (n < 2) return p.slice();
  const segs = closed ? n : n - 1;
  const cum = [0];
  for (let i = 0; i < segs; i++) {
    const j = (i + 1) % n;
    cum.push(cum[i] + Math.hypot(p[2 * j] - p[2 * i], p[2 * j + 1] - p[2 * i + 1]));
  }
  const L = cum[segs];
  if (L < 1e-6) return [p[0], p[1]];
  const m = closed ? Math.max(3, Math.round(L / step)) : Math.max(1, Math.round(L / step));
  const st = L / m;
  const out: number[] = [];
  let seg = 0;
  const cnt = closed ? m : m + 1;
  for (let k = 0; k < cnt; k++) {
    const s = Math.min(k * st, L);
    while (seg < segs - 1 && cum[seg + 1] < s) seg++;
    const j = (seg + 1) % n;
    const f = cum[seg + 1] > cum[seg] ? (s - cum[seg]) / (cum[seg + 1] - cum[seg]) : 0;
    out.push(p[2 * seg] + (p[2 * j] - p[2 * seg]) * f, p[2 * seg + 1] + (p[2 * j + 1] - p[2 * seg + 1]) * f);
  }
  return out;
}

/** Unit normals (left of travel direction in y-down space), smoothed over a small window. */
function normals(p: number[], closed: boolean, win = 2): number[] {
  const n = p.length / 2;
  const out = new Array<number>(p.length);
  for (let i = 0; i < n; i++) {
    let a = i - win,
      b = i + win;
    if (closed) {
      a = (a + n) % n;
      b = b % n;
    } else {
      a = Math.max(0, a);
      b = Math.min(n - 1, b);
    }
    let tx = p[2 * b] - p[2 * a],
      ty = p[2 * b + 1] - p[2 * a + 1];
    const l = Math.hypot(tx, ty) || 1;
    tx /= l;
    ty /= l;
    out[2 * i] = ty;
    out[2 * i + 1] = -tx;
  }
  return out;
}

// ── the canvas designs draw on ──────────────────────────────────────────

export interface GroupT {
  x?: number;
  y?: number;
  /** Uniform scale. */
  s?: number;
  /** Rotation in degrees (clockwise, y-down). */
  r?: number;
  /** Mirror horizontally. */
  fx?: boolean;
  /** Origin for scale/rotate/mirror (default the canvas centre). */
  ox?: number;
  oy?: number;
}

export class Canvas {
  shapes: Shape[] = [];
  private m: Mat = ID;

  private tp(p: number[]): number[] {
    const m = this.m;
    const out = new Array<number>(p.length);
    for (let i = 0; i < p.length; i += 2) {
      out[i] = m[0] * p[i] + m[2] * p[i + 1] + m[4];
      out[i + 1] = m[1] * p[i] + m[3] * p[i + 1] + m[5];
    }
    return out;
  }
  private get sc() {
    return Math.sqrt(Math.abs(this.m[0] * this.m[3] - this.m[1] * this.m[2]));
  }
  private get rot() {
    return Math.atan2(this.m[1], this.m[0]) / DEG;
  }

  /** Draw inside a transform. */
  group(t: GroupT, fn: () => void) {
    const ox = t.ox ?? 50,
      oy = t.oy ?? 50,
      s = t.s ?? 1,
      r = (t.r ?? 0) * DEG;
    const cs = Math.cos(r),
      sn = Math.sin(r),
      sx = t.fx ? -s : s;
    // translate(x+ox, y+oy) · rotate · scale · translate(-ox, -oy)
    const local: Mat = [cs * sx, sn * sx, -sn * s, cs * s, 0, 0];
    local[4] = (t.x ?? 0) + ox - (local[0] * ox + local[2] * oy);
    local[5] = (t.y ?? 0) + oy - (local[1] * ox + local[3] * oy);
    const prev = this.m;
    this.m = mul(prev, local);
    fn();
    this.m = prev;
  }
  /** Draw `fn` and its mirror image across x = 50. */
  sym(fn: () => void) {
    fn();
    this.group({ fx: true }, fn);
  }

  private fillPolys(polys: number[][], col: Key, o: FillOpts = {}, test?: (x: number, y: number) => boolean) {
    const tp = polys.map((p) => this.tp(p));
    const bb = bbox(tp, 0.01);
    this.shapes.push({ kind: 0, col, polys: tp, bb, test: test ?? polyTester(tp), o });
  }

  ellipse(cx: number, cy: number, rx: number, ry: number, col: Key, rot = 0, o: FillOpts = {}) {
    const n = Math.max(20, Math.ceil((Math.PI * (rx + ry) * this.sc) / 1.2));
    const cs = Math.cos(rot * DEG),
      sn = Math.sin(rot * DEG);
    const p: number[] = [];
    for (let i = 0; i < n; i++) {
      const a = (i / n) * Math.PI * 2;
      const ex = Math.cos(a) * rx,
        ey = Math.sin(a) * ry;
      p.push(cx + ex * cs - ey * sn, cy + ex * sn + ey * cs);
    }
    const im = inv(this.m);
    const test = (x: number, y: number) => {
      const u = im[0] * x + im[2] * y + im[4] - cx,
        v = im[1] * x + im[3] * y + im[5] - cy;
      const a = (u * cs + v * sn) / rx,
        b = (-u * sn + v * cs) / ry;
      return a * a + b * b <= 1;
    };
    this.fillPolys([p], col, o, test);
  }
  circle(cx: number, cy: number, r: number, col: Key, o: FillOpts = {}) {
    this.ellipse(cx, cy, r, r, col, 0, o);
  }
  /** Polygon from a flat point list. */
  poly(pts: number[], col: Key, o: FillOpts = {}) {
    this.fillPolys([pts], col, o);
  }
  rect(x: number, y: number, w: number, h: number, col: Key, o: FillOpts = {}) {
    this.poly([x, y, x + w, y, x + w, y + h, x, y + h], col, o);
  }
  /** Smooth closed shape through the points. */
  blob(pts: number[], col: Key, o: FillOpts = {}) {
    this.fillPolys([spline(pts, true)], col, o);
  }
  /** SVG path syntax; every subpath is closed; even-odd fill (subpaths can cut holes). */
  path(d: string, col: Key, o: FillOpts = {}) {
    this.fillPolys(parsePath(d), col, o);
  }

  private addLine(pts: number[], w: number, col: Key, closed: boolean, o: LineOpts) {
    this.shapes.push({ kind: 1, col, pts: this.tp(pts), w: w * this.sc, closed, o });
  }
  /** Straight polyline of tiles. */
  line(pts: number[], w: number, col: Key, o: LineOpts = {}) {
    this.addLine(pts, w, col, false, o);
  }
  /** Smooth open curve of tiles through the points. */
  curve(pts: number[], w: number, col: Key, o: LineOpts = {}) {
    this.addLine(spline(pts, false), w, col, false, o);
  }
  /** Smooth closed loop of tiles. */
  loop(pts: number[], w: number, col: Key, o: LineOpts = {}) {
    this.addLine(spline(pts, true), w, col, true, o);
  }
  /** Circular arc of tiles; angles in degrees, 0 = right, 90 = down. */
  arc(cx: number, cy: number, r: number, a0: number, a1: number, w: number, col: Key, o: LineOpts = {}) {
    const n = Math.max(4, Math.ceil((Math.abs(a1 - a0) * DEG * r) / 1));
    const p: number[] = [];
    for (let i = 0; i <= n; i++) {
      const a = (a0 + ((a1 - a0) * i) / n) * DEG;
      p.push(cx + Math.cos(a) * r, cy + Math.sin(a) * r);
    }
    this.addLine(p, w, col, false, o);
  }
  ring(cx: number, cy: number, r: number, w: number, col: Key, o: LineOpts = {}) {
    const n = Math.max(12, Math.ceil((2 * Math.PI * r) / 1));
    const p: number[] = [];
    for (let i = 0; i < n; i++) {
      const a = (i / n) * Math.PI * 2;
      p.push(cx + Math.cos(a) * r, cy + Math.sin(a) * r);
    }
    this.addLine(p, w, col, true, o);
  }
  /** A single tessera. */
  dot(x: number, y: number, s: number, col: Key, a = 0) {
    this.tile(x, y, s, s, col, a);
  }
  tile(x: number, y: number, w: number, h: number, col: Key, a = 0) {
    const [px, py] = this.tp([x, y]);
    this.shapes.push({ kind: 2, col, x: px, y: py, w: w * this.sc, h: h * this.sc, a: a + this.rot });
  }
}

// ── seeded randomness and colour ────────────────────────────────────────

export function hash(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

function mulberry(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const hex2 = (n: number) => Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, "0");

function parseHex(h: string): [number, number, number] {
  const v = h.replace("#", "");
  const f = v.length === 3 ? v.split("").map((c) => c + c).join("") : v.slice(0, 6);
  return [parseInt(f.slice(0, 2), 16), parseInt(f.slice(2, 4), 16), parseInt(f.slice(4, 6), 16)];
}

/** Four hand-cut variants of a stone colour: a little lighter/darker, a little warmer/cooler. */
function variants(key: Key, rnd: () => number, calm = 1): string[] {
  const entry = PALETTE[key];
  const base = entry ? entry[0] : key.startsWith("#") ? key : "#888888";
  const amp = (entry ? entry[1] : 0.07) * calm;
  const [r, g, b] = parseHex(base);
  const steps = [-1, -0.35, 0.35, 1];
  return steps.map((s) => {
    const f = s * amp * (0.8 + rnd() * 0.4);
    const warm = (rnd() - 0.5) * 10 * amp * 8;
    const mix = (c: number) => (f > 0 ? c + (255 - c) * f : c * (1 + f));
    return "#" + hex2(mix(r) + warm) + hex2(mix(g)) + hex2(mix(b) - warm);
  });
}

// ── tesserizing ─────────────────────────────────────────────────────────


interface Tile {
  layer: number;
  key: Key;
  /** Which of the four cut variants of the colour. */
  v: number;
  p: number[];
}

export interface RenderOpts {
  /** Corner radius of the panel, in canvas units (default 20). */
  radius?: number;
  /** Draw the border band (default true). */
  frame?: boolean;
  /** Small rendering (for ≤ 40 px): finer grout and calmer colour variation, so it stays legible. */
  small?: boolean;
}

export function tesserize(d: Design, opts: RenderOpts = {}): string {
  const seed = hash(d.id);
  // Each stage (courses, every shape, the frame) has its own random stream, so changing one
  // shape never reshuffles the tiles of the others.
  let rnd = mulberry(seed);
  const stream = (n: number) => (rnd = mulberry(seed ^ Math.imul(n + 1, 0x9e3779b1)));
  const cv = new Canvas();
  d.draw(cv);
  const shapes = cv.shapes;
  const T = d.t ?? 4.4;
  const G = opts.small ? 0.4 : 0.62; // grout joint (stroke width)
  const R = opts.radius ?? 20;
  const bgKey = d.bg;
  const fills: number[] = [];
  shapes.forEach((s, i) => s.kind === 0 && fills.push(i));

  /** Index of the top-most fill containing the point (fills only), or -1. */
  const topFill = (x: number, y: number, above = -1): number => {
    for (let j = fills.length - 1; j >= 0; j--) {
      const i = fills[j];
      if (i <= above) return -1;
      const s = shapes[i] as FillShape;
      if (x < s.bb[0] || y < s.bb[1] || x > s.bb[2] || y > s.bb[3]) continue;
      if (s.test(x, y)) return i;
    }
    return -1;
  };
  const colAt = (x: number, y: number): Key => {
    const i = topFill(x, y);
    return i < 0 ? bgKey : (shapes[i] as FillShape).col;
  };

  const tiles: Tile[] = [];
  let layer = 0;
  const jit = (v: number, a: number) => v + (rnd() - 0.5) * 2 * a;
  const push = (key: Key, p: number[], j = 0.06 * T) => {
    const q = p.map((v) => jit(v, j));
    tiles.push({ layer, key, v: Math.floor(rnd() * 4), p: q });
  };
  const quadArea = (p: number[]) => {
    let a = 0;
    for (let i = 0; i < 8; i += 2) {
      const j = (i + 2) % 8;
      a += p[i] * p[j + 1] - p[j] * p[i + 1];
    }
    return a / 2;
  };

  // 1. Straight courses over the whole field.
  {
    let y = -rnd() * T * 0.5;
    while (y < 100) {
      const h = T * (0.9 + rnd() * 0.2);
      let x = -rnd() * T;
      const tilt = (rnd() - 0.5) * 0.6;
      while (x < 100) {
        const w = T * (0.75 + rnd() * 0.5);
        const cx = x + w / 2,
          cy = y + h / 2;
        const key = colAt(cx, cy);
        const g = 0; // corners are jittered in push()
        push(key, [x + g, y + tilt, x + w - g, y - tilt, x + w - g, y + h, x + g, y + h], 0.08 * T);
        x += w;
      }
      y += h;
    }
  }

  // Samples along an offset polyline, as arc-length lookups.
  const interp = (P: number[], N: number[], S: Float64Array, closed: boolean, s: number) => {
    const n = P.length / 2;
    if (closed) {
      const L = S[S.length - 1];
      s = ((s % L) + L) % L;
    }
    let lo = 0,
      hi = S.length - 1;
    while (hi - lo > 1) {
      const mid = (lo + hi) >> 1;
      if (S[mid] <= s) lo = mid;
      else hi = mid;
    }
    const f = S[hi] > S[lo] ? Math.max(0, Math.min(1, (s - S[lo]) / (S[hi] - S[lo]))) : 0;
    const a = lo % n,
      b = closed ? hi % n : Math.min(hi, n - 1);
    let nx = N[2 * a] + (N[2 * b] - N[2 * a]) * f,
      ny = N[2 * a + 1] + (N[2 * b + 1] - N[2 * a + 1]) * f;
    const l = Math.hypot(nx, ny) || 1;
    nx /= l;
    ny /= l;
    return [P[2 * a] + (P[2 * b] - P[2 * a]) * f, P[2 * a + 1] + (P[2 * b + 1] - P[2 * a + 1]) * f, nx, ny];
  };

  /** How far from (x, y) along (nx, ny) the colour stays `expect`. */
  const probe = (x: number, y: number, nx: number, ny: number, expect: Key, max: number, from = 0) => {
    for (let t = from + 0.35; t < max; t += 0.55) if (colAt(x + nx * t, y + ny * t) !== expect) return t - 0.3;
    return max;
  };

  /** Split [0, L] into tile lengths near `len`. */
  const cutLengths = (L: number, len: number): number[] => {
    const m = Math.max(1, Math.round(L / len));
    const ls = Array.from({ length: m }, () => 0.78 + rnd() * 0.44);
    const sum = ls.reduce((a, b) => a + b, 0);
    return ls.map((v) => (v / sum) * L);
  };

  /**
   * One row of tiles alongside an edge polyline P (normals N point to `side` = +1 outwards).
   * The row covers depths [r·t, (r+1)·t] from the edge, trimmed where the colour stops being `expect`.
   */
  const row = (P: number[], N: number[], closed: boolean, side: number, r: number, t: number, expect: Key, key: Key, skip?: (x: number, y: number) => boolean) => {
    const n = P.length / 2;
    if (n < 2) return;
    const d0 = r * t,
      dm = (r + 0.5) * t,
      d1 = (r + 1) * t;
    const cnt = closed ? n + 1 : n;
    const S = new Float64Array(cnt);
    let px = P[0] + side * N[0] * dm,
      py = P[1] + side * N[1] * dm;
    for (let k = 1; k < cnt; k++) {
      const i = k % n;
      const qx = P[2 * i] + side * N[2 * i] * dm,
        qy = P[2 * i + 1] + side * N[2 * i + 1] * dm;
      S[k] = S[k - 1] + Math.hypot(qx - px, qy - py);
      px = qx;
      py = qy;
    }
    const L = S[cnt - 1];
    if (L < t * 0.45) return;
    // Edge-space arc length for positions: map offset arc positions back through the same index.
    const lens = cutLengths(L, t * (0.95 + rnd() * 0.15));
    let s = closed ? rnd() * lens[0] : 0;
    for (const len of lens) {
      const sa = closed ? s : Math.max(0, s),
        sb = closed ? s + len : Math.min(L, s + len);
      s += len;
      if (sb - sa < t * 0.3) continue;
      const A = interp(P, N, S, closed, sa),
        B = interp(P, N, S, closed, sb),
        M = interp(P, N, S, closed, (sa + sb) / 2);
      const mx = M[0],
        my = M[1],
        mnx = side * M[2],
        mny = side * M[3];
      const avail = probe(mx, my, mnx, mny, expect, d1 + 0.01, d0);
      if (avail < d0 + 0.42 * t) continue;
      const e1 = avail < d1 ? avail - 0.1 : d1;
      const q = [
        A[0] + side * A[2] * d0,
        A[1] + side * A[3] * d0,
        B[0] + side * B[2] * d0,
        B[1] + side * B[3] * d0,
        B[0] + side * B[2] * e1,
        B[1] + side * B[3] * e1,
        A[0] + side * A[2] * e1,
        A[1] + side * A[3] * e1,
      ];
      const cx = (q[0] + q[2] + q[4] + q[6]) / 4,
        cy = (q[1] + q[3] + q[5] + q[7]) / 4;
      if (skip && skip(cx, cy)) continue;
      const area = quadArea(q) * side;
      // Folded (concave) spots produce inverted or sliver quads: leave those to the courses below.
      if (!(Math.abs(area) > 0.3 * (sb - sa) * (e1 - d0))) continue;
      if (Math.sign(quadArea(q)) !== Math.sign(quadArea([A[0], A[1], B[0], B[1], B[0] + side * B[2], B[1] + side * B[3], A[0] + side * A[2], A[1] + side * A[3]]))) continue;
      push(key, q, 0.045 * t);
    }
  };

  const coveredAfter = (idx: number, x: number, y: number) => topFill(x, y, idx) >= 0;

  // 2–3. Shapes in painter's order.
  const E = 0.6;
  shapes.forEach((sh, idx) => {
    layer++;
    stream(idx + 1);
    if (sh.kind === 0) {
      const t = sh.o.t ?? T;
      for (const poly of sh.polys) {
        const P = resample(poly, true, 0.6);
        const n = P.length / 2;
        if (n < 4) continue;
        const N = normals(P, true);
        const vis = new Array<boolean>(n).fill(false);
        const outc = new Array<Key>(n).fill("");
        for (let k = 0; k < n; k++) {
          const x = P[2 * k],
            y = P[2 * k + 1];
          let nx = N[2 * k],
            ny = N[2 * k + 1];
          const a = sh.test(x + nx * 0.45, y + ny * 0.45),
            b = sh.test(x - nx * 0.45, y - ny * 0.45);
          if (a && !b) {
            nx = -nx;
            ny = -ny;
            N[2 * k] = nx;
            N[2 * k + 1] = ny;
          } else if (a === b) continue;
          if (topFill(x - nx * E, y - ny * E) !== idx) continue;
          const oc = colAt(x + nx * E, y + ny * E);
          if (oc === sh.col) continue;
          vis[k] = true;
          outc[k] = oc;
        }
        // Group into runs of the same outside colour.
        const runs: { P: number[]; N: number[]; closed: boolean; oc: Key }[] = [];
        const same = (a: number, b: number) => vis[a] && vis[b] && outc[a] === outc[b];
        let start = -1;
        for (let k = 0; k < n; k++)
          if (!same(k, (k + n - 1) % n)) {
            start = k;
            break;
          }
        if (start < 0) {
          if (vis[0]) runs.push({ P, N, closed: true, oc: outc[0] });
        } else {
          let k = start;
          for (let c = 0; c < n; ) {
            if (!vis[k]) {
              k = (k + 1) % n;
              c++;
              continue;
            }
            const rp: number[] = [],
              rn: number[] = [];
            const oc = outc[k];
            while (c < n && vis[k] && outc[k] === oc) {
              rp.push(P[2 * k], P[2 * k + 1]);
              rn.push(N[2 * k], N[2 * k + 1]);
              k = (k + 1) % n;
              c++;
            }
            if (rp.length >= 4) runs.push({ P: rp, N: rn, closed: false, oc });
          }
        }
        // Small shapes (a leaf, a gem) don't get rows outside them: that would bury them in halos.
        const small = (sh.bb[2] - sh.bb[0]) * (sh.bb[3] - sh.bb[1]) < 7 * T * T;
        for (const run of runs) {
          const out = sh.o.out ?? (small ? 0 : run.oc === bgKey ? (d.halo ?? 2) : 1);
          // Rows outside a shape belong to the neighbouring stone: they keep a normal size.
          const ot = Math.max(t, T * 0.85);
          for (let r = 0; r < out; r++) row(run.P, run.N, run.closed, 1, r, ot, run.oc, run.oc);
          const rows = sh.o.rows ?? 1;
          for (let r = 0; r < rows; r++) row(run.P, run.N, run.closed, -1, r, t, sh.col, r === 0 && sh.o.rim ? sh.o.rim : sh.col);
        }
      }
    } else if (sh.kind === 1) {
      const P = resample(sh.pts, sh.closed, 0.5);
      if (P.length < 4) return;
      const N = normals(P, sh.closed, 1);
      const w = sh.w;
      const skip = (x: number, y: number) => coveredAfter(idx, x, y);
      // Halo: rows of the surrounding colour along both sides.
      const halo = sh.o.halo ?? 0;
      if (halo) {
        for (const side of [1, -1]) {
          const n = P.length / 2;
          const oc: Key[] = [];
          for (let k = 0; k < n; k++) {
            const x = P[2 * k] + side * N[2 * k] * (w / 2 + 0.6),
              y = P[2 * k + 1] + side * N[2 * k + 1] * (w / 2 + 0.6);
            oc.push(coveredAfter(idx, x, y) ? "" : colAt(x, y));
          }
          let k = 0;
          while (k < n) {
            const c = oc[k];
            const rp: number[] = [],
              rn: number[] = [];
            while (k < n && oc[k] === c) {
              rp.push(P[2 * k] + side * N[2 * k] * (w / 2), P[2 * k + 1] + side * N[2 * k + 1] * (w / 2));
              rn.push(N[2 * k], N[2 * k + 1]);
              k++;
            }
            if (c && rp.length >= 4) for (let r = 0; r < halo; r++) row(rp, rn, sh.closed && rp.length === 2 * n, side, r, T * 0.9, c, c, skip);
          }
        }
      }
      const lanes = Math.max(1, Math.round(w / (T * 1.15)));
      const lw = w / lanes;
      const len = sh.o.t ?? Math.min(T * 1.05, Math.max(lw * 1.2, 1.7));
      for (let l = 0; l < lanes; l++) {
        const co = -w / 2 + (l + 0.5) * lw;
        const n = P.length / 2;
        const cnt = sh.closed ? n + 1 : n;
        const S = new Float64Array(cnt);
        const Q = P.map((v, i) => v + N[i] * co);
        for (let k = 1; k < cnt; k++) {
          const a = k - 1,
            b = k % n;
          S[k] = S[k - 1] + Math.hypot(Q[2 * b] - Q[2 * a], Q[2 * b + 1] - Q[2 * a + 1]);
        }
        const L = S[cnt - 1];
        const lens = cutLengths(L, len);
        let s = sh.closed ? rnd() * lens[0] : 0;
        for (const ln of lens) {
          const sa = sh.closed ? s : Math.max(0, s),
            sb = sh.closed ? s + ln : Math.min(L, s + ln);
          s += ln;
          if (sb - sa < 0.4) continue;
          const A = interp(Q, N, S, sh.closed, sa),
            B = interp(Q, N, S, sh.closed, sb);
          const h = lw / 2;
          const q = [A[0] + A[2] * h, A[1] + A[3] * h, B[0] + B[2] * h, B[1] + B[3] * h, B[0] - B[2] * h, B[1] - B[3] * h, A[0] - A[2] * h, A[1] - A[3] * h];
          const cx = (q[0] + q[2] + q[4] + q[6]) / 4,
            cy = (q[1] + q[3] + q[5] + q[7]) / 4;
          if (skip(cx, cy)) continue;
          push(sh.col, q, 0.035 * T);
        }
      }
    } else {
      if (coveredAfter(idx, sh.x, sh.y)) return;
      const a = sh.a * DEG,
        cs = Math.cos(a),
        sn = Math.sin(a);
      const hw = sh.w / 2,
        hh = sh.h / 2;
      const q: number[] = [];
      for (const [u, v] of [
        [-hw, -hh],
        [hw, -hh],
        [hw, hh],
        [-hw, hh],
      ])
        q.push(sh.x + u * cs - v * sn, sh.y + u * sn + v * cs);
      push(sh.col, q, 0.03 * T);
    }
  });

  // 4. Border band following the rounded frame.
  const frameKey = d.frame ?? "k";
  if (opts.frame !== false && frameKey) {
    layer++;
    stream(9999);
    const fw = 3.4;
    const inset = fw / 2 + 0.2;
    const rr = R - inset;
    const p: number[] = [];
    const corner = (cx: number, cy: number, a0: number) => {
      for (let i = 0; i <= 8; i++) {
        const a = (a0 + (i / 8) * 90) * DEG;
        p.push(cx + Math.cos(a) * rr, cy + Math.sin(a) * rr);
      }
    };
    const lo = inset + rr,
      hi = 100 - inset - rr;
    corner(hi, lo, -90);
    corner(hi, hi, 0);
    corner(lo, hi, 90);
    corner(lo, lo, 180);
    const P = resample(p, true, 0.5);
    const N = normals(P, true, 1);
    const n = P.length / 2;
    const S = new Float64Array(n + 1);
    for (let k = 1; k <= n; k++) {
      const a = k - 1,
        b = k % n;
      S[k] = S[k - 1] + Math.hypot(P[2 * b] - P[2 * a], P[2 * b + 1] - P[2 * a + 1]);
    }
    const lens = cutLengths(S[n], fw * 1.15);
    let s = rnd() * lens[0];
    for (const ln of lens) {
      const sa = s,
        sb = s + ln;
      s += ln;
      const A = interp(P, N, S, true, sa),
        B = interp(P, N, S, true, sb);
      const h = fw / 2;
      push(frameKey, [A[0] + A[2] * h, A[1] + A[3] * h, B[0] + B[2] * h, B[1] + B[3] * h, B[0] - B[2] * h, B[1] - B[3] * h, A[0] - A[2] * h, A[1] - A[3] * h], 0.1);
    }
  }

  // Emit: one <path> per (layer, colour variant), in order. Coordinates are quarter units
  // (viewBox 0..400) written as integers, relative within each tile.
  const vcache = new Map<Key, string[]>();
  const colourOf = (key: Key, v: number) => {
    let vs = vcache.get(key);
    if (!vs) {
      vs = variants(key, mulberry(seed ^ hash(key)), opts.small ? 0.6 : 1);
      vcache.set(key, vs);
    }
    return vs[v];
  };
  let body = "";
  let cur = -1;
  let groups = new Map<string, string>();
  const flush = () => {
    for (const [fill, dd] of groups) body += `<path fill='${fill}' d='${dd}'/>`;
    groups = new Map();
  };
  const num = (n: number, first: boolean) => (n < 0 ? String(n) : first ? String(n) : " " + n);
  for (const t of tiles) {
    if (t.layer !== cur) {
      flush();
      cur = t.layer;
    }
    const fill = colourOf(t.key, t.v);
    const p = t.p;
    let px = Math.round(p[0] * 4),
      py = Math.round(p[1] * 4);
    let s = "M" + px + num(py, false) + "l";
    for (let i = 2; i < 8; i += 2) {
      const x = Math.round(p[i] * 4),
        y = Math.round(p[i + 1] * 4);
      s += num(x - px, i === 2) + num(y - py, false);
      px = x;
      py = y;
    }
    s += "z";
    groups.set(fill, (groups.get(fill) ?? "") + s);
  }
  flush();
  const grout = GROUT;
  return (
    `<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 400 400'>` +
    `<clipPath id='c'><rect width='400' height='400' rx='${R * 4}'/></clipPath>` +
    `<g clip-path='url(#c)'><rect width='400' height='400' fill='${grout}'/>` +
    `<g stroke='${grout}' stroke-width='${G * 4}' stroke-linejoin='round'>${body}</g></g></svg>`
  );
}

/** Encode an SVG string as a compact data URL (usable in <img src> and CSS url("…")). */
export function svgDataUrl(svg: string): string {
  return "data:image/svg+xml," + svg.replace(/[%#<>"\n]/g, (c) => "%" + c.charCodeAt(0).toString(16).toUpperCase().padStart(2, "0"));
}
