// Archived chats: put away, not deleted. A list of their own, opened from the
// line under the sidebar's chats (and the palette); restore one (a tap opens
// it too), or delete it for good. Loaded on first use.
import { For, Show, createMemo, createSignal } from "solid-js";
import "../styles/settings.css";
import type { SessionInfo } from "../proto/SessionInfo";
import { deleteSession, setArchived } from "../lib/actions";
import { refreshArchived, state } from "../lib/store";
import { setArchivedOpen } from "../lib/ui";
import { relTime } from "../lib/util";
import { openSession } from "../lib/workspace";
import { sessionLabel } from "./bots";
import { Icon } from "./icons";
import { Modal } from "./primitives";
import { Card, LoadingBlock } from "./settings/kit";

export function ArchivedView() {
  const close = () => setArchivedOpen(false);
  return (
    <Modal onClose={close} title="Archived chats" size="lg" class="archived-view">
      <div class="modal-body">
        <p class="muted archived-lede">Chats you put away. Restore one to the sidebar, or delete it for good.</p>
        <ArchivedList onOpened={close} />
      </div>
    </Modal>
  );
}

/** The list: search, restore (and open), delete for good. `onOpened`: a chat was opened from it. */
export function ArchivedList(props: { onOpened: () => void }) {
  const [q, setQ] = createSignal("");
  const [error, setError] = createSignal<unknown>(null);
  if (!state.archived) void refreshArchived().catch(setError);
  const list = createMemo(() => {
    const needle = q().trim().toLowerCase();
    const all = (state.archived ?? []).filter((s) => !s.private).slice().sort((a, b) => b.updated_at - a.updated_at);
    return needle ? all.filter((s) => (s.title || "").toLowerCase().includes(needle) || s.preview.toLowerCase().includes(needle)) : all;
  });
  const restore = async (s: SessionInfo, open = false) => {
    await setArchived(s.id, false);
    if (open) {
      openSession(s.id, { newPane: true });
      props.onOpened();
    }
  };
  return (
    <Show when={state.archived} fallback={<LoadingBlock error={error()} />}>
      <div class="stack">
        <Show
          when={state.archived!.length}
          fallback={
            <div class="empty-state">
              <span class="empty-icon">
                <Icon name="archive" size={20} />
              </span>
              <p>Nothing archived. Archive a chat from its menu to put it away without deleting it.</p>
            </div>
          }
        >
          <label class="search-field">
            <Icon name="search" size={14} />
            <input class="input" type="search" placeholder="Search archived chats" value={q()} onInput={(e) => setQ(e.currentTarget.value)} aria-label="Search archived chats" />
          </label>
          <Card class="list-card">
            <Show when={list().length} fallback={<p class="muted small pad">No archived chat matches “{q()}”.</p>}>
              <ul class="archived-list">
                <For each={list()}>
                  {(s) => (
                    <li class="archived-row">
                      <button type="button" class="archived-main" onClick={() => void restore(s, true)} title="Restore and open">
                        <span class="archived-title">{sessionLabel(s)}</span>
                        <span class="archived-meta">
                          {relTime(s.updated_at)}
                          <Show when={s.preview}> · {s.preview}</Show>
                        </span>
                      </button>
                      <button type="button" class="btn btn-sm" onClick={() => void restore(s)}>
                        Restore
                      </button>
                      <button type="button" class="icon-btn danger" aria-label={`Delete ${sessionLabel(s)}`} data-tip="Delete for good" onClick={() => void deleteSession(s.id)}>
                        <Icon name="trash" size={15} />
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </Card>
        </Show>
      </div>
    </Show>
  );
}
