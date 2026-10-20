// Keeps the mosaics (tiles and their chats) the same on every device — desktop,
// phone and terminal — through the server's shared layout (`get_mosaics` /
// `save_mosaics`, optimistic `version`, `mosaics_changed` broadcasts).
//
// Local edits are pushed ~500 ms after they settle, and at once when the app
// goes to the background (phones suspend or kill backgrounded apps). On a
// `conflict` (or a broadcast from another device) the layout is merged
// three-way against the last agreed one, tile by tile, and the tile being
// typed in is never dropped.
//
// The last agreed layout (the merge base) is saved with the tiling, so a
// change that never reached the server (closed a tile, then the app was
// killed or offline) still wins over the server's older copy after a restart
// instead of being undone by it.
//
// Which tile is focused is per device: it travels with the layout but never
// counts as a change on its own.
// Private tiles never leave the device (the server would drop them anyway).
// Other clients' ids, names, layout names and tile fields are kept as they are.
import type { Mosaic } from "../proto/Mosaic";
import type { MosaicLayout } from "../proto/MosaicLayout";
import { connectionGeneration } from "./connectionGeneration";
import { api, onBootstrapped, onMosaicsChanged, state } from "./store";
import { debounce } from "./util";
import { exportMosaics, getSyncState, importMosaics, onLayoutCommit, onDraftChanged, setSyncState, typingPane, workspaceServer } from "./workspace";

/** Stable JSON: sorted keys, no undefined (the server may reorder keys). */
function canon(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map(canon).join(",")}]`;
  if (v && typeof v === "object") {
    const o = v as Record<string, unknown>;
    return `{${Object.keys(o)
      .filter((k) => o[k] !== undefined && o[k] !== null)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${canon(o[k])}`)
      .join(",")}}`;
  }
  return JSON.stringify(v ?? null);
}

/** A mosaic without this device's focus (focus alone is never a change). */
function sem(m: Mosaic | undefined): unknown {
  if (!m) return m;
  const g = m.geometry;
  if (!g || typeof g !== "object" || Array.isArray(g)) return m;
  const { focused: _focused, ...rest } = g as Record<string, unknown>;
  return { ...m, geometry: rest };
}
/** What the mosaic holds: split-tree fitting may reorder its tile list. */
const shape = (m: Mosaic | undefined) => (m ? { id: m.id, name: m.name, layout: m.layout, tiles: [...m.tiles].sort((a, b) => a.id.localeCompare(b.id)) } : m);

const byId = (list: Mosaic[]) => new Map(list.map((m) => [m.id, canon(sem(m))]));
const same = (a: unknown, b: unknown) => canon(a) === canon(b);

let enabled = false;
let version = 0;
/** The mosaics as last agreed with the server (the merge base). */
let base = new Map<string, Mosaic>();
let agreed = new Map<string, string>();
let inflight = false;
let again = false;
let syncGeneration = 0;
let connection = -1;
let deferred: MosaicLayout | null = null;
const currentScope = () => connection === connectionGeneration() && syncedServer === workspaceServer();

function setBase(list: Mosaic[], v: number) {
  version = v;
  base = new Map(list.map((m) => [m.id, m]));
  agreed = byId(list);
  setSyncState({ version: v, base: list });
}

/** Both sides changed one mosaic: merge its tiles against the base, one by one. */
function mergeTiles(b: Mosaic | undefined, l: Mosaic, r: Mosaic): Mosaic {
  const bt = new Map((b?.tiles ?? []).map((t) => [t.id, t]));
  const lt = new Map(l.tiles.map((t) => [t.id, t]));
  const rt = new Map(r.tiles.map((t) => [t.id, t]));
  const tiles: Mosaic["tiles"] = [];
  for (const t of r.tiles) {
    if (bt.has(t.id) && !lt.has(t.id)) continue; // closed here
    const mine = lt.get(t.id);
    tiles.push(mine && !same(mine, bt.get(t.id)) ? mine : t);
  }
  // Opened here: after the tile it followed here.
  l.tiles.forEach((t, k) => {
    if (rt.has(t.id) || bt.has(t.id)) return;
    const prev = l.tiles
      .slice(0, k)
      .reverse()
      .find((x) => tiles.some((y) => y.id === x.id));
    const at = prev ? tiles.findIndex((y) => y.id === prev.id) + 1 : tiles.length;
    tiles.splice(at, 0, t);
  });
  return {
    id: r.id,
    name: b && l.name !== b.name ? l.name : r.name,
    layout: b && l.layout !== b.layout ? l.layout : r.layout,
    tiles,
    // The split tree is this device's; import fits it to the merged tiles.
    geometry: l.geometry ?? r.geometry,
  };
}

/**
 * Three-way merge of the local mosaics and the server's, against the last
 * agreed layout. A side that didn't change a mosaic takes the other's; when
 * both did, tiles merge one by one. A mosaic closed on one side stays closed
 * unless the other side changed what's in it (a geometry-only change there
 * doesn't bring it back).
 */
function merge3(local: Mosaic[], remote: Mosaic[]): Mosaic[] {
  const lm = new Map(local.map((m) => [m.id, m]));
  const rm = new Map(remote.map((m) => [m.id, m]));
  const ids = [...remote.map((m) => m.id), ...local.map((m) => m.id).filter((id) => !rm.has(id))];
  const out: Mosaic[] = [];
  for (const id of ids) {
    const b = base.get(id);
    const l = lm.get(id);
    const r = rm.get(id);
    const lc = !same(sem(l), sem(b));
    const rc = !same(sem(r), sem(b));
    if (!lc) {
      if (r) out.push(r);
    } else if (!rc) {
      if (l) out.push(l);
    } else if (!l) {
      // Closed here, changed there: theirs only if they changed its tiles.
      if (r && !same(shape(r), shape(b))) out.push(r);
    } else if (!r) {
      if (!same(shape(l), shape(b))) out.push(l);
    } else out.push(mergeTiles(b, l, r));
  }
  return out;
}

/** Never lose the tile being typed in to a change from elsewhere. */
function protect(merged: Mosaic[], local: Mosaic[]): Mosaic[] {
  const id = typingPane();
  if (!id) return merged;
  const home = local.find((m) => m.tiles.some((t) => t.id === id));
  if (!home) return merged;
  const tile = home.tiles.find((t) => t.id === id)!;
  if (merged.some((m) => m.tiles.some((t) => t.id === id)))
    return merged.map((m) => ({ ...m, tiles: m.tiles.map((t) => t.id === id ? tile : t) }));
  const target = merged.find((m) => m.id === home.id);
  if (!target) return [...merged, home];
  return merged.map((m) => (m === target ? { ...m, tiles: [...m.tiles, tile] } : m));
}

/** Take the server's layout, merged with what changed here since the last sync. */
function adopt(layout: MosaicLayout, keepLocal: boolean) {
  const local = exportMosaics();
  const candidate = keepLocal && (base.size || local.length) ? merge3(local, layout.mosaics) : layout.mosaics;
  const protectedLayout = protect(candidate, local);
  if (!same(protectedLayout, candidate)) {
    // Keep the entire update pending, so its merge base is still valid when
    // typing ends. Pushing now would overwrite the other device's change.
    deferred = layout;
    return;
  }
  deferred = null;
  const merged = candidate;
  setBase(layout.mosaics, layout.version);
  if (canon(merged) !== canon(local)) importMosaics(merged);
  if (canon(merged.map(sem)) !== canon(layout.mosaics.map(sem))) push();
}

let current: Promise<void> | null = null;

/**
 * Share the layout now (no debounce) and wait for the server, at most `ms`:
 * a chat started in a tile must be on the server before the agent answers,
 * or a whiteboard it shows can't be put beside it.
 */
export async function syncLayoutNow(ms = 1500): Promise<void> {
  if (!enabled || state.conn.state !== "connected") return;
  push.cancel();
  const run = (async () => {
    if (current) await current.catch(() => {});
    await pushNow();
    if (current) await current.catch(() => {});
  })();
  await Promise.race([run, new Promise<void>((r) => setTimeout(r, ms))]);
}

function pushNow(): Promise<void> {
  const p = pushOnce();
  const generation = syncGeneration;
  if (inflight && !current) current = p.finally(() => { if (generation === syncGeneration) current = null; });
  return p;
}

async function pushOnce() {
  if (!enabled || !currentScope() || deferred) return;
  const generation = syncGeneration;
  const valid = () => generation === syncGeneration && currentScope();
  if (inflight) {
    again = true;
    return;
  }
  const mosaics = exportMosaics();
  const now = byId(mosaics);
  if (now.size === agreed.size && [...now].every(([id, c]) => agreed.get(id) === c)) return;
  inflight = true;
  try {
    const saved = await api.saveMosaics({ version, mosaics });
    if (!valid()) return;
    setBase(saved.mosaics, saved.version);
  } catch (e) {
    if (!valid()) return;
    if ((e as { code?: string }).code === "conflict") {
      try {
        const layout = await api.getMosaics();
        if (valid()) adopt(layout, true);
      } catch {
        /* try again on the next change */
      }
    }
    // Offline or disconnected: the change stays local (and saved with the
    // merge base); the next connect merges it in.
  } finally {
    if (!valid()) return;
    inflight = false;
    if (again) {
      again = false;
      push();
    }
  }
}

const push = debounce(() => void pushNow(), 500);
const applyDeferred = () => {
  if (!deferred || typingPane() || !currentScope()) return;
  const layout = deferred;
  deferred = null;
  adopt(layout, true);
  push();
};
const tryDeferred = () => queueMicrotask(applyDeferred);
document.addEventListener("focusout", tryDeferred);
document.addEventListener("input", tryDeferred);
onDraftChanged(tryDeferred);

onLayoutCommit(() => {
  if (enabled) push();
});

// Leaving (backgrounded, closed, killed): send what's pending now.
const flushNow = () => {
  if (enabled && state.conn.state === "connected") push.flush();
};
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") flushNow();
});
window.addEventListener("pagehide", flushNow);

onMosaicsChanged((layout) => {
  if (!enabled || !currentScope() || layout.version <= version) return;
  adopt(layout, true);
});

let syncedServer: string | null = null;
onBootstrapped(() => {
  const generation = ++syncGeneration;
  connection = connectionGeneration();
  const valid = () => generation === syncGeneration && currentScope();
  enabled = false;
  inflight = again = false;
  current = null;
  push.cancel();
  deferred = null;
  // The merge base belongs to the tiling it was agreed for (saved together).
  const server = workspaceServer();
  if (server !== syncedServer) {
    syncedServer = server;
    const saved = getSyncState();
    version = saved?.version ?? 0;
    base = new Map((saved?.base ?? []).map((m) => [m.id, m]));
    agreed = byId(saved?.base ?? []);
  }
  void (async () => {
    let layout: MosaicLayout;
    try {
      layout = await api.getMosaics();
      if (!valid()) return;
    } catch {
      return; // An older server without shared mosaics: stay local.
    }
    if (layout.version === 0 && !layout.mosaics.length) {
      // First device on this server (or it was reset): share what is here.
      setBase([], 0);
      enabled = true;
      void pushNow();
      return;
    }
    // The server went back in time (restored or reset): its copy wins.
    if (layout.version < version) {
      base = new Map();
      agreed = new Map();
    }
    adopt(layout, base.size > 0);
    enabled = true;
  })();
});
