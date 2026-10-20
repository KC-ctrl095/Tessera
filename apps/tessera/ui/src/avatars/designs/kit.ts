// Shared drawing helpers for the designs: busts, faces, wreaths, bolts, crescents, rays, waves.
import type { Canvas, FillOpts, Key } from "../engine";

const R = Math.PI / 180;
const cos = (a: number) => Math.cos(a * R);
const sin = (a: number) => Math.sin(a * R);

/** Shoulders and chest in a garment. `y` is the top of the shoulders at the centre. */
export function bust(c: Canvas, col: Key, o: { y?: number; w?: number; rim?: Key; neck?: Key | false; skin?: Key } = {}) {
  const y = o.y ?? 77,
    w = o.w ?? 40;
  if (o.neck !== false) {
    c.rect(44, y - 17, 12, 19, o.skin ?? "f", { t: 2.6, out: 0 });
    c.line([44.5, y - 14.5, 50, y - 13.2, 55.5, y - 14.5], 2.2, "F");
  }
  c.path(`M${50 - w - 8} 102C${50 - w - 4} ${y + 9} ${50 - w * 0.55} ${y} 50 ${y - 1}C${50 + w * 0.55} ${y} ${50 + w + 4} ${y + 9} ${50 + w + 8} 102Z`, col, { rim: o.rim });
}

export interface FaceOpts {
  x?: number;
  y?: number;
  rx?: number;
  ry?: number;
  skin?: Key;
  shade?: Key;
}

/** The face oval, with a contour of darker flesh. */
export function faceFill(c: Canvas, o: FaceOpts = {}) {
  c.ellipse(o.x ?? 50, o.y ?? 45, o.rx ?? 12.5, o.ry ?? 15.5, o.skin ?? "f", 0, { rim: o.shade ?? "F", rows: 2, t: 2.8 });
}

export interface FeatureOpts extends FaceOpts {
  brow?: Key;
  eye?: Key;
  white?: Key | false;
  mouth?: Key | false;
  nose?: Key | false;
  cheeks?: Key | false;
  /** Pupils look left (-) or right (+). */
  look?: number;
  /** Frown/stare: brows angled. */
  brows?: "arch" | "flat" | "frown" | false;
  /** Draw only one central eye. */
  cyclops?: boolean;
}

/** Eyes, brows, nose and mouth in the Byzantine manner: almond eyes, arched brows, a long nose line. */
export function features(c: Canvas, o: FeatureOpts = {}) {
  const x = o.x ?? 50,
    y = o.y ?? 45,
    rx = o.rx ?? 12.5,
    ry = o.ry ?? 15.5;
  const k = rx / 12.5,
    ky = ry / 15.5;
  const brow = o.brow ?? "H",
    eye = o.eye ?? "k",
    look = o.look ?? 0;
  const ey = y - 0.5 * ky,
    ex = 5.3 * k;
  if (o.cyclops) {
    if (o.brows !== false) c.curve([x - 6 * k, ey - 4.2 * ky, x, ey - 6 * ky, x + 6 * k, ey - 4.2 * ky], 2, brow);
    c.ellipse(x, ey, 5 * k, 3.2 * ky, o.white === false ? "W" : o.white ?? "W", 0, { rim: "k", t: 1.3, out: 0, rows: 1 });
    c.dot(x + look, ey, 2.9 * k, eye);
  } else {
    c.sym(() => {
      if (o.brows !== false) {
        const b = o.brows ?? "arch";
        if (b === "arch") c.curve([x - ex - 3.6 * k, ey - 2.6 * ky, x - ex, ey - 4.8 * ky, x - 1.6 * k, ey - 3.8 * ky], 1.7, brow);
        else if (b === "frown") c.line([x - ex - 3.8 * k, ey - 5.4 * ky, x - 1.4 * k, ey - 3.2 * ky], 1.9, brow);
        else c.line([x - ex - 3.6 * k, ey - 3.8 * ky, x - 1.8 * k, ey - 4 * ky], 1.8, brow);
      }
      if (o.white !== false) c.tile(x - ex, ey, 4.6 * k, 2.5 * ky, o.white ?? "W");
    });
    c.dot(x - ex + 0.3 + look, ey + 0.1, 2.3 * k, eye);
    c.dot(x + ex - 0.3 + look, ey + 0.1, 2.3 * k, eye);
  }
  if (o.nose !== false) {
    c.line([x, ey - 3 * ky, x - 0.3, y + 5.2 * ky], 1.6, o.nose ?? "F");
    c.tile(x, y + 6.2 * ky, 3.6 * k, 1.5, o.nose ?? "F");
  }
  if (o.cheeks) c.sym(() => c.dot(x - 6.8 * k, y + 5.5 * ky, 2.6 * k, o.cheeks as Key));
  if (o.mouth !== false) c.line([x - 3 * k, y + 10 * ky, x + 3 * k, y + 10 * ky], 1.9, o.mouth ?? "t");
}

/** A band of leaves along an arc (angles in degrees, 0 = right, 90 = down). */
export function wreath(c: Canvas, cx: number, cy: number, r: number, a0: number, a1: number, o: { leaf?: Key; leaf2?: Key; stem?: Key; size?: number; step?: number; berry?: Key } = {}) {
  const leaf = o.leaf ?? "v",
    leaf2 = o.leaf2 ?? "V",
    s = o.size ?? 1,
    step = o.step ?? 11;
  if (o.stem !== undefined) c.arc(cx, cy, r, a0, a1, 1.4 * s, o.stem);
  let i = 0;
  const dir = a1 > a0 ? 1 : -1;
  for (let a = a0; dir * (a1 - a) >= -0.01; a += step * dir, i++) {
    for (const side of [-1, 1]) {
      const rr = r + side * 2.1 * s;
      const px = cx + cos(a) * rr,
        py = cy + sin(a) * rr;
      c.tile(px, py, 4.4 * s, 2.3 * s, side < 0 ? leaf : leaf2, a + 90 + side * 32 * dir);
    }
    if (o.berry && i % 2 === 1) c.dot(cx + cos(a) * (r + 4.6 * s), cy + sin(a) * (r + 4.6 * s), 1.9 * s, o.berry);
  }
}

/** Zigzag thunderbolt, drawn in a 20×52 box at (x, y), scaled and rotated about its centre. */
export function bolt(c: Canvas, x: number, y: number, s: number, rot: number, col: Key = "G", rim: Key = "o") {
  c.group({ x: x - 10, y: y - 26, ox: 10, oy: 26, s, r: rot }, () => {
    c.poly([13, 0, 1, 27, 9, 27, 2, 52, 20, 20, 12, 20, 19, 0], col, { rim, t: 1.6 });
  });
}

/** Crescent moon: circle (cx, cy, r) minus a circle shifted along `dir` degrees by `cut`·r. */
export function crescent(c: Canvas, cx: number, cy: number, r: number, dir: number, col: Key, o: FillOpts & { cut?: number; inner?: number } = {}) {
  const cut = o.cut ?? 0.42,
    ri = r * (o.inner ?? 0.9);
  const bx = cx + cos(dir) * r * cut,
    by = cy + sin(dir) * r * cut;
  // Intersections of the two circles.
  const d = Math.hypot(bx - cx, by - cy);
  const a = (r * r - ri * ri + d * d) / (2 * d);
  const h = Math.sqrt(Math.max(0, r * r - a * a));
  const ux = (bx - cx) / d,
    uy = (by - cy) / d;
  const mx = cx + ux * a,
    my = cy + uy * a;
  const p1 = [mx - uy * h, my + ux * h],
    p2 = [mx + uy * h, my - ux * h];
  const ang = (px: number, py: number, ox: number, oy: number) => Math.atan2(py - oy, px - ox) / R;
  const pts: number[] = [];
  // Outer arc from p1 to p2 the long way round (away from the cut).
  let s0 = ang(p1[0], p1[1], cx, cy),
    s1 = ang(p2[0], p2[1], cx, cy);
  while (s1 <= s0) s1 += 360;
  // choose the arc that avoids direction `dir`
  const mid = (s0 + s1) / 2;
  const avoid = Math.cos((mid - dir) * R) > 0;
  if (avoid) {
    s1 -= 360;
  }
  const n = 36;
  for (let i = 0; i <= n; i++) {
    const t = s0 + ((s1 - s0) * i) / n;
    pts.push(cx + cos(t) * r, cy + sin(t) * r);
  }
  // Inner arc back from p2 to p1 inside the outer circle.
  let t0 = ang(p2[0], p2[1], bx, by),
    t1 = ang(p1[0], p1[1], bx, by);
  while (t1 <= t0) t1 += 360;
  const midi = (t0 + t1) / 2;
  if (Math.cos((midi - dir - 180) * R) < 0) t1 -= 360;
  for (let i = 1; i < n; i++) {
    const t = t0 + ((t1 - t0) * i) / n;
    pts.push(bx + cos(t) * ri, by + sin(t) * ri);
  }
  c.poly(pts, col, o);
}

/** Radiating rays (thin triangles) around a centre. */
export function rays(c: Canvas, cx: number, cy: number, r0: number, r1: number, n: number, col: Key, o: { a0?: number; a1?: number; w?: number; alt?: number; col2?: Key } = {}) {
  const a0 = o.a0 ?? 0,
    a1 = o.a1 ?? 360,
    w = o.w ?? 7;
  const full = a1 - a0 >= 360;
  const cnt = full ? n : n - 1;
  for (let i = 0; i < n; i++) {
    const a = a0 + ((a1 - a0) * i) / cnt;
    const long = i % 2 === 0;
    const re = long ? r1 : r0 + (r1 - r0) * (o.alt ?? 0.72);
    c.poly([cx + cos(a - w) * r0, cy + sin(a - w) * r0, cx + cos(a) * re, cy + sin(a) * re, cx + cos(a + w) * r0, cy + sin(a + w) * r0], long || !o.col2 ? col : o.col2, { t: 2.2 });
  }
}

/** A five- or eight-pointed star of tiles. */
export function star(c: Canvas, x: number, y: number, s: number, col: Key) {
  c.dot(x, y, 2.2 * s, col, 45);
  c.tile(x, y - 2.3 * s, 1.5 * s, 2 * s, col);
  c.tile(x, y + 2.3 * s, 1.5 * s, 2 * s, col);
  c.tile(x - 2.3 * s, y, 2 * s, 1.5 * s, col);
  c.tile(x + 2.3 * s, y, 2 * s, 1.5 * s, col);
}

/** Stylised waves: a band of water with curling crests (running spiral). */
export function waves(c: Canvas, y: number, o: { water?: Key; crest?: Key; n?: number; h?: number } = {}) {
  const water = o.water ?? "l",
    crest = o.crest ?? "W",
    n = o.n ?? 3,
    h = o.h ?? 16;
  const wl = 100 / n;
  let d = `M-2 102L-2 ${y + h * 0.35}`;
  for (let i = 0; i < n; i++) {
    const x0 = i * wl;
    d += `C${x0 + wl * 0.3} ${y - h * 0.1} ${x0 + wl * 0.75} ${y - h * 0.15} ${x0 + wl * 0.8} ${y + h * 0.25}C${x0 + wl * 0.85} ${y + h * 0.45} ${x0 + wl * 0.95} ${y + h * 0.45} ${x0 + wl} ${y + h * 0.35}`;
  }
  d += `L102 102Z`;
  c.path(d, water);
  for (let i = 0; i < n; i++) {
    const x0 = i * wl;
    c.curve([x0 + wl * 0.05, y + h * 0.3, x0 + wl * 0.35, y - h * 0.02, x0 + wl * 0.68, y + h * 0.02, x0 + wl * 0.76, y + h * 0.3, x0 + wl * 0.66, y + h * 0.45, x0 + wl * 0.56, y + h * 0.34], 2, crest);
  }
}

/** A lyre (tortoise-shell sound box, curved arms, crossbar, strings) in a 24×30 box at (x, y). */
export function lyre(c: Canvas, x: number, y: number, s: number, o: { body?: Key; arm?: Key; str?: Key; strings?: number } = {}) {
  const body = o.body ?? "o",
    arm = o.arm ?? "G",
    str = o.str ?? "W";
  c.group({ x: x - 12, y: y - 15, ox: 12, oy: 15, s }, () => {
    c.curve([7, 23, 3, 13, 5, 5, 2, 0], 2.6, arm);
    c.curve([17, 23, 21, 13, 19, 5, 22, 0], 2.6, arm);
    c.line([3.5, 6, 20.5, 6], 2, arm);
    const n = o.strings ?? 3;
    for (let i = 0; i < n; i++) c.line([12 + (i - (n - 1) / 2) * (n > 3 ? 2 : 3), 7, 12 + (i - (n - 1) / 2) * (n > 3 ? 2 : 3), 23], n > 3 ? 0.8 : 1, str);
    c.ellipse(12, 25, 7.5, 4.8, body, 0, { rim: "b", t: 1.8 });
  });
}

/** A three-lobed vine leaf, about 12 units across, pointing along `rot` (0 = up). */
export function vineLeaf(c: Canvas, x: number, y: number, s: number, rot: number, col: Key = "V", vein: Key = "O") {
  c.group({ x: x - 50, y: y - 50, s, r: rot }, () => {
    c.poly([50, 56, 44, 55, 43, 50, 44.5, 49, 42.5, 46, 45, 45, 46.5, 46, 47, 42, 50, 39.5, 53, 42, 53.5, 46, 55, 45, 57.5, 46, 55.5, 49, 57, 50, 56, 55], col, { t: 2, out: 0 });
    c.line([50, 55, 50, 43], 0.9, vein);
  });
}

/** A bunch of grapes: rows of round tesserae narrowing downwards. */
export function grapes(c: Canvas, x: number, y: number, s: number, col: Key = "p", hi: Key = "P") {
  const rows = [4, 3, 2, 1];
  rows.forEach((n, r) => {
    for (let i = 0; i < n; i++) {
      const gx = x + (i - (n - 1) / 2) * 3.1 * s,
        gy = y + r * 2.8 * s;
      c.dot(gx, gy, 2.6 * s, (i + r) % 3 === 0 ? hi : col, 45);
    }
  });
}

/** A rose seen from above: a round bloom with a spiral heart. */
export function rose(c: Canvas, x: number, y: number, r: number, col: Key = "R", heart: Key = "r") {
  c.circle(x, y, r, col, { t: 1.8, out: 0 });
  c.arc(x, y, r * 0.45, 0, 300, 1.3, heart);
  c.dot(x, y, 1.5, heart);
}

/** An ear of wheat: stalk plus pairs of grains, pointing along `rot` (0 = up). */
export function wheatEar(c: Canvas, x: number, y: number, len: number, rot: number, col: Key = "G", stalk: Key = "o") {
  const dx = Math.sin(rot * R),
    dy = -Math.cos(rot * R);
  c.line([x, y, x + dx * len, y + dy * len], 1.2, stalk);
  for (let i = 0; i < 4; i++) {
    const t = len * (0.45 + i * 0.16);
    const px = x + dx * t,
      py = y + dy * t;
    for (const sd of [-1, 1]) c.tile(px + -dy * sd * 1.4, py + dx * sd * 1.4, 2.6, 1.7, col, rot + 90 - sd * 35);
  }
  c.tile(x + dx * (len + 1.5), y + dy * (len + 1.5), 1.7, 2.6, col, rot);
}

/** A dove in flight, facing right, wings raised, in a 40×26 box centred at (x, y). */
export function dove(c: Canvas, x: number, y: number, s: number, o: { col?: Key; wing?: Key; flip?: boolean } = {}) {
  const col = o.col ?? "W",
    wing = o.wing ?? "q";
  c.group({ x: x - 20, y: y - 13, ox: 20, oy: 13, s, fx: o.flip }, () => {
    c.poly([19, 15, 18, 3, 23, 0, 25, 6, 26, 14], wing, { t: 1.4, out: 0 });
    c.poly([0, 14, 9, 16.5, 9, 21, 1, 24], col, { t: 1.4, out: 0 });
    c.blob([7, 18, 14, 15, 24, 15, 30, 11.5, 34.5, 11.5, 37, 14, 33.5, 16, 29, 19.5, 21, 22.5, 12, 22], col, { t: 1.6, out: 1 });
    c.poly([14, 17, 7, 5, 6, 0, 12, 2, 18, 7, 24, 16], col, { t: 1.4, out: 1 });
    c.line([9.5, 4, 15, 9], 0.9, wing);
    c.dot(33.4, 13.2, 1.3, "k");
    c.poly([36.5, 12.8, 40.5, 14, 36.5, 15.2], "o");
  });
}

/** A flame tongue with a hot core, base centred at (x, y), about 20×32 at scale 1. */
export function flame(c: Canvas, x: number, y: number, s: number, o: { outer?: Key; mid?: Key; core?: Key } = {}) {
  c.group({ x: x - 10, y: y - 32, ox: 10, oy: 32, s }, () => {
    c.blob([10, 0, 13, 7, 17.5, 13, 19.5, 21, 17, 29, 10, 32, 3, 29, 0.5, 21, 3, 13, 6, 9, 7, 13], o.outer ?? "R", { t: 1.8, out: 1 });
    c.blob([10.5, 9, 14.5, 17, 15.5, 24, 12.5, 30, 7.5, 30, 4.5, 24, 6, 18, 8, 19], o.mid ?? "g", { t: 1.6, out: 0 });
    c.blob([10, 19, 12.5, 25, 11.5, 30, 8.5, 30, 7.5, 25], o.core ?? "G", { t: 1.4, out: 0 });
  });
}

/** A leaf from (x, y) along `rot` degrees (0 = right, 90 = down): two rows of tiles meeting at the midrib. */
export function leaf(c: Canvas, x: number, y: number, len: number, w: number, rot: number, col: Key) {
  const r2 = len / 2;
  const cx = x + cos(rot) * r2,
    cy = y + sin(rot) * r2;
  const p: number[] = [];
  for (let i = 0; i < 24; i++) {
    const a = (i / 24) * Math.PI * 2;
    // pointed at both ends: narrow the ellipse towards the tips
    const ex = Math.cos(a) * r2,
      ey = Math.sin(a) * (w / 2) * Math.pow(Math.abs(Math.sin(a)), 0.35);
    p.push(cx + ex * cos(rot) - ey * sin(rot), cy + ex * sin(rot) + ey * cos(rot));
  }
  c.poly(p, col, { t: w / 2 + 0.2, out: 0 });
}

export const deg = { cos, sin };
