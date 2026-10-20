// "The agent sees what you see": a picture of a whiteboard tile as it is on
// this screen, for an agent that asked to look (`board_snapshot_request`)
// or for "Ask about this".
//
// Privacy rules, all enforced here or in the shell:
// - only on request: the server's request for one board, or the user's
//   own click (`captureTile`); never in the background or on a timer;
// - only while that board's tile can actually be seen on this screen (the
//   app in front, the tile on the current mosaic and page, nothing covering
//   it);
// - only the tile's content area: the shell copies exactly that rect of the
//   app's own web view, never the window, the app around it or the desktop;
// - nothing is kept: the picture goes to the server and is dropped;
// - each device can turn it off (Settings → Privacy & safety → More).
import { createSignal } from "solid-js";
import type { SnapshotImage } from "../proto/SnapshotImage";
import { backend } from "./backend";
import { boardCapture } from "./native";
import { call } from "./store";
import { isTauri, lsGet, lsSet } from "./util";
import { workspace } from "./workspace";

// ── the per-device switch ─────────────────────────────────────────────────

const KEY = "tessera.agentLooks";
const [looksOn, setLooksOn] = createSignal<boolean>(lsGet<boolean>(KEY, true) !== false);
/** Agents may look at whiteboard tiles on this device's screen (default on). */
export const agentLooks = looksOn;
export function setAgentLooks(on: boolean) {
  setLooksOn(on);
  lsSet(KEY, on);
}

/** This app can take pictures of tiles at all (the desktop or phone app; not a browser). */
export const canCaptureHere = isTauri;

// ── "The agent looked at this" ──────────────────────────────────────────

/** How long a tile shows that the agent just looked at it. */
export const LOOKED_MS = 6000;
const [looked, setLooked] = createSignal<Record<string, number>>({});
const lookTimers = new Map<string, ReturnType<typeof setTimeout>>();

/**
 * True for a few seconds after an agent got a picture of this board from
 * this device: the board tile shows a quiet "The agent looked at this".
 */
export const lookedAt = (boardId: string): boolean => !!looked()[boardId];

function markLooked(boardId: string) {
  setLooked((m) => ({ ...m, [boardId]: Date.now() }));
  clearTimeout(lookTimers.get(boardId));
  lookTimers.set(
    boardId,
    setTimeout(() => {
      lookTimers.delete(boardId);
      setLooked((m) => {
        const next = { ...m };
        delete next[boardId];
        return next;
      });
    }, LOOKED_MS),
  );
}

// ── finding the tile on screen ──────────────────────────────────────────

const paneEl = (paneId: string) => document.querySelector<HTMLElement>(`section.pane[data-pane-id="${CSS.escape(paneId)}"]`);

/**
 * The part of a whiteboard tile's content area that can be seen right now,
 * in CSS pixels, or why not. Not the tile's header or address bar: only what
 * the board itself shows.
 */
export function visibleTileRect(paneId: string): { rect: DOMRect } | { error: string } {
  const notShown = { error: "The whiteboard isn't on screen on this device" };
  if (document.visibilityState !== "visible") return notShown;
  const pane = workspace.panes[paneId];
  if (pane?.content.type !== "whiteboard") return notShown;
  if (workspace.paneWorkspace(paneId) !== workspace.active()) return notShown;
  const section = paneEl(paneId);
  const body = section?.querySelector<HTMLElement>(".board-body");
  if (!section || !body) return notShown;
  const b = body.getBoundingClientRect();
  const s = section.getBoundingClientRect();
  // Only what is inside the tile and inside the window (a tile mid-swipe is partly off screen).
  const x0 = Math.max(b.left, s.left, 0);
  const y0 = Math.max(b.top, s.top, 0);
  const x1 = Math.min(b.right, s.right, window.innerWidth);
  const y1 = Math.min(b.bottom, s.bottom, window.innerHeight);
  const w = x1 - x0;
  const h = y1 - y0;
  // Mostly hidden (a neighbour page peeking in) counts as not on screen.
  if (w < 48 || h < 48 || w * h < 0.4 * b.width * b.height) return notShown;
  // Nothing on top of it: a dialog, the drawer, the mosaic overview, a menu.
  const at = (fx: number, fy: number) => {
    const el = document.elementFromPoint(x0 + w * fx, y0 + h * fy);
    return !!el && body.contains(el);
  };
  const corners = [at(0.25, 0.25), at(0.75, 0.25), at(0.25, 0.75), at(0.75, 0.75)].filter(Boolean).length;
  if (!at(0.5, 0.5) || corners < 3) return { error: "Something covers the whiteboard on this device" };
  return { rect: new DOMRect(x0, y0, w, h) };
}

/** Every tile showing this board, on the current mosaic first. */
function panesShowing(boardId: string): string[] {
  return Object.values(workspace.panes)
    .filter((p) => p.content.type === "whiteboard" && p.content.boardId === boardId)
    .map((p) => p.id)
    .sort((a, b) => Number(workspace.paneWorkspace(b) === workspace.active()) - Number(workspace.paneWorkspace(a) === workspace.active()));
}

// ── taking the picture ──────────────────────────────────────────────────

let busy: Promise<unknown> = Promise.resolve();

/** One capture at a time: the shell reads the screen as it is now. */
function serial<T>(fn: () => Promise<T>): Promise<T> {
  const run = busy.then(fn, fn);
  busy = run.catch(() => {});
  return run;
}

/** A picture of a tile's content as it is on screen now, or why there is none. */
export function tryCaptureTile(paneId: string): Promise<{ image: SnapshotImage } | { error: string }> {
  return serial(async () => {
    if (!canCaptureHere) return { error: "can't capture on this device" };
    const v = visibleTileRect(paneId);
    if ("error" in v) return v;
    // Let the frame settle (a menu that just closed, a tile that just slid in).
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    const again = visibleTileRect(paneId);
    if ("error" in again) return again;
    const r = again.rect;
    try {
      const image = await boardCapture({ x: r.x, y: r.y, width: r.width, height: r.height, viewportWidth: window.innerWidth, viewportHeight: window.innerHeight });
      if (!image) return { error: "can't capture on this device" };
      return { image };
    } catch (e) {
      return { error: `Couldn't take the picture: ${String((e as Error)?.message ?? e)}` };
    }
  });
}

/**
 * A picture of a whiteboard tile's content (for "Ask about this"), or null
 * when this device can't take one or the tile isn't on screen. Only call it
 * on the user's own request.
 */
export async function captureTile(paneId: string): Promise<SnapshotImage | null> {
  const r = await tryCaptureTile(paneId);
  return "image" in r ? r.image : null;
}

// ── answering the server ────────────────────────────────────────────────

/** Requests per board, to keep a looping agent from filming the screen. */
const lastAsk = new Map<string, number>();
const MIN_GAP_MS = 1500;

async function answer(requestId: string, boardId: string) {
  let reply: { image: SnapshotImage } | { error: string };
  const prev = lastAsk.get(boardId) ?? 0;
  if (!looksOn()) reply = { error: "Looking at whiteboards is turned off on this device" };
  else if (!canCaptureHere) reply = { error: "can't capture on this device" };
  else if (Date.now() - prev < MIN_GAP_MS) reply = { error: "Asked again too soon; try again in a moment" };
  else {
    lastAsk.set(boardId, Date.now());
    const panes = panesShowing(boardId);
    reply = { error: "The whiteboard isn't on screen on this device" };
    for (const id of panes) {
      const r = await tryCaptureTile(id);
      reply = r;
      if ("image" in r) break;
    }
  }
  if ("image" in reply) markLooked(boardId);
  const params = "image" in reply ? { request_id: requestId, image: reply.image } : { request_id: requestId, error: reply.error };
  try {
    await call<null>({ method: "board_snapshot", params });
  } catch (e) {
    // An older server, or the request already timed out there.
    console.warn("board_snapshot", e);
  }
}

backend.onEvent((e) => {
  if (e.type !== "board_snapshot_request") return;
  if (typeof e.request_id !== "string" || typeof e.board_id !== "string") return;
  void answer(e.request_id, e.board_id);
});
