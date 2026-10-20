// Emblems: vessels, instruments, plants, symbols and patterns from ancient mosaics.
import type { Canvas, Design, Key } from "../engine";
import { bolt, crescent, leaf, lyre, rays, star, waves, wreath } from "./kit";

/** Points of a stadium (a rectangle with round ends) centred on (cx, cy), long axis at `rot` degrees. */
function stadium(cx: number, cy: number, hl: number, r: number, rot: number): number[] {
  const out: number[] = [];
  const cs = Math.cos((rot * Math.PI) / 180),
    sn = Math.sin((rot * Math.PI) / 180);
  const push = (x: number, y: number) => out.push(cx + x * cs - y * sn, cy + x * sn + y * cs);
  for (let i = 0; i <= 16; i++) {
    const a = -Math.PI / 2 + (i / 16) * Math.PI;
    push(hl + Math.cos(a) * r, Math.sin(a) * r);
  }
  for (let i = 0; i <= 16; i++) {
    const a = Math.PI / 2 + (i / 16) * Math.PI;
    push(-hl + Math.cos(a) * r, Math.sin(a) * r);
  }
  return out;
}

/** A strand of three coloured bands along a closed path (for interlace). */
function strand(c: Canvas, pts: (off: number) => number[], cols: [Key, Key, Key]) {
  c.line(pts(3.7), 2.6, cols[0]);
  c.line(pts(0), 3.2, cols[1]);
  c.line(pts(-3.7), 2.6, cols[2]);
}

/** Eight-petalled rosette. */
function rosette(c: Canvas, x: number, y: number, r: number, petal: Key, petal2: Key, heart: Key) {
  for (let i = 0; i < 8; i++) {
    const a = (i / 8) * 360;
    const px = x + Math.cos((a * Math.PI) / 180) * r * 0.55,
      py = y + Math.sin((a * Math.PI) / 180) * r * 0.55;
    c.ellipse(px, py, r * 0.5, r * 0.22, i % 2 ? petal2 : petal, a, { t: 1.4, out: 0 });
  }
  c.circle(x, y, r * 0.26, heart, { t: 1.2, out: 0 });
}

export const EMBLEMS: Design[] = [
  {
    // Black-figure amphora.
    id: "amphora",
    bg: "n",
    draw: (c) => {
      c.sym(() => c.curve([43, 21, 34, 22, 31, 30, 33, 38, 37, 41], 3, "t"));
      c.poly([45.5, 89, 54.5, 89, 57, 96, 43, 96], "k", { t: 1.4 });
      c.path("M50 31C66 33 72 44 70 59C68 73 59 83 53 91L47 91C41 83 32 73 30 59C28 44 34 33 50 31Z", "t", { rim: "R", t: 1.8 });
      c.rect(43.5, 16, 13, 17, "t", { rim: "R", t: 1.6 });
      c.rect(40, 11.5, 20, 5.5, "t", { rim: "R", t: 1.4 });
      c.curve([31, 45, 50, 47, 69, 45], 2.4, "k");
      c.curve([31.5, 69, 50, 71, 68.5, 69], 2.4, "k");
      c.curve([34, 52, 38, 56, 42, 52, 46, 56, 50, 52, 54, 56, 58, 52, 62, 56, 66, 52], 1.8, "k");
      c.curve([34, 62, 38, 58, 42, 62, 46, 58, 50, 62, 54, 58, 58, 62, 62, 58, 66, 62], 1.8, "k");
      c.line([43.5, 25, 56.5, 25], 1.6, "k");
      c.curve([36, 40, 34, 52, 36, 66], 1.4, "r");
    },
  },
  {
    // Seven-stringed lyre.
    id: "lyre",
    bg: "R",
    draw: (c) => {
      lyre(c, 50, 49, 2.6, { body: "o", arm: "G", str: "W", strings: 5 });
      for (const [x, y] of [
        [40, 78],
        [50, 76],
        [60, 78],
        [45, 85],
        [55, 85],
      ])
        c.dot(x, y, 3, "b", 45);
    },
  },
  {
    // Laurel wreath tied with a red ribbon.
    id: "laurel-wreath",
    bg: "W",
    draw: (c) => {
      wreath(c, 50, 50, 27, 104, 266, { leaf: "v", leaf2: "V", stem: "O", step: 14, size: 1.55, berry: "k" });
      wreath(c, 50, 50, 27, 76, -86, { leaf: "v", leaf2: "V", stem: "O", step: 14, size: 1.55, berry: "k" });
      c.poly([50, 79, 40, 74, 38, 86], "R", { t: 1.2 });
      c.poly([50, 79, 60, 74, 62, 86], "R", { t: 1.2 });
      c.curve([48, 81, 44, 88, 40, 96], 2.4, "R");
      c.curve([52, 81, 56, 88, 60, 96], 2.4, "R");
      c.dot(50, 79.5, 3.4, "R", 45);
    },
  },
  {
    // Olive branch with leaves and dark fruit.
    id: "olive-branch",
    bg: "t",
    draw: (c) => {
      c.curve([12, 92, 32, 72, 50, 54, 68, 34, 88, 10], 3.4, "b", { halo: 1 });
      const th = -46;
      const leaves: [number, number, number][] = [
        [24, 80, -1],
        [30, 74, 1],
        [38, 66, -1],
        [45, 59, 1],
        [53, 50, -1],
        [60, 43, 1],
        [68, 34, -1],
        [74, 27, 1],
        [81, 19, -1],
      ];
      for (const [x, y, sd] of leaves) leaf(c, x, y, 18, 6, th + sd * 34, sd > 0 ? "e" : "O");
      leaf(c, 86, 13, 12, 4.6, th, "e");
      for (const [x, y] of [
        [42, 76],
        [64, 55],
        [50, 40],
      ]) {
        c.ellipse(x, y, 4.2, 5.2, "p", -25, { rim: "k", t: 1.6 });
        c.dot(x - 1.3, y - 1.8, 1.6, "P");
      }
    },
  },
  {
    // Gold trident over the sea.
    id: "trident",
    bg: "l",
    draw: (c) => {
      c.line([50, 96, 50, 38], 4, "G", { halo: 1 });
      c.curve([30, 28, 35, 37, 50, 40, 65, 37, 70, 28], 3.6, "G");
      c.line([30, 29, 30, 13], 3.4, "G");
      c.line([70, 29, 70, 13], 3.4, "G");
      c.line([50, 40, 50, 10], 3.6, "G");
      for (const x of [30, 50, 70]) c.poly([x - 4.5, x === 50 ? 11 : 14, x, x === 50 ? 2 : 5, x + 4.5, x === 50 ? 11 : 14], "G", { rim: "o", t: 1.3 });
      c.line([45, 60, 55, 60], 2, "o");
      c.line([45, 64, 55, 64], 2, "o");
      waves(c, 82, { water: "n", crest: "W", n: 3, h: 12 });
    },
  },
  {
    // Winged thunderbolt: a spindle with flames at both ends and zigzag lightning.
    id: "thunderbolt",
    bg: "n",
    draw: (c) => {
      c.line([14, 34, 22, 26, 20, 36, 30, 30], 1.8, "W");
      c.line([70, 76, 78, 68, 76, 78, 86, 72], 1.8, "W");
      c.line([74, 20, 80, 16, 79, 23, 86, 20], 1.5, "G");
      bolt(c, 50, 51, 1.62, 16, "G", "o");
    },
  },
  {
    // Sun disc with alternating rays and a rosette at its heart.
    id: "sun-disc",
    bg: "l",
    draw: (c) => {
      rays(c, 50, 50, 20, 44, 24, "G", { w: 5.5, alt: 0.65, col2: "g" });
      c.circle(50, 50, 20.5, "G", { rim: "o", t: 1.8, out: 1 });
      rosette(c, 50, 50, 13, "R", "t", "o");
    },
  },
  {
    // Crescent moon and stars.
    id: "crescent-star",
    bg: "n",
    draw: (c) => {
      star(c, 20, 18, 0.8, "W");
      star(c, 82, 80, 0.7, "W");
      star(c, 24, 84, 0.55, "G");
      crescent(c, 44, 52, 28, 12, "W", { rim: "x", t: 2, cut: 0.42, inner: 0.9 });
      c.group({ x: 24, y: -12, s: 0.9 }, () => {
        const p: number[] = [];
        for (let i = 0; i < 16; i++) {
          const a = (i / 16) * Math.PI * 2 - Math.PI / 2;
          const r = i % 2 ? 4.6 : 11;
          p.push(50 + Math.cos(a) * r, 50 + Math.sin(a) * r);
        }
        c.poly(p, "G", { rim: "o", t: 1.3 });
      });
    },
  },
  {
    // Ionic column.
    id: "column",
    bg: "L",
    draw: (c) => {
      c.rect(-2, 88, 104, 14, "q", { rim: "Q", t: 2 });
      c.rect(28, 81, 44, 8, "W", { rim: "q", t: 1.6 });
      c.rect(33, 76, 34, 5.5, "W", { rim: "q", t: 1.4 });
      c.rect(36, 30, 28, 46.5, "W", { rim: "q", t: 1.6 });
      for (const x of [41, 45.5, 50, 54.5, 59]) c.line([x, 32, x, 75], 1.2, "q");
      c.rect(32, 24, 36, 7, "W", { rim: "q", t: 1.4 });
      c.sym(() => {
        c.circle(30, 28, 6.5, "W", { rim: "q", t: 1.4 });
        c.arc(30, 28, 3.2, 0, 300, 1.2, "q");
        c.dot(30, 28, 1.6, "q");
      });
      c.rect(24, 17, 52, 7, "W", { rim: "q", t: 1.4 });
      c.rect(14, 8, 72, 9, "w", { rim: "o", t: 1.6 });
    },
  },
  {
    // Square labyrinth with a red heart.
    id: "labyrinth",
    bg: "w",
    draw: (c) => {
      const w = 3,
        col = "k";
      // rings with openings, alternating top/bottom
      const ring = (h: number, gapTop: boolean) => {
        const g = 5;
        if (gapTop) c.line([50 - g, 50 - h, 50 - h, 50 - h, 50 - h, 50 + h, 50 + h, 50 + h, 50 + h, 50 - h, 50 + g, 50 - h], w, col);
        else c.line([50 + g, 50 + h, 50 + h, 50 + h, 50 + h, 50 - h, 50 - h, 50 - h, 50 - h, 50 + h, 50 - g, 50 + h], w, col);
      };
      ring(38, false);
      ring(30, true);
      ring(22, false);
      ring(14, true);
      c.line([12, 50, 20, 50], w, col);
      c.line([80, 50, 88, 50], w, col);
      c.line([50, 20, 50, 28], w, col);
      c.line([28, 50, 36, 50], w, col);
      c.line([64, 50, 72, 50], w, col);
      c.rect(44.5, 44.5, 11, 11, "R", { rim: "R", t: 1.8 });
    },
  },
  {
    // Galley under a striped sail.
    id: "galley",
    bg: "L",
    draw: (c) => {
      waves(c, 76, { water: "l", crest: "W", n: 4, h: 10 });
      for (let i = 0; i < 6; i++) c.line([30 + i * 8, 74, 24 + i * 8, 90], 1.8, "b");
      c.line([50, 66, 50, 12], 2.4, "b");
      c.poly([28, 18, 72, 18, 70, 56, 30, 56], "W", { rim: "q", t: 1.6 });
      for (const x of [37, 50, 63]) c.line([x, 19, x, 55], 3, "R");
      c.line([25, 18, 75, 18], 2.2, "b");
      c.poly([50, 10, 62, 12, 50, 15], "R");
      c.path("M9 63L89 63C86 71 80 76 70 78L26 78C18 76 12 71 9 63Z", "b", { rim: "H", t: 1.8 });
      c.poly([10, 69, 1, 71.5, 10, 74], "b", { t: 1.2 });
      c.curve([87, 64, 93, 56, 91, 47], 3, "b");
      c.line([12, 66, 86, 66], 1.4, "G");
      c.dot(18, 70.5, 2.6, "W");
      c.dot(18.3, 70.6, 1.4, "k");
    },
  },
  {
    // Greek-key meander framing a rosette.
    id: "meander",
    bg: "W",
    draw: (c) => {
      const h = 37,
        n = 5;
      const L = 2 * h;
      const cell = L / (6 * n);
      // one side of running key, drawn along the top, then rotated for the others
      const side: number[] = [];
      for (let i = 0; i < n; i++) {
        const x0 = -h + i * 6 * cell;
        const y0 = -h;
        const P = (x: number, y: number) => side.push(50 + x0 + x * cell, 50 + y0 + y * cell);
        P(0, 4);
        P(0, 0);
        P(4, 0);
        P(4, 2.2);
        P(2, 2.2);
        P(2, 4);
      }
      side.push(50 + h, 50 - h + 4 * cell);
      for (const r of [0, 90, 180, 270]) c.group({ r }, () => c.line(side, 2, "k"));
      c.ring(50, 50, 22, 1.8, "R");
      rosette(c, 50, 50, 19, "t", "R", "G");
    },
  },
  {
    // Comic and tragic theatre masks.
    id: "theatre-mask",
    bg: "p",
    draw: (c) => {
      c.group({ x: 14, y: -8, r: 12, ox: 50, oy: 50 }, () => {
        c.blob([38, 28, 50, 22, 62, 28, 66, 44, 62, 62, 54, 72, 46, 72, 38, 62, 34, 44], "w", { rim: "q", t: 1.6, out: 1 });
        c.path("M36 36C35 26 42 20 50 20C58 20 65 26 64 36C60 31 56 29 50 29.5C44 29 40 31 36 36Z", "b", { t: 1.4, out: 0 });
        c.line([40, 37, 46, 40], 1.4, "k");
        c.line([60, 37, 54, 40], 1.4, "k");
        c.tile(43.5, 43, 4.6, 3, "k");
        c.tile(56.5, 43, 4.6, 3, "k");
        c.path("M42 64C45 56 55 56 58 64C54 61 46 61 42 64Z", "k", { t: 1, out: 0 });
      });
      c.group({ x: -14, y: 6, r: -8, ox: 50, oy: 50 }, () => {
        c.blob([38, 28, 50, 22, 62, 28, 66, 44, 62, 62, 54, 72, 46, 72, 38, 62, 34, 44], "W", { rim: "q", t: 1.6, out: 1 });
        c.path("M36 36C35 26 42 20 50 20C58 20 65 26 64 36C60 31 56 29 50 29.5C44 29 40 31 36 36Z", "g", { t: 1.4, out: 0 });
        c.line([40, 38, 46, 36], 1.4, "k");
        c.line([60, 38, 54, 36], 1.4, "k");
        c.tile(43.5, 43, 4.6, 2.6, "k");
        c.tile(56.5, 43, 4.6, 2.6, "k");
        c.sym(() => c.dot(40, 53, 3, "r"));
        c.path("M40 57C44 68 56 68 60 57C55 60 45 60 40 57Z", "k", { t: 1, out: 0 });
      });
      c.curve([24, 78, 18, 86, 22, 94], 2.2, "g");
      c.curve([30, 80, 30, 88, 36, 96], 2.2, "R");
    },
  },
  {
    // Interlaced knot of two banded loops.
    id: "knot",
    bg: "k",
    draw: (c) => {
      const A = (off: number) => stadium(50, 50, 14.5, 19 + off, 45);
      const B = (off: number) => stadium(50, 50, 14.5, 19 + off, -45);
      const loop = (f: (o: number) => number[]) => (o: number) => {
        const p = f(o);
        return [...p, p[0], p[1]];
      };
      strand(c, loop(B), ["W", "l", "W"]);
      strand(c, loop(A), ["W", "R", "W"]);
      // B passes over A at the top and bottom crossings.
      for (const cy of [15.5, 84.5]) {
        const part = (o: number) => {
          const p = B(o);
          const n = p.length / 2;
          const inside = (i: number) => Math.hypot(p[2 * i] - 50, p[2 * i + 1] - cy) < 12;
          // Start just after a point outside the window, so the run never wraps the loop's seam.
          let k = 0;
          while (k < n && inside(k)) k++;
          const out: number[] = [];
          for (let m = 1; m <= n; m++) {
            const i = (k + m) % n;
            if (inside(i)) out.push(p[2 * i], p[2 * i + 1]);
            else if (out.length) break;
          }
          return out;
        };
        if (part(0).length >= 4) strand(c, part, ["W", "l", "W"]);
      }
    },
  },
];
