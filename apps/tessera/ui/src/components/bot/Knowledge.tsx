// A bot's knowledge: notes from the library it knows (`Bot.knowledge`, note
// ids). Pick notes, or add a text / Markdown file: it becomes a note (yours)
// and the bot knows it. Files are read here and never uploaded as files.
import { For, Show, createMemo, createSignal } from "solid-js";
import type { NoteInfo } from "../../proto/NoteInfo";
import { NOTE_MAX, rememberNote } from "../../lib/notes";
import { api, state } from "../../lib/store";
import { toastError } from "../../lib/ui";
import { relTime } from "../../lib/util";
import { Icon, Spinner } from "../icons";
import { noteList } from "../notes/NotesList";

/** Text formats a note can hold as they are. */
const TEXT_EXT = /\.(md|markdown|mdown|txt|text|csv|tsv|json|ya?ml|toml|log|tex|rst|org|adoc)$/i;
export const KNOWLEDGE_ACCEPT = ".md,.markdown,.txt,.text,.csv,.tsv,.json,.yaml,.yml,.toml,.log,.tex,.rst,.org,.adoc,text/plain,text/markdown";

/** Read a text file as a note's content, or say why not. */
async function fileText(f: File): Promise<string> {
  if (!TEXT_EXT.test(f.name) && !f.type.startsWith("text/")) throw new Error(`${f.name}: only text and Markdown files can become notes.`);
  if (f.size > NOTE_MAX) throw new Error(`${f.name} is ${Math.round(f.size / 1024)} KB; a note holds at most ${NOTE_MAX >> 10} KB.`);
  const text = await f.text();
  if (text.includes("\u0000")) throw new Error(`${f.name} isn't a text file.`);
  const ext = f.name.match(/\.([a-z0-9]+)$/i)?.[1]?.toLowerCase();
  // Data files keep their layout in a code block; prose stays prose.
  return ext && /^(csv|tsv|json|ya?ml|toml|log)$/.test(ext) ? "```" + ext + "\n" + text.replace(/\n$/, "") + "\n```\n" : text;
}

/** Make notes from files (titles from the file names); their ids. */
export async function notesFromFiles(files: File[]): Promise<string[]> {
  const ids: string[] = [];
  for (const f of files) {
    try {
      const content = await fileText(f);
      const title = f.name.replace(/\.[^.]+$/, "").replace(/[_-]+/g, " ").trim() || f.name;
      const note = await api.saveNote({ id: null, title, content, base_version: null });
      rememberNote(note);
      ids.push(note.id);
    } catch (e) {
      toastError(e, "Couldn't add the file");
    }
  }
  return ids;
}

export function KnowledgeEditor(props: { ids: string[]; onChange: (ids: string[]) => void; botName: string }) {
  const [picking, setPicking] = createSignal(false);
  const [q, setQ] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  let file!: HTMLInputElement;
  const has = (id: string) => props.ids.includes(id);
  const toggle = (id: string) => props.onChange(has(id) ? props.ids.filter((x) => x !== id) : [...props.ids, id]);
  const matches = createMemo(() => {
    const needle = q().trim().toLowerCase();
    return noteList().filter((n) => !needle || n.title.toLowerCase().includes(needle) || n.preview.toLowerCase().includes(needle));
  });
  const chosen = createMemo(() => props.ids.map((id) => ({ id, note: state.notes[id] as NoteInfo | undefined })));

  const addFiles = async (files: File[]) => {
    if (!files.length) return;
    setBusy(true);
    try {
      const ids = await notesFromFiles(files);
      // The new notes show up in the list above (and in your notes).
      if (ids.length) props.onChange([...props.ids, ...ids.filter((id) => !has(id))]);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Show
      when={state.notesLoaded}
      fallback={<p class="muted small">This server doesn't keep notes yet. Update Tessera on it to give bots knowledge.</p>}
    >
      <div
        class="kn"
        onDragOver={(e) => {
          if (e.dataTransfer?.types.includes("Files")) e.preventDefault();
        }}
        onDrop={(e) => {
          const files = [...(e.dataTransfer?.files ?? [])];
          if (!files.length) return;
          e.preventDefault();
          void addFiles(files);
        }}
      >
        <Show when={chosen().length}>
          <ul class="kn-list">
            <For each={chosen()}>
              {(c) => (
                <li class="kn-item" classList={{ missing: !c.note }} title={c.note?.preview || undefined}>
                  <Icon name="note" size={14} />
                  <span class="kn-title">{c.note ? c.note.title || "Untitled note" : "A deleted note"}</span>
                  <Show when={c.note}>
                    <span class="kn-meta">{relTime(c.note!.updated_at)}</span>
                  </Show>
                  <button type="button" class="icon-btn sm" aria-label={`Remove ${c.note?.title ?? "note"} from ${props.botName}'s knowledge`} onClick={() => toggle(c.id)}>
                    <Icon name="x" size={13} />
                  </button>
                </li>
              )}
            </For>
          </ul>
        </Show>
        <div class="kn-actions">
          <button type="button" class="btn btn-sm" aria-expanded={picking()} onClick={() => setPicking(!picking())}>
            <Icon name="note" size={14} /> {picking() ? "Done" : "Add notes"}
          </button>
          <button type="button" class="btn btn-sm" disabled={busy()} onClick={() => file.click()}>
            <Show when={busy()} fallback={<Icon name="paperclip" size={14} />}>
              <Spinner size={13} />
            </Show>
            Add a file
          </button>
          <input
            ref={file}
            type="file"
            accept={KNOWLEDGE_ACCEPT}
            multiple
            hidden
            onChange={(e) => {
              const files = [...(e.currentTarget.files ?? [])];
              e.currentTarget.value = "";
              void addFiles(files);
            }}
          />
          <span class="muted small kn-hint">or drop text files here</span>
        </div>
        <Show when={picking()}>
          <div class="kn-pick">
            <label class="kn-search">
              <Icon name="search" size={14} />
              <input value={q()} placeholder="Find a note" aria-label="Find a note" onInput={(e) => setQ(e.currentTarget.value)} />
            </label>
            <ul class="kn-options" role="listbox" aria-multiselectable="true" aria-label="Notes">
              <For each={matches()} fallback={<li class="muted small kn-empty">{noteList().length ? "No note matches." : "No notes yet. Add a file, or write notes from the sidebar's Notes tab."}</li>}>
                {(n) => (
                  <li>
                    <button type="button" role="option" class="kn-option" aria-selected={has(n.id)} classList={{ on: has(n.id) }} onClick={() => toggle(n.id)}>
                      <span class="kn-check">
                        <Show when={has(n.id)}>
                          <Icon name="check" size={13} stroke={2.4} />
                        </Show>
                      </span>
                      <span class="kn-opt-text">
                        <span class="kn-opt-title">{n.title || "Untitled note"}</span>
                        <Show when={n.preview}>
                          <span class="kn-opt-prev">{n.preview}</span>
                        </Show>
                      </span>
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </div>
        </Show>
      </div>
    </Show>
  );
}
