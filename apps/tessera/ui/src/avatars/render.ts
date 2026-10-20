// Rendering cache: each avatar is tesserized once per size class and kept as a data URL.
// Generation takes a couple of milliseconds, so work is spread over frames: a component asks for
// a URL, gets it at once if it is cached (or cheap enough this frame), otherwise a callback.
import { tesserize, svgDataUrl, type Design } from "./engine";
import { PALETTE } from "./palette";
import { designFor } from "./catalog";

const cache = new Map<string, string>();
const FRAME_BUDGET = 7; // ms of synchronous generation per frame

let windowStart = 0;
let spent = 0;
const now = () => (typeof performance !== "undefined" ? performance.now() : Date.now());
function budgetLeft(): boolean {
  const t = now();
  if (t - windowStart > 16) {
    windowStart = t;
    spent = 0;
  }
  return spent < FRAME_BUDGET;
}

function build(key: string, d: Design, small: boolean): string {
  const t0 = now();
  const url = svgDataUrl(tesserize(d, { small }));
  spent += now() - t0;
  cache.set(key, url);
  return url;
}

type Job = { key: string; d: Design; small: boolean; cbs: ((url: string) => void)[] };
const queue: Job[] = [];
const pending = new Map<string, Job>();
let scheduled = false;

function pump() {
  scheduled = false;
  const start = now();
  while (queue.length && now() - start < FRAME_BUDGET) {
    const job = queue.shift()!;
    pending.delete(job.key);
    const url = cache.get(job.key) ?? build(job.key, job.d, job.small);
    for (const cb of job.cbs) cb(url);
  }
  if (queue.length) schedule();
}
function schedule() {
  if (scheduled) return;
  scheduled = true;
  if (typeof requestAnimationFrame !== "undefined") requestAnimationFrame(pump);
  else setTimeout(pump, 0);
}

/**
 * The data URL for a design. Returns it immediately when cached or when this frame still has
 * budget; otherwise returns null and calls `onReady` once it has been drawn.
 */
export function requestDesignUrl(d: Design, small: boolean, onReady?: (url: string) => void): string | null {
  const key = d.id + (small ? "|s" : "|l");
  const hit = cache.get(key);
  if (hit) return hit;
  if (!onReady || budgetLeft()) return build(key, d, small);
  const job = pending.get(key);
  if (job) job.cbs.push(onReady);
  else {
    const j: Job = { key, d, small, cbs: [onReady] };
    pending.set(key, j);
    // Large renders are few and prominent (an editor preview): cut them first.
    if (small) queue.push(j);
    else queue.unshift(j);
  }
  schedule();
  return null;
}

/** Synchronous data URL for a mosaic avatar id (undefined if unknown). */
export function avatarDataUrl(id: string, opts: { small?: boolean } = {}): string | undefined {
  const d = designFor(id);
  return d ? (requestDesignUrl(d, !!opts.small) ?? undefined) : undefined;
}

/** Warm the cache in idle time (e.g. before opening the picker). */
export function preloadAvatars(ids: readonly string[], small = true) {
  for (const id of ids) {
    const d = designFor(id);
    if (d) requestDesignUrl(d, small, () => {});
  }
}

/** Flat colour to show while a portrait is still being cut. */
export function designBackground(d: Design): string {
  return PALETTE[d.bg]?.[0] ?? (d.bg.startsWith("#") ? d.bg : "#3a342d");
}

// ── fields for emoji / text avatars and the unknown fallback ────────────

const HEX_RE = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i;

function mix(a: string, b: string, t: number): string {
  const p = (h: string) => {
    const v = h.slice(1);
    const f = v.length === 3 ? v.split("").map((ch) => ch + ch).join("") : v;
    return [0, 2, 4].map((i) => parseInt(f.slice(i, i + 2), 16));
  };
  const [x, y] = [p(a), p(b)];
  return "#" + x.map((v, i) => Math.round(v * (1 - t) + y[i] * t).toString(16).padStart(2, "0")).join("");
}

/** A plain tile field in a stone tinted by `color` (for emoji and letter avatars). */
export function fieldDesign(color?: string): Design {
  const c = color && HEX_RE.test(color) ? color.toLowerCase() : "#8a7f70";
  const bg = mix("#3b342d", c, 0.55);
  return { id: "field-" + c, bg, frame: "k", halo: 0, draw: () => {} };
}

/** Shown for an `m:` id this build doesn't know (a newer app, or a typo). */
export const FALLBACK_DESIGN: Design = {
  id: "unknown-tile",
  bg: "K",
  draw: (c) => {
    c.poly([50, 26, 74, 50, 50, 74, 26, 50], "q", { rim: "Q", t: 2.2, out: 1 });
    c.poly([50, 38, 62, 50, 50, 62, 38, 50], "g", { rim: "o", t: 1.8 });
    for (const [x, y] of [
      [50, 14],
      [86, 50],
      [50, 86],
      [14, 50],
    ])
      c.dot(x, y, 3.4, "q", 45);
  },
};
