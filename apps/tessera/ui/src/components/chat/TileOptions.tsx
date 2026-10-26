// One compact, flat menu for the chat. All capabilities stay directly reachable.
import { For, Show, Suspense, lazy, onMount, onCleanup } from "solid-js";
import {
  PERMISSIONS,
  chooseCwd,
  compactSession,
  fastAvailable,
  forkAndOpen,
  paneSettings,
  replaceWithNewChat,
  requestClosePane,
  setArchived,
  setPaneFast,
  setPanePermission,
  setPaneSubagents,
  setPinned,
} from "../../lib/actions";
import { daybreakAvailable, daybreakOffer, setPaneDaybreak } from "../../lib/daybreak";
import { botById, isBotDm, isBusy, isRoom, modelLabel, state, type SessionState } from "../../lib/store";
import { openBotEditor } from "../../lib/ui";
import { openBotProfile } from "../BotEditor";
import { fmtCost, fmtTokens, tildify, touchUi } from "../../lib/util";
import { isPrivatePane, toggleMaximize, workspace } from "../../lib/workspace";
import { Icon, type IconName } from "../icons";
import "../../styles/daybreak.css";
import { ALT } from "../TopBar";
import { ContextRing } from "./Pickers";
import { Toggle } from "../primitives";

import { paneAction } from "../../lib/bus";
/** This chat's helper limits: loaded the first time the options show them. */
const ChatHelperLimits = lazy(() => import("./HelperLimits").then((m) => ({ default: m.ChatHelperLimits })));

export function TileOptions(props: {
  paneId: string;
  session: SessionState | undefined;
  onClose: () => void;
  onRename: () => void;
  onMembers: () => void;
  focus?: "permission" | "context" | "fast";
  /** Phones: the model, which has no chip in their one-line header. */
  model?: { label: string; detail?: string; fast?: boolean; onChange: () => void };
}) {
  const settings = () => paneSettings(props.paneId);
  const info = () => props.session?.info;
  const channel = () => info()?.kind === "channel";
  const dm = () => isBotDm(info());
  const room = () => isRoom(info());
  const bot = () => (channel() ? undefined : botById(info()?.bot_id ?? draftBot()));
  const draftBot = () => {
    const c = workspace.panes[props.paneId]?.content;
    return c?.type === "chat" && !c.sessionId ? c.draft?.botId : undefined;
  };
  const tiled = () => workspace.mode() === "tile";
  const isMax = () => workspace.maximized() === props.paneId;
  const ctx = () => info()?.context;
  const ctxPct = () => (ctx()?.window ? Math.min(100, Math.round((ctx()!.tokens / ctx()!.window) * 100)) : 0);
  const act = (fn: () => void) => () => {
    props.onClose();
    fn();
  };
  const priv = () => isPrivatePane(props.paneId);
  const perm = () => PERMISSIONS.find((p) => p.value === settings().permission) ?? PERMISSIONS[0];

  const fast = () => !channel() && fastAvailable(props.paneId);
  const helpers = () => !channel() && !priv();
  const length = () => !!info() && !channel() && !!ctx();
  const branch = () => !!info() && !priv() && !channel();
  const pin = () => !!info() && !priv();

  let dialog!: HTMLDivElement;
  onMount(() => {
    // A lazy arrival changes the sheet's height. Finish only its entrance so
    // the controls are hit where they are drawn as soon as they appear.
    for (const animation of dialog.closest(".popover")?.getAnimations() ?? []) {
      if (animation instanceof CSSAnimation && animation.animationName === "sheet-in") animation.finish();
    }
    const frame = requestAnimationFrame(() => {
      if (!dialog.isConnected) return;
      const section = dialog.querySelector<HTMLElement>("[data-focus]");
      const target = (section ?? dialog).querySelector<HTMLElement>("button:not(:disabled), input, select") ?? section ?? dialog;
      if (!target.matches("button, input, select")) target.tabIndex = -1;
      target.focus({ preventScroll: true });
      section?.scrollIntoView({ block: "nearest" });
    });
    onCleanup(() => cancelAnimationFrame(frame));
  });

  return (
    <div ref={dialog} class="tile-opts" role="dialog" aria-label="Chat options">
      <Show when={props.model}>
        {(m) => (
          <section class="opt-sec opt-row">
            <button type="button" class="opt-line opt-model" onClick={act(m().onChange)}>
              <Icon name="cpu" size={15} />
              <span class="opt-line-label">Model</span>
              <span class="opt-line-value">
                {m().label}
                <Show when={m().detail}>
                  {" · "}
                  <span class="opt-model-think">{m().detail}</span>
                </Show>
              </span>
              <Show when={m().fast}><Icon name="fast" size={12} class="fast-mark" aria-label="Fast mode" /></Show>
              <span class="opt-line-action">Change</span>
            </button>
          </section>
        )}
      </Show>
      <Show when={!channel() && !dm() && !priv()}>
        <section class="opt-sec" data-focus={props.focus === "permission" ? "" : undefined}>
          <div class="opt-head">
            <Icon name="shield" size={13} />
            <span>How much freedom</span>
          </div>
          <div class="perm-seg" role="radiogroup" aria-label="How much freedom">
            <For each={PERMISSIONS}>
              {(p) => (
                <button
                  type="button"
                  role="radio"
                  aria-checked={settings().permission === p.value}
                  classList={{ on: settings().permission === p.value, yolo: p.value === "auto" }}
                  title={p.desc}
                  onClick={() => void setPanePermission(props.paneId, p.value)}
                >
                  <Icon name={p.icon} size={14} stroke={p.value === "auto" ? 2 : undefined} />
                  <span>{p.label}</span>
                </button>
              )}
            </For>
          </div>
          <p class="opt-hint" classList={{ yolo: settings().permission === "auto" }}>
            {perm().short}.
          </p>
        </section>
      </Show>

      <Show when={!dm()}>
        <section class="opt-sec opt-row">
          <Show when={!priv()}>
            <button type="button" class="opt-line" onClick={act(() => void chooseCwd(props.paneId))}>
              <Icon name="folder" size={15} />
              <span class="opt-line-label">Folder</span>
              <span class="opt-line-value mono">{settings().cwd ? tildify(settings().cwd, state.server?.home) : "Server default"}</span>
              <span class="opt-line-action">Change</span>
            </button>
          </Show>
          <div class="opt-more-body">
              <Show when={fast()}>
                <div class="opt-line static" data-focus={props.focus === "fast" ? "" : undefined}>
                  <Icon name="fast" size={15} />
                  <span class="opt-line-label">Fast mode</span>
                  <span class="opt-line-value">Quicker, uses more credits</span>
                  <Toggle checked={settings().fast} onChange={(v) => void setPaneFast(props.paneId, v)} label="Fast mode" />
                </div>
              </Show>
              {/* A saved opt-in this model can't confirm offers only the way back (below). */}
              <Show when={!channel() && !(settings().daybreak && !daybreakAvailable(props.paneId)) && daybreakOffer(props.paneId)}>
                {(offer) => (
                  <>
                    <div class="opt-line static daybreak-option">
                      <Icon name="shield" size={15} />
                      <span class="opt-line-label">Daybreak Blue</span>
                      <span class="opt-line-value">{settings().daybreak ? "On" : "Off"}</span>
                      <Toggle checked={settings().daybreak} onChange={(v) => void setPaneDaybreak(props.paneId, v)} label="Daybreak Blue" />
                    </div>
                    <p class="opt-hint daybreak-hint">
                      {(() => {
                        const o = offer();
                        return o.here
                          ? "OpenAI’s program for defensive security work. Access comes from OpenAI."
                          : `Your account doesn’t offer it on ${modelLabel(settings().model)}. Turning it on switches this chat to ${o.target.name || o.target.id}.`;
                      })()}
                    </p>
                  </>
                )}
              </Show>
              <Show when={!channel() && settings().daybreak && !daybreakAvailable(props.paneId)}>
                <button class="opt-line daybreak-recovery" onClick={() => void setPaneDaybreak(props.paneId, false)}>
                  <Icon name="shieldCheck" size={15} />
                  <span class="opt-line-label">Use standard safeguards</span>
                </button>
                <p class="opt-hint daybreak-hint">OpenAI could not confirm this chat’s program access. This does not retry a stopped request.</p>
              </Show>
              <Show when={helpers()}>
                <div class="opt-line static">
                  <Icon name="users" size={15} />
                  <span class="opt-line-label">Helpers</span>
                  <span class="opt-line-value">{settings().subagents ? "May start helpers" : "Works alone"}</span>
                  <Toggle checked={settings().subagents} onChange={(v) => void setPaneSubagents(props.paneId, v)} label="Helpers" />
                </div>
                <Show when={settings().subagents && info()?.kind !== "subagent" && info()}>
                  {(i) => (
                    <Suspense>
                      <ChatHelperLimits session={i()} />
                    </Suspense>
                  )}
                </Show>
              </Show>
              <Show when={length()}>
                <div class="opt-line static" data-focus={props.focus === "context" ? "" : undefined}>
                  <ContextRing tokens={ctx()!.tokens} window={ctx()!.window} size={15} />
                  <span class="opt-line-label">Chat length</span>
                  <span class="opt-line-value">
                    {ctxPct()}% of {fmtTokens(ctx()!.window)} · {ctx()!.source === "reported" ? "reported" : "estimated"}
                    <Show when={ctx()!.compactions}> · summarized {ctx()!.compactions}×</Show>
                    <Show when={fmtCost(info()!.usage?.cost_usd)}>
                      <span class="opt-cost" data-tip="Estimated cost of this chat so far">
                        {" · "}
                        {fmtCost(info()!.usage?.cost_usd)}
                      </span>
                    </Show>
                  </span>
                  <button type="button" class="opt-line-action link-btn" disabled={isBusy(info())} onClick={act(() => void compactSession(info()!.id))} data-tip="Summarize older messages to make room">
                    Summarize
                  </button>
                </div>
              </Show>
              <Show when={branch()}>
                <button type="button" class="opt-line" onClick={act(() => void forkAndOpen(info()!.id))}>
                  <Icon name="fork" size={15} />
                  <span class="opt-line-label">Branch into a new chat</span>
                  <span class="opt-line-value">A copy to try something else</span>
                </button>
              </Show>
              <Show when={pin()}>
                <button type="button" class="opt-line" onClick={act(() => void setPinned(info()!.id, !info()!.pinned))}>
                  <Icon name="pin" size={15} />
                  <span class="opt-line-label">{info()!.pinned ? "Unpin" : "Pin to the top"}</span>
                  <span class="opt-line-value">{info()!.pinned ? "It goes back among the others" : "Of the chat list"}</span>
                </button>
              </Show>
            </div>
        </section>
      </Show>

      <section class="opt-sec opt-acts">
        <Show when={info() || bot()}>
          <div class="acts-row" role="group" aria-label="Chat">
            <Show when={info()}><ActBtn icon="search" label="Find in chat" onClick={act(() => paneAction(props.paneId, "find"))} /></Show>
            <ActBtn icon="compose" label="New chat here" tip="Replace this chat with a fresh one, in this tile" onClick={act(() => void replaceWithNewChat(props.paneId))} />
            <Show when={info() && !priv()}>
              <ActBtn icon="pencil" label="Rename" onClick={act(props.onRename)} />
              <ActBtn icon="archive" label="Archive" onClick={act(() => void setArchived(info()!.id, true))} />
            </Show>
          </div>
        </Show>
        <div class="acts-row" role="group" aria-label="Tile">
          <Show when={tiled()}>
            <ActBtn icon={isMax() ? "minimize" : "maximize"} label={isMax() ? "Restore" : "Maximize"} onClick={act(() => toggleMaximize(props.paneId))} kbd={`${ALT} F`} />
          </Show>
          <ActBtn icon="x" label={priv() ? "Close & delete" : "Close"} danger onClick={act(() => void requestClosePane(props.paneId))} kbd={`${ALT} W`} />
        </div>
        <Show when={room() || bot() || (info() && priv())}>
          <div class="acts-row" role="group" aria-label={room() ? "Channel" : bot() ? "Bot" : "Private chat"}>
            <Show when={room()}>
              <ActBtn icon="users" label="Members" onClick={act(props.onMembers)} />
            </Show>
            <Show when={bot()}>
              <ActBtn icon="user" label="Profile" onClick={act(() => openBotProfile(bot()!.id))} />
              <ActBtn icon="bot" label={`Edit ${bot()!.name}`} onClick={act(() => openBotEditor(bot()!.id))} />
            </Show>
            <Show when={info() && priv()}>
              <ActBtn icon="trash" label="Delete for good" danger onClick={act(() => void requestClosePane(props.paneId))} />
            </Show>
          </div>
        </Show>
      </section>
    </div>
  );
}

function ActBtn(props: { icon: IconName; label: string; onClick: () => void; danger?: boolean; kbd?: string; tip?: string }) {
  return (
    <button type="button" class="act-btn" classList={{ danger: !!props.danger }} onClick={props.onClick} data-tip={props.tip ?? (props.kbd && !touchUi() ? props.label : undefined)} data-kbd={touchUi() ? undefined : props.kbd}>
      <Icon name={props.icon} size={15} />
      <span>{props.label}</span>
    </button>
  );
}
