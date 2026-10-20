// A note an agent wrote, in the chat transcript: a compact card. One tap
// opens it in a tile; an agent's note only gets a tile by itself when the
// user asked to see it (`show: true`). The card of a `create` stands on its
// own (never folded into the steps); later edits are steps with an "Open".
import { Show } from "solid-js";
import { openNote } from "../../lib/notes";
import { plainText, state, type SessionState } from "../../lib/store";
import { argsObject, makesNote } from "../../lib/tools";
import { paneShowingNote, workspace } from "../../lib/workspace";
import { Icon, Tessellate } from "../icons";

export const isNoteTool = (name: string) => /^note$/i.test(name);
export { makesNote };

export interface NoteRef {
  id: string;
  title: string;
  version?: number;
  action?: string;
}

/** The note a `note` result is about (`details.note`). */
export function noteOfCall(details: unknown): NoteRef | null {
  const n = (details as { note?: { id?: unknown; title?: unknown; version?: unknown; action?: unknown } } | null | undefined)?.note;
  if (!n || typeof n.id !== "string") return null;
  return {
    id: n.id,
    title: typeof n.title === "string" ? n.title : "",
    version: typeof n.version === "number" ? n.version : undefined,
    action: typeof n.action === "string" ? n.action : undefined,
  };
}

const DONE: Record<string, string> = {
  create: "New note",
  update: "Note updated",
  edit: "Note edited",
  image: "Picture added",
  read: "Note",
};

/** Whether the note's tile is in the mosaic on screen. */
function noteOnScreen(id: string): boolean {
  const p = paneShowingNote(id);
  return !!p && workspace.paneWorkspace(p) === workspace.active();
}

export function NoteCallCard(props: { session: SessionState; callId: string; args: unknown; preparing?: boolean }) {
  const result = () => props.session.results[props.callId];
  const ref = () => noteOfCall(result()?.details);
  const title = () => state.notes[ref()?.id ?? ""]?.title ?? (ref()?.title || (argsObject(props.args)?.title as string | undefined) || "Note");
  const failed = () => !!result()?.is_error;
  const onScreen = () => !!ref() && noteOnScreen(ref()!.id);
  const open = () => {
    const r = ref();
    if (r) openNote(r.id);
  };
  return (
    <Show
      when={!failed()}
      fallback={
        <div class="board-card note-card gone" role="note">
          <span class="board-card-art">
            <Icon name="note" size={20} />
          </span>
          <span class="board-card-text">
            <span class="board-card-title">{title()}</span>
            <span class="board-card-meta">{plainText(result()!.content).split("\n")[0].slice(0, 160) || "The note couldn't be written"}</span>
          </span>
        </div>
      }
    >
      <button type="button" class="board-card note-card" disabled={!ref()} onClick={open} aria-label={ref() ? `Open note “${title()}”` : title()}>
        <span class="board-card-art" aria-hidden="true">
          <Icon name="note" size={20} />
        </span>
        <span class="board-card-text">
          <span class="board-card-title">{title()}</span>
          <span class="board-card-meta">
            <Show when={result()} fallback={<><Tessellate size={7} /><span>{props.preparing ? "Writing the note…" : "Saving it…"}</span></>}>
              <span>{DONE[ref()?.action ?? "create"] ?? "Note"}</span>
              <Show when={onScreen()}>
                <span class="dot-sep">·</span>
                <span>On screen</span>
              </Show>
            </Show>
          </span>
        </span>
        <Show when={ref()}>
          <span class="btn btn-sm board-card-open" aria-hidden="true">
            {onScreen() ? "Show" : "Open"}
          </span>
        </Show>
      </button>
    </Show>
  );
}

/** "Open" on a step that changed a note (an edit, an append, a picture). */
export function NoteOpenButton(props: { details: unknown }) {
  const ref = () => noteOfCall(props.details);
  return (
    <Show when={ref() && !noteOnScreen(ref()!.id)}>
      <button type="button" class="tool-show" onClick={() => openNote(ref()!.id)}>
        <Icon name="note" size={12} /> Open
      </button>
    </Show>
  );
}
