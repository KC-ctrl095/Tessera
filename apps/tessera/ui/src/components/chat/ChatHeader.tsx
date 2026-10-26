import { Match, Show, Switch, createSignal, onCleanup, onMount, type ComponentProps } from "solid-js";
import { fastAvailable, paneSettings, patchPane, renameSession, requestClosePane, setChannelMembers } from "../../lib/actions";
import { onPaneAction, type PaneAction } from "../../lib/bus";
import { botById, findModel, isBotDm, isBusy, isRoom, modelLabel, pendingAsks, state, type SessionState } from "../../lib/store";
import { shortLabel, thinkingLabel, thinkingOptions } from "../../lib/thinking";
import { openBotEditor, setDrawerOpen, setOverviewOpen } from "../../lib/ui";
import { hintSeen } from "../../lib/prefs";
import { isPrivatePane, workspace } from "../../lib/workspace";
import { AvatarStack, BotAvatar, dmLabel, botColor } from "../bots";
import { avatarGlyph } from "../../avatars/value";
import { Icon, Tessellate } from "../icons";
import { usePane } from "../PaneContext";
import { RestoreButton } from "../RestoreButton";
import { Menu, Popover, longPress, type MenuItem } from "../primitives";
import { openMosaics } from "../../lib/mosaicsPage";
import { ALT } from "../TopBar";
import { ContextRing, ModelMenu, offlineText, type ThinkingControl } from "./Pickers";
import { onDemand } from "../../lib/onDemand";
import { PartError } from "../PartError";
import { NewTileButton, newInHeader } from "../NewMenu";
import "../../styles/phone-controls.css";

const optionsPart = onDemand(() => import("./TileOptions"), "TileOptions");
function OptionsGate(props: ComponentProps<typeof import("./TileOptions").TileOptions>) {
  if (!optionsPart.get() && !optionsPart.error()) void optionsPart.load();
  let cancel: HTMLButtonElement | undefined;
  onMount(() => { if (!optionsPart.get()) cancel?.focus(); });
  return <Show when={optionsPart.get()} fallback={<div class="tile-opts tile-opts-loading" role="dialog" aria-label="Chat options" aria-busy={!optionsPart.error()}>
    <Show when={optionsPart.error()} fallback={<p role="status">Loading chat options…</p>}>
      <PartError error={optionsPart.error()} retry={optionsPart.retry} />
    </Show>
    <button ref={cancel} type="button" class="btn btn-ghost" onClick={props.onClose}>Cancel</button>
  </div>}>{m => { const View = m().TileOptions; return <View {...props} />; }}</Show>;
}

type PopKind = "model" | "options" | "members";
type Pop = { kind: PopKind; anchor: HTMLElement; focus?: "thinking" | "permission" | "context" | "fast" } | null;


const STATUS: Partial<Record<string, { label: string; tone: string }>> = {
  awaiting_approval: { label: "Needs approval", tone: "warn" },
  interrupted: { label: "Interrupted", tone: "warn" },
  error: { label: "Error", tone: "err" },
};

/**
 * Calm tile header: identity and title, a compact model chip, and one
 * options button for everything else. A YOLO badge (a struck-through shield) shows when it never asks.
 */
export function ChatHeader(props: { session: SessionState | undefined }) {
  const pane = usePane();
  const [pop, setPop] = createSignal<Pop>(null);
  const [editing, setEditing] = createSignal(false);
  const settings = () => paneSettings(pane.paneId);
  const info = () => props.session?.info;
  const model = () => findModel(settings().model);
  /** The chat's model is on a server of the user's that isn't answering. */
  const offline = () => {
    const ref = settings().model ?? state.server?.default_model;
    const p = ref ? state.providers.find((x) => x.id === ref.provider) : undefined;
    return p?.offline ? offlineText(p.base_url) : null;
  };
  const draftBotId = () => {
    const c = workspace.panes[pane.paneId]?.content;
    return c?.type === "chat" && !c.sessionId ? c.draft?.botId : undefined;
  };
  const room = () => isRoom(info());
  const dm = () => isBotDm(info());
  const channel = () => info()?.kind === "channel";
  const bot = () => (channel() ? undefined : botById(info()?.bot_id ?? draftBotId()));
  const yolo = () => !channel() && settings().permission === "auto";
  /** Fast mode is on and the model offers it (elsewhere it does nothing, so nothing shows). */
  const fast = () => !channel() && settings().fast && fastAvailable(pane.paneId);
  const phone = () => workspace.mode() === "mobile";
  /** Phones and foldables (folded or not): one quiet line; the title opens everything else. */
  const compact = () => workspace.mode() !== "tile";
  const priv = () => isPrivatePane(pane.paneId);
  const busy = () => isBusy(info());
  const asks = () => pendingAsks(info()?.id);
  const ctxPct = () => {
    const c = info()?.context;
    return c?.window ? c.tokens / c.window : 0;
  };
  let modelBtn: HTMLButtonElement | undefined;
  /** The model line under the title (what narrow tiles show instead of the chip). */
  let subBtn: HTMLButtonElement | undefined;
  let heldAt = 0;
  let optsBtn: HTMLButtonElement | undefined;
  let membersAnchor: HTMLButtonElement | undefined;

  const visible = (el: HTMLElement | undefined): el is HTMLElement => !!el && el.offsetParent !== null;
  const headEl = () => document.querySelector<HTMLElement>(`[data-pane-id="${pane.paneId}"] .pane-head`) ?? undefined;

  onMount(() => {
    const off = onPaneAction(pane.paneId, (a: PaneAction) => {
      if (a === "voice") return setPop(null);
      if (a === "find") return;
      if (a === "rename") {
        if (info()) setEditing(true);
        return;
      }
      if (a === "model" || a === "thinking") {
        const anchor = visible(modelBtn) ? modelBtn : visible(subBtn) ? subBtn : visible(optsBtn) ? optsBtn : headEl();
        if (anchor) setPop({ kind: "model", anchor, focus: a === "thinking" ? "thinking" : undefined });
        return;
      }
      const anchor = visible(optsBtn) ? optsBtn : headEl();
      if (anchor) setPop({ kind: "options", anchor, focus: a === "permission" ? "permission" : a === "fast" ? "fast" : "context" });
    });
    onCleanup(off);
  });

  const onHeadDown = (e: PointerEvent) => {
    const t = e.target as HTMLElement;
    if (t.closest("button:not(.title-btn), input, a, [data-nodrag]")) return;
    pane.startDrag(e);
  };

  const openMembers = () => {
    const a = membersAnchor && visible(membersAnchor) ? membersAnchor : headEl();
    if (a) setPop({ kind: "members", anchor: a });
  };

  const openOptions = (anchor: HTMLElement) => setPop(pop()?.kind === "options" ? null : { kind: "options", anchor });
  const titleButton = (fallback: string, text?: () => string) => (
    <Show
      when={editing() && info()}
      fallback={
        <Show
          when={compact()}
          fallback={
            <button type="button" class="title-btn" disabled={!info()} data-tip={info() ? "Rename" : undefined} onClick={() => info() && setEditing(true)}>
              <span class="title-text">{info()?.title || fallback}</span>
            </button>
          }
        >
          <button
            type="button"
            class="title-btn title-menu"
            aria-haspopup="dialog"
            aria-expanded={pop()?.kind === "options"}
            aria-label={`${text?.() || info()?.title || fallback}: options`}
            onClick={(e) => openOptions(e.currentTarget)}
          >
            <span class="title-text">{text?.() || info()?.title || fallback}</span>
            <Show when={busy()}>
              <Tessellate size={8} label="Working" class="head-busy" />
            </Show>
            <Show when={yolo()}>
              <Icon name="shieldOff" size={12} stroke={2} class="title-yolo" aria-label="YOLO: never asks" />
            </Show>
            <Show when={fast()}><Icon name="fast" size={12} class="title-fast fast-mark" aria-label="Fast mode" /></Show>
            <Icon name="chevronDown" size={12} class="title-chev" />
          </button>
        </Show>
      }
    >
      <TitleEditor
        value={info()!.title}
        onDone={(v) => {
          setEditing(false);
          if (v !== null && v.trim() && v.trim() !== info()!.title) void renameSession(info()!.id, v.trim());
        }}
      />
    </Show>
  );

  const modelSub = () => (
    <button ref={subBtn} type="button" class="model-sub" aria-haspopup="menu" onClick={(e) => setPop({ kind: "model", anchor: e.currentTarget })}>
      <span>{modelLabel(settings().model)}</span>
      <Show when={thinkShort()}>
        <span class="model-sub-think">{thinkShort()}</span>
      </Show>
      <Show when={fast()}>
        <Icon name="fast" size={11} class="fast-mark" aria-label="Fast mode" />
      </Show>
      <Icon name="chevronDown" size={11} />
    </button>
  );

  const thinkShort = () => {
    const opts = model()?.thinking ?? [];
    if (!opts.length) return "";
    return shortLabel(thinkingLabel(settings().thinking, model()));
  };
  /** The thinking level, offered inside the model menu (nothing for models without reasoning modes). */
  const thinkControl = (): ThinkingControl | undefined => {
    const options = thinkingOptions(model());
    return options.length && !channel() ? { level: settings().thinking, options, fallback: model()?.default_thinking, onSelect: (level) => void patchPane(pane.paneId, { thinking: level }) } : undefined;
  };

  return (
    <header
      class="pane-head"
      onPointerDown={onHeadDown}
      classList={{ "has-bot": !!bot(), "is-channel": channel(), busy: busy() }}
      style={{ "--bot": bot() ? botColor(bot()!.color) : undefined }}
    >
      <Show when={pane.showMenuButton()}>
        <button type="button" class="icon-btn head-menu" data-keep-focus aria-label="Open sidebar" onClick={() => setDrawerOpen(true)}>
          <Icon name="menu" />
        </button>
      </Show>

      <div class="head-title">
        <Switch
          fallback={
            <div class="title-stack">
              <div class="title-line">
                {titleButton(priv() ? "Private chat" : info() ? "Untitled chat" : "New chat")}
                <Show when={busy() && !compact()}>
                  <Tessellate size={8} label="Working" class="head-busy" />
                </Show>
              </div>
              <Show when={!compact()}>{modelSub()}</Show>
            </div>
          }
        >
          <Match when={dm()}>
            <span class="head-lock" data-tip="Private conversation between bots">
              <Icon name="lock" size={14} />
            </span>
            <Show
              when={compact()}
              fallback={
                <div class="title-stack">
                  <span class="title-static">{dmLabel(info()!)}</span>
                  <span class="head-sub">Private · you can watch</span>
                </div>
              }
            >
              <div class="title-stack">
                <div class="title-line">{titleButton("", () => dmLabel(info()!))}</div>
              </div>
            </Show>
          </Match>
          <Match when={room()}>
            <button type="button" class="head-members" ref={membersAnchor} aria-label="Members" data-tip="Members" onClick={(e) => setPop({ kind: "members", anchor: e.currentTarget })}>
              <AvatarStack ids={info()!.members} size={20} />
            </button>
            <div class="title-stack">
              <div class="title-line">
                {titleButton("Untitled channel")}
                <Show when={busy() && !compact()}>
                  <Tessellate size={8} label="Working" class="head-busy" />
                </Show>
              </div>
              <Show when={!compact()}>
                <span class="head-sub">
                  {info()!
                    .members.map((id) => (botById(id) ? "@" + botById(id)!.handle : ""))
                    .filter(Boolean)
                    .join(" ")}
                </span>
              </Show>
            </div>
          </Match>
          <Match when={bot()}>
            <button type="button" class="head-av" aria-label={`Edit ${bot()!.name}`} data-tip={bot()!.role} onClick={() => openBotEditor(bot()!.id)}>
              <BotAvatar bot={bot()} size={26} />
            </button>
            <div class="title-stack">
              <div class="title-line">
                <Show
                  when={compact()}
                  fallback={
                    <>
                      <span class="bot-name">{bot()!.name}</span>
                      <Show when={info()?.title}>
                        <span class="bot-sep">·</span>
                        {titleButton("")}
                      </Show>
                    </>
                  }
                >
                  {titleButton(bot()!.name)}
                </Show>
                <Show when={busy() && !compact()}>
                  <Tessellate size={8} label="Working" class="head-busy" />
                </Show>
              </div>
              <Show when={!compact()}>{modelSub()}</Show>
            </div>
          </Match>
        </Switch>
        <Show when={info() && (STATUS[info()!.status] || asks().length)}>
          <Show when={asks().length} fallback={<span class={`status-pill tone-${STATUS[info()!.status]!.tone}`} classList={{ keep: info()!.status === "awaiting_approval" }}>{STATUS[info()!.status]!.label}</span>}>
            <span class="needs-you" role="status" data-tip={`Waiting for your answer on “${asks()[0].title}”`}>
              <i aria-hidden="true" />
              Needs you
            </span>
          </Show>
        </Show>
        <Show when={priv()}>
          <span class="private-badge" data-tip="Held only in the server's memory, and only on this device. Closing the tile deletes it.">
            <Icon name="veil" size={13} />
            <span>Private</span>
            <span class="pb-more">· not saved</span>
          </span>
        </Show>
      </div>

      <div class="head-controls">
        {/* Phones: the tiles of this mosaic, once there is more than one. */}
        <Show when={phone() && pane.showMenuButton() && pane.count() > 1}>
          <button
            type="button"
            class="icon-btn head-overview"
            classList={{ nudge: pane.count() > 1 && !hintSeen("overview") }}
            aria-label={`All tiles of Mosaic ${workspace.active() + 1}`}
            onClick={() => {
              // The click that ends a long-press: it already went up a level, to every mosaic.
              if (Date.now() - heldAt > 800) setOverviewOpen(true);
            }}
            {...longPress(() => {
              heldAt = Date.now();
              openMosaics();
            })}
          >
            <Icon name="mosaic" size={17} />
          </button>
        </Show>
        <Show when={yolo() && !compact()}>
          <button
            type="button"
            class="yolo-badge"
            aria-label="YOLO mode is on — change permission"
            data-tip="YOLO: never asks before acting"
            onClick={(e) => setPop({ kind: "options", anchor: e.currentTarget, focus: "permission" })}
          >
            <Icon name="shieldOff" size={12} stroke={2} />
            <span>YOLO</span>
          </button>
        </Show>
        <Show when={!channel() && !compact()}>
          <button
            ref={modelBtn}
            type="button"
            class="model-chip"
            aria-haspopup="menu"
            aria-expanded={pop()?.kind === "model"}
            aria-label={`Model: ${modelLabel(settings().model)}`}
            data-tip={
              (settings().model ? `${settings().model!.provider}/${settings().model!.model}` : "Default model") +
              (fast() ? " · fast mode (uses more credits)" : "") +
              (offline() ? ` · ${offline()}` : "")
            }
            onClick={(e) => setPop({ kind: "model", anchor: e.currentTarget })}
          >
            <Show when={fast()}>
              <Icon name="fast" size={12} class="fast-mark" aria-label="Fast mode" />
            </Show>
            <Show when={offline()}>
              <i class="model-chip-off" aria-label={offline()!} />
            </Show>
            <span class="model-chip-name">{modelLabel(settings().model)}</span>
            <Show when={thinkShort()}>
              <span class="model-chip-think">{thinkShort()}</span>
            </Show>
            <Icon name="chevronDown" size={12} class="chip-chev" />
          </button>
        </Show>
        <Show when={info() && !channel() && ctxPct() > 0.7}>
          <button
            type="button"
            class="icon-btn ctx-warn"
            aria-label={`Chat length ${Math.round(ctxPct() * 100)}% of what the model can hold`}
            data-tip={`This chat is ${Math.round(ctxPct() * 100)}% of what the model can hold — summarize it to make room`}
            onClick={(e) => setPop({ kind: "options", anchor: e.currentTarget, focus: "context" })}
          >
            <ContextRing tokens={info()!.context.tokens} window={info()!.context.window} size={16} />
          </button>
        </Show>
        <Show when={!compact()}>
          <button
            ref={optsBtn}
            type="button"
            class="icon-btn opts-btn"
            aria-haspopup="dialog"
            aria-expanded={pop()?.kind === "options"}
            aria-label="Chat options"
            data-tip="Options"
            onClick={(e) => openOptions(e.currentTarget)}
          >
            <Icon name="more" size={17} />
          </button>
        </Show>
        <RestoreButton />
        <button type="button" class="icon-btn head-close" aria-label="Close tile" data-tip="Close tile" data-kbd={`${ALT} W`} onClick={() => void requestClosePane(pane.paneId)}>
          <Icon name="x" size={15} />
        </button>
        <Show when={newInHeader() && pane.showMenuButton()}><NewTileButton /></Show>
      </div>

      <Show when={pop()} keyed>
        {(p) => (
          <Popover
            anchor={p.anchor}
            placement="bottom-end"
            onClose={() => setPop(null)}
            class={p.kind === "model" ? "pop-model" : p.kind === "options" ? "pop-options" : "menu-pop"}
            label={p.kind === "model" ? "Model" : p.kind === "options" ? "Chat options" : "Members"}
          >
            <Switch>
              <Match when={p.kind === "model"}>
                <ModelMenu current={settings().model} onSelect={(m) => patchPane(pane.paneId, { model: m })} onClose={() => setPop(null)} thinking={thinkControl()} focusThinking={p.focus === "thinking"} />
              </Match>
              <Match when={p.kind === "options"}>
                <OptionsGate
                  paneId={pane.paneId}
                  session={props.session}
                  model={
                    compact() && !channel()
                      ? {
                          label: modelLabel(settings().model),
                          detail: thinkShort(),
                          fast: fast(),
                          onChange: () => requestAnimationFrame(() => setPop({ kind: "model", anchor: p.anchor })),
                        }
                      : undefined
                  }
                  focus={p.focus === "thinking" ? undefined : p.focus}
                  onClose={() => setPop(null)}
                  onRename={() => setEditing(true)}
                  onMembers={() => requestAnimationFrame(openMembers)}
                />
              </Match>
              <Match when={p.kind === "members" && props.session}>
                <Menu onClose={() => setPop(null)} items={memberItems(props.session!)} />
              </Match>
            </Switch>
          </Popover>
        )}
      </Show>
    </header>
  );
}

function memberItems(session: SessionState): MenuItem[] {
  const members = session.info.members;
  const items: MenuItem[] = [{ heading: "Members" }];
  for (const b of state.bots) {
    const on = members.includes(b.id);
    items.push({
      label: `${avatarGlyph(b.avatar)} ${b.name}`.trim(),
      description: `@${b.handle} · ${b.role}`,
      checked: on,
      disabled: on && members.length <= 1,
      onSelect: () => setChannelMembers(session.info.id, on ? members.filter((m) => m !== b.id) : [...members, b.id]),
    });
  }
  if (!state.bots.length) items.push({ label: "Create a bot first…", icon: "bot", onSelect: () => openBotEditor() });
  return items;
}

function TitleEditor(props: { value: string; onDone: (v: string | null) => void }) {
  let input!: HTMLInputElement;
  let done = false;
  const finish = (v: string | null) => {
    if (done) return;
    done = true;
    props.onDone(v);
  };
  onMount(() => {
    input.focus();
    input.select();
  });
  return (
    <input
      ref={input}
      class="title-input"
      value={props.value}
      aria-label="Chat title"
      onKeyDown={(e) => {
        if (e.key === "Enter") finish(e.currentTarget.value);
        else if (e.key === "Escape") {
          e.stopPropagation();
          finish(null);
        }
      }}
      onBlur={(e) => finish(e.currentTarget.value)}
    />
  );
}
