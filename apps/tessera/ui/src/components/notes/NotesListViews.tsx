// Note-list rows, menus and filter controls load only when the notes list or search needs them.
import { For, Show, createMemo, createSignal } from "solid-js";
import type { NoteInfo } from "../../proto/NoteInfo";
import { forgetNote, openNote, rememberNote } from "../../lib/notes";
import { api, botById, state } from "../../lib/store";
import { RpcError } from "../../lib/transport";
import { confirmDialog, promptDialog, toastError } from "../../lib/ui";
import { relTime } from "../../lib/util";
import { openSession, type OpenOpts } from "../../lib/workspace";
import { BotAvatar } from "../bots";
import { Icon } from "../icons";
import { Snippet } from "../Snippet";
import { longPress, openMenu, type MenuItem } from "../primitives";
import { noteList, noteAuthor } from "./NotesList";

// ── actions ──────────────────────────────────────────────────────────────

export async function renameNote(id: string) {
  const cur = state.notes[id];
  const title = (await promptDialog({ title: "Rename note", label: "Title", value: cur?.title ?? "", confirmLabel: "Rename" }))?.trim();
  if (!title || title === cur?.title) return;
  // The title travels with the content: save both on the latest version.
  for (let attempt = 0; ; attempt++) {
    try {
      const n = await api.getNote(id);
      rememberNote(await api.saveNote({ id, title, content: n.content, base_version: n.version }));
      return;
    } catch (e) {
      if (e instanceof RpcError && e.code === "conflict" && attempt < 2) continue;
      toastError(e, "Couldn't rename the note");
      return;
    }
  }
}

export async function deleteNote(id: string) {
  const title = state.notes[id]?.title || "this note";
  const ok = await confirmDialog({
    title: "Delete note?",
    body: `“${title}” will be deleted for good, on every device. Its tiles close.`,
    confirmLabel: "Delete",
    danger: true,
  });
  if (!ok) return;
  try {
    await api.deleteNote(id);
    forgetNote(id);
  } catch (e) {
    toastError(e, "Couldn't delete the note");
  }
}

const noteMenu = (n: NoteInfo, onPick: () => void): MenuItem[] => [
  { label: "Open beside", icon: "splitRight", onSelect: () => (openNote(n.id, { newPane: true, zone: "right" }), onPick()) },
  ...(n.session_id && state.sessions[n.session_id]
    ? [{ label: "Open the chat that wrote it", icon: "chat" as const, onSelect: () => (openSession(n.session_id!, { newPane: true }), onPick()) }]
    : []),
  { separator: true },
  { label: "Rename…", icon: "pencil", onSelect: () => void renameNote(n.id) },
  { label: "Delete…", icon: "trash", danger: true, onSelect: () => void deleteNote(n.id) },
];

// ── rows ─────────────────────────────────────────────────────────────────

/** Ctrl/Cmd-click or middle-click opens in a new tile. */
const placement = (e?: MouseEvent): OpenOpts => ({ newPane: !!e && (e.ctrlKey || e.metaKey || e.button === 1) });

export function NoteRow(props: { note: NoteInfo; onPick: () => void; active?: boolean; query?: string }) {
  const n = () => props.note;
  const by = () => noteAuthor(n());
  const lp = longPress((x, y) => openMenu({ x, y }, noteMenu(n(), props.onPick)));
  const open = (e?: MouseEvent) => {
    openNote(n().id, placement(e));
    props.onPick();
  };
  return (
    <li
      class="srow nrow"
      classList={{ active: !!props.active }}
      onContextMenu={(e) => {
        e.preventDefault();
        openMenu({ x: e.clientX, y: e.clientY }, noteMenu(n(), props.onPick));
      }}
      {...lp}
    >
      <button type="button" class="srow-main nrow-main" onClick={(e) => open(e)} onAuxClick={(e) => e.button === 1 && open(e)} title={n().title}>
        <span class="nrow-line">
          <span class="srow-title nrow-title">
            <Snippet text={n().title || "Untitled note"} query={props.query} raw />
          </span>
          <span class="srow-meta nrow-time" title={new Date(n().updated_at).toLocaleString()}>
            {relTime(n().updated_at)}
          </span>
        </span>
        <Show when={n().preview}>
          <span class="nrow-preview">
            <Snippet text={n().preview} query={props.query} />
          </span>
        </Show>
        <span class="nrow-by" classList={{ you: n().by_user }} title={by().title}>
          <Show when={botById(by().botId)} fallback={<Icon name={n().by_user ? "user" : "chat"} size={11} />}>
            <BotAvatar bot={botById(by().botId)} size={13} />
          </Show>
          <span class="nrow-by-label">{by().label}</span>
          <Show when={n().keep_from_agents}>
            <span class="nrow-kept" title="Kept from agents" aria-label="Kept from agents">
              <Icon name="eyeOff" size={11} />
            </span>
          </Show>
        </span>
      </button>
      <button type="button" class="icon-btn sm srow-more" aria-label={`Options for ${n().title || "note"}`} aria-haspopup="menu" onClick={(e) => openMenu(e.currentTarget, noteMenu(n(), props.onPick), "bottom-end")}>
        <Icon name="more" size={15} />
      </button>
    </li>
  );
}

// ── the tab ──────────────────────────────────────────────────────────────

export type NoteFilter = "all" | "mine" | "agents";
const [filter, setFilter] = createSignal<NoteFilter>("all");
const FILTERS: { id: NoteFilter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "mine", label: "Yours" },
  { id: "agents", label: "From chats" },
];

/** The Notes tab: a filter and the list. `focusedNote`: the note in the focused tile. */
export function NotesList(props: { onPick: () => void; focusedNote?: string | null }) {
  const shown = createMemo(() =>
    noteList().filter((n) => (filter() === "mine" ? n.by_user : filter() === "agents" ? !n.by_user : true)),
  );
  return (
    <div class="notes-list">
      <Show
        when={state.notesLoaded}
        fallback={
          <div class="sb-note-empty">
            <Icon name="note" size={16} />
            <p>This server doesn't keep notes yet. Update Tessera on it to use notes.</p>
          </div>
        }
      >
        <Show when={noteList().length}>
          <div class="nl-filter" role="radiogroup" aria-label="Show notes">
            <For each={FILTERS}>
              {(f) => (
                <button type="button" role="radio" class="nl-chip" aria-checked={filter() === f.id} classList={{ on: filter() === f.id }} onClick={() => setFilter(f.id)}>
                  {f.label}
                </button>
              )}
            </For>
          </div>
        </Show>
        <ul class="srows">
          <For each={shown()}>{(n) => <NoteRow note={n} onPick={props.onPick} active={props.focusedNote === n.id} />}</For>
        </ul>
        {/* Notes are started with New (top right), like everything else. */}
        <Show when={!noteList().length}>
          <p class="sb-empty sb-first">No notes yet. Write one with New → Note at the top right, or ask an agent to keep one for you.</p>
        </Show>
        <Show when={noteList().length && !shown().length}>
          <p class="sb-empty">{filter() === "mine" ? "You haven't written a note yet." : "No notes from chats yet."}</p>
        </Show>
      </Show>
    </div>
  );
}
