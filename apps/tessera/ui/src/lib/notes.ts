// Notes: markdown documents kept by the server ("like artifacts"), written by
// the user or an agent, listed in the notes library (components/notes/NotesList,
// APP-PANELS) and shown in note tiles (components/notes/NoteTile). A note is
// rendered with the same sanitizer as chat markdown: never raw HTML. Its HTML
// code can be previewed, but only like an HTML whiteboard: on another origin,
// with no network, in a sandboxed frame (`previewSrc`).
//
// `openNote(id)` puts a note on screen (focus its tile, or open one).
import { createSignal } from "solid-js";
import { produce } from "solid-js/store";
import type { Note } from "../proto/Note";
import type { NoteInfo } from "../proto/NoteInfo";
import { backend, webAccess } from "./backend";
import { pinFrames, previewFrameUrl } from "./native";
import { resolvedTheme } from "./prefs";
import { api, botById, call, setState, state } from "./store";
import { toastError } from "./ui";
import { isTauri, lsGet, lsSet } from "./util";
import { openPane, place, paneShowingNote, focusPane, workspace, type OpenOpts } from "./workspace";

export const NOTE_MAX = 200 << 10;

/** What a note's info is from the full note. */
export const infoOf = (n: Note): NoteInfo => ({
  id: n.id,
  title: n.title,
  preview: n.content.replace(/\s+/g, " ").trim().slice(0, 140),
  version: n.version,
  created_at: n.created_at,
  updated_at: n.updated_at,
  ...(n.session_id ? { session_id: n.session_id } : {}),
  by_user: n.by_user,
  ...(n.all_bots !== undefined ? { all_bots: n.all_bots } : {}),
  ...(n.keep_from_agents !== undefined ? { keep_from_agents: n.keep_from_agents } : {}),
});

/** Keep the store's list in step with a note we just saved or fetched. */
export function rememberNote(n: Note) {
  setState("notes", n.id, infoOf(n));
}

/** Show a note: focus its tile, or open one (beside the focused tile by default). */
export function openNote(noteId: string, opts: OpenOpts = {}): string | null {
  const existing = paneShowingNote(noteId);
  if (existing && !opts.pane && !opts.zone) {
    focusPane(existing);
    return existing;
  }
  const content = { type: "note" as const, noteId };
  if (!opts.pane && !opts.zone && workspace.focused()) return openPane(content, workspace.focused());
  return place(content, { newPane: true, ...opts });
}

/** A new, empty note, open in a tile (the tile's editor starts focused on the title). */
export async function newNote(opts: OpenOpts & { sessionId?: string | null } = {}): Promise<string | null> {
  try {
    const note = await api.saveNote({ id: null, title: "Untitled note", content: "", base_version: null, session_id: opts.sessionId ?? null });
    rememberNote(note);
    return openNote(note.id, opts);
  } catch (e) {
    toastError(e, "Couldn't make a note");
    return null;
  }
}

/** Forget a note locally (it was deleted). */
export function forgetNote(id: string) {
  if (state.notes[id]) setState("notes", produce((d) => void delete d[id]));
}

// ── links between notes ──────────────────────────────────────────────────

/** The note a wikilink means: `[[Title]]` (any case) or `[[note id]]`; the newest when titles repeat. */
export function findNote(target: string): NoteInfo | null {
  const t = target.trim();
  if (!t) return null;
  if (state.notes[t]) return state.notes[t];
  const low = t.toLowerCase();
  let best: NoteInfo | null = null;
  for (const n of Object.values(state.notes)) if (n.title.trim().toLowerCase() === low && (!best || n.updated_at > best.updated_at)) best = n;
  return best;
}

/** The notes that link to this one (older servers: none). */
export async function backlinks(id: string): Promise<NoteInfo[]> {
  try {
    return await call<NoteInfo[]>({ method: "note_backlinks", params: { id } });
  } catch {
    return [];
  }
}

/** Open the note a wikilink means, or make it (Obsidian's way: a link to a note that isn't there yet makes it). */
export async function openLinkedNote(target: string, opts: OpenOpts = {}): Promise<void> {
  const hit = findNote(target);
  if (hit) {
    openNote(hit.id, opts);
    return;
  }
  try {
    const n = await api.saveNote({ id: null, title: target.trim().slice(0, 120) || "Untitled note", content: "", base_version: null });
    rememberNote(n);
    openNote(n.id, opts);
  } catch (e) {
    toastError(e, "Couldn't make the note");
  }
}

// ── who changed it ───────────────────────────────────────────────────────

/** "Nova", "the agent": whose agent wrote a version (the chat's bot, if any). */
export function agentName(sessionId: string | null | undefined): string {
  const info = sessionId ? state.sessions[sessionId]?.info : undefined;
  return botById(info?.bot_id)?.name ?? "the agent";
}

// ── per device: read-only notes, agent changes already looked at ─────────

const RO_KEY = "tessera.notes.readOnly";
const [readOnlyIds, setReadOnlyIds] = createSignal<string[]>(lsGet<string[]>(RO_KEY, []));
/** Shown read-only on this device (the ⋯ menu). */
export const isReadOnly = (id: string) => readOnlyIds().includes(id);
export function setReadOnly(id: string, on: boolean) {
  const next = readOnlyIds().filter((x) => x !== id);
  if (on) next.push(id);
  setReadOnlyIds(next.slice(-500));
  lsSet(RO_KEY, readOnlyIds());
}

const SEEN_KEY = "tessera.notes.reviewed";
/** The newest version of each note whose agent changes were looked at (or waved off) here. */
export function reviewedUpTo(id: string): number {
  return lsGet<Record<string, number>>(SEEN_KEY, {})[id] ?? 0;
}
export function markReviewed(id: string, version: number) {
  const all = lsGet<Record<string, number>>(SEEN_KEY, {});
  if ((all[id] ?? 0) >= version) return;
  all[id] = version;
  const keys = Object.keys(all);
  // Keep the newest few hundred.
  for (const k of keys.slice(0, Math.max(0, keys.length - 400))) delete all[k];
  lsSet(SEEN_KEY, all);
}

// ── previews of HTML ─────────────────────────────────────────────────────

/**
 * Where to frame a preview of `html`: the app's board origin (Tauri), or
 * the server's preview page (browser UI). Never srcdoc or a blob: those
 * would run with the app's origin or policy.
 */
export async function previewSrc(html: string): Promise<string> {
  const theme = resolvedTheme();
  if (isTauri) {
    const url = await previewFrameUrl(html);
    if (!url) throw new Error("This version of the app can't preview HTML yet. Update Tessera to see it.");
    return theme === "light" ? `${url}?theme=light` : url;
  }
  if (webAccess) throw new Error("HTML previews show in the Tessera app, not in a web browser.");
  const { path } = await api.previewPage(html);
  const u = new URL(path, backend.sameOrigin ? location.origin : state.conn.endpoint ?? location.origin);
  u.searchParams.set("theme", theme);
  pinFrames(u.toString());
  return u.toString();
}

// ── export ───────────────────────────────────────────────────────────────

/** A file name for a note: its title, made safe. */
export function noteFileName(title: string, ext: string): string {
  const base = title.replace(/[\\/:*?"<>|\u0000-\u001f]+/g, " ").replace(/\s+/g, " ").trim().slice(0, 80) || "note";
  return `${base}.${ext}`;
}
