// The notes library in the sidebar's Notes tab: every saved note, newest
// first, with who made it (you, or the chat whose agent wrote it). A click
// opens the note in a tile (lib/notes.ts, NoteTile). The list itself lives in
// the store (`state.notes`), kept live by `note_updated` / `note_removed`.
import { Show, createEffect, createMemo, createRoot, createSignal } from "solid-js";
import type { NoteInfo } from "../../proto/NoteInfo";
import { state } from "../../lib/store";
import { lsGet, lsSet } from "../../lib/util";
import { sessionLabel } from "../bots";
import { onDemand } from "../../lib/onDemand";
import { PartError } from "../PartError";

/** Every note, newest first. */
export const noteList = createRoot(() => createMemo(() => Object.values(state.notes).sort((a, b) => b.updated_at - a.updated_at)));

/** Who made a note, for its row: "You", the chat's title, or "An agent". */
export function noteAuthor(n: NoteInfo): { label: string; title: string; botId?: string } {
  if (n.by_user) return { label: "You", title: "Written by you" };
  const chat = n.session_id ? state.sessions[n.session_id]?.info ?? state.archived?.find((s) => s.id === n.session_id) : undefined;
  if (!chat) return { label: "An agent", title: n.session_id ? "Written by an agent in a chat that's no longer here" : "Written by an agent" };
  const label = sessionLabel(chat);
  return { label, title: `Written in “${label}”`, botId: chat.bot_id ?? undefined };
}

export const noteMatches = (n: NoteInfo, q: string) => !q || n.title.toLowerCase().includes(q) || n.preview.toLowerCase().includes(q) || noteAuthor(n).label.toLowerCase().includes(q);

// ── new from agents (the tab's badge) ────────────────────────────────────

const SEEN_KEY = "tessera.notes.seen";
/** The newest note time this device has shown in the list (server clock). */
const [seenUpTo, setSeenUpTo] = createSignal<number | null>(lsGet<number | null>(SEEN_KEY, null));

/** Notes agents wrote or changed since the list was last looked at here. */
export const unseenNotes = createRoot(() =>
  createMemo(() => {
    if (!state.notesLoaded) return 0;
    const seen = seenUpTo();
    // First run on this device: nothing is news yet.
    if (seen === null) return 0;
    return noteList().filter((n) => !n.by_user && n.updated_at > seen).length;
  }),
);

// First time this device sees the library: what is there already isn't news.
createRoot(() =>
  createEffect(() => {
    if (state.notesLoaded && seenUpTo() === null) markNotesSeen();
  }),
);

/** The list is on screen: what it shows isn't news any more. */
export function markNotesSeen() {
  if (!state.notesLoaded) return;
  const newest = noteList().reduce((m, n) => Math.max(m, n.updated_at), 0);
  const seen = seenUpTo();
  if (seen !== null && newest <= seen) return;
  setSeenUpTo(Math.max(newest, seen ?? 0));
  lsSet(SEEN_KEY, Math.max(newest, seen ?? 0));
}

// The badge/author/matching state above stays eager. Rows, menus and filters are not startup code.
const views = onDemand(() => import("./NotesListViews"), "NotesListViews");

export function NoteRow(props: { note: NoteInfo; onPick: () => void; active?: boolean; query?: string }) {
  void views.load();
  return <Show when={views.get()} fallback={<>
    <li class="srow nrow nrow-loading" aria-busy={!views.error()}>
      <div class="srow-main nrow-main">
        <span class="nrow-line"><span class="srow-title nrow-title">{props.note.title || "Untitled note"}</span></span>
        <Show when={props.note.preview}><span class="nrow-preview" aria-hidden="true">{props.note.preview}</span></Show>
        <span class="nrow-by"><span class="nrow-by-label">{noteAuthor(props.note).label}</span></span>
      </div>
      <span class="icon-btn sm srow-more" aria-hidden="true" />
    </li>
    <Show when={views.error()}><li class="note-list-error"><PartError error={views.error()} retry={views.retry} /></li></Show>
  </>}>{(m) => { const Row = m().NoteRow; return <Row {...props} />; }}</Show>;
}

export function NotesList(props: { onPick: () => void; focusedNote?: string | null }) {
  createEffect(() => { if (noteList().length && !views.get() && !views.error()) void views.load(); });
  return <Show when={noteList().length} fallback={<div class="notes-list">
    <Show when={state.notesLoaded} fallback={<div class="sb-note-empty"><p>This server doesn't keep notes yet. Update Tessera on it to use notes.</p></div>}>
      <p class="sb-empty sb-first">No notes yet. Write one with New → Note at the top right, or ask an agent to keep one for you.</p>
    </Show>
  </div>}><Show when={views.get()} fallback={<Show when={views.error()} fallback={<p class="muted" role="status">Loading notes…</p>}>
    <PartError error={views.error()} retry={views.retry} />
  </Show>}>{(m) => { const List = m().NotesList; return <List {...props} />; }}</Show></Show>;
}
