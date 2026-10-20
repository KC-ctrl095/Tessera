// Client-side appearance preferences, applied as CSS custom properties.
import { createEffect, createRoot, createSignal } from "solid-js";
import { createStore } from "solid-js/store";
import type { PermissionMode } from "../proto/PermissionMode";
import { lsGet, lsSet, touchUi } from "./util";

export type ThemePref = "dark" | "light" | "system";
export type Density = "comfortable" | "compact";

export interface Prefs {
  /** Schema version (for migrating old defaults). */
  v: number;
  theme: ThemePref;
  /** Accent material id (see MATERIALS). */
  accent: string;
  fontSize: number;
  density: Density;
  sidebarCollapsed: boolean;
  /** Grout between tiles, px (0–12). */
  gaps: number;
  /** Dim tiles that don't have focus. */
  dimInactive: boolean;
  /** Permission mode for new chats. */
  defaultPermission: PermissionMode;
  /** The user confirmed what YOLO means once. */
  yoloAck: boolean;
  /** Send a voice message as soon as it is transcribed. */
  voiceAutoSend: boolean;
  /** Show the "Try" tips and suggestion chips on new chats. */
  tips: boolean;
  /** Desktop: the tile under the mouse takes the focus (and the keyboard), like Hyprland. */
  focusFollowsMouse: boolean;
  /** Dismissed first-run hints. */
  hints: Record<string, boolean>;
  /** Model picker: provider id -> folded (true) or open (false). Absent: folded only when the list is long. */
  modelGroups: Record<string, boolean>;
  /** Where a tap on a file in a chat opens it: a tile here, a window of its own, or this device's app. */
  openFiles: "tile" | "window" | "app";
}

export interface Material {
  id: string;
  name: string;
  /** What it is, for the picker. */
  note: string;
  dark: string;
  light: string;
}

/**
 * Accents are materials of a mosaic. Each has a tone tuned for Nocturne
 * (dark) and a deeper one for Travertine (light) that keeps AA contrast.
 */
export const MATERIALS: Material[] = [
  { id: "smalti", name: "Gold", note: "Gold glass", dark: "#D4AF5F", light: "#8A6620" },
  { id: "lapis", name: "Blue", note: "Lapis lazuli", dark: "#7D9BE0", light: "#3D5CA8" },
  { id: "verdigris", name: "Green", note: "Weathered copper", dark: "#5FB39E", light: "#2C7564" },
  { id: "terracotta", name: "Clay", note: "Fired clay", dark: "#E07A56", light: "#A94B2A" },
  { id: "porphyry", name: "Plum", note: "Imperial stone", dark: "#B07A9C", light: "#84506F" },
  { id: "marble", name: "Stone", note: "Black and white", dark: "#DDD6CB", light: "#2E2A25" },
];

export const materialOf = (id: string) => MATERIALS.find((m) => m.id === id) ?? MATERIALS[0];

const DEFAULTS: Prefs = {
  v: 3,
  theme: "dark",
  accent: "smalti",
  fontSize: 14,
  density: "comfortable",
  sidebarCollapsed: false,
  gaps: 6,
  dimInactive: true,
  defaultPermission: "default",
  yoloAck: false,
  voiceAutoSend: false,
  tips: true,
  focusFollowsMouse: true,
  hints: {},
  modelGroups: {},
  openFiles: "tile",
};

function load(): Prefs {
  const saved = lsGet<Partial<Prefs> & Record<string, unknown>>("tessera.prefs", {});
  const p: Prefs = { ...DEFAULTS, ...saved, hints: { ...(saved.hints ?? {}) }, modelGroups: { ...(saved.modelGroups ?? {}) } };
  delete (p as unknown as Record<string, unknown>).borderWidth;
  delete (p as unknown as Record<string, unknown>).animatedBorder;
  if (!saved.v || saved.v < 3) {
    // Tesserae: colour accents became materials, tiles sit in 6px of grout,
    // and the base size is 14px. Keep the user's other choices.
    p.accent = DEFAULTS.accent;
    p.gaps = DEFAULTS.gaps;
    if (!saved.fontSize || saved.fontSize === 15) p.fontSize = DEFAULTS.fontSize;
    p.v = 3;
  }
  // Any accent from before Tesserae (a hex colour such as the old teal
  // #2dd4bf) becomes smalti gold.
  if (typeof p.accent !== "string" || !MATERIALS.some((m) => m.id === p.accent)) p.accent = DEFAULTS.accent;
  // Dismissing the "Try" card used to be a hint; it now turns tips off.
  if (saved.tips === undefined && saved.hints?.try) p.tips = false;
  p.fontSize = Math.min(17, Math.max(12, Number(p.fontSize) || DEFAULTS.fontSize));
  p.gaps = Math.min(12, Math.max(0, Number(p.gaps) || 0));
  if (!["tile", "window", "app"].includes(p.openFiles)) p.openFiles = DEFAULTS.openFiles;
  return p;
}

export const [prefs, setPrefs] = createStore<Prefs>(load());

const systemLight = matchMedia("(prefers-color-scheme: light)");
const [sysLight, setSysLight] = createSignal(systemLight.matches);
systemLight.addEventListener("change", (e) => setSysLight(e.matches));

export const resolvedTheme = () => (prefs.theme === "system" ? (sysLight() ? "light" : "dark") : prefs.theme);

type RGB = [number, number, number];

function hexToRgb(hex: string): RGB {
  let h = hex.replace("#", "").trim();
  if (h.length === 3) h = h.split("").map((c) => c + c).join("");
  const n = parseInt(h.slice(0, 6), 16);
  if (!Number.isFinite(n)) return [212, 175, 95];
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

const mix = (a: RGB, b: RGB, t: number) => a.map((v, i) => Math.round(v + (b[i] - v) * t)) as RGB;

function luminance([r, g, b]: RGB) {
  const f = (c: number) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

// Touch devices never show keyboard hints (see `touchUi`); CSS keys off this.
const coarse = matchMedia("(pointer: coarse)");
const applyTouch = () => document.documentElement.toggleAttribute("data-touch", touchUi());
applyTouch();
coarse.addEventListener("change", applyTouch);

createRoot(() => {
  createEffect(() => {
    const root = document.documentElement;
    const theme = resolvedTheme();
    const light = theme === "light";
    root.dataset.theme = theme;
    root.dataset.density = prefs.density;
    const m = materialOf(prefs.accent);
    root.dataset.material = m.id;
    const rgb = hexToRgb(light ? m.light : m.dark);
    // Text in the accent: a touch brighter on stone, a touch deeper on travertine.
    const text = light ? mix(rgb, [0, 0, 0], 0.08) : mix(rgb, [255, 255, 255], 0.1);
    // The ink that reads better on the button: the two inks tie at a luminance of 0.19.
    const ink = luminance(rgb) > 0.19 ? "#1A1408" : "#FFFDF8";
    root.style.setProperty("--accent", `rgb(${rgb.join(",")})`);
    root.style.setProperty("--accent-rgb", rgb.join(","));
    root.style.setProperty("--accent-text", `rgb(${text.join(",")})`);
    root.style.setProperty("--accent-fg", ink);
    root.style.setProperty("--fs", `${prefs.fontSize}px`);
    root.style.setProperty("--tile-gap", `${prefs.gaps}px`);
    root.dataset.dim = prefs.dimInactive ? "1" : "0";
    const meta = document.querySelector('meta[name="theme-color"]');
    meta?.setAttribute("content", light ? "#E7E0D4" : "#0E0D0C");
    lsSet("tessera.prefs", { ...prefs, hints: { ...prefs.hints }, modelGroups: { ...prefs.modelGroups } });
  });
});

export const hintSeen = (k: string) => !!prefs.hints[k];
export const dismissHint = (k: string) => setPrefs("hints", k, true);
