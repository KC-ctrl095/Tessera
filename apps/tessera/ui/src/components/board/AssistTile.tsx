// Snap assist: the empty space a snap layout left over (Windows-style). It
// offers what could fill it: recent chats and notes as cards, or something
// new. Picking one puts it right here; "Leave empty" (or Esc) closes the
// empty spaces and the tiles around grow back. It never leaves this device.
import { For, Show, createMemo, onCleanup, onMount } from "solid-js";
import { openNote, newNote } from "../../lib/notes";
import { botById, isBotDm, state } from "../../lib/store";
import { relTime } from "../../lib/util";
import { closeAssists, focusComposer, focusPane, openSession, paneShowing, setPaneContent, workspace } from "../../lib/workspace";
import { BotAvatar, sessionLabel } from "../bots";
import { Icon } from "../icons";

export function AssistTile(props: { paneId: string }) {
  const here = () => workspace.paneWorkspace(props.paneId);
  const chats = createMemo(() =>
    Object.values(state.sessions)
      .map((s) => s.info)
      .filter((i) => (i.kind === "chat" || i.kind === "channel") && !i.archived && !i.private && !i.internal && !isBotDm(i))
      .filter((i) => {
        const p = paneShowing(i.id);
        return !p || workspace.paneWorkspace(p) !== here();
      })
      .sort((a, b) => b.updated_at - a.updated_at)
      .slice(0, 6),
  );
  const notes = createMemo(() => Object.values(state.notes).sort((a, b) => b.updated_at - a.updated_at).slice(0, 3));

  // Esc leaves the spaces empty (not while typing somewhere).
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      if ((document.activeElement as HTMLElement | null)?.closest?.("input, textarea, [contenteditable], .popover, .modal-backdrop")) return;
      if (workspace.paneWorkspace(props.paneId) !== workspace.active()) return;
      e.preventDefault();
      closeAssists();
    };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  const put = (fn: () => void) => () => {
    fn();
    focusPane(props.paneId);
  };
  const newChatHere = () => {
    setPaneContent(props.paneId, { type: "chat", sessionId: null });
    focusPane(props.paneId);
    focusComposer(props.paneId);
  };

  return (
    <div class="board assist-tile" role="region" aria-label="Fill this space">
      <header class="pane-head board-head assist-head">
        <div class="head-title">
          <span class="board-kind" aria-hidden="true">
            <Icon name="addTile" size={15} />
          </span>
          <div class="title-stack">
            <span class="title-static">Fill this space</span>
            <span class="head-sub board-sub">Pick something to put here</span>
          </div>
        </div>
        <div class="head-controls">
          <button type="button" class="btn btn-xs btn-ghost" onClick={() => closeAssists()} data-tip="The tiles around grow back" data-kbd="Esc">
            Leave empty
          </button>
        </div>
      </header>
      <div class="board-body assist-body">
        <div class="assist-new" role="group" aria-label="Something new">
          <button type="button" class="assist-btn" onClick={newChatHere}>
            <Icon name="chat" size={15} /> New chat
          </button>
          <button type="button" class="assist-btn" onClick={put(() => setPaneContent(props.paneId, { type: "draft", kind: "url" }))}>
            <Icon name="globe" size={15} /> Web view
          </button>
          <button
            type="button"
            class="assist-btn"
            onClick={() =>
              void newNote({ pane: props.paneId }).then((id) => {
                if (id) focusPane(id);
              })
            }
          >
            <Icon name="note" size={15} /> Note
          </button>
        </div>
        <Show when={chats().length}>
          <h4 class="assist-head-line">Recent chats</h4>
          <div class="assist-grid">
            <For each={chats()}>
              {(i) => {
                const bot = () => (i.kind === "chat" ? botById(i.bot_id) : undefined);
                return (
                  <button type="button" class="assist-card" onClick={put(() => openSession(i.id, { pane: props.paneId }))}>
                    <span class="assist-card-top">
                      <Show when={bot()} fallback={<Icon name={i.kind === "channel" ? "hash" : "chat"} size={13} />}>
                        <BotAvatar bot={bot()} size={16} />
                      </Show>
                      <span class="assist-card-title">{sessionLabel(i)}</span>
                    </span>
                    <span class="assist-card-text">{i.preview || "No messages yet"}</span>
                    <span class="assist-card-when">{relTime(i.updated_at)}</span>
                  </button>
                );
              }}
            </For>
          </div>
        </Show>
        <Show when={notes().length}>
          <h4 class="assist-head-line">Notes</h4>
          <div class="assist-grid">
            <For each={notes()}>
              {(n) => (
                <button type="button" class="assist-card" onClick={put(() => void openNote(n.id, { pane: props.paneId }))}>
                  <span class="assist-card-top">
                    <Icon name="note" size={13} />
                    <span class="assist-card-title">{n.title}</span>
                  </span>
                  <span class="assist-card-text">{n.preview || "Empty note"}</span>
                  <span class="assist-card-when">{relTime(n.updated_at)}</span>
                </button>
              )}
            </For>
          </div>
        </Show>
      </div>
    </div>
  );
}
