// What the editor asks of the tile around it (components/notes/NoteEditor):
// the notes to link to, opening things, menus, pictures and the parts drawn
// with Solid (the HTML preview frame, the backlinks).
import { Facet, StateEffect } from "@codemirror/state";

export interface Picture {
  from: number;
  to: number;
  alt: string;
  src: string;
  /** Width asked for in the alt (`alt|300`), if any. */
  width: number | null;
}

export interface NoteHooks {
  /** The note being edited (left out of its own link suggestions). */
  noteId: string;
  /** Notes to link to, newest first. */
  notes(): { id: string; title: string }[];
  /** A wikilink's note, by its title (or id). */
  resolve(target: string): string | null;
  /** A wikilink was clicked (`anchor`: its text on the page). */
  openLink(target: string, anchor: HTMLElement): void;
  openUrl(url: string): void;
  /** A picture was clicked: its menu. */
  pictureMenu(anchor: HTMLElement, p: Picture): void;
  /** "Image" from the insert menu: pick one (inserted where the caret is). */
  pickPicture(): void;
  /** Pictures pasted or dropped, to go in at `at`. */
  addPictures(files: File[], at: number): void;
  /** A table cell's menu (rows, columns, alignment). */
  tableMenu(anchor: HTMLElement, items: { label: string; icon?: string; danger?: boolean; run: () => void }[]): void;
  /** Draw an HTML preview into `dom`; returns how to stop it. */
  preview(dom: HTMLElement, html: string): () => void;
  /** Draw what comes after the text (links to this note) into `dom`. */
  footer(dom: HTMLElement): () => void;
}

export const noteHooks = Facet.define<NoteHooks, NoteHooks>({ combine: (v) => v[0] });

/** Something the page draws changed outside the text (the notes list, a theme): draw again. */
export const redraw = StateEffect.define<null>();
