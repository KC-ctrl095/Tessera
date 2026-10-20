import { For, Show, createMemo, createSignal } from "solid-js";
import type { ContentPart } from "../../proto/ContentPart";
import type { NoticeLevel } from "../../proto/NoticeLevel";
import type { LearnedItem } from "../../proto/LearnedItem";
import { imageSrc, parseAttachedFile, typedText, type AttachedFile } from "../../lib/images";
import { download, downloads, openSaved, uploadFile } from "../../lib/files";
import { REWIND_BLOCKED, canRewind, openRewind, rewindBlocked } from "../../lib/rewind";
import { api, botById, modelLabel, plainText, state, upsertSession, type AssistantEntry, type SessionState } from "../../lib/store";
import { goalNote, harnessNote, parseBotReply, turnText, type CompactionEntry, type UserEntry } from "../../lib/transcript";
import { BotReplySlot } from "./SubagentGate";
import { offlineLate, toastError } from "../../lib/ui";
import { copyText, fmtCost, fmtDateTime, fmtTokens, touchUi } from "../../lib/util";
import { openSession } from "../../lib/workspace";
import { BotAvatar, botColor } from "../bots";
import { Icon, Tessellate, type IconName } from "../icons";
import { CopyButton, longPress, openMenu, type MenuItem } from "../primitives";
import { openFileMenu } from "../files/fileMenu";
import type { ViewFile } from "../../lib/viewFile";
import { Markdown } from "./Markdown";
import { claudeLimitText } from "../../lib/claude-limit";

const EXT_ICON: [RegExp, IconName][] = [
  [/\.(png|jpe?g|gif|webp|svg|bmp|heic|avif)$/i, "image"],
  [/\.(pdf|docx?|odt|rtf|pages|epub)$/i, "book"],
  [/\.(zip|tar|gz|tgz|xz|7z|rar|zst)$/i, "archive"],
  [/\.(rs|ts|tsx|js|jsx|mjs|py|go|rb|java|kt|swift|c|h|cpp|hpp|cs|php|sh|sql|lua|toml|ya?ml|json|xml|html?|css)$/i, "variable"],
  [/\.(csv|tsv|xlsx?|ods|numbers)$/i, "grid"],
];

/** An icon for a file, by its extension. */
export function FileIcon(props: { name: string; size?: number }) {
  const icon = () => EXT_ICON.find(([re]) => re.test(props.name))?.[1] ?? "file";
  return <Icon name={icon()} size={props.size ?? 15} class="file-icon" />;
}

/**
 * A file the user attached: icon, name and size; text files open to show
 * what was sent. Its ⋯ menu opens it elsewhere (a tile, a window, this
 * device's app) or saves it (components/files/fileMenu.ts).
 */
export function FileChip(props: { file: AttachedFile; sessionId?: string; private?: boolean }) {
  const [open, setOpen] = createSignal(false);
  const f = () => props.file;
  // Downloadable again, e.g. on another device.
  const shared = createMemo(() => uploadFile(props.sessionId, f().path));
  const vf = (): ViewFile => {
    const s = shared();
    return {
      name: f().name,
      ...(s ? { id: s.id, mime: s.mime, size: s.size } : {}),
      ...(f().text !== undefined ? { text: f().text } : {}),
      ...(f().path ? { path: f().path } : {}),
      ...(props.sessionId ? { sessionId: props.sessionId } : {}),
      ...(props.private ? { private: true } : {}),
    };
  };
  const hasMenu = () => !!shared() || f().text !== undefined;
  const got = () => {
    const s = shared();
    return s ? downloads[s.id] : undefined;
  };
  return (
    <div class="file-att" classList={{ open: open() }}>
      <button
        type="button"
        class="file-chip"
        aria-expanded={f().text !== undefined ? open() : undefined}
        disabled={f().text === undefined}
        title={f().path ? `Saved on the server at ${f().path}` : f().name}
        onClick={() => setOpen(!open())}
      >
        <FileIcon name={f().name} />
        <span class="file-chip-name">{f().name}</span>
        <span class="file-chip-size">{f().size}</span>
        <Show when={f().text !== undefined}>
          <Icon name="chevronDown" size={12} class="chev" />
        </Show>
      </button>
      <Show when={shared()}>
        {(s) => (
          <button
            type="button"
            class="file-get"
            disabled={got()?.phase === "downloading"}
            aria-label={got()?.phase === "saved" && got()?.location ? `Open ${f().name}` : `Download ${f().name}`}
            title={got()?.phase === "saved" && got()?.location ? "Open" : "Download"}
            onClick={() => void (got()?.phase === "saved" && got()?.location ? openSaved(s()) : download(s()))}
          >
            <Icon name={got()?.phase === "saved" && got()?.location ? "external" : "download"} size={14} />
          </button>
        )}
      </Show>
      <Show when={hasMenu()}>
        <button type="button" class="file-get file-more" aria-label={`More for ${f().name}`} aria-haspopup="menu" title="Open with…, download" onClick={(e) => openFileMenu(e.currentTarget, vf())}>
          <Icon name="more" size={14} />
        </button>
      </Show>
      <Show when={open() && f().text !== undefined}>
        <pre class="file-att-text">{f().text}</pre>
      </Show>
    </div>
  );
}

export function UserMessage(props: { content: ContentPart[]; pending?: boolean; ts?: number; seq?: number; session?: SessionState; paneId?: string }) {
  /** What the user typed, without the file notes the server appended. */
  const typed = () => typedText(props.content);
  const files = () =>
    props.content.flatMap((p) => {
      if (p.type !== "text") return [];
      const f = parseAttachedFile(p.text);
      return f ? [f] : [];
    });
  const text = typed;
  const images = () => props.content.filter((c): c is Extract<ContentPart, { type: "image" }> => c.type === "image");
  const forkable = () => !!props.session && props.seq !== undefined && props.seq > 1;
  /** Hidden where there's nothing to go back to (private chats, subagents); off while the agent works. */
  const rewindable = () => !!props.session && props.seq !== undefined && canRewind(props.session.info);
  const blocked = createMemo(() => rewindable() && rewindBlocked(props.session?.info));
  const rewind = () => {
    if (rewindable() && !blocked()) openRewind(props.session!.info.id, props.seq!, props.paneId ?? null);
  };
  // Touch: no hover, so a long press on the message offers the same actions.
  const menu = (): MenuItem[] => [
    { label: "Copy message", icon: "copy", onSelect: () => void copyText(text()) },
    ...(forkable() ? [{ label: "Branch from here", icon: "fork" as const, onSelect: () => void forkAt(props.session!, props.seq! - 1) }] : []),
    ...(rewindable()
      ? [{ label: "Rewind to here", icon: "rewind" as const, disabled: blocked(), description: blocked() ? REWIND_BLOCKED : "Undo this message and what came after", onSelect: rewind }]
      : []),
  ];
  const press = longPress((x, y) => openMenu({ x, y }, menu()));
  return (
    <div class="msg-user" classList={{ pending: !!props.pending }} data-seq={props.seq}>
      <div
        class="bubble"
        {...(props.pending ? {} : press)}
        onContextMenu={(e) => {
          // The long press opens our menu instead of the system's.
          if (!props.pending && touchUi()) e.preventDefault();
        }}
      >
        <Show when={images().length}>
          <div class="bubble-images">
            <For each={images()}>
              {(img) => (
                // Opens the picture viewer (lib/viewer.ts: one click handler for every chat image).
                <button type="button" class="bubble-img" aria-label="View image">
                  <img src={imageSrc(img)} alt="Attached image" loading="lazy" decoding="async" />
                </button>
              )}
            </For>
          </div>
        </Show>
        <Show when={text()}>
          <div class="bubble-text">{text()}</div>
        </Show>
        <Show when={files().length}>
          <div class="bubble-files">
            <For each={files()}>{(f) => <FileChip file={f} sessionId={props.session?.info.id} private={!!props.session?.info.private} />}</For>
          </div>
        </Show>
      </div>
      <Show when={props.pending && offlineLate()}>
        <div class="msg-wait" role="status">
          <Icon name="wifiOff" size={12} /> Not sent yet. It goes out when you're back online.
        </div>
      </Show>
      <Show when={!props.pending}>
        <div class="msg-actions">
          <CopyButton text={text} label="Copy message" size={14} class="sm" />
          <Show when={forkable()}>
            <button
              type="button"
              class="icon-btn sm"
              aria-label="Branch from before this message"
              data-tip="Branch from here"
              onClick={() => forkAt(props.session!, props.seq! - 1)}
            >
              <Icon name="fork" size={14} />
            </button>
          </Show>
          <Show when={rewindable()}>
            {/* aria-disabled rather than disabled, so the tooltip still says why. */}
            <button
              type="button"
              class="icon-btn sm msg-rewind"
              classList={{ off: blocked() }}
              aria-label="Rewind to before this message"
              aria-disabled={blocked()}
              data-tip={blocked() ? REWIND_BLOCKED : "Rewind to here"}
              onClick={rewind}
            >
              <Icon name="rewind" size={14} />
            </button>
          </Show>
        </div>
      </Show>
    </div>
  );
}

async function forkAt(s: SessionState, seq: number) {
  try {
    const info = await api.forkSession(s.info.id, seq);
    upsertSession(info);
    openSession(info.id, { newPane: true });
  } catch (e) {
    toastError(e, "Couldn't branch the chat");
  }
}

export function AssistantText(props: { text: string; streaming?: boolean; seq?: number }) {
  return (
    <div class="msg-assistant" data-seq={props.seq}>
      <Markdown text={props.text} streaming={props.streaming} />
    </div>
  );
}


export function TurnFooter(props: { entry: AssistantEntry; first: number; session: SessionState; last: boolean; check?: UserEntry }) {
  const u = () => props.entry.usage;
  return (
    <>
      <div class="turn-foot" classList={{ last: props.last }}>
        <CopyButton text={async () => {
          if (!props.session.hasOlderHistory) return turnText(props.session.entries, props.first, props.entry.seq);
          try { return await import("../../lib/history").then((m) => m.responseText(props.session.info.id, props.entry.seq)); }
          catch (e) { toastError(e, "Could not load the full response"); return ""; }
        }} label="Copy response" size={14} class="sm" />
        <button type="button" class="icon-btn sm" aria-label="Branch from here" data-tip="Branch from here" onClick={() => forkAt(props.session, props.entry.seq)}>
          <Icon name="fork" size={14} />
        </button>
        <span class="turn-meta" data-tip={`${fmtTokens(u().input)} in · ${fmtTokens(u().output)} out · ${fmtTokens(u().cache_read)} cached${fmtCost(u().cost_usd) ? ` · ${fmtCost(u().cost_usd)}` : ""}`}>
          {modelLabel(props.entry.model)} · {fmtDateTime(props.entry.ts)}
        </span>
      </div>
    </>
  );
}

const NOTICE_ICON: Record<NoticeLevel, "info" | "alert" | "alertCircle"> = { info: "info", warning: "alert", error: "alertCircle" };

/** "A failed (why); continuing with B for the rest of this turn." — the engine's model fallback. */
const FALLBACK = /^(.+?) failed \(([\s\S]*)\); continuing with (.+?) for the rest of this turn\.?$/;

export function NoticeLine(props: { level: NoticeLevel; text: string }) {
  const fallback = () => FALLBACK.exec(props.text.trim());
  return (
    <Show
      when={fallback()}
      fallback={
        <div class={`notice notice-${props.level}`} role={props.level === "error" ? "alert" : "note"}>
          <Icon name={NOTICE_ICON[props.level]} size={14} />
          <span>{claudeLimitText(props.text)}</span>
        </div>
      }
    >
      {(m) => (
        <div class="notice notice-switch" role="note" title={props.text}>
          <Icon name="refresh" size={13} />
          <span class="switch-main">
            Continued with <b>{modelName(m()[3])}</b>
          </span>
          <span class="switch-why">
            {modelName(m()[1])} failed: {m()[2]}
          </span>
        </div>
      )}
    </Show>
  );
}

/** "anthropic/claude-sonnet-5-5" → the model's display name when it's known. */
function modelName(ref: string): string {
  const i = ref.indexOf("/");
  return i > 0 ? modelLabel({ provider: ref.slice(0, i), model: ref.slice(i + 1) }) : ref;
}

const LEARNED_PREFIX = /^\s*learned from this conversation\s*[—–:-]+\s*/i;
const KIND_LABEL: Record<LearnedItem["kind"], string> = { profile: "About you", note: "Note", skill: "Skill" };

/** A notice about what was learned: its items, or (older servers) parsed from its text. */
export function learnedItems(text: string, learned: LearnedItem[] | undefined): LearnedItem[] | null {
  if (Array.isArray(learned) && learned.length) return learned.filter((l) => l && typeof l.text === "string");
  if (!LEARNED_PREFIX.test(text)) return null;
  const items = text
    .replace(LEARNED_PREFIX, "")
    .split(/;\s+/)
    .map((part): LearnedItem | null => {
      const m = /^(about you|note|skill)\s*:\s*([\s\S]*)$/i.exec(part.trim());
      const kind = m ? (m[1].toLowerCase() === "about you" ? "profile" : (m[1].toLowerCase() as "note" | "skill")) : "note";
      const t = (m ? m[2] : part).trim();
      return t ? { kind, action: "added", text: t } : null;
    })
    .filter((x): x is LearnedItem => !!x);
  return items.length ? items : null;
}

/**
 * "Learned from this conversation": one calm, collapsible line ("Remembered
 * 3 things") that opens into what was saved — about you, notes and skills.
 */
export function LearnedNotice(props: { items: LearnedItem[] }) {
  const [open, setOpen] = createSignal(false);
  const kept = () => props.items.filter((i) => i.action !== "removed").length;
  const dropped = () => props.items.filter((i) => i.action === "removed").length;
  const things = (n: number) => `${n} ${n === 1 ? "thing" : "things"}`;
  const label = () => (kept() ? `Remembered ${things(kept())}` : `Forgot ${things(dropped())}`);
  const kinds = () => {
    const order: LearnedItem["kind"][] = ["profile", "note", "skill"];
    const have = new Set(props.items.map((i) => i.kind));
    return order.filter((k) => have.has(k)).map((k) => (k === "profile" ? "about you" : k === "note" ? "notes" : "skills"));
  };
  return (
    <div class="learned" classList={{ open: open() }}>
      <button type="button" class="learned-head" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <span class="learned-glyph" aria-hidden="true">
          <i />
          <i />
          <i />
          <i />
        </span>
        <span class="learned-label">{label()}</span>
        <Show when={kept() && dropped()}>
          <span class="learned-more">· forgot {dropped()}</span>
        </Show>
        <span class="learned-kinds">{kinds().join(" · ")}</span>
        <Icon name="chevronDown" size={13} class="chev" />
      </button>
      <Show when={open()}>
        <ul class="learned-list">
          <For each={props.items}>
            {(i) => (
              <li class="learned-item" classList={{ removed: i.action === "removed" }} data-kind={i.kind}>
                <span class="learned-kind">{KIND_LABEL[i.kind] ?? "Note"}</span>
                <span class="learned-text">{i.text}</span>
                <Show when={i.action !== "added"}>
                  <span class="learned-action">{i.action === "updated" ? "updated" : "removed"}</span>
                </Show>
              </li>
            )}
          </For>
        </ul>
      </Show>
    </div>
  );
}

export function CompactionDivider(props: { entry: CompactionEntry }) {
  const [open, setOpen] = createSignal(false);
  return (
    <div class="compaction" classList={{ open: open() }} data-seq={props.entry.seq}>
      <button type="button" class="compaction-head" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <span class="rule" />
        <span class="compaction-label">
          <Icon name="compress" size={13} />
          Context compacted
          <Show when={props.entry.tokens_before > 0 && props.entry.tokens_after > 0}> · {fmtTokens(props.entry.tokens_before)} → {fmtTokens(props.entry.tokens_after)} tokens</Show>
          <Show when={props.entry.tokens_before > 0 && !props.entry.tokens_after}> · was {fmtTokens(props.entry.tokens_before)} tokens</Show>
          <Icon name="chevronDown" size={13} class="chev" />
        </span>
        <span class="rule" />
      </button>
      <Show when={open()}>
        <div class="compaction-body">
          <Show when={props.entry.external} fallback={<Markdown text={props.entry.summary} />}>
            <p>Claude compacted its own context. Tessera keeps your full history and task ledger. Claude's private summary is not available here.</p>
          </Show>
        </div>
      </Show>
    </div>
  );
}

export function StopLine(props: { entry: AssistantEntry }) {
  const text = () => {
    const e = props.entry;
    if (e.error) return e.error;
    if (e.stop === "aborted") return "Stopped";
    if (e.stop === "length") return "Response cut off at the output limit";
    return "The model returned an error";
  };
  const level = (): NoticeLevel => (props.entry.stop === "aborted" ? "info" : props.entry.stop === "length" ? "warning" : "error");
  return <NoticeLine level={level()} text={text()} />;
}

const HARNESS_ICON: [RegExp, IconName][] = [
  [/rewind/i, "rewind"],
  [/completion|check/i, "listChecks"],
  [/continue/i, "cornerDownRight"],
  [/empty/i, "circleMinus"],
  [/restart/i, "refresh"],
  [/budget/i, "gauge"],
];

/**
 * Something the harness told the agent ("[Completion check] …", "[Harness
 * restarted] …"): a calm system row, not a user message. Opens to the full text.
 */
export function HarnessNote(props: { entry: UserEntry }) {
  const note = () => harnessNote(plainText(props.entry.content));
  const [open, setOpen] = createSignal(false);
  const first = () => note().body.split("\n")[0];
  const long = () => note().body.includes("\n") || note().body.length > 140;
  return (
    <div class="harness" classList={{ open: open() }} data-seq={props.entry.seq}>
      <button type="button" class="harness-head" aria-expanded={long() ? open() : undefined} disabled={!long()} onClick={() => setOpen(!open())}>
        <Icon name={HARNESS_ICON.find(([re]) => re.test(note().label))?.[1] ?? "info"} size={13} />
        <span class="harness-label">{note().label}</span>
        <Show when={!open()}>
          <span class="harness-text">{first()}</span>
        </Show>
        <Show when={long()}>
          <Icon name="chevronDown" size={13} class="chev" />
        </Show>
      </button>
      <Show when={open()}>
        <div class="harness-body">{note().body}</div>
      </Show>
    </div>
  );
}

/** "[The user set the goal…]" notes: one quiet inscription line. */
export function GoalNote(props: { entry: UserEntry }) {
  const goal = () => goalNote(plainText(props.entry.content));
  return (
    <div class="goal-note" role="note">
      <Icon name="flag" size={13} />
      <span class="goal-note-label">{goal() !== null ? "Goal set" : "Goal cleared"}</span>
      <Show when={goal()}>
        <span class="goal-note-text">{goal()}</span>
      </Show>
    </div>
  );
}

/**
 * Who wrote a message that isn't the user's: a bot, another chat's agent
 * (`chat:<id>`; `chat:` alone for older messages whose chat isn't known),
 * or a scheduled job (`job:<id>`).
 */
export function authorInfo(author: string): { kind: "bot" | "chat" | "job"; name: string; tag?: string; id?: string } {
  if (author.startsWith("chat:")) {
    const id = author.slice(5);
    const info = id ? state.sessions[id]?.info : undefined;
    const title = info?.title?.trim();
    return {
      kind: "chat",
      id: info ? id : undefined,
      name: title || (id ? "Another chat" : "One of your chats"),
      tag: info?.kind === "subagent" ? "subagent" : "agent",
    };
  }
  if (author.startsWith("job:")) {
    const job = state.jobs.find((j) => j.id === author.slice(4));
    return { kind: "job", name: job?.name || "Scheduled job", tag: "scheduled" };
  }
  const bot = botById(author);
  return { kind: "bot", name: bot?.name ?? "Removed bot", tag: bot ? `@${bot.handle}` : undefined };
}

/** Who is talking: avatar, name and role in the bot's colour; another chat's agent or a job by name. */
export function Speaker(props: { author: string; ts?: number; typing?: boolean }) {
  const bot = () => botById(props.author);
  const who = () => authorInfo(props.author);
  return (
    <div
      class="speaker"
      classList={{ "speaker-other": who().kind !== "bot" }}
      data-author={who().kind}
      style={{ "--bot": botColor(bot()?.color) }}
      role={props.typing ? "status" : undefined}
    >
      <Show when={who().kind !== "bot"} fallback={<BotAvatar bot={bot()} size={26} />}>
        <span class="speaker-icon" aria-hidden="true">
          <Icon name={who().kind === "job" ? "clock" : "chat"} size={14} />
        </span>
      </Show>
      <Show when={who().id} fallback={<span class="speaker-name">{who().name}</span>}>
        <button type="button" class="speaker-name" title="Open this chat" onClick={() => openSession(who().id!, { newPane: true })}>
          {who().name}
        </button>
      </Show>
      <Show when={who().tag}>
        <span class="speaker-handle">{who().tag}</span>
      </Show>
      <Show when={props.typing}>
        <span class="speaker-typing">
          <Tessellate size={8} />
          is typing
        </span>
      </Show>
      <Show when={props.ts && !props.typing}>
        <time class="speaker-time">{fmtDateTime(props.ts!)}</time>
      </Show>
    </div>
  );
}

/**
 * A message someone other than the user wrote on the "user" side: a bot in
 * a private bot conversation, another chat's agent handing work over, a
 * subagent's task, a scheduled job's instructions.
 */
export function AuthoredMessage(props: { entry: UserEntry; author: string }) {
  const text = () => plainText(props.entry.content);
  // A bot's reply to work this chat handed it: a quiet card, like a helper's report.
  const reply = createMemo(() => parseBotReply(text()));
  return (
    <Show
      when={reply()}
      fallback={
        <div class="msg-authored" data-seq={props.entry.seq}>
          <Speaker author={props.author} ts={props.entry.ts} />
          <div class="msg-assistant authored-body">
            <Markdown text={text()} />
          </div>
        </div>
      }
    >
      {(r) => <BotReplySlot entry={props.entry} author={props.author} reply={r()} />}
    </Show>
  );
}
