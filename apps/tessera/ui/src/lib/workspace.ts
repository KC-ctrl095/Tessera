// App-wide tiling (Hyprland-style): nine workspaces ("mosaics" in the UI),
// each a binary split tree
// of tiles plus a layout. The tree is the source of truth for tile *order*
// and for the dwindle geometry; the other layouts (master, columns, rows,
// grid, monocle) derive their geometry from that order, so switching layouts
// never loses which chats are open. Presentation (tiled, two-up carousel,
// single-tile swipe) is derived from the viewport, so folding/unfolding a
// device never changes the layout, drafts or scroll positions.
import { batch, createMemo, createRoot, createSignal, type Accessor, type Setter } from "solid-js";
import { createStore, produce, reconcile } from "solid-js/store";
import type { ModelRef } from "../proto/ModelRef";
import type { Mosaic } from "../proto/Mosaic";
import type { MosaicTile } from "../proto/MosaicTile";
import type { PermissionMode } from "../proto/PermissionMode";
import type { ThinkingLevel } from "../proto/ThinkingLevel";
import type { Attachment } from "./images";
import { ensureLoaded, isBusy, onBoardRemoved, onNoteEvent, onSessionRemoved, pendingAsks, privateIds, setOpenSessionsProvider, state } from "./store";
import { toast } from "./ui";
import { clamp, debounce, lsGet, lsSet, uid } from "./util";
import { storedFile, viewFileOf, type ViewFile } from "./viewFile";

/** Settings chosen for a chat before its session exists. */
export interface NewChatDraft {
  model?: ModelRef;
  thinking?: ThinkingLevel;
  permission?: PermissionMode;
  cwd?: string;
  /** Start the chat with this bot. */
  botId?: string;
  /** A private chat: held only in the server's memory, never saved anywhere. */
  private?: boolean;
  /** Fast mode (priority processing) once the chat starts. */
  fast?: boolean;
  /** Opt into Daybreak Blue only when OpenAI confirms model access. */
  daybreak?: boolean;
  /** false: the agent may not start subagents. */
  subagents?: boolean;
}

/**
 * What a tile shows. `chat` covers every session kind (plain chats, bot
 * chats, channels and private bot DMs); `sessionId: null` is a new chat.
 */
export type PaneContent =
  | { type: "chat"; sessionId: string | null; draft?: NewChatDraft }
  /** Something an agent shows the user (see components/board). */
  | { type: "whiteboard"; boardId: string }
  /** A note from the notes library, with its editor (see components/notes). */
  | { type: "note"; noteId: string }
  /** A text file from a chat, to read (see components/files). Kept on this device only. */
  | { type: "file"; file: ViewFile }
  /**
   * A tile spawned from "+" before it has content: a web page waiting for its
   * address, an image for its file, a web app for its port. It becomes a board
   * (`create_board`) once given one; until then it stays on this device.
   */
  | { type: "draft"; kind: DraftKind; sessionId?: string | null }
  /**
   * A terminal (see components/terminal): on the server (`machine: null`) or a
   * machine; `machine` undefined while the place is being chosen. It belongs to
   * this device: kept here only, never shared with other devices' mosaics.
   */
  | { type: "terminal"; machine?: string | null; termId?: string }
  /** A tile another client made that this version doesn't understand: kept as it is. */
  | { type: "other"; tile: Record<string, unknown> };

/** Spawned tiles waiting for content; "assist" is an empty space left by a snap, offering things to fill it. */
/** A spawned web view waiting for its address ("url"), or empty space a snap layout left ("assist"). */
export type DraftKind = "url" | "assist";

export interface Pane {
  id: string;
  content: PaneContent;
}

/** `h` = children side by side (vertical divider); `v` = stacked. */
export type SplitDir = "h" | "v";
export type LayoutNode =
  | { t: "leaf"; pane: string }
  | { t: "split"; id: string; dir: SplitDir; ratio: number; a: LayoutNode; b: LayoutNode };

export type LayoutKind = "dwindle" | "master" | "columns" | "rows" | "grid" | "monocle";

export const LAYOUTS: { id: LayoutKind; label: string; desc: string }[] = [
  { id: "dwindle", label: "Dwindle", desc: "Each new tile splits the focused one" },
  { id: "master", label: "Master", desc: "One large tile plus a stack" },
  { id: "columns", label: "Columns", desc: "Side by side" },
  { id: "rows", label: "Rows", desc: "Stacked" },
  { id: "grid", label: "Grid", desc: "Even grid" },
  { id: "monocle", label: "Monocle", desc: "One tile at a time, with tabs" },
];

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export type DividerTarget = { kind: "split"; id: string } | { kind: "mfact" } | { kind: "weights"; a: string; b: string };

export interface DividerGeom {
  id: string;
  dir: SplitDir;
  /** Rect the divider splits (fractions of the workspace). */
  parent: Rect;
  ratio: number;
  target: DividerTarget;
}

export interface Geometry {
  panes: Map<string, Rect>;
  dividers: DividerGeom[];
}

export type Direction = "left" | "right" | "up" | "down";
export type DropZone = "left" | "right" | "top" | "bottom" | "center";
export type PresentationMode = "mobile" | "dual" | "tile";

export const MAX_PANES = 9;
export const WORKSPACES = 9;
/** Two tiles side by side from here: unfolded foldables (a Galaxy Z Fold's inner screen is ~673 wide) and landscape phones. */
export const BP_DUAL = 640;
export const BP_TILE = 1100;

interface WsData {
  root: LayoutNode | null;
  focused: string | null;
  layout: LayoutKind;
  mfact: number;
  weights: Record<string, number>;
  /** Kept on this device only: never shared with the server (see `setMosaicLocal`). */
  local?: boolean;
  /** Two-up (foldables): the left tile's share of the width. */
  dual?: number;
  /** A name the user gave it ("Research"); none shows as "Mosaic N". */
  name?: string;
}

interface PersistedV2 {
  v: 2;
  active: number;
  workspaces: WsData[];
  panes: Record<string, Pane>;
  /** The shared layout as last agreed with the server (see mosaicSync.ts). */
  sync?: SyncState | null;
  /** Tiles of this device that aren't shared until used (see `placeholders`). */
  placeholders?: string[];
  /** What the server calls each slot's mosaic (see `remoteMeta`). */
  meta?: RemoteMeta[];
  /** Shared mosaics with no free slot here (see `overflow`). */
  overflow?: Mosaic[];
}

/** The server's layout as last agreed: the base of the three-way merge. */
export interface SyncState {
  version: number;
  base: Mosaic[];
}

/** The pre-workspaces format (a single dwindle tree). */
interface PersistedV1 {
  root: LayoutNode;
  panes: Record<string, Pane>;
  focused: string;
}

// ── tree utilities (pure) ────────────────────────────────────────────────

export function leaves(n: LayoutNode | null): string[] {
  if (!n) return [];
  return n.t === "leaf" ? [n.pane] : [...leaves(n.a), ...leaves(n.b)];
}

function mapTree(n: LayoutNode, f: (n: LayoutNode) => LayoutNode | null): LayoutNode | null {
  const r = f(n);
  if (r !== n) return r;
  if (n.t === "leaf") return n;
  const a = mapTree(n.a, f);
  const b = mapTree(n.b, f);
  if (!a) return b;
  if (!b) return a;
  return a === n.a && b === n.b ? n : { ...n, a, b };
}

const removeLeaf = (root: LayoutNode | null, id: string): LayoutNode | null =>
  root ? mapTree(root, (n) => (n.t === "leaf" && n.pane === id ? null : n)) : null;

function insertAt(root: LayoutNode | null, target: string | null, pane: string, zone: DropZone | SplitDir): LayoutNode {
  const leaf: LayoutNode = { t: "leaf", pane };
  if (!root || !target || !leaves(root).includes(target)) {
    if (!root) return leaf;
    // Append at the end of the order, beside the whole tree.
    return { t: "split", id: uid("s_"), dir: "h", ratio: 0.5, a: root, b: leaf };
  }
  return mapTree(root, (n) => {
    if (n.t !== "leaf" || n.pane !== target) return n;
    const dir: SplitDir = zone === "h" || zone === "left" || zone === "right" ? "h" : "v";
    const before = zone === "left" || zone === "top";
    return { t: "split", id: uid("s_"), dir, ratio: 0.5, a: before ? leaf : n, b: before ? n : leaf };
  })!;
}

function validTree(n: unknown, panes: Record<string, Pane>, seen: Set<string>): n is LayoutNode {
  if (!n || typeof n !== "object") return false;
  const o = n as LayoutNode;
  if (o.t === "leaf") {
    if (!panes[o.pane] || seen.has(o.pane)) return false;
    seen.add(o.pane);
    return true;
  }
  if (o.t === "split") {
    return (
      typeof o.ratio === "number" &&
      (o.dir === "h" || o.dir === "v") &&
      validTree(o.a, panes, seen) &&
      validTree(o.b, panes, seen)
    );
  }
  return false;
}

/** A saved tree without leaves whose tile is gone or already placed (in this or another mosaic). */
function pruneTree(n: LayoutNode, panes: Record<string, Pane>, seen: Set<string>): LayoutNode | null {
  if (n.t === "leaf") {
    if (!panes[n.pane] || seen.has(n.pane)) return null;
    seen.add(n.pane);
    return n;
  }
  const a = pruneTree(n.a, panes, seen);
  const b = pruneTree(n.b, panes, seen);
  if (!a) return b;
  if (!b) return a;
  return a === n.a && b === n.b ? n : { ...n, a, b };
}

// ── geometry per layout (pure) ───────────────────────────────────────────

const FULL: Rect = { x: 0, y: 0, w: 1, h: 1 };

function dwindleGeometry(root: LayoutNode | null): Geometry {
  const panes = new Map<string, Rect>();
  const dividers: DividerGeom[] = [];
  const walk = (n: LayoutNode, r: Rect) => {
    if (n.t === "leaf") {
      panes.set(n.pane, r);
      return;
    }
    dividers.push({ id: n.id, dir: n.dir, parent: r, ratio: n.ratio, target: { kind: "split", id: n.id } });
    if (n.dir === "h") {
      const aw = r.w * n.ratio;
      walk(n.a, { x: r.x, y: r.y, w: aw, h: r.h });
      walk(n.b, { x: r.x + aw, y: r.y, w: r.w - aw, h: r.h });
    } else {
      const ah = r.h * n.ratio;
      walk(n.a, { x: r.x, y: r.y, w: r.w, h: ah });
      walk(n.b, { x: r.x, y: r.y + ah, w: r.w, h: r.h - ah });
    }
  };
  if (root) walk(root, FULL);
  return { panes, dividers };
}

/** Lay `ids` out along one axis inside `box`, sized by `weights`. */
function strip(ids: string[], box: Rect, dir: SplitDir, weights: Record<string, number>, g: Geometry) {
  const w = ids.map((id) => Math.max(0.05, weights[id] ?? 1));
  const total = w.reduce((a, b) => a + b, 0);
  let off = 0;
  const rects: Rect[] = ids.map((_, i) => {
    const f = w[i] / total;
    const r = dir === "h" ? { x: box.x + box.w * off, y: box.y, w: box.w * f, h: box.h } : { x: box.x, y: box.y + box.h * off, w: box.w, h: box.h * f };
    off += f;
    return r;
  });
  ids.forEach((id, i) => g.panes.set(id, rects[i]));
  for (let i = 0; i + 1 < ids.length; i++) {
    const a = rects[i];
    const b = rects[i + 1];
    const parent = dir === "h" ? { x: a.x, y: a.y, w: a.w + b.w, h: a.h } : { x: a.x, y: a.y, w: a.w, h: a.h + b.h };
    g.dividers.push({
      id: `w:${ids[i]}:${ids[i + 1]}`,
      dir,
      parent,
      ratio: w[i] / (w[i] + w[i + 1]),
      target: { kind: "weights", a: ids[i], b: ids[i + 1] },
    });
  }
}

export function computeGeometry(
  kind: LayoutKind,
  root: LayoutNode | null,
  order: string[],
  mfact: number,
  weights: Record<string, number>,
  area: { w: number; h: number },
): Geometry {
  const g: Geometry = { panes: new Map(), dividers: [] };
  const n = order.length;
  if (!n) return g;
  if (n === 1 || kind === "monocle") {
    for (const id of order) g.panes.set(id, FULL);
    return g;
  }
  const wide = area.w >= area.h * 0.9;
  switch (kind) {
    case "dwindle":
      return dwindleGeometry(root);
    case "columns":
      strip(order, FULL, "h", weights, g);
      return g;
    case "rows":
      strip(order, FULL, "v", weights, g);
      return g;
    case "master": {
      const [master, ...stack] = order;
      const m = clamp(mfact, 0.15, 0.85);
      const mRect = wide ? { x: 0, y: 0, w: m, h: 1 } : { x: 0, y: 0, w: 1, h: m };
      const sRect = wide ? { x: m, y: 0, w: 1 - m, h: 1 } : { x: 0, y: m, w: 1, h: 1 - m };
      g.panes.set(master, mRect);
      g.dividers.push({ id: "mfact", dir: wide ? "h" : "v", parent: FULL, ratio: m, target: { kind: "mfact" } });
      strip(stack, sRect, wide ? "v" : "h", weights, g);
      return g;
    }
    case "grid": {
      let cols = Math.ceil(Math.sqrt(n));
      let rows = Math.ceil(n / cols);
      if (!wide) [cols, rows] = [rows, cols];
      rows = Math.ceil(n / cols);
      let i = 0;
      for (let r = 0; r < rows; r++) {
        const inRow = Math.min(cols, n - i);
        for (let c = 0; c < inRow; c++, i++) {
          g.panes.set(order[i], { x: c / inRow, y: r / rows, w: 1 / inRow, h: 1 / rows });
        }
      }
      return g;
    }
  }
  return g;
}

// ── reactive workspaces ──────────────────────────────────────────────────

const arrEq = (a: string[], b: string[]) => a.length === b.length && a.every((v, i) => v === b[i]);

interface WsSig {
  root: Accessor<LayoutNode | null>;
  setRoot: Setter<LayoutNode | null>;
  focused: Accessor<string | null>;
  setFocused: Setter<string | null>;
  layout: Accessor<LayoutKind>;
  setLayout: Setter<LayoutKind>;
  maximized: Accessor<string | null>;
  setMaximized: Setter<string | null>;
  mfact: Accessor<number>;
  setMfact: Setter<number>;
  weights: Accessor<Record<string, number>>;
  setWeights: Setter<Record<string, number>>;
  local: Accessor<boolean>;
  setLocal: Setter<boolean>;
  name: Accessor<string | null>;
  setName: Setter<string | null>;
  dual: Accessor<number>;
  setDual: Setter<number>;
  order: Accessor<string[]>;
}

function makeWs(): WsSig {
  const [root, setRoot] = createSignal<LayoutNode | null>(null);
  const [focused, setFocused] = createSignal<string | null>(null);
  const [layout, setLayout] = createSignal<LayoutKind>("dwindle");
  const [maximized, setMaximized] = createSignal<string | null>(null);
  const [mfact, setMfact] = createSignal(0.55);
  const [weights, setWeights] = createSignal<Record<string, number>>({});
  const [local, setLocal] = createSignal(false);
  const [dual, setDual] = createSignal(0.5);
  const [name, setName] = createSignal<string | null>(null);
  const order = createMemo(() => leaves(root()), undefined, { equals: arrEq });
  return { root, setRoot, focused, setFocused, layout, setLayout, maximized, setMaximized, mfact, setMfact, weights, setWeights, local, setLocal, name, setName, dual, setDual, order };
}

function freshPane(content: PaneContent = { type: "chat", sessionId: null }): Pane {
  return { id: uid("p_"), content };
}

const ws = createRoot(() => {
  const list = Array.from({ length: WORKSPACES }, makeWs);
  const [active, setActive] = createSignal(0);
  const [panes, setPanes] = createStore<Record<string, Pane>>({});
  const [viewport, setViewport] = createSignal({ w: window.innerWidth, h: window.innerHeight });
  const [areaSize, setAreaSize] = createSignal({ w: window.innerWidth, h: window.innerHeight });
  const [serverKey, setServerKey] = createSignal<string | null>(null);

  const cur = () => list[active()];
  const geometry = createMemo(() => {
    const w = cur();
    return computeGeometry(w.layout(), w.root(), w.order(), w.mfact(), w.weights(), areaSize());
  });
  /** pane id → workspace index */
  const owner = createMemo(() => {
    const m = new Map<string, number>();
    list.forEach((w, i) => w.order().forEach((p) => m.set(p, i)));
    return m;
  });
  const mode = createMemo<PresentationMode>(() => {
    const w = viewport().w;
    return w < BP_DUAL ? "mobile" : w < BP_TILE ? "dual" : "tile";
  });

  // Start with one new-chat tile on workspace 1.
  const first = freshPane();
  setPanes(first.id, first);
  list[0].setRoot({ t: "leaf", pane: first.id });
  list[0].setFocused(first.id);

  return { list, active, setActive, panes, setPanes, viewport, setViewport, areaSize, setAreaSize, serverKey, setServerKey, cur, geometry, owner, mode };
});

/** Read-only reactive view of the tiling state (the active workspace unless stated). */
export const workspace = {
  panes: ws.panes,
  /** Index (0–8) of the active workspace. */
  active: ws.active,
  root: () => ws.cur().root(),
  focused: () => ws.cur().focused(),
  maximized: () => ws.cur().maximized(),
  layoutKind: () => ws.cur().layout(),
  order: () => ws.cur().order(),
  geometry: ws.geometry,
  mode: ws.mode,
  areaSize: ws.areaSize,
  setAreaSize: ws.setAreaSize,
  /** Which workspace a pane lives on. */
  paneWorkspace: (id: string) => ws.owner().get(id) ?? -1,
  /** Tiles of any workspace, in order. */
  orderOf: (i: number) => ws.list[i]?.order() ?? [],
  focusedOf: (i: number) => ws.list[i]?.focused() ?? null,
  layoutOf: (i: number) => ws.list[i]?.layout() ?? "dwindle",
  /** The mosaic is kept on this device only (not shared with your other devices). */
  localOf: (i: number) => !!ws.list[i]?.local(),
  /** The name the user gave a mosaic, if any. */
  nameOf: (i: number) => ws.list[i]?.name() ?? null,
  /** "Mosaic 2", or "Research" when it has a name. */
  titleOf: (i: number) => ws.list[i]?.name() ?? `Mosaic ${i + 1}`,
  /** Two-up (foldables): the left tile's share of the width, for the active mosaic. */
  dualRatio: () => ws.cur().dual(),
  /** A mosaic's tiled geometry for an area of the given size (previews); only tiles that exist take room. */
  geometryOf: (i: number, area: { w: number; h: number }) => {
    const w = ws.list[i];
    if (!w) return { panes: new Map<string, Rect>(), dividers: [] };
    const root = w.root();
    const tree = root ? pruneTree(root, ws.panes, new Set()) : null;
    return computeGeometry(w.layout(), tree, leaves(tree), w.mfact(), w.weights(), area);
  },
  maximizedOf: (i: number) => ws.list[i]?.maximized() ?? null,
};

window.addEventListener("resize", () => ws.setViewport({ w: window.innerWidth, h: window.innerHeight }));

const sessionOf = (p: Pane | undefined) => (p?.content.type === "chat" ? p.content.sessionId : null);

// The store resyncs whatever the tiles show (every workspace).
setOpenSessionsProvider(() =>
  Object.values(ws.panes)
    .map(sessionOf)
    .filter((x): x is string => !!x),
);

onNoteEvent((e) => {
  if (e.type !== "note_removed") return;
  for (const p of Object.values(ws.panes)) {
    if (p.content.type === "note" && p.content.noteId === e.id) closePane(p.id);
  }
});

onBoardRemoved((boardId) => {
  for (const p of Object.values(ws.panes)) {
    if (p.content.type === "whiteboard" && p.content.boardId === boardId) closePane(p.id);
  }
});

onSessionRemoved((id) => {
  for (const p of Object.values(ws.panes)) {
    if (sessionOf(p) === id) ws.setPanes(p.id, "content", { type: "chat", sessionId: null });
  }
});

// ── persistence (per server) ─────────────────────────────────────────────

const storageKey = (k: string) => `tessera.workspace.${k}`;
const LAYOUT_IDS = new Set(LAYOUTS.map((l) => l.id));

/**
 * The tile a brand-new install starts with: this device's own, not shared
 * until it's used (a chat opened in it, or something typed), so signing in
 * on a second device never adds it to the shared mosaic.
 */
const placeholders = new Set<string>();

/** Saved together with the tiling, so the two always match. */
let syncState: SyncState | null = null;
export const getSyncState = () => syncState;
export function setSyncState(s: SyncState | null) {
  syncState = s;
  persist();
}

const persist = debounce((strict = false) => {
  const key = ws.serverKey();
  if (!key) { if (strict) throw new Error('Workspace identity is missing. Reload was blocked.'); return; }
  const data: PersistedV2 = {
    v: 2,
    sync: syncState,
    placeholders: [...placeholders].filter((id) => ws.panes[id]),
    meta: remoteMeta.map((m) => ({ ...m })),
    overflow,
    active: ws.active(),
    workspaces: ws.list.map((w) => ({
      root: w.root(),
      focused: w.focused(),
      layout: w.layout(),
      mfact: w.mfact(),
      weights: w.weights(),
      ...(w.local() ? { local: true } : {}),
      ...(w.name() ? { name: w.name()! } : {}),
      ...(w.dual() !== 0.5 ? { dual: w.dual() } : {}),
    })),
    panes: Object.fromEntries(
      Object.values(ws.panes).map((p) => [
        p.id,
        isPrivatePane(p.id) ? { id: p.id, content: { type: "chat", sessionId: null } } : p.content.type === "file" ? { id: p.id, content: { type: "file", file: storedFile(p.content.file) } } : JSON.parse(JSON.stringify(p)),
      ]),
    ),
  };
  if (strict) {
    const text = JSON.stringify(data), id = storageKey(key);
    localStorage.setItem(id, text);
    if (localStorage.getItem(id) !== text) throw new Error('Workspace save failed. Reload was blocked.');
  } else lsSet(storageKey(key), data);
}, 250);
window.addEventListener("beforeunload", () => persist.flush());
// Phones background (and then kill) apps without an unload: save on the way out.
window.addEventListener("pagehide", () => persist.flush());
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") persist.flush();
});

function normalizeContent(c: unknown): PaneContent {
  const o = c as { type?: string; sessionId?: unknown; channelId?: unknown; draft?: NewChatDraft; boardId?: string } | undefined;
  if (o?.type === "chat") return { type: "chat", sessionId: typeof o.sessionId === "string" ? o.sessionId : null, draft: o.draft };
  if (o?.type === "whiteboard" && typeof o.boardId === "string") return { type: "whiteboard", boardId: o.boardId };
  const n = o as { type?: string; noteId?: unknown; kind?: unknown; sessionId?: unknown } | undefined;
  if (n?.type === "note" && typeof n.noteId === "string") return { type: "note", noteId: n.noteId };
  const f = (o as { file?: Partial<ViewFile> } | undefined)?.file;
  if (o?.type === "file" && f && typeof f.name === "string") return { type: "file", file: viewFileOf(f) };
  // (Image and web-app drafts from older versions open as web views.)
  if (n?.type === "draft" && typeof n.kind === "string")
    return { type: "draft", kind: n.kind === "assist" ? "assist" : "url", sessionId: typeof n.sessionId === "string" ? n.sessionId : null };
  if (o?.type === "terminal") {
    const t = o as { machine?: unknown; termId?: unknown };
    return {
      type: "terminal",
      ...(t.machine === null || typeof t.machine === "string" ? { machine: t.machine as string | null } : {}),
      ...(typeof t.termId === "string" ? { termId: t.termId } : {}),
    };
  }
  if (o?.type === "other" && (o as { tile?: unknown }).tile && typeof (o as { tile?: unknown }).tile === "object")
    return { type: "other", tile: (o as { tile: Record<string, unknown> }).tile };
  // v1 reserved a `channel` type; channels are sessions now.
  if (o?.type === "channel" && typeof o.channelId === "string") return { type: "chat", sessionId: o.channelId };
  return { type: "chat", sessionId: null };
}

function readSaved(serverId: string): PersistedV2 | null {
  const raw = lsGet<PersistedV2 | PersistedV1 | null>(storageKey(serverId), null);
  if (!raw || typeof raw !== "object" || !raw.panes) return null;
  const panes: Record<string, Pane> = {};
  for (const [id, p] of Object.entries(raw.panes)) panes[id] = { id, content: normalizeContent(p?.content) };
  if ((raw as PersistedV2).v === 2) {
    const r = raw as PersistedV2;
    const seen = new Set<string>();
    const workspaces = Array.from({ length: WORKSPACES }, (_, i): WsData => {
      const w = r.workspaces?.[i];
      // A tile that went missing only loses its own place, never the whole mosaic.
      const root = w && w.root && treeShape(w.root) ? pruneTree(w.root, panes, seen) : null;
      const live = leaves(root);
      return {
        root,
        focused: w?.focused && live.includes(w.focused) ? w.focused : live[0] ?? null,
        layout: w && LAYOUT_IDS.has(w.layout) ? w.layout : "dwindle",
        mfact: typeof w?.mfact === "number" ? w.mfact : 0.55,
        weights: w?.weights && typeof w.weights === "object" ? w.weights : {},
        local: w?.local === true,
        name: typeof w?.name === "string" && w.name.trim() ? w.name.trim().slice(0, 60) : undefined,
        dual: typeof w?.dual === "number" ? clamp(w.dual, 0.2, 0.8) : 0.5,
      };
    });
    const sync = r.sync && typeof r.sync.version === "number" && Array.isArray(r.sync.base) ? r.sync : null;
    const held = Array.isArray(r.placeholders) ? r.placeholders.filter((id) => typeof id === "string") : [];
    const meta = Array.isArray(r.meta)
      ? Array.from({ length: WORKSPACES }, (_, i): RemoteMeta => {
          const m = r.meta![i] as RemoteMeta | undefined;
          const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);
          return { id: str(m?.id), name: str(m?.name), layout: str(m?.layout) };
        })
      : undefined;
    const over = Array.isArray(r.overflow) ? r.overflow.filter((m) => m && typeof m.id === "string" && Array.isArray(m.tiles)) : [];
    return { v: 2, active: clamp(Number(r.active) || 0, 0, WORKSPACES - 1), workspaces, panes, sync, placeholders: held, meta, overflow: over };
  }
  // Migrate v1: its single tree becomes workspace 1.
  const v1 = raw as PersistedV1;
  if (!validTree(v1.root, panes, new Set())) return null;
  const live = leaves(v1.root);
  const workspaces = Array.from({ length: WORKSPACES }, (_, i): WsData =>
    i === 0
      ? { root: v1.root, focused: live.includes(v1.focused) ? v1.focused : live[0], layout: "dwindle", mfact: 0.55, weights: {} }
      : { root: null, focused: null, layout: "dwindle", mfact: 0.55, weights: {} },
  );
  return { v: 2, active: 0, workspaces, panes };
}

/** The server whose tiling is loaded (the shared layout's sync state is kept per server). */
export const workspaceServer = () => ws.serverKey();

/** Load the saved tiling for a server (call when the active server changes). */
export function loadWorkspace(serverId: string) {
  if (ws.serverKey() === serverId) return;
  persist.flush();
  const saved = readSaved(serverId);
  syncState = saved?.sync ?? null;
  placeholders.clear();
  for (const id of saved?.placeholders ?? []) placeholders.add(id);
  // Mosaic ids and names as the server knows them (a mosaic keeps its identity
  // across restarts, wherever it sits here).
  for (let i = 0; i < WORKSPACES; i++) remoteMeta[i] = { ...(saved?.meta?.[i] ?? {}) };
  overflow = saved?.overflow ?? [];
  tileExtra.clear();
  batch(() => {
    ws.setServerKey(serverId);
    const panes: Record<string, Pane> = {};
    if (saved) {
      for (const w of saved.workspaces) for (const id of leaves(w.root)) panes[id] = saved.panes[id];
    }
    let workspaces = saved?.workspaces;
    // A tiling saved empty (every tile closed) stays empty: the launcher shows.
    if (!workspaces || (!Object.keys(panes).length && !saved?.sync)) {
      // A new install (or nothing usable saved): one tile of its own, not
      // shared until used, and never merged against an old base.
      syncState = null;
      for (let i = 0; i < WORKSPACES; i++) remoteMeta[i] = {};
      overflow = [];
      const p = freshPane();
      panes[p.id] = p;
      placeholders.add(p.id);
      workspaces = Array.from({ length: WORKSPACES }, (_, i): WsData =>
        i === 0 ? { root: { t: "leaf", pane: p.id }, focused: p.id, layout: "dwindle", mfact: 0.55, weights: {} } : { root: null, focused: null, layout: "dwindle", mfact: 0.55, weights: {} },
      );
    }
    ws.setPanes(
      produce((d) => {
        for (const k of Object.keys(d)) delete d[k];
        Object.assign(d, panes);
      }),
    );
    ws.list.forEach((w, i) => {
      const s = workspaces![i];
      w.setRoot(s.root);
      w.setFocused(s.focused);
      w.setLayout(s.layout);
      w.setMfact(s.mfact);
      w.setWeights(s.weights);
      w.setLocal(!!s.local);
      w.setDual(s.dual ?? 0.5);
      w.setName(s.name ?? null);
      w.setMaximized(null);
    });
    ws.setActive(saved && workspaces === saved.workspaces ? saved.active : 0);
  });
  for (const [id, text] of Object.entries(legacyDrafts)) {
    if (!ws.panes[id]) continue;
    if (!getDraft(id)) setDraft(id, text);
    delete legacyDrafts[id];
  }
  lsSet("tessera.drafts", legacyDrafts);
}

const commitListeners = new Set<() => void>();
/** Called after every local change to the layout (for server sync). */
export function onLayoutCommit(cb: () => void) {
  commitListeners.add(cb);
  return () => commitListeners.delete(cb);
}
let applyingRemote = false;
const commit = () => {
  forgetEmptyLocal();
  persist();
  if (!applyingRemote) for (const cb of commitListeners) cb();
  placeOverflow();
};

/**
 * Shared mosaics that had no room here (see `overflow`) move into a slot as
 * soon as one is free. Not while this device's own first tile is unused: it
 * would be replaced by the shared layout (see `placeholders`).
 */
function placeOverflow() {
  if (!overflow.length || applyingRemote || [...placeholders].some((id) => ws.panes[id])) return;
  if (!ws.list.some((w, i) => !w.local() && !w.order().length && i !== ws.active())) return;
  importMosaics(exportMosaics());
}

/**
 * A mosaic kept on this device that was emptied and left is gone: the slot
 * is free again, and a new mosaic there is shared as usual. (The mosaic on
 * screen keeps its setting while it's empty: the launcher is showing it.)
 */
function forgetEmptyLocal() {
  ws.list.forEach((w, i) => {
    if (w.local() && i !== ws.active() && !w.order().length) w.setLocal(false);
  });
}

// ── sync with the server's shared mosaic layout ──────────────────────────

/**
 * Fields of shared tiles this version doesn't know (by tile id), sent back
 * unchanged so other clients' additions survive a round trip.
 */
const tileExtra = new Map<string, Record<string, unknown>>();
const KNOWN_TILE_KEYS = new Set(["id", "session_id", "board_id", "note_id"]);

/** What a tile is on the server: private tiles never leave this device. */
function tileOut(id: string): MosaicTile | null {
  const p = ws.panes[id];
  if (!p) return null;
  const extra = tileExtra.get(id);
  if (p.content.type === "other") return { ...(p.content.tile as object), id } as MosaicTile;
  if (p.content.type === "whiteboard") return { ...extra, id, board_id: p.content.boardId };
  if (p.content.type === "note") return { ...extra, id, note_id: p.content.noteId };
  // A spawned tile without content yet lives on this device only, and so do a
  // file being read and a terminal (it belongs to this device).
  if (p.content.type === "draft" || p.content.type === "file" || p.content.type === "terminal") return null;
  if (isPrivatePane(id)) return null;
  if (placeholders.has(id)) {
    if (isUnusedPane(id)) return null;
    placeholders.delete(id); // used now: shared from here on
  }
  return p.content.sessionId ? { ...extra, id, session_id: p.content.sessionId } : { ...extra, id };
}

/**
 * What the server calls each mosaic slot: other clients (the terminal) pick
 * their own ids, names and layout names, and those must survive a round trip.
 * `layout` is kept only while it's one this app doesn't know and the user
 * hasn't picked another here.
 */
interface RemoteMeta {
  id?: string;
  name?: string;
  layout?: string;
}
const remoteMeta: RemoteMeta[] = Array.from({ length: WORKSPACES }, () => ({}));

/**
 * Shared mosaics that found no free slot here (mosaics kept on this device
 * take slots the server doesn't know about). They stay on the server as they
 * are: sent back unchanged, never dropped.
 */
let overflow: Mosaic[] = [];

/** The local mosaics as the shared layout (without a version). */
export function exportMosaics(): Mosaic[] {
  const out: Mosaic[] = [];
  const taken = new Set([...remoteMeta.map((m) => m.id), ...overflow.map((m) => m.id)].filter(Boolean));
  ws.list.forEach((w, i) => {
    // Kept on this device only: the server never sees it.
    if (w.local()) return;
    const tiles = w.order().map(tileOut).filter((t): t is MosaicTile => !!t);
    if (!tiles.length) return;
    const keep = new Set(tiles.map((t) => t.id));
    // The desktop split tree, minus anything private.
    let tree = w.root();
    for (const id of leaves(tree)) if (!keep.has(id)) tree = removeLeaf(tree, id);
    const meta = remoteMeta[i];
    let id = meta.id;
    if (!id) {
      id = String(i + 1);
      if (taken.has(id)) id = `m${i + 1}-${uid("")}`;
      taken.add(id);
    }
    out.push({
      id,
      name: w.name() ?? meta.name ?? `Mosaic ${i + 1}`,
      layout: meta.layout ?? w.layout(),
      tiles,
      geometry: { tree, mfact: w.mfact(), weights: w.weights(), focused: keep.has(w.focused() ?? "") ? w.focused() : null },
    });
  });
  for (const m of overflow) if (!out.some((x) => x.id === m.id)) out.push(m);
  return out;
}

/**
 * The tile being typed in (focused, its composer focused, with something in
 * it): a change from another device never takes it away. An empty composer
 * that merely has the focus (a new chat focuses it) doesn't count, or a tile
 * closed elsewhere would keep coming back.
 */
export function typingPane(): string | null {
  const f = ws.cur().focused();
  if (!f || isPrivatePane(f)) return null;
  const typing = !!document.activeElement?.closest?.(`[data-pane-id="${f}"] textarea`);
  return typing && !!getDraft(f).trim() ? f : null;
}

/** Only the shape of a split tree (leaf ids are checked against the tile list separately). */
function treeShape(n: unknown, depth = 0): n is LayoutNode {
  if (!n || typeof n !== "object" || depth > 64) return false;
  const o = n as LayoutNode;
  if (o.t === "leaf") return typeof o.pane === "string" && !!o.pane;
  return o.t === "split" && typeof o.ratio === "number" && (o.dir === "h" || o.dir === "v") && treeShape(o.a, depth + 1) && treeShape(o.b, depth + 1);
}

/**
 * Fit a stored split tree to the actual tiles. Other clients (the terminal)
 * add and remove tiles without touching `geometry`: splits of missing tiles go,
 * new tiles are placed the default way next to the tile before them.
 */
function reconcileTree(g: unknown, ids: string[]): LayoutNode | null {
  if (!treeShape(g)) return chain(ids);
  let root: LayoutNode | null = g;
  const want = new Set(ids);
  const seen = new Set<string>();
  for (const id of leaves(root)) {
    if (!want.has(id) || seen.has(id)) root = removeLeaf(root, id);
    seen.add(id);
  }
  const have = new Set(leaves(root));
  ids.forEach((id, k) => {
    if (have.has(id)) return;
    // Beside the tile it follows (a whiteboard next to its chat), splitting
    // that tile along its longer side, like opening a tile here would.
    let prev = ids.slice(0, k).reverse().find((x) => have.has(x)) ?? null;
    let dir: SplitDir = have.size % 2 ? "v" : "h";
    if (prev && root) {
      const a = ws.areaSize();
      const rects = dwindleGeometry(root).panes;
      const px = (r: Rect) => ({ w: r.w * a.w, h: r.h * a.h });
      const half = (r: { w: number; h: number }) => (r.w * 1.1 >= r.h ? { w: r.w / 2, h: r.h } : { w: r.w, h: r.h / 2 });
      const usable = (r: { w: number; h: number }) => half(r).w >= 340 && half(r).h >= 220;
      // Like opening a tile here: when that tile's halves would be too small
      // (an agent putting up board after board), split the largest one instead.
      const pr = rects.get(prev);
      if (pr && !usable(px(pr))) {
        let area = 0;
        for (const [pid, r] of rects) {
          const q = px(r);
          if (q.w * q.h > area) {
            area = q.w * q.h;
            prev = pid;
          }
        }
      }
      const r = rects.get(prev);
      if (r) dir = px(r).w * 1.1 >= px(r).h ? "h" : "v";
    }
    root = insertAt(root, prev, id, dir);
    have.add(id);
  });
  return root;
}

/** A plain dwindle tree for tiles that came without one (phones, the terminal). */
function chain(ids: string[]): LayoutNode | null {
  let root: LayoutNode | null = null;
  ids.forEach((id, k) => {
    // Dwindle: each tile splits the previous one, alternating side by side and stacked.
    root = insertAt(root, k ? ids[k - 1] : null, id, k % 2 ? "h" : "v");
  });
  return root;
}

/**
 * Replace the local mosaics with the shared layout. Private tiles (which the
 * server never stores) stay where they were.
 */
export function importMosaics(mosaics: Mosaic[]) {
  // Mosaics kept on this device take their slots and tiles out of the game.
  const isLocal = (i: number) => ws.list[i].local();
  const localTiles = new Set(ws.list.flatMap((w) => (w.local() ? w.order() : [])));
  // A board made from a tile spawned here keeps that tile: the one the server
  // added beside its chat is dropped (and goes from the layout on our next push).
  const spawnedElsewhere = (t: MosaicTile) => {
    const own = typeof t.board_id === "string" ? spawned.get(t.board_id) : undefined;
    if (!own) return false;
    const c = ws.panes[own]?.content;
    if (own === t.id || c?.type !== "whiteboard" || c.boardId !== t.board_id) {
      spawned.delete(t.board_id!); // the server has ours now (or it's gone)
      return false;
    }
    return true;
  };
  // A tile id can only live in one mosaic (first one wins).
  const claimed = new Set<string>();
  mosaics = mosaics.map((m) => ({
    ...m,
    tiles: m.tiles.filter((t) => typeof t.id === "string" && !!t.id && !localTiles.has(t.id) && !spawnedElsewhere(t) && !claimed.has(t.id) && !!claimed.add(t.id)),
  }));
  // One board, one tile (and one note, one tile): two arrive when this device
  // opened one while the server was adding its own (a slow connection, a
  // merge). The one already here wins; the other goes from the layout on our
  // next push.
  const thing = (t: MosaicTile) => (typeof t.board_id === "string" ? `b:${t.board_id}` : typeof t.note_id === "string" ? `n:${t.note_id}` : null);
  const winner = new Map<string, string>();
  for (const m of mosaics)
    for (const t of m.tiles) {
      const k = thing(t);
      if (!k) continue;
      const cur = winner.get(k);
      if (!cur || (!ws.panes[cur] && ws.panes[t.id])) winner.set(k, t.id);
    }
  mosaics = mosaics.map((m) => ({ ...m, tiles: m.tiles.filter((t) => winner.get(thing(t) ?? "") === undefined || winner.get(thing(t)!) === t.id) }));
  const slots = new Map<number, Mosaic>();
  const rest: Mosaic[] = [];
  const placed = new Set<Mosaic>();
  // A mosaic stays in its slot: by the id it had here, then by number.
  for (const m of mosaics) {
    const i = remoteMeta.findIndex((x, k) => x.id === m.id && !isLocal(k));
    if (i >= 0 && !slots.has(i)) {
      slots.set(i, m);
      placed.add(m);
    }
  }
  for (const m of mosaics) {
    if (placed.has(m)) continue;
    const n = Number(m.id);
    if (Number.isInteger(n) && n >= 1 && n <= WORKSPACES && !slots.has(n - 1) && !isLocal(n - 1)) slots.set(n - 1, m);
    else rest.push(m);
  }
  const unplaced: Mosaic[] = [];
  for (const m of rest) {
    let i = 0;
    while (i < WORKSPACES && (slots.has(i) || isLocal(i))) i++;
    if (i < WORKSPACES) slots.set(i, m);
    else unplaced.push(m);
  }
  // No room here (mosaics kept on this device fill the slots): keep them as they are.
  overflow = unplaced;
  const typing = typingPane();
  const incoming = claimed;
  const known = new Set(Object.keys(ws.panes));
  // The mosaic on screen had tiles before this (see below).
  const hadTiles = ws.cur().order().length > 0;
  // Shared mosaics that land in a slot empty until now, and whether they came
  // without a layout (the server made it up to hold a board; see placeBesideChat).
  const arrived = new Map<number, boolean>();
  ws.list.forEach((w, i) => {
    const m = slots.get(i);
    if (m && !w.local() && !w.order().length) arrived.set(i, m.geometry == null);
  });
  applyingRemote = true;
  try {
    batch(() => {
      const keepPanes = new Set<string>();
      ws.list.forEach((w, i) => {
        if (w.local()) {
          for (const id of w.order()) keepPanes.add(id);
          return;
        }
        const m = slots.get(i);
        remoteMeta[i] = m ? { id: m.id, name: m.name, layout: LAYOUT_IDS.has(m.layout as LayoutKind) ? undefined : m.layout } : {};
        // A name someone gave it (a default "Mosaic N" says nothing: the number here may differ).
        w.setName(m && m.name?.trim() && !DEFAULT_NAME.test(m.name.trim()) ? m.name.trim().slice(0, 60) : null);
        // Private tiles never leave this device; the tile being typed in is never taken away.
        const privateHere = w.order().filter((id) => isPrivatePane(id) || ws.panes[id]?.content.type === "draft" || ws.panes[id]?.content.type === "file" || ws.panes[id]?.content.type === "terminal" || (id === typing && !incoming.has(id)));
        const tiles = m?.tiles ?? [];
        for (const t of tiles) {
          keepPanes.add(t.id);
          const cur = ws.panes[t.id];
          if (t.id === typing && cur) continue;
          const raw = t as unknown as Record<string, unknown>;
          const extra = Object.fromEntries(Object.entries(raw).filter(([k]) => !KNOWN_TILE_KEYS.has(k)));
          const known = typeof t.session_id === "string" || typeof t.board_id === "string" || typeof t.note_id === "string";
          if (Object.keys(extra).length) tileExtra.set(t.id, extra);
          else tileExtra.delete(t.id);
          if (typeof t.note_id === "string") {
            if (cur?.content.type !== "note" || cur.content.noteId !== t.note_id) ws.setPanes(t.id, { id: t.id, content: { type: "note", noteId: t.note_id } });
            continue;
          }
          if (typeof t.board_id === "string") {
            if (cur?.content.type !== "whiteboard" || cur.content.boardId !== t.board_id) ws.setPanes(t.id, { id: t.id, content: { type: "whiteboard", boardId: t.board_id } });
            continue;
          }
          if (!known && Object.keys(extra).length) {
            // Something newer than this app: keep it exactly, show a placeholder.
            tileExtra.delete(t.id);
            if (cur?.content.type !== "other" || JSON.stringify(cur.content.tile) !== JSON.stringify(raw)) ws.setPanes(t.id, { id: t.id, content: { type: "other", tile: { ...raw } } });
            continue;
          }
          const sid = t.session_id ?? null;
          if (!cur || cur.content.type !== "chat" || cur.content.sessionId !== sid) {
            ws.setPanes(t.id, { id: t.id, content: { type: "chat", sessionId: sid, draft: cur?.content.type === "chat" && !sid ? cur.content.draft : undefined } });
          }
        }
        for (const id of privateHere) keepPanes.add(id);
        const ids = tiles.map((t) => t.id);
        const g = (m?.geometry ?? {}) as { tree?: unknown; mfact?: unknown; weights?: unknown; focused?: unknown };
        let root: LayoutNode | null = ids.length ? reconcileTree(g.tree, ids) : null;
        for (const id of privateHere) root = insertAt(root, root ? leaves(root)[leaves(root).length - 1] : null, id, "h");
        const live = leaves(root);
        w.setRoot(root);
        if (m && LAYOUT_IDS.has(m.layout as LayoutKind)) w.setLayout(m.layout as LayoutKind);
        if (typeof g.mfact === "number") w.setMfact(g.mfact);
        if (g.weights && typeof g.weights === "object") w.setWeights(g.weights as Record<string, number>);
        const f = w.focused();
        if (!f || !live.includes(f)) w.setFocused(typeof g.focused === "string" && live.includes(g.focused) ? g.focused : live[0] ?? null);
        if (w.maximized() && !live.includes(w.maximized()!)) w.setMaximized(null);
      });
      ws.setPanes(
        produce((d) => {
          for (const id of Object.keys(d)) if (!keepPanes.has(id)) delete d[id];
        }),
      );
      for (const id of [...tileExtra.keys()]) if (!keepPanes.has(id)) tileExtra.delete(id);
      // A mosaic this change emptied (its tiles were closed or moved on another
      // device) hands the screen to one that has tiles. A new empty mosaic you
      // opened yourself stays: nothing another device does takes you from it.
      if (hadTiles && !ws.cur().order().length) {
        const firstUsed = ws.list.findIndex((w) => w.order().length);
        if (firstUsed >= 0) ws.setActive(firstUsed);
      }
      // Everything closed everywhere: the launcher shows (no tile is invented,
      // it would only come back as a new shared tile).
      for (const id of [...placeholders]) if (!ws.panes[id]) placeholders.delete(id);
    });
    persist();
  } finally {
    applyingRemote = false;
  }
  forgetEmptyLocal();
  // Whiteboards that just arrived belong beside their chat (see placeBesideChat).
  const fresh = Object.values(ws.panes).filter((p) => (p.content.type === "whiteboard" || p.content.type === "note") && !known.has(p.id));
  if (fresh.length) queueMicrotask(() => fresh.forEach((p) => placeBesideChat(p.id, true, arrived)));
}

/**
 * The server puts a new whiteboard's tile after its chat's tile, or at the
 * end of the first mosaic when it can't find one (the chat started a moment
 * ago on this device). When the chat is in the mosaic on screen and the
 * board landed in another one, move the board beside the chat — a normal
 * layout change that syncs. Only for what an agent put up (or this device
 * just asked for): a tile the user made is where its device put it.
 */
function placeBesideChat(id: string, retry = true, arrived?: Map<number, boolean>) {
  const c = ws.panes[id]?.content;
  if (c?.type !== "whiteboard" && c?.type !== "note") return;
  // Who showed it, and when: a board, or a note an agent showed (`note` with `show`).
  const b = c.type === "whiteboard" ? state.boards[c.boardId] : state.notes[c.noteId];
  if (!b) {
    // Its board_updated / note_updated may still be on the way.
    if (retry) setTimeout(() => placeBesideChat(id, false, arrived), 500);
    return;
  }
  if (!b.session_id || Date.now() - b.created_at > 2 * 60_000) return; // only ones just put up, by a chat
  // What the user made was placed by the device they made it on (a web view
  // spawned in a mosaic of its own, a note): no other device moves it, however
  // its chat happens to be open there. Only a page this device just asked the
  // server to put beside a chat (/show) is moved here, like an agent's.
  if (b.by_user && (askedToShow.get(b.session_id) ?? 0) < Date.now()) return;
  const info = state.sessions[b.session_id]?.info;
  const root = info?.kind === "subagent" && info.parent_id ? info.parent_id : b.session_id;
  const chat = paneShowing(root);
  const to = chat ? ws.owner().get(chat) : undefined;
  const from = ws.owner().get(id);
  if (!chat || to === undefined || from === undefined || to === from || to !== ws.active()) return;
  const src = ws.list[from];
  const dst = ws.list[to];
  if (dst.order().length >= MAX_PANES) return;
  batch(() => {
    const idx = src.order().indexOf(id);
    src.setRoot((root) => removeLeaf(root, id));
    if (src.maximized() === id) src.setMaximized(null);
    if (src.focused() === id) {
      const next = src.order();
      src.setFocused(next[clamp(idx - 1, 0, next.length - 1)] ?? next[0] ?? null);
    }
    const r = pixelRectInTree(dst, chat);
    const moved = arriving(dst, src, id);
    dst.setRoot((root) => insertAt(root, chat, moved, r.w * 1.1 >= r.h ? "h" : "v"));
  });
  // The chat's only tile is in a mosaic kept on this device, so the server
  // made a mosaic up for the board ("Main": the chat and the board, no layout
  // of its own). With the board beside the chat here, that mosaic holds
  // nothing new: it goes, instead of lingering as an empty or duplicate one.
  // Only a mosaic that arrived just now in an empty slot, never one of yours.
  const left = src.order();
  const bare = arrived?.get(from);
  if (bare !== undefined && (!left.length || (bare && left.every((p) => sessionOf(ws.panes[p]) === root)))) {
    deleteWorkspace(from);
    return;
  }
  commit();
}

/** Chats this device just asked the server to put a page beside (session id → until when), see placeBesideChat. */
const askedToShow = new Map<string, number>();
export function askServerToShowBeside(sessionId: string) {
  askedToShow.set(sessionId, Date.now() + 20_000);
}

/** Boards made from a tile spawned on this device (board id → that tile), until the server has that tile. */
const spawned = new Map<string, string>();

/**
 * A spawned tile got its content: it shows the new board from now on, right
 * where it was spawned. The tile the server put beside the board's chat (if
 * it arrived already) closes; one arriving later is dropped (see importMosaics).
 */
export function adoptSpawnedBoard(paneId: string, boardId: string) {
  if (!ws.panes[paneId]) return;
  spawned.set(boardId, paneId);
  const other = Object.values(ws.panes).find((p) => p.id !== paneId && p.content.type === "whiteboard" && p.content.boardId === boardId);
  setPaneContent(paneId, { type: "whiteboard", boardId });
  if (other) closePane(other.id);
}

/**
 * Delete a mosaic: its tiles close, the chats stay (they remain in the
 * sidebar). The active mosaic moves to a neighbour that still has tiles.
 */
export function deleteWorkspace(index: number) {
  const w = ws.list[index];
  if (!w) return;
  const ids = w.order();
  remoteMeta[index] = {};
  batch(() => {
    w.setRoot(null);
    w.setFocused(null);
    w.setMaximized(null);
    w.setWeights({});
    w.setLocal(false);
    w.setDual(0.5);
    w.setName(null);
    ws.setPanes(
      produce((d) => {
        for (const id of ids) delete d[id];
      }),
    );
    if (ws.active() === index) {
      const next = ws.list.findIndex((x, i) => i !== index && x.order().length);
      if (next >= 0) ws.setActive(next);
      else {
        const p = freshPane();
        placeholders.add(p.id);
        ws.setPanes(p.id, p);
        w.setRoot({ t: "leaf", pane: p.id });
        w.setFocused(p.id);
      }
    }
  });
  for (const id of ids) clearDraft(id);
  commit();
}

// ── mosaics kept on this device ──────────────────────────────────────────

/**
 * Give tiles new ids (their drafts, focus and sizes follow). A mosaic kept on
 * this device holds its own copies: the ids the server knew stay with the
 * shared layout, so a change made elsewhere at the same moment can never
 * bring a tile back here twice, or take one away.
 */
function rekey(w: WsSig, ids: string[]): Map<string, string> {
  const map = new Map<string, string>();
  for (const id of ids) if (ws.panes[id]) map.set(id, uid("p_"));
  if (!map.size) return map;
  const to = (id: string | null) => (id && map.get(id)) || id;
  batch(() => {
    ws.setPanes(
      produce((d) => {
        for (const [a, b] of map) {
          d[b] = { id: b, content: JSON.parse(JSON.stringify(d[a].content)) };
          delete d[a];
        }
      }),
    );
    w.setRoot((root) => (root ? mapTree(root, (n) => (n.t === "leaf" && map.has(n.pane) ? { t: "leaf", pane: map.get(n.pane)! } : n)) : root));
    w.setFocused((f) => to(f));
    w.setMaximized((m) => to(m));
    w.setWeights((m) => Object.fromEntries(Object.entries(m).map(([k, v]) => [to(k)!, v])));
  });
  for (const [a, b] of map) {
    moveDrafts(a, b);
    clearDraft(a);
    if (placeholders.delete(a)) placeholders.add(b);
    tileExtra.delete(a);
  }
  return map;
}

/** A tile moving from `from` into `to`: into a mosaic kept here, it becomes this device's own copy. */
function arriving(to: WsSig, from: WsSig, id: string): string {
  if (!to.local() || from.local()) return id;
  // Not in any tree right now (it's between mosaics): swap its id directly.
  const b = uid("p_");
  const p = ws.panes[id];
  if (!p) return id;
  ws.setPanes(
    produce((d) => {
      d[b] = { id: b, content: JSON.parse(JSON.stringify(d[id].content)) };
      delete d[id];
    }),
  );
  moveDrafts(id, b);
  clearDraft(id);
  if (placeholders.delete(id)) placeholders.add(b);
  tileExtra.delete(id);
  from.setWeights((m) => {
    if (!(id in m)) return m;
    const next = { ...m };
    delete next[id];
    return next;
  });
  return b;
}

/**
 * Keep a mosaic on this device only, or share it again. Kept here, it leaves
 * the shared layout (your other devices no longer show it) and changes to it
 * stay here; shared again, it's sent to them as a new mosaic.
 */
export function setMosaicLocal(index: number, on: boolean) {
  const w = ws.list[index];
  if (!w || w.local() === on) return;
  const focusedHere = index === ws.active() && !!document.activeElement?.closest?.(".pane textarea");
  let map = new Map<string, string>();
  batch(() => {
    if (on) map = rekey(w, w.order());
    w.setLocal(on);
    // Shared again: a mosaic of its own on the server, never merged into
    // one another device just made in the same place.
    remoteMeta[index] = on ? {} : { id: `m${index + 1}-${uid("")}` };
  });
  commit();
  // The tiles were set again under their new ids: keep typing where you were.
  if (focusedHere && map.size) focusComposer(w.focused());
}

const DEFAULT_NAME = /^Mosaic \d+$/;

/** Name a mosaic ("" or null: back to "Mosaic N"). Shared mosaics share the name. */
export function setMosaicName(index: number, name: string | null) {
  const w = ws.list[index];
  if (!w) return;
  const v = name?.trim().slice(0, 60) || null;
  const next = v && !DEFAULT_NAME.test(v) ? v : null;
  if (next === w.name()) return;
  w.setName(next);
  remoteMeta[index].name = next ?? undefined;
  commit();
}

/** Two-up (foldables): the left tile's share of the width (double-tap the handle for half and half). */
export function setDualRatio(ratio: number, index = ws.active()) {
  const w = ws.list[index];
  if (!w) return;
  w.setDual(clamp(ratio, 0.2, 0.8));
  persist();
}

// ── closing animation ghosts ─────────────────────────────────────────────

export interface Ghost {
  id: string;
  rect: Rect;
}
const [ghosts, setGhosts] = createSignal<Ghost[]>([]);
export { ghosts };

// ── operations ───────────────────────────────────────────────────────────

const wsOf = (paneId: string) => ws.list[ws.owner().get(paneId) ?? -1];

export function focusPane(id: string) {
  const i = ws.owner().get(id);
  if (i === undefined) return;
  const w = ws.list[i];
  batch(() => {
    if (ws.active() !== i) ws.setActive(i);
    w.setFocused(id);
    if (w.maximized() && w.maximized() !== id) w.setMaximized(id);
  });
  commit();
}

function pixelRectInTree(w: WsSig, id: string): Rect {
  const r = dwindleGeometry(w.root()).panes.get(id) ?? FULL;
  const a = ws.areaSize();
  return { x: r.x * a.w, y: r.y * a.h, w: r.w * a.w, h: r.h * a.h };
}

function tooMany(w: WsSig): boolean {
  if (w.order().length < MAX_PANES) return false;
  toast({ level: "info", message: `A mosaic holds up to ${MAX_PANES} tiles — try another mosaic (Alt+2…9).` });
  return true;
}

/**
 * Open a new tile on the active workspace. By default it splits `target`
 * (the focused tile) along its longer axis (dwindle); `zone` places it on a
 * specific side instead.
 */
export function openPane(
  content: PaneContent = { type: "chat", sessionId: null },
  target: string | null = workspace.focused(),
  zone?: DropZone,
  identityPane?: string,
): string | null {
  const w = ws.cur();
  if (tooMany(w)) return null;
  const pane: Pane = { id: identityPane ?? uid('p_'), content };
  let where: DropZone | SplitDir = "h";
  if (zone && zone !== "center") where = zone;
  else if (target) {
    // Dwindle splits the focused tile — unless its halves would be too small
    // to use; then the largest tile is split instead, so nine tiles stay usable.
    const half = (r: Rect) => (r.w * 1.1 >= r.h ? { w: r.w / 2, h: r.h } : { w: r.w, h: r.h / 2 });
    const usable = (r: Rect) => half(r).w >= 340 && half(r).h >= 220;
    if (!usable(pixelRectInTree(w, target))) {
      let best = target;
      let area = 0;
      for (const id of w.order()) {
        const r = pixelRectInTree(w, id);
        if (r.w * r.h > area) {
          area = r.w * r.h;
          best = id;
        }
      }
      target = best;
    }
    const pr = pixelRectInTree(w, target);
    // Split along the longer axis, with a small bias toward side by side
    // (chat columns read better than stacked rows).
    where = pr.w * 1.1 >= pr.h ? "h" : "v";
  }
  batch(() => {
    ws.setPanes(pane.id, pane);
    w.setRoot((root) => insertAt(root, target, pane.id, where));
    w.setFocused(pane.id);
    w.setMaximized(null);
  });
  commit();
  return pane.id;
}

export function closePane(id: string | null = workspace.focused()) {
  if (!id) return;
  const w = wsOf(id);
  if (!w) return;
  const order = w.order();
  const idx = order.indexOf(id);
  const rect = ws.list.indexOf(w) === ws.active() ? ws.geometry().panes.get(id) : undefined;
  batch(() => {
    w.setRoot((root) => removeLeaf(root, id));
    ws.setPanes(produce((d) => void delete d[id]));
    w.setWeights((m) => {
      if (!(id in m)) return m;
      const next = { ...m };
      delete next[id];
      return next;
    });
    if (w.maximized() === id) w.setMaximized(null);
    if (w.focused() === id) {
      const next = w.order();
      w.setFocused(next[clamp(idx - 1, 0, next.length - 1)] ?? next[0] ?? null);
    }
  });
  if (rect && ws.mode() === "tile" && order.length > 1) {
    const g = { id, rect };
    setGhosts((l) => [...l, g]);
    setTimeout(() => setGhosts((l) => l.filter((x) => x !== g)), 170);
  }
  clearDraft(id);
  tileExtra.delete(id);
  placeholders.delete(id);
  commit();
}

export function swapPanes(a: string, b: string) {
  if (a === b) return;
  const w = wsOf(a);
  if (!w || w !== wsOf(b)) return;
  w.setRoot((root) =>
    root
      ? mapTree(root, (n) => {
          if (n.t !== "leaf") return n;
          if (n.pane === a) return { t: "leaf", pane: b };
          if (n.pane === b) return { t: "leaf", pane: a };
          return n;
        })
      : root,
  );
  commit();
}

/** Move tile `id` next to `target` (same workspace), on the side given by `zone`. */
export function movePane(id: string, target: string, zone: DropZone) {
  if (id === target) return;
  if (zone === "center") {
    swapPanes(id, target);
    return;
  }
  const w = wsOf(target);
  if (!w || w !== wsOf(id)) return;
  w.setRoot((root) => insertAt(removeLeaf(root, id), target, id, zone));
  focusPane(id);
}

/** Move tile `id` to position `to` in its mosaic's order (the mobile overview). */
export function reorderPane(id: string, to: number) {
  const w = wsOf(id);
  if (!w) return;
  const order = w.order();
  const from = order.indexOf(id);
  if (from < 0 || to === from) return;
  const rest = order.filter((x) => x !== id);
  const idx = clamp(to, 0, rest.length);
  w.setRoot((root) => {
    const without = removeLeaf(root, id);
    if (!rest.length) return insertAt(without, null, id, "h");
    return idx < rest.length ? insertAt(without, rest[idx], id, "left") : insertAt(without, rest[rest.length - 1], id, "right");
  });
  commit();
}

/** Move a tile to another workspace. `follow` switches there too (Hyprland's movetoworkspace). */
export function moveToWorkspace(id: string | null, index: number, follow = true) {
  if (!id || index < 0 || index >= WORKSPACES) return;
  const from = wsOf(id);
  const to = ws.list[index];
  if (!from || from === to) return;
  if (tooMany(to)) return;
  batch(() => {
    const idx = from.order().indexOf(id);
    from.setRoot((root) => removeLeaf(root, id));
    if (from.maximized() === id) from.setMaximized(null);
    if (from.focused() === id) {
      const next = from.order();
      from.setFocused(next[clamp(idx - 1, 0, next.length - 1)] ?? next[0] ?? null);
    }
    const target = to.focused();
    const where: SplitDir = target && pixelRectInTree(to, target).w * 1.1 < pixelRectInTree(to, target).h ? "v" : "h";
    const moved = arriving(to, from, id);
    to.setRoot((root) => insertAt(root, target, moved, where));
    to.setFocused(moved);
    to.setMaximized(null);
    if (follow) ws.setActive(index);
  });
  if (!follow) toast({ level: "info", message: `Moved to Mosaic ${index + 1}`, timeout: 1800 });
  commit();
}

export function switchWorkspace(index: number) {
  if (index < 0 || index >= WORKSPACES || index === ws.active()) return;
  ws.setActive(index);
  commit();
}

/** Workspaces worth showing: those with tiles, plus the active one. */
export function usedWorkspaces(): number[] {
  const out: number[] = [];
  for (let i = 0; i < WORKSPACES; i++) if (ws.list[i].order().length || i === ws.active()) out.push(i);
  return out;
}

/** The lowest-numbered workspace with no tiles (other than the active one), or -1. */
export function firstFreeWorkspace(): number {
  for (let i = 0; i < WORKSPACES; i++) if (!ws.list[i].order().length && i !== ws.active()) return i;
  return -1;
}

/** Open a fresh workspace (the next free number). */
export function newWorkspace(): number {
  const i = firstFreeWorkspace();
  if (i < 0) {
    toast({ level: "info", message: `All ${WORKSPACES} mosaics are in use.` });
    return -1;
  }
  switchWorkspace(i);
  return i;
}

export function setLayoutKind(kind: LayoutKind, index = ws.active()) {
  const w = ws.list[index];
  remoteMeta[index].layout = undefined;
  batch(() => {
    w.setLayout(kind);
    w.setMaximized(null);
  });
  commit();
}

export function cycleLayout(step = 1) {
  const i = LAYOUTS.findIndex((l) => l.id === workspace.layoutKind());
  const next = LAYOUTS[(i + step + LAYOUTS.length) % LAYOUTS.length];
  setLayoutKind(next.id);
  toast({ level: "info", message: `Layout: ${next.label}`, timeout: 1200 });
}

/** Set a divider's ratio (dwindle split, master factor or neighbour weights). */
export function setDividerRatio(target: DividerTarget, ratio: number) {
  const w = ws.cur();
  if (target.kind === "split") {
    w.setRoot((root) => (root ? mapTree(root, (n) => (n.t === "split" && n.id === target.id ? { ...n, ratio } : n)) : root));
  } else if (target.kind === "mfact") {
    w.setMfact(ratio);
  } else {
    w.setWeights((m) => {
      const sum = (m[target.a] ?? 1) + (m[target.b] ?? 1);
      return { ...m, [target.a]: sum * ratio, [target.b]: sum * (1 - ratio) };
    });
  }
  commit();
}

/** Double-click on a divider: even out. */
export function equalizeDivider(target: DividerTarget) {
  const w = ws.cur();
  if (target.kind === "weights") w.setWeights({});
  else setDividerRatio(target, target.kind === "mfact" ? 0.55 : 0.5);
  commit();
}

/** Keyboard resize of the focused tile: grow/shrink by moving its nearest edge. */
export function resizeFocused(dir: Direction, stepPx = 48) {
  const id = workspace.focused();
  if (!id || ws.mode() !== "tile") return;
  const geo = ws.geometry();
  const r = geo.panes.get(id);
  if (!r) return;
  const horizontal = dir === "left" || dir === "right";
  const grow = dir === "right" || dir === "down";
  const eps = 1e-4;
  const area = ws.areaSize();
  const edge = (d: DividerGeom) => (d.dir === "h" ? d.parent.x + d.parent.w * d.ratio : d.parent.y + d.parent.h * d.ratio);
  const candidates = geo.dividers.filter((d) => {
    if ((d.dir === "h") !== horizontal) return false;
    const overlap = horizontal
      ? Math.min(r.y + r.h, d.parent.y + d.parent.h) - Math.max(r.y, d.parent.y)
      : Math.min(r.x + r.w, d.parent.x + d.parent.w) - Math.max(r.x, d.parent.x);
    return overlap > eps;
  });
  const far = horizontal ? r.x + r.w : r.y + r.h;
  const near = horizontal ? r.x : r.y;
  const after = candidates.find((d) => Math.abs(edge(d) - far) < eps);
  const before = candidates.find((d) => Math.abs(edge(d) - near) < eps);
  const d = after ?? before;
  if (!d) return;
  const total = (horizontal ? d.parent.w * area.w : d.parent.h * area.h) || 1;
  // Moving the far edge outward grows; moving the near edge outward (toward 0) grows too.
  const sign = (d === after ? 1 : -1) * (grow ? 1 : -1);
  setDividerRatio(d.target, clamp(d.ratio + (sign * stepPx) / total, 0.1, 0.9));
}

// ── snapping (Windows-style): dragging a tile to a side or corner ─────────

/** A snap layout: a split tree whose leaves are numbered slots. */
export type SnapNode = { s: number } | { dir: SplitDir; ratio: number; a: SnapNode; b: SnapNode };
export interface SnapLayout {
  id: string;
  label: string;
  tree: SnapNode;
}
const slot = (s: number): SnapNode => ({ s });
const sp = (dir: SplitDir, ratio: number, a: SnapNode, b: SnapNode): SnapNode => ({ dir, ratio, a, b });
const HALVES: SnapLayout = { id: "halves", label: "Side by side", tree: sp("h", 0.5, slot(0), slot(1)) };
const QUARTERS: SnapLayout = { id: "grid", label: "Four quarters", tree: sp("v", 0.5, sp("h", 0.5, slot(0), slot(1)), sp("h", 0.5, slot(2), slot(3))) };
const ROWS: SnapLayout = { id: "rows", label: "Above and below", tree: sp("v", 0.5, slot(0), slot(1)) };

/** Snapping a tile to the side or corner of the mosaic (dragging it there): which layout, which slot. */
export type SnapRegion = "left" | "right" | "top" | "bottom" | "tl" | "tr" | "bl" | "br";
const REGION: Record<SnapRegion, [SnapLayout, number]> = {
  left: [HALVES, 0],
  right: [HALVES, 1],
  top: [ROWS, 0],
  bottom: [ROWS, 1],
  tl: [QUARTERS, 0],
  tr: [QUARTERS, 1],
  bl: [QUARTERS, 2],
  br: [QUARTERS, 3],
};

/** Where a slot of a snap layout sits (fractions of the mosaic). */
export function snapSlotRect(tree: SnapNode, want: number, r: Rect = FULL): Rect | null {
  if ("s" in tree) return tree.s === want ? r : null;
  const [ra, rb]: [Rect, Rect] =
    tree.dir === "h"
      ? [{ x: r.x, y: r.y, w: r.w * tree.ratio, h: r.h }, { x: r.x + r.w * tree.ratio, y: r.y, w: r.w * (1 - tree.ratio), h: r.h }]
      : [{ x: r.x, y: r.y, w: r.w, h: r.h * tree.ratio }, { x: r.x, y: r.y + r.h * tree.ratio, w: r.w, h: r.h * (1 - tree.ratio) }];
  return snapSlotRect(tree.a, want, ra) ?? snapSlotRect(tree.b, want, rb);
}
export const regionRect = (region: SnapRegion): Rect => snapSlotRect(REGION[region][0].tree, REGION[region][1])!;

const slotsOf = (t: SnapNode): number[] => ("s" in t ? [t.s] : [...slotsOf(t.a), ...slotsOf(t.b)]);

/**
 * Lay the active mosaic out as `layout`, with tile `id` in slot `at` and the
 * others in the remaining slots in their order (extra ones share the last
 * slot). Slots left over become empty spaces offering what could fill them
 * (snap assist, see components/board/AssistTile).
 */
export function applySnapLayout(id: string, layout: SnapLayout, at: number) {
  const w = wsOf(id);
  if (!w || w !== ws.cur()) return;
  const rest = w.order().filter((x) => x !== id);
  const free = slotsOf(layout.tree).filter((s) => s !== at);
  const last = free[free.length - 1];
  const assist: Pane[] = [];
  const area = ws.areaSize();
  /** Several tiles sharing one slot: stacked along its long side (a dwindle when there are many). */
  const share = (ids: string[], dir: SplitDir): LayoutNode => {
    if (ids.length === 1) return { t: "leaf", pane: ids[0] };
    const even = ids.length <= 3;
    const [head, ...tail] = ids;
    const next: SplitDir = even ? dir : dir === "h" ? "v" : "h";
    return { t: "split", id: uid("s_"), dir, ratio: even ? 1 / ids.length : 0.5, a: { t: "leaf", pane: head }, b: share(tail, next) };
  };
  const fill = (s: number): LayoutNode | null => {
    if (s === at) return { t: "leaf", pane: id };
    if (!rest.length) {
      const p = freshPane({ type: "draft", kind: "assist" });
      assist.push(p);
      return { t: "leaf", pane: p.id };
    }
    // The last free slot takes everything that's left.
    if (s === last && rest.length > 1) {
      const r = snapSlotRect(layout.tree, s)!;
      return share(rest.splice(0), r.h * area.h > r.w * area.w ? "v" : "h");
    }
    return { t: "leaf", pane: rest.shift()! };
  };
  const build = (t: SnapNode): LayoutNode | null => {
    if ("s" in t) return fill(t.s);
    const a = build(t.a);
    const b = build(t.b);
    if (!a) return b;
    if (!b) return a;
    return { t: "split", id: uid("s_"), dir: t.dir, ratio: t.ratio, a, b };
  };
  batch(() => {
    const root = build(layout.tree);
    for (const p of assist) ws.setPanes(p.id, p);
    w.setRoot(root);
    w.setLayout("dwindle");
    w.setMaximized(null);
    w.setFocused(id);
    remoteMeta[ws.active()].layout = undefined;
  });
  commit();
}

/** Drag a tile to a side or corner of the mosaic: it snaps there, the others make room. */
export function snapToRegion(id: string, region: SnapRegion) {
  const [layout, at] = REGION[region];
  applySnapLayout(id, layout, at);
}

/** Empty spaces left by a snap that nothing filled: they close (the tiles around grow back). */
export function closeAssists(index = ws.active()) {
  for (const id of ws.list[index]?.order() ?? []) {
    const c = ws.panes[id]?.content;
    if (c?.type === "draft" && c.kind === "assist") closePane(id);
  }
}

export function toggleMaximize(id: string | null = workspace.focused()) {
  if (!id) return;
  const w = wsOf(id);
  if (!w) return;
  w.setMaximized((m) => (m === id ? null : id));
  w.setFocused(id);
}

/** Geometric neighbour of `id` in the current layout. */
export function neighbor(id: string, dir: Direction): string | null {
  const rects = ws.geometry().panes;
  const cur = rects.get(id);
  if (!cur) return null;
  const eps = 1e-6;
  let best: string | null = null;
  let bestScore = Infinity;
  for (const [pid, r] of rects) {
    if (pid === id) continue;
    let dist: number;
    let overlap: number;
    if (dir === "left" || dir === "right") {
      dist = dir === "right" ? r.x - (cur.x + cur.w) : cur.x - (r.x + r.w);
      overlap = Math.min(cur.y + cur.h, r.y + r.h) - Math.max(cur.y, r.y);
    } else {
      dist = dir === "down" ? r.y - (cur.y + cur.h) : cur.y - (r.y + r.h);
      overlap = Math.min(cur.x + cur.w, r.x + r.w) - Math.max(cur.x, r.x);
    }
    if (dist < -eps || overlap <= eps) continue;
    const score = dist * 10 - overlap;
    if (score < bestScore) {
      bestScore = score;
      best = pid;
    }
  }
  return best;
}

/** Neighbour in order (carousel, monocle), or geometric (tiled). */
function stepTarget(dir: Direction): string | null {
  const cur = workspace.focused();
  if (!cur) return null;
  const order = workspace.order();
  if (ws.mode() !== "tile" || workspace.layoutKind() === "monocle" || workspace.maximized()) {
    const i = order.indexOf(cur);
    const j = dir === "left" || dir === "up" ? i - 1 : i + 1;
    if (ws.mode() === "tile" && (workspace.layoutKind() === "monocle" || workspace.maximized())) return order[(j + order.length) % order.length] ?? null;
    return order[j] ?? null;
  }
  return neighbor(cur, dir);
}

export function moveFocus(dir: Direction) {
  const n = stepTarget(dir);
  if (n) focusPane(n);
}

export function swapWith(dir: Direction) {
  const cur = workspace.focused();
  const other = stepTarget(dir);
  if (cur && other) swapPanes(cur, other);
}

export function setPaneContent(id: string, content: PaneContent) {
  if (!ws.panes[id]) return;
  // Replaced, not merged: what a tile showed before leaves nothing behind.
  ws.setPanes(id, "content", reconcile(content));
  commit();
}

export function updateChatDraft(id: string, patch: Partial<NewChatDraft>) {
  const p = ws.panes[id];
  if (!p || p.content.type !== "chat") return;
  ws.setPanes(
    id,
    "content",
    produce((c) => {
      if (c.type === "chat") c.draft = { ...(c.draft ?? {}), ...patch };
    }),
  );
  commit();
}

/** Which tile (if any, preferring the active workspace) shows this session. */
export function paneShowing(sessionId: string): string | null {
  let other: string | null = null;
  for (const p of Object.values(ws.panes)) {
    if (sessionOf(p) !== sessionId) continue;
    if (ws.owner().get(p.id) === ws.active()) return p.id;
    other ??= p.id;
  }
  return other;
}

/** A tile nobody has used yet: a new chat with nothing typed. */
export function isUnusedPane(id: string | null): boolean {
  const c = id ? ws.panes[id]?.content : undefined;
  return !!c && c.type === 'chat' && !c.sessionId && !getDraft(id!) && !memoryDrafts().some(d => d.identity === draftIdentity(id!) && (d.attachments.length || d.preparing));
}

export interface OpenOpts {
  /** Open in a new tile beside the focused one. */
  newPane?: boolean;
  /** Replace this tile's content. */
  pane?: string;
  /** Split `pane` (or the focused tile) on this side. */
  zone?: DropZone;
}

/** Put `content` on screen according to `opts`; returns the tile used. */
export function place(content: PaneContent, opts: OpenOpts): string | null {
  const w = ws.cur();
  const focused = w.focused();
  if (opts.zone && opts.zone !== "center") return openPane(content, opts.pane ?? focused, opts.zone);
  let target = opts.pane ?? null;
  if (!target) {
    if (!focused || (opts.newPane && !isUnusedPane(focused))) return openPane(content, focused);
    target = focused;
  }
  setPaneContent(target, content);
  focusPane(target);
  return target;
}

/**
 * Open a session. If a tile already shows it, focus that tile (switching
 * workspace if needed); otherwise replace the focused tile's content, or
 * open a new tile with `newPane` / `zone`.
 */
export function openSession(sessionId: string, opts: OpenOpts = {}): string | null {
  const existing = paneShowing(sessionId);
  if (existing && !opts.pane && !opts.zone) {
    focusPane(existing);
    void ensureLoaded(sessionId).catch(() => {});
    return existing;
  }
  const target = place({ type: "chat", sessionId }, opts);
  void ensureLoaded(sessionId).catch(() => {});
  return target;
}

/**
 * New chat. With `newPane` (the default for the button, Ctrl+N and
 * Alt+Enter) it opens a new tile beside the focused one, unless the focused
 * tile is an unused new chat, which is reused.
 */
export function newChat(opts: OpenOpts & { botId?: string } = {}): string | null {
  const w = ws.cur();
  const focused = w.focused();
  const target = opts.pane ?? focused;
  const cur = target ? ws.panes[target] : undefined;
  // Keep the pending model/permission choices when resetting an empty chat.
  const keep = cur?.content.type === "chat" && !cur.content.sessionId ? cur.content.draft : undefined;
  const draft: NewChatDraft | undefined = opts.botId ? { botId: opts.botId } : keep ? { ...keep, botId: undefined, private: undefined } : undefined;
  const content: PaneContent = { type: "chat", sessionId: null, draft };
  if (opts.zone && opts.zone !== "center") return openPane(content, target, opts.zone);
  if (!opts.pane && (!focused || (opts.newPane && !isUnusedPane(focused)))) return openPane(content, focused);
  if (!target) return null;
  setPaneContent(target, content);
  focusPane(target);
  return target;
}

/** Which tile (if any, preferring the active workspace) shows this whiteboard. */
/** Which tile (if any, preferring the active workspace) shows this note. */
export function paneShowingNote(noteId: string): string | null {
  let other: string | null = null;
  for (const p of Object.values(ws.panes)) {
    if (p.content.type !== "note" || p.content.noteId !== noteId) continue;
    if (ws.owner().get(p.id) === ws.active()) return p.id;
    other ??= p.id;
  }
  return other;
}

export function paneShowingBoard(boardId: string): string | null {
  let other: string | null = null;
  for (const p of Object.values(ws.panes)) {
    if (p.content.type !== "whiteboard" || p.content.boardId !== boardId) continue;
    if (ws.owner().get(p.id) === ws.active()) return p.id;
    other ??= p.id;
  }
  return other;
}

/**
 * Show a whiteboard: focus its tile, or open one beside its chat (or the
 * focused tile) when it was closed.
 */
export function openBoard(boardId: string, opts: OpenOpts = {}): string | null {
  const existing = paneShowingBoard(boardId);
  if (existing && !opts.pane && !opts.zone) {
    focusPane(existing);
    return existing;
  }
  const b = state.boards[boardId];
  const chat = b ? paneShowing(b.session_id) : null;
  const content: PaneContent = { type: "whiteboard", boardId };
  if (!opts.pane && !opts.zone && chat && ws.owner().get(chat) === ws.active()) {
    const r = ws.mode() === "tile" ? ws.geometry().panes.get(chat) : undefined;
    const a = ws.areaSize();
    return openPane(content, chat, r && r.w * a.w * 1.1 < r.h * a.h ? "bottom" : "right");
  }
  return place(content, { newPane: true, ...opts });
}

/** Chat with a bot: continue its latest chat, or start one. */
export function openBot(botId: string, opts: OpenOpts & { fresh?: boolean } = {}): string | null {
  if (!opts.fresh) {
    const latest = Object.values(state.sessions)
      .map((s) => s.info)
      .filter((i) => i.bot_id === botId && i.kind === "chat" && !i.archived)
      .sort((a, b) => b.updated_at - a.updated_at)[0];
    if (latest) return openSession(latest.id, opts);
  }
  return newChat({ ...opts, botId });
}

/** Split a tile (default: focused) with a new chat on the given side. */
export function splitPane(zone: "right" | "bottom", id: string | null = workspace.focused()) {
  if (ws.mode() !== "tile" && id) return openPane(undefined, id);
  return openPane({ type: "chat", sessionId: null }, id, id ? zone : undefined);
}

// ── status per workspace (for the strip) ─────────────────────────────────

export type TileStatus = "idle" | "running" | "approval" | "error";

export function paneStatus(id: string): TileStatus {
  const c = ws.panes[id]?.content;
  if (c?.type === "whiteboard") return state.boards[c.boardId]?.ask ? "approval" : "idle";
  const sid = sessionOf(ws.panes[id]);
  const info = sid ? state.sessions[sid]?.info : undefined;
  if (!info) return "idle";
  if (info.status === "awaiting_approval" || pendingAsks(sid).length) return "approval";
  if (isBusy(info)) return "running";
  if (info.status === "error") return "error";
  return "idle";
}

// ── drafts (per pane, persisted) ─────────────────────────────────────────

const DRAFTS_KEY = "tessera.drafts.v2";
type DraftText = { paneId: string; text: string; private: boolean; title?: string };
const [drafts, setDrafts] = createStore<Record<string, DraftText>>(lsGet(DRAFTS_KEY, {}));
const legacyDrafts = lsGet<Record<string, string>>("tessera.drafts", {});
/** A draft belongs to what the pane showed when it was typed, on this server. */
export const draftIdentity = (paneId: string) => {
  const c = ws.panes[paneId]?.content;
  return JSON.stringify([ws.serverKey(), paneId, c?.type === "chat" ? c.sessionId : c?.type]);
};
const privateDrafts = new Set<string>();
const draftListeners = new Set<() => void>();
export function onDraftChanged(cb: () => void) { draftListeners.add(cb); return () => draftListeners.delete(cb); }
let draftsChanged = false;
const storedDrafts = () => Object.fromEntries(Object.entries(drafts).filter(([, d]) => d.text && !d.private));
const saveDrafts = debounce(() => {
  draftsChanged = false;
  lsSet(DRAFTS_KEY, storedDrafts());
}, 400);
window.addEventListener("beforeunload", () => draftsChanged && saveDrafts.flush());

/** Chunk-update reload must not depend on beforeunload/debounced storage writes. */
export function persistDraftsForReload() {
  const warning = reloadWarning();
  if (warning) throw new Error(warning);
  const text = JSON.stringify(storedDrafts());
  localStorage.setItem(DRAFTS_KEY, text); // Throw and keep the page if storage is unavailable/full.
  if (localStorage.getItem(DRAFTS_KEY) !== text) throw new Error("Draft save failed. Copy them before reload.");
  saveDrafts.cancel();
  draftsChanged = false;
  persist.flush(true); // Workspace/pane identity is needed to restore the saved drafts.
}
// Rekeys carry the token; removing an entry invalidates its pending preparation.
export type ComposerMemory = { attachments: Attachment[]; preparing: number; token: number };
let attachmentToken = 0;
const [attachmentDrafts, setAttachmentDrafts] = createStore<Record<string, ComposerMemory>>({});
export const composerAttachments = (identity: string) => attachmentDrafts[identity]?.attachments ?? [];
export function setComposerAttachments(identity: string, next: Attachment[] | ((list: Attachment[]) => Attachment[])) {
  const pane = JSON.parse(identity)[1];
  if (draftIdentity(pane) === identity && isPrivatePane(pane)) privateDrafts.add(identity);
  if (!attachmentDrafts[identity]) setAttachmentDrafts(identity, { attachments: [], preparing: 0, token: ++attachmentToken });
  setAttachmentDrafts(identity, 'attachments', typeof next === 'function' ? next(composerAttachments(identity)) : next);
}
export function prepareAttachments(identity: string) {
  setComposerAttachments(identity, composerAttachments(identity));
  const memory = attachmentDrafts[identity], token = memory.token;
  setAttachmentDrafts(identity, 'preparing', memory.preparing + 1);
  let finished = false;
  const current = () => !finished ? Object.keys(attachmentDrafts).find(key => attachmentDrafts[key].token === token) : undefined;
  return {
    append: (attachment: Attachment) => { const key = current(); if (key) setComposerAttachments(key, list => [...list, attachment]); },
    finish: () => { const key = current(); if (key) setAttachmentDrafts(key, 'preparing', n => n - 1); finished = true; },
  };
}
const memoryDrafts = () => [
  ...Object.entries(staged).map(([identity, attachments]) => ({ identity, attachments, preparing: 0 })),
  ...Object.entries(attachmentDrafts).map(([identity, memory]) => ({ identity, ...memory })),
];
const blockingDrafts = () => memoryDrafts().filter(d => d.attachments.length || d.preparing);
/** Explicit backup of blocking composers only, never unrelated retained/private drafts. */
export function draftBackup(storageFailed = false, pickedPrivate: string[] = []) {
  const blocking = blockingDrafts();
  const ordinary = blocking.length ? blocking : storageFailed ? Object.keys(drafts).map(identity => ({ identity, attachments: composerAttachments(identity), preparing: 0 })) : [];
  const selected = [...ordinary.filter(d => !privateReloadDrafts().some(p => p.identity === d.identity)), ...pickedPrivate.flatMap(identity => {
    if (!privateReloadDrafts().some(p => p.identity === identity)) return [];
    return memoryDrafts().filter(d => d.identity === identity).length ? memoryDrafts().filter(d => d.identity === identity) : [{ identity, attachments: [], preparing: 0 }];
  })];
  return JSON.stringify({ drafts: [...new Set(selected.map(d => d.identity))].flatMap(id => drafts[id] ? [{ ...drafts[id], identity: id }] : []), attachments: selected.flatMap(d => d.attachments) }, null, 2);
}
export function privateReloadDrafts() {
  const identities = new Set([...Object.entries(drafts).filter(([, d]) => d.private && d.text).map(([id]) => id), ...blockingDrafts().filter(d => privateDrafts.has(d.identity) || drafts[d.identity]?.private).map(d => d.identity)]);
  return [...identities].map(identity => {
    const [server, pane, chat] = JSON.parse(identity);
    return { identity, title: drafts[identity]?.title || (server === ws.serverKey() && chat ? state.sessions[chat]?.info.title : '') || 'New private chat', canOpen: server === ws.serverKey(), pane, chat };
  });
}
export function openPrivateDraft(identity: string) {
  const item = privateReloadDrafts().find(d => d.identity === identity);
  if (!item?.canOpen) return;
  if (ws.panes[item.pane] && draftIdentity(item.pane) === identity) {
    if (!item.chat && !isPrivatePane(item.pane)) setPaneContent(item.pane, { type: 'chat', sessionId: null, draft: { private: true } });
    focusPane(item.pane); focusComposer(item.pane); return;
  }
  const occupied = ws.panes[item.pane] && (ws.panes[item.pane].content.type !== 'chat' || getDraft(item.pane) || memoryDrafts().some(d => d.identity === draftIdentity(item.pane) && (d.attachments.length || d.preparing)));
  if (occupied) { toast({ level: 'warning', message: 'This draft’s pane has another unsent draft. Send or discard that draft first. Nothing was moved.' }); return; }
  const content: PaneContent = { type: 'chat', sessionId: item.chat, draft: item.chat ? undefined : { private: true } };
  if (ws.panes[item.pane]) { setPaneContent(item.pane, content); focusPane(item.pane); }
  else if (!openPane(content, workspace.focused(), undefined, item.pane)) return;
  focusComposer(item.pane);
}
export function discardPrivateDraft(identity: string) {
  batch(() => {
    setDrafts(identity, undefined!);
    setStaged(identity, undefined!);
    setAttachmentDrafts(identity, undefined!);
    privateDrafts.delete(identity);
  });
  draftsChanged = true;
  saveDrafts();
  for (const callback of draftListeners) callback();
}
export function reloadWarning() {
  const blocking = blockingDrafts();
  if (blocking.some(d => d.preparing)) return 'Still preparing attachments. Wait until preparation finishes before reloading or backing up.';
  const priv = privateReloadDrafts();
  if (priv.length) return `Private drafts are memory-only: ${priv.map(d => d.title).join(', ')}. Open or discard each one before reloading. Select a private draft explicitly to include it in a backup.`;
  return blocking.length ? 'Unsent attachments and pastes are memory-only. Save or copy the blocking drafts before reloading.' : '';
}

export const getDraft = (paneId: string, identity = draftIdentity(paneId)) => drafts[identity]?.text ?? "";
export function setDraft(paneId: string, text: string, identity = draftIdentity(paneId)) {
  if (isPrivatePane(paneId)) privateDrafts.add(identity);
  const chat = JSON.parse(identity)[2];
  setDrafts(identity, text ? { paneId, text, private: privateDrafts.has(identity), title: typeof chat === 'string' ? state.sessions[chat]?.info.title : undefined } : undefined!);
  draftsChanged = true;
  saveDrafts();
  for (const cb of draftListeners) cb();
}
function moveDrafts(from: string, to: string) {
  for (const [key, draft] of Object.entries(drafts)) if (draft.paneId === from) {
    const identity = JSON.parse(key) as unknown[];
    if (identity[0] !== ws.serverKey()) continue;
    identity[1] = to;
    const moved = JSON.stringify(identity);
    setDrafts(moved, { ...draft, paneId: to });
    if (draft.private) privateDrafts.add(moved);
  }
  for (const [key, memory] of Object.entries(attachmentDrafts)) {
    const [server, pane, chat] = JSON.parse(key);
    if (server !== ws.serverKey() || pane !== from) continue;
    const moved = JSON.stringify([server, to, chat]);
    setAttachmentDrafts(moved, memory);
    setAttachmentDrafts(key, undefined!);
    if (privateDrafts.delete(key)) privateDrafts.add(moved);
  }
  for (const [key, attachments] of Object.entries(staged)) {
    const [server, pane, chat] = JSON.parse(key);
    if (server !== ws.serverKey() || pane !== from) continue;
    const moved = JSON.stringify([server, to, chat]);
    setStaged(moved, attachments);
    setStaged(key, undefined!);
    if (privateDrafts.delete(key)) privateDrafts.add(moved);
  }
}
function clearDraft(paneId: string) {
  setDrafts(produce((d) => {
    for (const key of Object.keys(d)) if (d[key].paneId === paneId && JSON.parse(key)[0] === ws.serverKey()) delete d[key];
  }));
  draftsChanged = true;
  saveDrafts();
}

// Attachments handed to a tile's composer (a rewound message's images come
// back with its text). Memory only; whichever composer the tile shows takes them.
const [staged, setStaged] = createStore<Record<string, Attachment[]>>({});
export const stagedAttachments = (paneId: string): readonly Attachment[] => staged[draftIdentity(paneId)] ?? [];
export function stageAttachments(paneId: string, atts: Attachment[]) {
  const identity = draftIdentity(paneId);
  if (isPrivatePane(paneId)) privateDrafts.add(identity);
  if (atts.length) setStaged(identity, [...(staged[identity] ?? []), ...atts]);
}
/** Take (and forget) what was staged for this exact composer identity. */
export function takeStagedAttachments(paneId: string): Attachment[] {
  const identity = draftIdentity(paneId);
  const list = (staged[identity] ?? []).map((a) => ({ ...a }));
  if (list.length) setStaged(identity, undefined!);
  return list;
}

/** A tile showing a private chat (or a new private chat not sent yet). */
export function isPrivatePane(paneId: string): boolean {
  const c = ws.panes[paneId]?.content;
  if (c?.type !== 'chat') return false;
  if (c.sessionId) return privateIds.has(c.sessionId) || !!state.sessions[c.sessionId]?.info.private;
  return !!c.draft?.private;
}

/** The session shown in a pane, if any. */
export function paneSession(paneId: string) {
  const sid = sessionOf(ws.panes[paneId]);
  return sid ? state.sessions[sid] : undefined;
}

// ── composer focus registry ──────────────────────────────────────────────

const composerFocus = new Map<string, () => void>();
const composerInsert = new Map<string, (text: string) => void>();
// A tile created a moment ago may not have mounted its composer yet; focus it
// as soon as it registers instead of leaving the keyboard in the old tile.
let pendingFocus: string | null = null;
export function registerComposer(paneId: string, focus: () => void, insert?: (text: string) => void) {
  composerFocus.set(paneId, focus);
  if (insert) composerInsert.set(paneId, insert);
  if (pendingFocus === paneId) {
    pendingFocus = null;
    requestAnimationFrame(focus);
  }
  return () => {
    // Only unregister ourselves: a replacement composer may already be mounted.
    if (composerFocus.get(paneId) === focus) composerFocus.delete(paneId);
    if (insert && composerInsert.get(paneId) === insert) composerInsert.delete(paneId);
  };
}

/** The tile has a message box to type into. */
export const hasComposer = (paneId: string) => composerFocus.has(paneId);

/** Put text into a tile's message box where its caret is, and focus it (typing over a tile). */
export function typeIntoComposer(paneId: string, text: string): boolean {
  const insert = composerInsert.get(paneId);
  if (!insert) return false;
  pendingFocus = null;
  insert(text);
  return true;
}
export function focusComposer(paneId: string | null = workspace.focused()) {
  if (!paneId) return;
  if (!composerFocus.has(paneId)) {
    pendingFocus = paneId;
    // Move focus off the previous tile's composer right away so keystrokes
    // typed before the new one mounts don't land in the wrong chat.
    (document.activeElement as HTMLElement | null)?.blur?.();
    return;
  }
  pendingFocus = null;
  // Focus now (so the very next keystroke lands here) and again after layout
  // in case the tile was still animating in.
  composerFocus.get(paneId)?.();
  requestAnimationFrame(() => composerFocus.get(paneId)?.());
}
