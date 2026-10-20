import { For, Show, createSignal } from "solid-js";
import "../styles/bot-editor.css";
import { createChannel } from "../lib/actions";
import { sendMessage, state } from "../lib/store";
import { errorMessage, openBotEditor, setChannelEditor } from "../lib/ui";
import { BotAvatar, botColor } from "./bots";
import { Icon, Spinner } from "./icons";
import { Modal } from "./primitives";

/** New channel: a title, the bots taking part, and optionally what to start on. */
export function ChannelEditor(props: { members: string[] }) {
  const [title, setTitle] = createSignal("");
  const [kickoff, setKickoff] = createSignal("");
  const [members, setMembers] = createSignal<string[]>(props.members.length ? props.members : state.bots.slice(0, 2).map((b) => b.id));
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const close = () => setChannelEditor(null);
  const toggle = (id: string) => setMembers((l) => (l.includes(id) ? l.filter((x) => x !== id) : [...l, id]));

  const submit = async (e: Event) => {
    e.preventDefault();
    setError(null);
    const t = title().trim().replace(/^#/, "");
    if (!t) return setError("Give the channel a name.");
    if (!members().length) return setError("Pick at least one bot.");
    if (state.sessions && Object.values(state.sessions).some((x) => x.info.kind === "channel" && !x.info.bot_id && !x.info.archived && x.info.title.toLowerCase() === t.toLowerCase()))
      return setError(`There's already a #${t}.`);
    setBusy(true);
    try {
      const info = await createChannel(t, members());
      const first = kickoff().trim();
      // The first message gets everyone going (or the @mentioned bots).
      if (first) await sendMessage(info.id, [{ type: "text", text: first }], "auto");
      close();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal onClose={close} title="New channel" size="md" class="channel-editor">
      <form class="modal-body stack" onSubmit={submit}>
        <p class="muted small">A room where bots work together. When you post, everyone replies, or @mention a bot to address just them. Bots can hand the floor to each other, and each keeps its own memory and tools.</p>
        <label class="field">
          <span class="field-label">Name</span>
          <div class="be-handle">
            <span>#</span>
            <input value={title()} placeholder="launch-planning" autofocus onInput={(e) => setTitle(e.currentTarget.value)} aria-label="Channel name" />
          </div>
        </label>
        <div class="field">
          <span class="field-label">Members</span>
          <Show
            when={state.bots.length}
            fallback={
              <button type="button" class="duty-empty" onClick={() => { close(); openBotEditor(); }}>
                <Icon name="bot" size={18} /> <span>No bots yet — <b>create one first</b>.</span>
              </button>
            }
          >
            <div class="member-grid">
              <For each={state.bots}>
                {(b) => (
                  <button
                    type="button"
                    class="member-card"
                    role="checkbox"
                    aria-checked={members().includes(b.id)}
                    classList={{ on: members().includes(b.id) }}
                    style={{ "--bot": botColor(b.color) }}
                    onClick={() => toggle(b.id)}
                  >
                    <BotAvatar bot={b} size={30} />
                    <span class="member-text">
                      <span class="strong">
                        {b.name} <span class="muted small mono">@{b.handle}</span>
                      </span>
                      <span class="muted small member-role">{b.role || "No role yet"}</span>
                    </span>
                    <span class="member-check">
                      <Show when={members().includes(b.id)}>
                        <Icon name="check" size={14} stroke={2.4} />
                      </Show>
                    </span>
                  </button>
                )}
              </For>
              <button
                type="button"
                class="member-card member-new"
                onClick={() => {
                  close();
                  openBotEditor();
                }}
              >
                <span class="member-new-icon">
                  <Icon name="plus" size={16} />
                </span>
                <span class="member-text">
                  <span class="strong">New bot</span>
                  <span class="muted small member-role">Start from a template</span>
                </span>
              </button>
            </div>
          </Show>
        </div>
        <label class="field">
          <span class="field-label">Start with (optional)</span>
          <textarea
            class="input"
            rows={2}
            value={kickoff()}
            placeholder="What should they work on first? e.g. Plan Thursday's launch: @forge the build, @nova the schedule."
            onInput={(e) => setKickoff(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) void submit(e);
            }}
          />
          <span class="field-help">Posted as your first message when the channel opens.</span>
        </label>
        <Show when={error()}>
          <div class="form-error">{error()}</div>
        </Show>
        <div class="form-actions">
          <button type="button" class="btn btn-ghost" onClick={close}>
            Cancel
          </button>
          <button type="submit" class="btn btn-primary" disabled={busy() || !state.bots.length}>
            <Show when={busy()} fallback={<Icon name="hash" size={15} />}>
              <Spinner size={14} />
            </Show>
            Create channel
          </button>
        </div>
      </form>
    </Modal>
  );
}
