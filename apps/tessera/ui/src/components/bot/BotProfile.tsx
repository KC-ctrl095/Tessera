// A bot's profile: who it is, what it knows (notes, memory, your profile),
// its duties, its recent chats and channels, who it talks to, and how to
// start. "Edit" switches the same modal to the editor.
import { For, Show, createMemo } from "solid-js";
import "../../styles/bot-editor.css";
import type { Bot } from "../../proto/Bot";
import { permissionInfo } from "../../lib/actions";
import { openNote } from "../../lib/notes";
import { botById, isBusy, modelLabel, state } from "../../lib/store";
import { openChannelEditor, setBotEditor } from "../../lib/ui";
import { plural, relTime } from "../../lib/util";
import { focusComposer, openBot, openSession, setDraft } from "../../lib/workspace";
import { BotAvatar, botColor, botKnowledge, botSuggestions, sessionLabel } from "../bots";
import { TOOL_PRESETS } from "./presets";
import { Icon, Tessellate } from "../icons";
import { describeSchedule } from "../Jobs";
import { Modal } from "../primitives";
import "../../styles/botkit.css";

const RECENT = 6;

export function BotProfile(props: { id: string; onEdit: () => void }) {
  const b = (): Bot | undefined => botById(props.id);
  const close = () => setBotEditor(null);
  const notes = createMemo(() => botKnowledge(b()).map((id) => ({ id, note: state.notes[id] })));
  const duties = createMemo(() => state.jobs.filter((j) => j.bot_id === props.id).sort((x, y) => (x.next_run ?? Infinity) - (y.next_run ?? Infinity)));
  const chats = createMemo(() =>
    Object.values(state.sessions)
      .map((s) => s.info)
      .filter((i) => i.kind === "chat" && i.bot_id === props.id && !i.archived)
      .sort((x, y) => y.updated_at - x.updated_at),
  );
  const channels = createMemo(() =>
    Object.values(state.sessions)
      .map((s) => s.info)
      .filter((i) => i.kind === "channel" && !i.archived && (i.members.includes(props.id) || i.bot_id === props.id))
      .sort((x, y) => Number(!!x.bot_id) - Number(!!y.bot_id) || y.updated_at - x.updated_at),
  );
  const talksTo = createMemo(() => {
    const cm = b()?.can_message ?? [];
    if (cm.includes("*")) return "any";
    return cm.map((h) => state.bots.find((x) => x.handle === h || x.id === h)).filter((x): x is Bot => !!x);
  });

  const chat = (fresh: boolean, text?: string) => {
    close();
    const pane = openBot(props.id, { newPane: true, fresh });
    if (pane && text) {
      setDraft(pane, text);
      focusComposer(pane);
    }
  };

  return (
    <Show when={b()} fallback={<Modal onClose={close} title="Bot not found" size="md"><p class="modal-body muted">This bot was deleted.</p></Modal>}>
      {(bot) => (
        <Modal onClose={close} title={bot().name} size="lg" class="bot-profile" hideHeader>
          <div class="bp" style={{ "--bot": botColor(bot().color) }}>
            <header class="bp-head">
              <div class="bp-glow" />
              <BotAvatar bot={bot()} size={64} ring />
              <div class="bp-id">
                <h2 class="bp-name">{bot().name}</h2>
                <div class="bp-handle">@{bot().handle}</div>
                <p class="bp-role">{bot().role || "No role yet"}</p>
                <div class="bp-badges">
                  <span class="badge" classList={{ "badge-yolo": bot().permission === "auto" }}>
                    <Icon name={permissionInfo(bot().permission).icon} size={11} /> {permissionInfo(bot().permission).label}
                  </span>
                  <span class="badge">
                    <Icon name="wrench" size={11} /> {TOOL_PRESETS.find((t) => t.value === bot().tools)?.label ?? bot().tools}
                  </span>
                  <span class="badge">
                    <Icon name="sparkles" size={11} /> {bot().model ? modelLabel(bot().model) : "Default model"}
                  </span>
                </div>
              </div>
              <button type="button" class="icon-btn bp-x" aria-label="Close" data-tip="Close" onClick={close}>
                <Icon name="x" />
              </button>
            </header>

            <div class="bp-actions">
              <button type="button" class="btn btn-primary" onClick={() => chat(false)}>
                <Icon name="chat" size={15} /> {chats().length ? "Continue chatting" : "Start chatting"}
              </button>
              <Show when={chats().length}>
                <button type="button" class="btn" onClick={() => chat(true)}>
                  <Icon name="compose" size={15} /> New chat
                </button>
              </Show>
              <button type="button" class="btn" onClick={props.onEdit}>
                <Icon name="pencil" size={15} /> Edit
              </button>
              <button
                type="button"
                class="btn"
                onClick={() => {
                  close();
                  openChannelEditor([props.id]);
                }}
              >
                <Icon name="hash" size={15} /> New channel with {bot().name}
              </button>
            </div>

            <div class="bp-grid">
              <section class="bp-sec">
                <h3 class="be-h">
                  <Icon name="brain" size={16} /> What {bot().name} knows
                </h3>
                <ul class="bp-facts">
                  <li>
                    <Icon name="bookmark" size={14} />
                    <span>{bot().own_memory ? "Keeps its own memory, apart from yours." : "Shares your memory."}</span>
                  </li>
                  <li>
                    <Icon name="user" size={14} />
                    <span>{bot().share_user_profile ? "Knows your profile (what Tessera knows about you)." : "Doesn't see your profile."}</span>
                  </li>
                  <li>
                    <Icon name="search" size={14} />
                    <span>{bot().own_memory ? "Can look back through its own chats and channels." : "Can look back through your chats."}</span>
                  </li>
                </ul>
                <Show
                  when={notes().length}
                  fallback={
                    <button type="button" class="duty-empty" onClick={props.onEdit}>
                      <Icon name="note" size={18} />
                      <span>
                        No knowledge yet. <b>Add notes or files</b> it should know.
                      </span>
                    </button>
                  }
                >
                  <ul class="bp-list">
                    <For each={notes()}>
                      {(n) => (
                        <li>
                          <button
                            type="button"
                            class="bp-row"
                            disabled={!n.note}
                            onClick={() => {
                              close();
                              openNote(n.id, { newPane: true });
                            }}
                          >
                            <Icon name="note" size={14} />
                            <span class="bp-row-title">{n.note ? n.note.title || "Untitled note" : "A deleted note"}</span>
                            <Show when={n.note}>
                              <span class="bp-row-meta">{relTime(n.note!.updated_at)}</span>
                            </Show>
                          </button>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
              </section>

              <section class="bp-sec">
                <h3 class="be-h">
                  <Icon name="clock" size={16} /> Duties
                </h3>
                <Show
                  when={duties().length}
                  fallback={
                    <button type="button" class="duty-empty" onClick={props.onEdit}>
                      <Icon name="calendar" size={18} />
                      <span>
                        No duties yet. <b>Every weekday at 8:00: brief me on…</b>
                      </span>
                    </button>
                  }
                >
                  <ul class="bp-list">
                    <For each={duties()}>
                      {(j) => (
                        <li>
                          <button
                            type="button"
                            class="bp-row"
                            classList={{ paused: !j.enabled }}
                            onClick={() => {
                              if (j.last_session) {
                                close();
                                openSession(j.last_session, { newPane: true });
                              } else props.onEdit();
                            }}
                            title={j.prompt}
                          >
                            <Icon name="clock" size={14} />
                            <span class="bp-row-title">{j.name}</span>
                            <span class="bp-row-meta">{!j.enabled ? "paused" : j.next_run ? `${describeSchedule(j.schedule)} · next ${relTime(j.next_run)}` : describeSchedule(j.schedule)}</span>
                          </button>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
              </section>

              <section class="bp-sec">
                <h3 class="be-h">
                  <Icon name="chat" size={16} /> Recent chats
                </h3>
                <Show when={chats().length || channels().length} fallback={<p class="muted small">No chats yet.</p>}>
                  <ul class="bp-list">
                    <For each={chats().slice(0, RECENT)}>
                      {(i) => (
                        <li>
                          <button
                            type="button"
                            class="bp-row"
                            onClick={() => {
                              close();
                              openSession(i.id, { newPane: true });
                            }}
                          >
                            <Icon name="chat" size={14} />
                            <span class="bp-row-title">{sessionLabel(i)}</span>
                            <Show when={isBusy(i)} fallback={<span class="bp-row-meta">{relTime(i.updated_at)}</span>}>
                              <Tessellate size={8} label="Working" />
                            </Show>
                          </button>
                        </li>
                      )}
                    </For>
                    <For each={channels()}>
                      {(i) => (
                        <li>
                          <button
                            type="button"
                            class="bp-row"
                            onClick={() => {
                              close();
                              openSession(i.id, { newPane: true });
                            }}
                          >
                            <Icon name={i.bot_id ? "lock" : "hash"} size={14} />
                            <span class="bp-row-title">{sessionLabel(i)}</span>
                            <span class="bp-row-meta">{relTime(i.updated_at)}</span>
                          </button>
                        </li>
                      )}
                    </For>
                  </ul>
                  <Show when={chats().length > RECENT}>
                    <p class="muted small">{plural(chats().length - RECENT, "older chat", "older chats")} in the sidebar.</p>
                  </Show>
                </Show>
              </section>

              <section class="bp-sec">
                <h3 class="be-h">
                  <Icon name="sparkles" size={16} /> Try asking
                </h3>
                <div class="bp-starters">
                  <For each={botSuggestions(bot())}>
                    {(text) => (
                      <button type="button" class="suggestion" onClick={() => chat(true, text)}>
                        <span>{text}</span>
                      </button>
                    )}
                  </For>
                </div>
                <Show when={talksTo() === "any" || (talksTo() as Bot[]).length}>
                  <p class="muted small bp-talks">
                    <Icon name="at" size={13} />{" "}
                    {talksTo() === "any" ? "Can message any bot privately." : `Can message ${(talksTo() as Bot[]).map((x) => x.name).join(", ")} privately.`}
                  </p>
                </Show>
              </section>
            </div>
          </div>
        </Modal>
      )}
    </Show>
  );
}
