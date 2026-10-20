// Drop targets shared by tile drags and sidebar drags (both pointer-event
// drags, see drag.ts): tile zones, mosaic pills and the empty mosaic.
import { createSignal } from "solid-js";
import { movePane, newChat, openBot, openSession, paneShowing, switchWorkspace, workspace, type DropZone, type OpenOpts, type SnapRegion } from "./workspace";

export type DragPayload = { kind: "session"; id: string } | { kind: "bot"; id: string } | { kind: "new" };

/** Where a drop would land: a tile edge/centre, a workspace pill, or (tiles only) a side or corner of the mosaic. */
export type DropHint = { paneId: string; zone: DropZone } | { ws: number } | { launcher: true } | { snap: SnapRegion };

export const [dropHint, setDropHint] = createSignal<DropHint | null>(null);

/** Which part of a tile the pointer is over: the middle third replaces, edges split. */
export function zoneAt(r: DOMRect, x: number, y: number): DropZone {
  const fx = (x - r.left) / Math.max(1, r.width);
  const fy = (y - r.top) / Math.max(1, r.height);
  if (fx > 0.3 && fx < 0.7 && fy > 0.3 && fy < 0.7) return "center";
  const d: [DropZone, number][] = [
    ["left", fx],
    ["right", 1 - fx],
    ["top", fy],
    ["bottom", 1 - fy],
  ];
  d.sort((a, b) => a[1] - b[1]);
  return d[0][0];
}

/** Two tiles side by side (foldables): only before, after, or swap. */
function zoneInRow(r: DOMRect, x: number): DropZone {
  const fx = (x - r.left) / Math.max(1, r.width);
  return fx < 0.3 ? "left" : fx > 0.7 ? "right" : "center";
}

/** What is under the pointer: a mosaic pill, a tile zone (not `exclude`), or the empty mosaic. */
export function hitTestAt(x: number, y: number, exclude?: string): DropHint | null {
  const under = document.elementFromPoint(x, y) as HTMLElement | null;
  if (!under) return null;
  const pill = under.closest<HTMLElement>("[data-ws]");
  if (pill) return { ws: Number(pill.dataset.ws) };
  const mode = workspace.mode();
  const tile = under.closest<HTMLElement>("[data-pane-id]");
  const id = tile?.dataset.paneId;
  // Phones show one tile at a time: a tile dragged there goes before or after it, or swaps.
  if (tile && id && id !== exclude && (mode !== "mobile" || exclude)) {
    const r = tile.getBoundingClientRect();
    return { paneId: id, zone: mode === "tile" ? zoneAt(r, x, y) : zoneInRow(r, x) };
  }
  if (under.closest("[data-drop='launcher']")) return { launcher: true };
  return null;
}

/** Open a dragged item with the given placement. */
export function openPayload(p: DragPayload, opts: OpenOpts) {
  if (p.kind === "session") return openSession(p.id, opts);
  if (p.kind === "bot") return openBot(p.id, opts);
  return newChat({ ...opts, newPane: !opts.pane });
}

/** Drop onto workspace pill `index`: switch there and open in a new tile. */
export function dropOnWorkspace(p: DragPayload, index: number) {
  switchWorkspace(index);
  openPayload(p, { newPane: true });
}

/** Drop a sidebar item. A chat already on screen moves instead of opening twice. */
export function dropPayload(p: DragPayload, hint: DropHint): boolean {
  if ("ws" in hint) {
    dropOnWorkspace(p, hint.ws);
    return true;
  }
  if ("launcher" in hint) {
    openPayload(p, { newPane: true });
    return true;
  }
  if (!("paneId" in hint)) return false;
  const { paneId: id, zone } = hint;
  if (p.kind === "session") {
    const existing = paneShowing(p.id);
    if (existing === id) return true;
    if (existing && workspace.paneWorkspace(existing) === workspace.active()) {
      movePane(existing, id, zone);
      return true;
    }
  }
  openPayload(p, zone === "center" ? { pane: id } : { pane: id, zone });
  return true;
}

/**
 * A tile dragged to a side or corner of the mosaic (tiled desktop) snaps
 * there, Windows-style: the band along each side, wider at the corners.
 */
const SNAP_BAND = 26;
const SNAP_CORNER = 90;
export function snapRegionAt(area: DOMRect, x: number, y: number): SnapRegion | null {
  if (x < area.left - 12 || x > area.right + 12 || y < area.top - 12 || y > area.bottom + 12) return null;
  const l = x - area.left;
  const r = area.right - x;
  const t = y - area.top;
  const b = area.bottom - y;
  const near = Math.min(l, r, t, b);
  if (near > SNAP_BAND) return null;
  const sideX = l < SNAP_CORNER ? "l" : r < SNAP_CORNER ? "r" : "";
  const sideY = t < SNAP_CORNER ? "t" : b < SNAP_CORNER ? "b" : "";
  if (sideX && sideY) return (sideY + sideX) as SnapRegion;
  if (near === l) return "left";
  if (near === r) return "right";
  return near === t ? "top" : "bottom";
}

export const SNAP_LABEL: Record<SnapRegion, string> = {
  left: "Snap to the left half",
  right: "Snap to the right half",
  top: "Snap to the top half",
  bottom: "Snap to the bottom half",
  tl: "Snap to the top-left quarter",
  tr: "Snap to the top-right quarter",
  bl: "Snap to the bottom-left quarter",
  br: "Snap to the bottom-right quarter",
};

export const ZONE_LABEL: Record<DropZone, string> = {
  left: "Open on the left",
  right: "Open on the right",
  top: "Open above",
  bottom: "Open below",
  center: "Open here",
};

export const MOVE_LABEL: Record<DropZone, string> = {
  left: "Move to the left",
  right: "Move to the right",
  top: "Move above",
  bottom: "Move below",
  center: "Swap",
};
