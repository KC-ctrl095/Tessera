// Unsaved note text outlives a tile, including a failed save or a conflict.
// Keep it in memory only: the note itself may contain private information.
import type { Note } from '../proto/Note';
export interface NoteDraft { base: Note; title: string; content: string }
const drafts = new Map<string, NoteDraft>();
const key = (server: string, id: string) => JSON.stringify([server, id]);
export const noteDraft = (server: string, id: string) => drafts.get(key(server, id));
export function keepNoteDraft(server: string, draft: NoteDraft) {
  const k = key(server, draft.base.id);
  if (draft.title === draft.base.title && draft.content === draft.base.content) drafts.delete(k);
  else drafts.set(k, draft);
}
export const discardNoteDraft = (server: string, id: string) => drafts.delete(key(server, id));
/** An answer cannot discard edits typed since that save was sent. */
export function noteDraftSaved(server: string, saved: Note, sent: {title: string; content: string}) {
  const draft = noteDraft(server, saved.id);
  if (!draft || saved.version < draft.base.version) return;
  if (draft.title === sent.title && draft.content === sent.content) discardNoteDraft(server, saved.id);
  else keepNoteDraft(server, { ...draft, base: saved });
}
