import { For, Show, createMemo } from "solid-js";
import { dropHint } from "../lib/dnd";
import { state } from "../lib/store";
import { setDrawerOpen } from "../lib/ui";
import { relTime, touchUi } from "../lib/util";
import { openBot, openSession, paneShowing, workspace } from "../lib/workspace";
import { AvatarStack, BotAvatar, botBusy, sessionLabel, botColor } from "./bots";
import { Icon, Tessellate } from "./icons";
import { MosaicArt } from "./Mosaic";
import { NewTileButton } from "./NewMenu";
import { spawnTile } from "../lib/spawn";
import { MOD } from "./TopBar";

/** Shown by an empty mosaic: start something here. */
export function Launcher() {
  const recent = createMemo(() =>
    Object.values(state.sessions)
      .map((s) => s.info)
      .filter((i) => (i.kind === "chat" || i.kind === "channel") && !i.archived && !i.private && !paneShowing(i.id))
      .sort((a, b) => b.updated_at - a.updated_at)
      .slice(0, 5),
  );
  const over = () => {
    const h = dropHint();
    return !!h && "launcher" in h;
  };

  return (
    <div class="launcher" classList={{ over: over() }} data-drop="launcher">
      {/* Phones have no top bar: the sidebar and New are one tap away here too, where tiles have them. */}
      <Show when={workspace.mode() === "mobile"}>
        <button type="button" class="icon-btn launcher-menu" aria-label="Open sidebar" onClick={() => setDrawerOpen(true)}>
          <Icon name="menu" />
        </button>
        <span class="launcher-new">
          <NewTileButton />
        </span>
      </Show>
      <div class="launcher-inner">
        <div class="launcher-head">
          <MosaicArt class="launcher-mosaic" seed={workspace.active() * 17 + 5} width={560} height={110} cell={12} focus={[0.5, 0.7]} />
          <p class="inscription">Mosaic {workspace.active() + 1}</p>
          <h2 class="serif">An empty mosaic</h2>
          {/* Where the word is introduced: what a mosaic is, in one line. */}
          <p>A mosaic is a screen of tiles. Start a chat here, or drop one in from the sidebar.</p>
        </div>

        <div class="launcher-actions">
          <button type="button" class="launch-card primary" onClick={() => spawnTile("chat")} data-tip="New chat" data-kbd={touchUi() ? undefined : `${MOD} N`}>
            <span class="launch-icon">
              <Icon name="compose" size={18} />
            </span>
            <span class="launch-text">
              <span class="launch-title">New chat</span>
              <span class="launch-sub">Ask anything, run tasks</span>
            </span>
          </button>
        </div>

        <Show when={state.bots.length}>
          <section class="launcher-sec">
            <h3>Your bots</h3>
            <div class="launcher-bots">
              <For each={state.bots}>
                {(b) => (
                  <button type="button" class="bot-tile" style={{ "--bot": botColor(b.color) }} onClick={() => openBot(b.id, { newPane: true })} data-tip={b.role}>
                    <span class="bot-tile-av">
                      <BotAvatar bot={b} size={32} />
                      <Show when={botBusy(b.id)}>
                        <Tessellate size={7} class="bot-tile-busy" label="Working" />
                      </Show>
                    </span>
                    <span class="bot-tile-name">{b.name}</span>
                  </button>
                )}
              </For>
            </div>
          </section>
        </Show>

        <Show when={recent().length}>
          <section class="launcher-sec">
            <h3>Jump back in</h3>
            <ul class="launcher-list">
              <For each={recent()}>
                {(i) => (
                  <li>
                    <button type="button" class="launcher-row" onClick={() => openSession(i.id, { newPane: true })}>
                      <span class="launcher-row-icon">
                        <Show
                          when={i.kind === "channel" && !i.bot_id}
                          fallback={
                            <Show when={i.kind !== "channel" && i.bot_id} fallback={<Icon name={i.kind === "channel" ? "lock" : "chat"} size={15} />}>
                              <BotAvatar bot={state.bots.find((b) => b.id === i.bot_id)} size={18} />
                            </Show>
                          }
                        >
                          <AvatarStack ids={i.members} size={15} max={2} />
                        </Show>
                      </span>
                      <span class="launcher-row-title">{sessionLabel(i)}</span>
                      <span class="launcher-row-when">{relTime(i.updated_at)}</span>
                      <Icon name="arrowRight" size={14} class="launcher-row-go" />
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </section>
        </Show>
      </div>
    </div>
  );
}
