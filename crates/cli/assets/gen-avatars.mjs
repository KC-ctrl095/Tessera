// Renders the apps' mosaic avatars (apps/tessera/ui/src/avatars) into small
// colour grids for the terminal, where each half-block character shows two
// of these pixels: `crates/cli/assets/avatars.bin`.
//
//   node crates/cli/assets/gen-avatars.mjs        (Node 23.6 or newer)
//
// Run it again after adding or changing a design. Every portrait is cut by
// the apps' own engine (so it matches the desktop exactly), then drawn at a
// high resolution and box-filtered down to each size. The grout between the
// tesserae is left out: at a few pixels per portrait it would only darken
// it, and each terminal pixel already reads as a tile.
//
// Format (little-endian):
//   "TAV1", u8 n_sizes, n_sizes × u8 size, u16 n_designs, then per design:
//   u8 id_len, id (ASCII), u8 category (0 figure, 1 creature, 2 emblem; 255
//   for the neutral tile shown for ids this build doesn't know, last),
//   and for each size s: s·s pixels of r, g, b, a (a = how much of the pixel
//   the rounded panel covers).

import { registerHooks } from "node:module";
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// The UI sources import without extensions (a bundler resolves them).
registerHooks({
  resolve(spec, ctx, next) {
    try {
      return next(spec, ctx);
    } catch (e) {
      if (spec.startsWith(".") && !spec.endsWith(".ts")) return next(spec + ".ts", ctx);
      throw e;
    }
  },
});

const here = dirname(fileURLToPath(import.meta.url));
const src = join(here, "../../../apps/tessera/ui/src/avatars");
const { ALL_AVATARS, designFor } = await import(join(src, "catalog.ts"));
const { tesserize } = await import(join(src, "engine.ts"));
const { FALLBACK_DESIGN } = await import(join(src, "render.ts"));

export const SIZES = [2, 4, 6, 8, 12, 16, 24];
const MASTER = 480; // divisible by every size
const RADIUS = 80; // the panel's corner radius in SVG units (viewBox 400)
const CAT = { figure: 0, creature: 1, emblem: 2 };

/** Tiles in paint order: [r, g, b, x0, y0, … x3, y3] in SVG units. */
function tiles(svg) {
  const out = [];
  for (const m of svg.matchAll(/<path fill='#([0-9a-f]{6})' d='([^']*)'\/>/g)) {
    const rgb = [0, 2, 4].map((i) => parseInt(m[1].slice(i, i + 2), 16));
    for (const piece of m[2].split("z")) {
      const n = piece.match(/-?\d+/g);
      if (!n || n.length < 8) continue;
      const v = n.map(Number);
      const p = [v[0], v[1]];
      for (let i = 2; i < 8; i += 2) p.push(p[i - 2] + v[i], p[i - 1] + v[i + 1]);
      out.push([...rgb, ...p]);
    }
  }
  return out;
}

/** Whether (x, y) is inside the quad (any winding, convex or not). */
function inQuad(q, x, y) {
  let inside = false;
  for (let i = 0, j = 3; i < 4; j = i++) {
    const xi = q[3 + 2 * i],
      yi = q[4 + 2 * i],
      xj = q[3 + 2 * j],
      yj = q[4 + 2 * j];
    if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) inside = !inside;
  }
  return inside;
}

function inPanel(x, y) {
  const r = RADIUS;
  const cx = Math.min(Math.max(x, r), 400 - r),
    cy = Math.min(Math.max(y, r), 400 - r);
  return (x - cx) ** 2 + (y - cy) ** 2 <= r * r;
}

/** RGB per master pixel (-1 where no tile lands: a grout gap), in paint order. */
function master(list) {
  const px = new Int32Array(MASTER * MASTER).fill(-1);
  const k = 400 / MASTER;
  for (const q of list) {
    let x0 = Infinity,
      y0 = Infinity,
      x1 = -Infinity,
      y1 = -Infinity;
    for (let i = 3; i < 11; i += 2) {
      x0 = Math.min(x0, q[i]);
      x1 = Math.max(x1, q[i]);
      y0 = Math.min(y0, q[i + 1]);
      y1 = Math.max(y1, q[i + 1]);
    }
    const colour = (q[0] << 16) | (q[1] << 8) | q[2];
    const ia = Math.max(0, Math.floor(x0 / k)),
      ib = Math.min(MASTER - 1, Math.ceil(x1 / k));
    const ja = Math.max(0, Math.floor(y0 / k)),
      jb = Math.min(MASTER - 1, Math.ceil(y1 / k));
    for (let j = ja; j <= jb; j++)
      for (let i = ia; i <= ib; i++) if (inQuad(q, (i + 0.5) * k, (j + 0.5) * k)) px[j * MASTER + i] = colour;
  }
  return px;
}

/** Box-filter the master down to s×s RGBA. Gaps don't count; the panel's corners give alpha. */
function shrink(px, s) {
  const out = Buffer.alloc(s * s * 4);
  const f = MASTER / s;
  const k = 400 / MASTER;
  for (let y = 0; y < s; y++)
    for (let x = 0; x < s; x++) {
      let r = 0,
        g = 0,
        b = 0,
        n = 0,
        cover = 0,
        gaps = 0;
      for (let j = y * f; j < (y + 1) * f; j++)
        for (let i = x * f; i < (x + 1) * f; i++) {
          if (!inPanel((i + 0.5) * k, (j + 0.5) * k)) continue;
          cover++;
          const c = px[j * MASTER + i];
          if (c < 0) {
            gaps++;
            continue;
          }
          // Average in linear light so thin bright lines don't turn muddy.
          r += lin(c >> 16);
          g += lin((c >> 8) & 255);
          b += lin(c & 255);
          n++;
        }
      const o = (y * s + x) * 4;
      if (n) {
        out[o] = srgb(r / n);
        out[o + 1] = srgb(g / n);
        out[o + 2] = srgb(b / n);
      } else {
        out[o] = 0x2a;
        out[o + 1] = 0x23;
        out[o + 2] = 0x1d;
      }
      out[o + 3] = Math.round((255 * cover) / (f * f));
    }
  return out;
}

const lin = (v) => ((v /= 255) <= 0.04045 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
const srgb = (v) => Math.round(255 * (v <= 0.0031308 ? v * 12.92 : 1.055 * v ** (1 / 2.4) - 0.055));

const parts = [Buffer.from("TAV1"), Buffer.from([SIZES.length, ...SIZES])];
const count = Buffer.alloc(2);
const list = [...ALL_AVATARS.map((a) => [designFor(a.id), CAT[a.category]]), [FALLBACK_DESIGN, 255]];
count.writeUInt16LE(list.length);
parts.push(count);
for (const [d, cat] of list) {
  // Full colour variation for the big sizes, the calmer "small" cut below 16 pixels.
  const full = master(tiles(tesserize(d)));
  const calm = master(tiles(tesserize(d, { small: true })));
  parts.push(Buffer.from([d.id.length]), Buffer.from(d.id, "ascii"), Buffer.from([cat]));
  for (const s of SIZES) parts.push(shrink(s >= 16 ? full : calm, s));
}
const out = join(here, "avatars.bin");
const buf = Buffer.concat(parts);
writeFileSync(out, buf);
console.log(`${ALL_AVATARS.length} avatars, sizes ${SIZES.join(", ")}: ${out} (${(buf.length / 1024).toFixed(0)} KiB)`);
