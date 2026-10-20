import { For, Match, Show, Switch, createEffect, createMemo, createResource, createSignal, on, onCleanup } from "solid-js";
import { connectionGeneration } from "../../lib/connectionGeneration";
import type { Bot } from "../../proto/Bot";
import { api, botById, ensureLoaded, isBotDm, isBusy, isRoom, modelLabel, state, type SessionState } from "../../lib/store";
import { paneSettings } from "../../lib/actions";
import { isLocalEndpoint } from "../../lib/app";
import { prefs, setPrefs } from "../../lib/prefs";
import { openBotEditor } from "../../lib/ui";
import { openBotProfile } from "../BotEditor";
import { greeting, plural, relTime } from "../../lib/util";
import { focusComposer, openBot, openSession, paneShowing, setDraft, setPaneContent, workspace } from "../../lib/workspace";
import { AvatarStack, BotAvatar, botSuggestions, dmLabel, sessionLabel, botColor } from "../bots";
import { Icon, type IconName } from "../icons";
import { MosaicArt } from "../Mosaic";
import { ActivityLine } from "./Activity";
import { ChatHeader } from "./ChatHeader";
import { Composer } from "./Composer";
import { MemoryReview, SubagentBarSlot } from "./SubagentGate";
import { GoalStrip } from "./TaskLedger";
import { Transcript, type FollowHandle } from "./Transcript";
import { createVoiceHost, VoiceContext } from "../../lib/voice-host";
import { onPaneAction } from "../../lib/bus";

const SUGGESTIONS: { icon: IconName; text: string }[] = [
  { icon: "search", text: "Explain how this project is organized" },
  { icon: "listChecks", text: "Plan my week" },
  { icon: "globe", text: "Research a topic for me" },
];

export function ChatPane(props: { paneId: string }) {
  const content = () => workspace.panes[props.paneId]?.content;
  const sid = () => {
    const c = content();
    return c?.type === "chat" ? c.sessionId : null;
  };
  const session = (): SessionState | undefined => (sid() ? state.sessions[sid()!] : undefined);
  const info = () => session()?.info;
  const draftBot = () => {
    const c = content();
    return c?.type === "chat" && !c.sessionId ? botById(c.draft?.botId) : undefined;
  };
  const bot = () => (info()?.kind === "channel" ? undefined : botById(info()?.bot_id)) ?? draftBot();
  const priv = () => {
    const c = content();
    return !!info()?.private || (c?.type === "chat" && !c.sessionId && !!c.draft?.private);
  };
  const [detached, setDetached] = createSignal(false);
  let follow: FollowHandle | undefined;
  onCleanup(onPaneAction(props.paneId, (action) => { if (action === "find") follow?.find(); }));
  const voiceHost = createVoiceHost(props.paneId, session);

  createEffect(
    on(
      () => [sid(), state.ready] as const,
      ([id, ready]) => {
        if (!id || !ready) return;
        const generation = connectionGeneration();
        ensureLoaded(id).catch((e) => {
          if (sid() === id && generation === connectionGeneration() && (e as { code?: string }).code === "not_found") setPaneContent(props.paneId, { type: "chat", sessionId: null });
        });
      },
    ),
  );

  const empty = () => {
    const s = session();
    return !!s && s.loaded && !s.hasOlderHistory && !s.entries.length && !s.pending.length && !isBusy(s.info) && !s.streaming;
  };

  return (
    <VoiceContext.Provider value={voiceHost}>
    <div
      class="chat"
      classList={{ "is-bot": !!bot(), "is-channel": info()?.kind === "channel", "is-private": priv() }}
      onKeyDown={(e) => {
        if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f" && follow) {
          e.preventDefault();
          follow.find();
        }
      }}
      style={{ "--bot": bot() ? botColor(bot()!.color) : undefined }}
    >
      <ChatHeader session={session()} />
      <Show when={session()} keyed>
        {(s) => <SubagentBarSlot session={s} />}
      </Show>
      <Show
        when={sid() && !empty()}
        fallback={
          <Switch fallback={<Hero paneId={props.paneId} session={session()} />}>
            <Match when={priv()}>
              <PrivateHero paneId={props.paneId} session={session()} />
            </Match>
            <Match when={info()?.kind === "channel"}>
              <ChannelIntro paneId={props.paneId} session={session()!} />
            </Match>
            <Match when={bot()}>
              <BotHero paneId={props.paneId} session={session()} bot={bot()!} />
            </Match>
          </Switch>
        }
      >
        <Show when={session()?.loaded} fallback={<TranscriptSkeleton missing={state.ready && !session()} />}>
          <Show when={session()} keyed>
            {(s) => {
              // A new transcript starts at the live tail, whatever the last one was doing.
              onCleanup(() => setDetached(false));
              return <Transcript session={s} paneId={props.paneId} onDetached={setDetached} handle={(h) => (follow = h)} />;
            }}
          </Show>
        </Show>
        <div class="composer-dock">
          <Show when={session()}>
            <MemoryReview session={session()!} />
            <ActivityLine session={session()!} detached={detached()} onJump={() => follow?.jump()} />
            <GoalStrip info={session()!.info} />
          </Show>
          <Switch fallback={<Composer paneId={props.paneId} session={session()} />}>
            <Match when={isBotDm(info())}>
              <DmNote session={session()!} />
            </Match>
            <Match when={info()?.parent_call && info()?.helper_owner !== "tessera"}>
              <HelperNote session={session()!} />
            </Match>
          </Switch>
        </div>
      </Show>
    </div>
    </VoiceContext.Provider>
  );
}

/** Where a private chat's words go: the provider (cloud) or nowhere (local). */
function privacyOfModel(paneId: string): { local: boolean; text: string } {
  const ref = paneSettings(paneId).model;
  const provider = ref ? state.providers.find((p) => p.id === ref.provider) : undefined;
  const local = !!provider && isLocalEndpoint(provider.base_url);
  if (local) return { local, text: `${modelLabel(ref)} runs on your server: this chat stays on your hardware.` };
  return { local, text: `Runs at ${provider?.name ?? ref?.provider ?? "the model provider"}: they receive this chat.` };
}

/** A new private chat: what "private" means here, honestly. */
function PrivateHero(props: { paneId: string; session?: SessionState }) {
  const where = () => privacyOfModel(props.paneId);
  // Older servers have no web search settings: just say web search is on.
  const [search] = createResource(() => api.getWebSearch().catch(() => null));
  const searx = () => search()?.search === "searxng";
  return (
    <div class="hero private-hero">
      <div class="hero-inner">
        <MosaicArt class="hero-mosaic" tone="veiled" seed={31} width={520} height={120} cell={12} />
        <h1 class="hero-title serif">A private chat</h1>
        <p class="hero-sub">Held only in your server's memory, and only for as long as you need it.</p>
        <ul class="private-facts">
          <li>
            <Icon name="eyeOff" size={14} />
            <span>
              <b>Nothing is written to disk</b> — not the chat, not your drafts, not on this device either.
            </span>
          </li>
          <li>
            <Icon name="phone" size={14} />
            <span>
              <b>Only on this device</b> — your other phones, computers and browsers never see it.
            </span>
          </li>
          <li>
            <Icon name="bookmark" size={14} />
            <span>
              <b>No memory or history</b> is used or kept: it starts from nothing and learns nothing.
            </span>
          </li>
          <li>
            <Icon name="globe" size={14} />
            <span>
              <b>Web search is on</b>
              {searx() ? " — searches go through your own SearXNG" : ""}; files, commands and other tools are off.
            </span>
          </li>
          <li>
            <Icon name="trash" size={14} />
            <span>
              <b>Deleting it erases it for good</b> — so does closing this tile, or the server restarting.
            </span>
          </li>
        </ul>
        <p class="private-where" classList={{ local: where().local }}>
          <Icon name={where().local ? "home" : "cloud"} size={14} />
          <span>{where().text}</span>
        </p>
        <Composer paneId={props.paneId} session={props.session} hero />
      </div>
    </div>
  );
}

/** New chat: greeting, composer, suggestions, and quick picks to fill this tile. */
function Hero(props: { paneId: string; session?: SessionState }) {
  const recent = createMemo(() =>
    Object.values(state.sessions)
      .map((s) => s.info)
      .filter((i) => (i.kind === "chat" || i.kind === "channel") && !i.archived && !i.private && !paneShowing(i.id))
      .sort((a, b) => b.updated_at - a.updated_at)
      .slice(0, 3),
  );
  return (
    <div class="hero">
      <div class="hero-inner">
        <MosaicArt class="hero-mosaic" seed={11} width={520} height={120} cell={12} />
        <h1 class="hero-title serif">{greeting()}</h1>
        <p class="hero-sub">What are we working on?</p>
        <Composer paneId={props.paneId} session={props.session} hero />
        {/* Three things to try, one line each; the × hides them (Settings → Help & tour brings them back). */}
        <Show when={prefs.tips}>
          <div class="suggestions hero-tips" role="group" aria-label="Suggestions">
            <For each={SUGGESTIONS}>
              {(s) => (
                <button
                  type="button"
                  class="suggestion"
                  onClick={() => {
                    setDraft(props.paneId, s.text);
                    focusComposer(props.paneId);
                  }}
                >
                  <Icon name={s.icon} size={13} />
                  <span>{s.text}</span>
                </button>
              )}
            </For>
            <button type="button" class="icon-btn sm tips-x" aria-label="Hide suggestions" data-tip="Hide suggestions (Settings → Help & tour brings them back)" onClick={() => setPrefs("tips", false)}>
              <Icon name="x" size={12} />
            </button>
          </div>
        </Show>
        {/* One set of choices at a time: tips first, quick picks after they are dismissed. */}
        <Show when={!prefs.tips && (state.bots.length || recent().length)}>
          <div class="quick-picks" aria-label="Open in this tile">
            <For each={state.bots.slice(0, 4)}>
              {(b) => (
                <button type="button" class="quick-pick" style={{ "--bot": botColor(b.color) }} onClick={() => openBot(b.id, { pane: props.paneId })} title={`${b.name} — ${b.role}`}>
                  <BotAvatar bot={b} size={18} />
                  <span>{b.name}</span>
                </button>
              )}
            </For>
            <For each={recent()}>
              {(i) => (
                <button type="button" class="quick-pick" onClick={() => openSession(i.id, { pane: props.paneId })} title={`Open “${sessionLabel(i)}” here · ${relTime(i.updated_at)}`}>
                  <Show when={i.kind === "chat" && botById(i.bot_id)} fallback={<Icon name={i.kind === "channel" ? (i.bot_id ? "lock" : "hash") : "chat"} size={14} />}>
                    <BotAvatar bot={botById(i.bot_id)} size={18} />
                  </Show>
                  <span>{sessionLabel(i)}</span>
                </button>
              )}
            </For>
          </div>
        </Show>
      </div>
    </div>
  );
}

/** A bot's greeting card: who it is and what to ask it. */
function BotHero(props: { paneId: string; session?: SessionState; bot: Bot }) {
  const b = () => props.bot;
  return (
    <div class="hero bot-hero" style={{ "--bot": botColor(b().color) }}>
      <div class="hero-inner">
        <div class="bot-greet">
          <BotAvatar bot={b()} size={76} class="bot-greet-av" />
          <h1 class="hero-title">
            Hi, I'm <span class="bot-name-big">{b().name}</span>
          </h1>
          <p class="hero-sub">{b().role || "Your bot"}</p>
          <div class="bot-greet-meta">
            <span class="badge">@{b().handle}</span>
            <Show when={b().permission === "auto"}>
              <span class="badge badge-yolo">
                <Icon name="shieldOff" size={11} /> YOLO
              </span>
            </Show>
            <Show when={state.jobs.some((j) => j.bot_id === b().id)}>
              <span class="badge">
                <Icon name="clock" size={11} /> {plural(state.jobs.filter((j) => j.bot_id === b().id).length, "duty", "duties")}
              </span>
            </Show>
            <button type="button" class="link-btn" onClick={() => openBotProfile(b().id)}>
              Profile
            </button>
            <button type="button" class="link-btn" onClick={() => openBotEditor(b().id)}>
              Edit
            </button>
          </div>
        </div>
        <Composer paneId={props.paneId} session={props.session} hero />
        <div class="suggestions" hidden={!prefs.tips}>
          <For each={botSuggestions(b())}>
            {(text) => (
              <button
                type="button"
                class="suggestion"
                onClick={() => {
                  setDraft(props.paneId, text);
                  focusComposer(props.paneId);
                }}
              >
                <span>{text}</span>
              </button>
            )}
          </For>
        </div>
      </div>
    </div>
  );
}

/** Empty channel: who's here and how mentions work. */
function ChannelIntro(props: { paneId: string; session: SessionState }) {
  const info = () => props.session.info;
  const dm = () => isBotDm(info());
  return (
    <div class="hero channel-hero">
      <div class="hero-inner">
        <Show
          when={!dm()}
          fallback={
            <>
              <span class="channel-hero-icon">
                <Icon name="lock" size={26} />
              </span>
              <h1 class="hero-title">{dmLabel(info())}</h1>
              <p class="hero-sub">
                {info().members.length === 1 ? "Where your chats' agents hand work to this bot" : "A private conversation between bots"}. Nothing has been said yet — you can watch it here.
              </p>
            </>
          }
        >
          <AvatarStack ids={info().members} size={44} max={5} />
          <h1 class="hero-title"># {info().title || "Untitled channel"}</h1>
          <p class="hero-sub">
            Everyone replies when you post. <b>@mention</b> a bot to address just them.
          </p>
          <Composer paneId={props.paneId} session={props.session} hero />
          <div class="suggestions">
            <For each={info().members.map((id) => botById(id)).filter((b): b is Bot => !!b)}>
              {(b) => (
                <button
                  type="button"
                  class="suggestion"
                  onClick={() => {
                    setDraft(props.paneId, `@${b.handle} `);
                    focusComposer(props.paneId);
                  }}
                >
                  <BotAvatar bot={b} size={18} />
                  <span>@{b.handle}</span>
                </button>
              )}
            </For>
          </div>
        </Show>
      </div>
    </div>
  );
}

function DmNote(props: { session: SessionState }) {
  const members = () => props.session.info.members.map((id) => botById(id)).filter((b): b is Bot => !!b);
  return (
    <div class="dm-note" role="note">
      <Icon name="lock" size={15} />
      <span>
        {members().length === 1 ? "Your chats' agents hand work to this bot here — you can watch." : "Private conversation between bots — you can watch."}
        <Show when={isRoom(props.session.info) === false && members().length}>
          {" "}
          Message{" "}
          <For each={members()}>
            {(b, i) => (
              <>
                {i() > 0 ? " or " : ""}
                <button type="button" class="link-btn" onClick={() => openBot(b.id, { newPane: true })}>
                  {b.name}
                </button>
              </>
            )}
          </For>{" "}
          directly instead.
        </Show>
      </span>
    </div>
  );
}

/** One of Claude Code's subagents: its steps come from Claude Code; talk to it through the main chat. */
function HelperNote(props: { session: SessionState }) {
  const parent = () => props.session.info.parent_id;
  return (
    <div class="dm-note" role="note">
      <Icon name="eye" size={15} />
      <span>
        A helper, working on what the{" "}
        <Show when={parent()} fallback="main chat">
          <button type="button" class="link-btn" onClick={() => openSession(parent()!, { newPane: true })}>
            main chat
          </button>
        </Show>{" "}
        gave it. You can watch it here; write in the main chat to steer it.
      </span>
    </div>
  );
}

function TranscriptSkeleton(props: { missing: boolean }) {
  return (
    <div class="transcript-wrap">
      <Show
        when={!props.missing}
        fallback={
          <div class="empty-state">
            <Icon name="alertCircle" size={22} />
            <p>This chat is no longer available.</p>
          </div>
        }
      >
        <div class="skeleton" aria-busy="true" aria-label="Loading chat">
          <div class="sk sk-user" />
          <div class="sk sk-line" />
          <div class="sk sk-line w80" />
          <div class="sk sk-line w60" />
          <div class="sk sk-user short" />
          <div class="sk sk-line" />
          <div class="sk sk-line w70" />
        </div>
      </Show>
    </div>
  );
}
