// Keeping a note tile in step with the server (components/notes/NoteTile).
//
// The tile edits a copy of one version of the note (its base) and saves on
// its own. Every save comes back to every device as `note_updated`, this one
// included, often before the save's own answer. So a newer version is only a
// change from elsewhere when it is neither the base nor something this tile
// sent; and a change from elsewhere only needs asking about when it touches
// the same lines as what is typed here. Everything else follows quietly.
import type { Note } from "../proto/Note";
import { merge3 } from "./textDiff";

/** The title as the server keeps it: its first line, trimmed, at most 120 bytes. */
export function serverTitle(raw: string): string {
  const line = raw.split(/\r?\n/).map((l) => l.trim()).find(Boolean) ?? "";
  const bytes = new TextEncoder().encode(line);
  if (bytes.length <= 120) return line || "Untitled note";
  let cut = 120;
  // Back to a character boundary.
  while (cut > 0 && (bytes[cut] & 0xc0) === 0x80) cut--;
  return new TextDecoder().decode(bytes.slice(0, cut)) + "…";
}

/** A short fingerprint of a title and text (what this tile sent). */
export function noteKey(title: string, content: string): number {
  const s = `${title}\u0000${content}`;
  let h1 = 0xdeadbeef;
  let h2 = 0x41c6ce57;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    h1 = Math.imul(h1 ^ c, 2654435761);
    h2 = Math.imul(h2 ^ c, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return 4294967296 * (2097151 & h2) + (h1 >>> 0);
}

export type Draft = { title: string; content: string };

export type Decision =
  /** Nothing newer than the base. */
  | { kind: "ignore" }
  /** Newer, but nothing to show: this tile's own save, or only the version moved. Keep what's typed. */
  | { kind: "rebase" }
  /** Nothing typed here since the base: show theirs. */
  | { kind: "follow" }
  /** Both changed, in different places: both kept. */
  | { kind: "merge"; title: string; content: string }
  /** Both changed the same lines (or the title): ask. */
  | { kind: "conflict" };

/**
 * What to do with `remote`, a version of the note newer than this tile's
 * `base`, given what is typed here (`local`, title as the server keeps it)
 * and the fingerprints of what this tile sent (`sent`).
 */
export function reconcile(base: Note, local: Draft, remote: Note, sent: readonly number[]): Decision {
  if (remote.version <= base.version) return { kind: "ignore" };
  if (sent.includes(noteKey(remote.title, remote.content))) return { kind: "rebase" };
  if (remote.title === base.title && remote.content === base.content) return { kind: "rebase" };
  if (local.title === base.title && local.content === base.content) return { kind: "follow" };
  if (local.title === remote.title && local.content === remote.content) return { kind: "rebase" };
  const title = local.title === base.title || local.title === remote.title ? remote.title : remote.title === base.title ? local.title : null;
  const content = merge3(base.content, local.content, remote.content);
  if (title === null || !content.ok) return { kind: "conflict" };
  return { kind: "merge", title, content: content.text };
}
