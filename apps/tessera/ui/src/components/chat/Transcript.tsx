import { For, Show, Suspense, lazy, createEffect, createMemo, createSignal, on, onCleanup, onMount, untrack, type JSX } from "solid-js";
import { unwrap } from "solid-js/store";
import { isBotDm, isBusy, type SessionState } from "../../lib/store";
import type { LearnedItem } from "../../proto/LearnedItem";
import { ToolDurationIndex } from "../../lib/tools";
import { TranscriptDeriver, LiveRows, continuesThinking, itemSeq, isCheckAck, isProtocolEntry, quietCheckOpen, thinkingRun, type LiveParts, type LiveRow, type Row } from "../../lib/transcript";
import { possibleCheckAck } from "../../lib/completion";
import { jumpTarget } from "../../lib/ui";
import { Follow } from "../../lib/follow";
import { reducedMotion } from "../../lib/util";
import { Icon } from "../icons";
import { usePane } from "../PaneContext";
import { ApprovalCard } from "./Approval";
import { AssistantText, AuthoredMessage, CompactionDivider, GoalNote, HarnessNote, LearnedNotice, NoticeLine, Speaker, StopLine, TurnFooter, UserMessage, learnedItems } from "./Messages";
import { SecretNotice, savedSecrets } from "./SecretNotice";
import { ReportCardSlot, ReportGroupSlot } from "./SubagentGate";
import { ThinkingBlock } from "./Thinking";
import { DurationContext, StepsGroup, ToolCard, toolView } from "./ToolCard";
import { BoardCallCard, ChatAsks } from "../board/BoardCard";
import { SharedFileCall } from "./FileCard";
import { NoteCallCard } from "./NoteCard";
import type { HistoryHost } from "./TranscriptHistory";

const ChatFind = lazy(() => import("./ChatFind"));

/** Items rendered at first; "Show earlier" adds more. */
const PAGE = 60;
/** The newest items never skip rendering (see `.item.settled` in chat.css). */
const TAIL = 8;
/** Within this distance of the bottom, a finished scroll gesture re-attaches. */
const REPIN = 24;

export interface FollowHandle {
  /** Re-attach to the bottom and follow the live tail. */
  jump: () => void;
  find: () => void;
}

/**
 * The conversation, following its tail while the reader is at the bottom.
 *
 * Gestures, not scroll events or layout, choose whether to follow (lib/follow.ts).
 * One ResizeObserver watches both content and viewport: markdown bursts, late images,
 * expanding cards, fonts, the composer, tile dividers and keyboard/window resizing all
 * pin synchronously before paint. No smooth-scroll target can race a growing end.
 * While detached, the block at the top keeps its place without native scroll anchoring.
 * Returning within 24px of the painted end or "Jump to live" reattaches.
 */
export function Transcript(props: {
  session: SessionState;
  paneId: string;
  onDetached?: (detached: boolean) => void;
  handle?: (h: FollowHandle) => void;
}) {
  const s = () => props.session;
  const [finding, setFinding] = createSignal(false);
  const pane = usePane();
  const cache = new TranscriptDeriver();
  const running = () => isBusy(s().info) || !!s().streaming;
  /** The agent is visibly working (the composer's status line is up). */
  const live = () => isBusy(s().info) || (!!s().streaming && !s().streaming!.ended);
  const channel = () => s().info.kind === "channel";
  const speakers = () => channel() || !!s().info.bot_id;
  // Stored entries change only on history events, not token deltas. Track that boundary once:
  // scanning thousands of reactive entry/part properties otherwise creates and cleans up tens
  // of thousands of subscriptions per append, even though only PAGE rows are mounted.
  const entries = createMemo(() => {
    s().entriesVersion;
    const all = s().entries;
    all.length;
    return unwrap(all);
  }, undefined, { equals: false });
  const items = createMemo(() =>
    cache.derive(entries(), running(), {
      speakers: speakers(),
      defaultAuthor: channel() ? undefined : s().info.bot_id,
      collapseAll: channel(),
      // Nobody types into a bot DM: an older message without an author came from one of the user's chats.
      userAuthor: isBotDm(s().info) ? "chat:" : undefined,
    }, s().entriesVersion),
  );
  /** The bot that spoke last (null after a user message). */
  const lastSpeaker = createMemo(() => {
    const all = items();
    for (let i = all.length - 1; i >= 0; i--) {
      const it = all[i];
      if (it.k === "speaker" || it.k === "authored") return it.author;
      if (it.k === "user") return null;
    }
    return null;
  });
  /** The bot about to speak in the reply being streamed (shown above it when it's a new voice). */
  const liveAuthor = () => {
    const st = s().streaming;
    if (!speakers() || !st) return null;
    const a = st.author ?? (channel() ? undefined : s().info.bot_id);
    return a && a !== lastSpeaker() ? a : null;
  };
  const [limit, setLimit] = createSignal(PAGE);
  const visible = createMemo(() => {
    const all = items();
    return all.length > limit() ? all.slice(all.length - limit()) : all;
  });
  const hidden = () => Math.max(0, items().length - limit());
  // The reply being streamed shows as rows after the items; when it's committed, its rows stay
  // and show the committed items (lib/transcript.ts LiveRows): no second render, nothing moves.
  const liveRows = new LiveRows();
  // Tokens change a row's text, not the list's shape. Keep that boundary stable:
  // otherwise every token reconciles all the history the reader has opened.
  const liveParts = createMemo<LiveParts>(() => {
    const st = s().streaming;
    const parts: LiveParts["parts"] = [];
    // A legacy check buffers only acknowledgement-shaped prefixes; release
    // substantive text as soon as it stops being a possible protocol mark.
    const quiet = !!st && quietCheckOpen(entries());
    (st?.content ?? []).forEach((p, index) => {
      if (!p || st?.author === "harness:completion" || (p.type === "text" && isCheckAck(p.text))) return;
      if (quiet && (p.type === "thinking" || (p.type === "tool_call" && p.name === "task") || (p.type === "text" && possibleCheckAck(p.text)))) return;
      parts.push({ index, type: p.type, id: p.type === "tool_call" ? p.id : undefined });
    });
    return { stream: st, speaker: liveAuthor(), parts };
  }, { stream: null, speaker: null, parts: [] }, { equals: (a, b) => a.stream === b.stream && a.speaker === b.speaker && a.parts.length === b.parts.length && a.parts.every((p, i) => p.index === b.parts[i].index && p.type === b.parts[i].type && p.id === b.parts[i].id) });
  const rows = createMemo(() => {
    const vis = visible();
    const now = liveParts();
    return untrack(() => liveRows.merge(vis, now));
  });
  const lastTurnKey = createMemo(() => {
    const all = items();
    for (let i = all.length - 1; i >= 0; i--) if (all[i].k === "turn") return all[i].key;
    return null;
  });

  // Tool durations: measured live when we saw them run, else the time stored with the result, else from timestamps.
  const timings = new ToolDurationIndex();
  const derived = createMemo(() => timings.update(entries(), s().entriesVersion), undefined, { equals: false });
  const durationOf = (id: string) => s().toolMs[id] ?? derived()[id];

  const [seenAt, setSeenAt] = createSignal(Infinity);
  /** Messages appended after detaching, not older pages the reader opens. */
  const messageCount = createMemo(() => {
    let n = 0;
    for (const e of entries()) if (e.seq > seenAt() && !isProtocolEntry(e) && (e.kind === "user" || (e.kind === "assistant" && e.content.some((p) => p.type === "text" && p.text.trim() && !isCheckAck(p.text))))) n++;
    return n;
  });

  let scroller!: HTMLDivElement;
  let content!: HTMLDivElement;

  // ── follow state ──────────────────────────────────────────────────────
  const [detached, setDetached] = createSignal(false);
  const newCount = () => detached() ? messageCount() : 0;
  /** A block the reader just expanded/collapsed: keep it where it was. */
  let anchor: { el: HTMLElement; top: number; until: number; below: { el: HTMLElement; top: number }[] } | null = null;
  /** Items mounted after this point settle in with a short animation. */
  let settleNew = false;
  /** When an optimistic message was last replaced by its echo (no second entrance). */
  let echoedAt = 0;
  /**
   * What the reader is looking at while they read: the block at the top of the view (deepest
   * first, then its parents up to its row) and each block's viewport position.
   */
  let reading: { el: HTMLElement; at: number }[] = [];

  /** Top of `el` in the transcript's content: the same whatever the scroll position or a running animation. */
  const layoutTop = (el: HTMLElement) => {
    let y = 0;
    for (let n: HTMLElement | null = el; n && n !== scroller; n = n.offsetParent as HTMLElement | null) y += n.offsetTop;
    return y;
  };
  /** Notes the block at the top of the view: the first one that ends below the top edge, as deep as it straddles it. */
  const pickReading = () => {
    reading = [];
    const top = scroller.scrollTop;
    let parent: Element = content;
    for (let depth = 0; depth < 6; depth++) {
      let hit: HTMLElement | null = null;
      // Rows are in document order. A linear walk through the whole history on every
      // scroll also asks the engine for the layout of every skipped row. Locate the
      // visible row in logarithmic time, then inspect only its small block tree.
      let first = 0;
      if (parent === content || parent.classList.contains("md")) {
        let end = parent.children.length;
        while (first < end) {
          const mid = (first + end) >>> 1;
          const row = parent.children[mid] as HTMLElement;
          if (layoutTop(row) + row.offsetHeight <= top + 1) first = mid + 1;
          else end = mid;
        }
      }
      for (let c: Element | null | undefined = parent.children[first]; c; c = c.nextElementSibling) {
        if (!(c instanceof HTMLElement) || !c.offsetParent || c.classList.contains("load-more")) continue;
        if (layoutTop(c) + c.offsetHeight > top + 1) {
          hit = c;
          break;
        }
      }
      if (!hit) break;
      const at = layoutTop(hit);
      reading.unshift({ el: hit, at: hit.getBoundingClientRect().top });
      if (at >= top - 1 || !hit.firstElementChild) break;
      parent = hit;
    }
    // A page boundary can replace an orphan result with its newly loaded call.
    // Keep a few following rows as fallbacks if that first row no longer exists.
    let next = reading.at(-1)?.el.nextElementSibling as HTMLElement | null;
    for (let i = 0; next && i < 4; i++, next = next.nextElementSibling as HTMLElement | null)
      reading.push({ el: next, at: next.getBoundingClientRect().top });
  };
  /** Content above the view grew or shrank: scroll by as much, so what the reader looks at stays put. */
  const keepReading = () => {
    if (historyHost.restoreReading) return historyHost.restoreReading();
    if (follow.gesturing) return pickReading();
    const a = reading.find((r) => r.el.isConnected);
    if (a) {
      const d = a.el.getBoundingClientRect().top - a.at;
      if (Math.abs(d) >= 1) follow.scrollTo(scroller.scrollTop + d);
    }
    pickReading();
  };
  // The one "following / reading" state (lib/follow.ts): only the reader changes it.
  let follow!: Follow;
  const watch = (el: HTMLDivElement) => {
    scroller = el;
    follow = new Follow(el, {
      repin: REPIN,
      onChange: (following) => {
        if (!following) setSeenAt(untrack(() => s().entries.at(-1)?.seq ?? 0));
        else historyHost.releaseReading?.();
        setDetached(!following);
        props.onDetached?.(!following);
      },
      onDetach: pickReading,
      // A genuine wheel/key/touch scroll chooses a new reading anchor. Restoring
      // the old one here can interrupt native Home/PageUp and touch momentum.
      onReaderScroll: () => {
        pickReading();
        historyHost.restoreReading = undefined;
        if (scroller.scrollTop < 200) showEarlier();
      },
    });
  };
  onCleanup(() => follow?.dispose());
  // Visited phone pages/mosaics retain their DOM. Reopening one starts at the latest,
  // just like remounting a chat, but focusing another already-visible tile does not move it.
  createEffect(on(pane.visible, (visible) => { if (visible) follow.attach(); }, { defer: true }));

  /** Rows after `el` that are on screen, with where they are now. */
  const rowsBelow = (el: HTMLElement) => {
    if (reducedMotion()) return [];
    const row = el.closest<HTMLElement>(".transcript-inner > *");
    const out: { el: HTMLElement; top: number }[] = [];
    const bottom = scroller.getBoundingClientRect().bottom;
    for (let n = row?.nextElementSibling as HTMLElement | null; n && out.length < 24; n = n.nextElementSibling as HTMLElement | null) {
      const top = n.getBoundingClientRect().top;
      if (top > bottom) break;
      out.push({ el: n, top });
    }
    return out;
  };
  /** FLIP: rows that moved because a block opened or closed glide from where they were. */
  const glideBelow = (rows: { el: HTMLElement; top: number }[]) => {
    const at = performance.now();
    if (rows.length) void readerHistory().then((m) => m.glideBelow(rows, at));
  };

  onMount(() => {
    const ro = new ResizeObserver(() => {
      if (anchor) {
        const a = anchor;
        anchor = null;
        if ((!follow.following || !live()) && performance.now() < a.until && a.el.isConnected) {
          // Live output stays pinned; an explicit expansion in finished history keeps its heading.
          follow.scrollTo(scroller.scrollTop + a.el.getBoundingClientRect().top - a.top);
          if (!live()) follow.set(follow.gap() <= REPIN);
          glideBelow(a.below);
          if (!follow.following) pickReading();
          return;
        }
      }
      // Observe the end even while detached: a reader can reach it just before a burst.
      follow.stick();
      if (!follow.following) keepReading();
    });
    ro.observe(content);
    ro.observe(scroller);

    let disposed = false;
    follow.stick();
    const initialFrame = requestAnimationFrame(() => follow.stick());
    // What is already there appears at once; what arrives from now on settles in.
    const entrance = setTimeout(() => (settleNew = true), 400);
    void document.fonts?.ready.then(() => { if (!disposed) follow.stick(); });
    props.handle?.({ jump: jumpToLatest, find: () => setFinding(true) });
    onCleanup(() => {
      disposed = true;
      cancelAnimationFrame(initialFrame);
      clearTimeout(entrance);
      ro.disconnect();
    });
  });

  const jumpToLatest = () => follow.attach();

  let alive = true;
  onCleanup(() => { alive = false; historyHost.releaseReading?.(); });
  const historyHost: HistoryHost = {
    session: s, items, limit, setLimit, pickReading,
    get scroller() { return scroller; }, get content() { return content; }, get follow() { return follow; },
    alive: () => alive && pane.visible(),
  };
  let historyPart: Promise<typeof import("./TranscriptHistory")> | undefined;
  const readerHistory = () => historyPart ??= import("./TranscriptHistory");
  const showEarlier = () => { void readerHistory().then((m) => m.showEarlier(historyHost)); };

  // Sidebar search, recall cards and the palette can point outside the newest page.
  createEffect(on(() => [jumpTarget(), s().loaded] as const, ([j, loaded]) => {
    if (j && loaded && j.paneId === props.paneId && j.sessionId === s().info.id)
      void readerHistory().then((m) => m.jump(historyHost, j));
  }));

  // Your own rewind: once the rewound rows are gone, follow the (new) end again,
  // where your message waits in the composer.
  createEffect(
    on(
      () => s().rewind,
      (r, prev) => {
        // Only once the rows are really gone (a failed rewind leaves everything where it was).
        if (prev?.local && !r && untrack(() => s().entries.at(-1)?.seq ?? 0) <= prev.from) requestAnimationFrame(() => follow.attach());
      },
      { defer: true },
    ),
  );

  // Sending a message re-attaches to the bottom.
  createEffect(
    on(
      () => s().pending.length,
      (n, prev) => {
        if (prev !== undefined && n > prev) follow.attach();
        if (prev !== undefined && n < prev) echoedAt = performance.now();
      },
    ),
  );
  /** Whether an item mounting now should settle in: new arrivals only, and never the
   *  final copy of something that was already on screen (streamed text, a sent message). */
  const entering = (it: Row) => {
    if (!settleNew || reducedMotion()) return false;
    if (it.k === "live") return true;
    if (it.k === "user") return performance.now() - echoedAt > 400;
    return it.k === "report" || it.k === "reports" || it.k === "goal" || it.k === "harness" || it.k === "notice" || it.k === "compaction" || it.k === "authored" || it.k === "stop";
  };

  const streamParts = createMemo(() => (s().streaming?.content ?? []).filter(Boolean));
  const orphanRunning = createMemo(() => {
    const known = new Set<string>();
    for (const e of entries()) if (e.kind === "assistant") for (const p of e.content) if (p.type === "tool_call") known.add(p.id);
    for (const p of streamParts()) if (p.type === "tool_call") known.add(p.id);
    return Object.values(s().running).filter((r) => !known.has(r.call_id));
  });

  return (
    <DurationContext.Provider value={durationOf}>
      <div class="transcript-wrap">
        <Show when={finding()}><Suspense><ChatFind session={s()} paneId={props.paneId} close={() => setFinding(false)} /></Suspense></Show>
        <div class="transcript" ref={watch} tabIndex={-1}>
          <div
            class="transcript-inner"
            ref={content}
            onClick={(e) => {
              const t = (e.target as HTMLElement).closest<HTMLElement>("[aria-expanded]");
              if (t && (!follow.following || !live())) anchor = { el: t, top: t.getBoundingClientRect().top, until: performance.now() + 400, below: rowsBelow(t) };
            }}
          >
            <Show when={hidden() > 0 || s().hasOlderHistory || s().loadingOlderHistory}>
              <div class="load-more" aria-busy={s().loadingOlderHistory}>
                <button type="button" class="link-btn" disabled={s().loadingOlderHistory} onClick={showEarlier}>
                  {s().loadingOlderHistory ? "Loading earlier messages…" : hidden() ? `Show earlier · ${hidden()} more` : "Show earlier"}
                </button>
              </div>
            </Show>
            <For each={rows()}>
              {(it, i) => (
                <ItemView item={it} session={s()} paneId={props.paneId} lastTurn={lastTurnKey() === it.key} settled={i() < rows().length - TAIL} enter={entering(it)} />
              )}
            </For>
            <For each={orphanRunning()}>{(r) => <ToolCard view={toolView(s(), r.call_id, r.name, {})} session={s()} />}</For>
            <For each={s().pending}>{(p) => <UserMessage content={p.content} pending />}</For>
            <For each={s().approvals}>{(a) => <ApprovalCard sessionId={s().info.id} request={a} />}</For>
            <ChatAsks session={s()} />
            <Show when={s().info.status === "interrupted" && !running()}>
              <NoticeLine level="warning" text="This chat was interrupted (the server restarted mid-turn). Send a message to pick up where it left off." />
            </Show>
            <div class="transcript-end" />
          </div>
        </div>
        {/* While the agent works, the status line above the composer offers "Jump to live". */}
        <Show when={detached() && !live()}>
          <button type="button" class="jump-latest" aria-label="Jump to latest" onClick={jumpToLatest}>
            <Icon name="arrowDown" size={13} />
            <span>{newCount() > 0 ? `${newCount()} new ${newCount() === 1 ? "message" : "messages"}` : "Jump to latest"}</span>
          </button>
        </Show>
      </div>
    </DurationContext.Provider>
  );
}


/**
 * Finished items away from the live tail may skip rendering while off screen
 * (content-visibility), which keeps paint cheap in long chats. Seed the
 * fallback with the first layout's real height, before offscreen rows can
 * skip layout and before `auto` has remembered any size.
 */
function ItemView(props: { item: Row; session: SessionState; paneId: string; lastTurn: boolean; settled: boolean; enter: boolean }) {
  const node = (<ItemBody item={props.item} session={props.session} paneId={props.paneId} lastTurn={props.lastTurn} />) as unknown;
  // Hot reloading (dev) hands back an accessor instead of the element itself.
  const el = () => {
    const n = typeof node === "function" ? (node as () => unknown)() : node;
    return n instanceof HTMLElement ? n : undefined;
  };
  const first = el();
  if (untrack(() => props.enter) && first) {
    first.classList.add("enter");
    first.addEventListener("animationend", () => first.classList.remove("enter"), { once: true });
  }
  const [laidOut, setLaidOut] = createSignal(false);
  onMount(() => requestAnimationFrame(() => requestAnimationFrame(() => {
    const n = el();
    // `auto` remembers sizes only once containment is active, not the layout
    // before adding it. Seed the fallback so offscreen prepends never use 80px.
    if (n) n.style.containIntrinsicSize = `auto ${n.offsetHeight}px`;
    setLaidOut(true);
  })));
  createEffect(() => el()?.classList.toggle("settled", props.settled && laidOut()));
  // Being rewound: fade out before the store drops it.
  const it = props.item;
  const seq = () => (it.k !== "live" ? itemSeq(it) : it.item() ? itemSeq(it.item()!) : Infinity);
  createEffect(() => {
    const r = props.session.rewind;
    el()?.classList.toggle("leaving", !!r && seq() > r.from);
  });
  return node as JSX.Element;
}

function ItemBody(props: { item: Row; session: SessionState; paneId: string; lastTurn: boolean }) {
  const it = props.item;
  switch (it.k) {
    case "live":
      return <LiveRowView row={it} session={props.session} />;
    case "speaker":
      return (
        <div class="item item-speaker" data-seq={it.seq}>
          <Speaker author={it.author} ts={it.ts} />
        </div>
      );
    case "authored":
      return (
        <div class="item">
          <AuthoredMessage entry={it.entry} author={it.author} />
        </div>
      );
    case "report":
      return (
        <div class="item" data-seq={it.entry.seq}>
          <ReportCardSlot entry={it.entry} childId={it.childId} />
        </div>
      );
    case "goal":
      return (
        <div class="item" data-seq={it.entry.seq}>
          <GoalNote entry={it.entry} />
        </div>
      );
    case "harness":
      return (
        <div class="item" data-seq={it.entry.seq}>
          <HarnessNote entry={it.entry} />
        </div>
      );
    case "reports":
      return (
        <div class="item" data-seq={it.items[0].entry.seq}>
          <ReportGroupSlot items={it.items} />
        </div>
      );
    case "user":
      return (
        <div class="item item-user">
          <UserMessage content={it.entry.content} ts={it.entry.ts} seq={it.entry.seq} session={props.session} paneId={props.paneId} />
        </div>
      );
    case "text":
    case "narr":
      return (
        <div class="item">
          <AssistantText text={it.text} seq={it.entry.seq} />
        </div>
      );
    case "thinking":
      return (
        <div class="item">
          <ThinkingBlock text={it.text} ms={props.session.thinkMs[it.entry.seq]} />
        </div>
      );
    case "tool":
      return (
        <div class="item">
          <ToolCard view={toolView(props.session, it.call.id, it.call.name, it.call.arguments)} session={props.session} ts={it.entry.ts} />
        </div>
      );
    case "steps":
      return (
        <div class="item">
          <StepsGroup items={it.items} tools={it.tools} session={props.session} />
        </div>
      );
    case "notice": {
      // `learned` is optional in practice: older entries don't have it.
      const learned = it.entry.level === "info" ? learnedItems(it.entry.text, (it.entry as { learned?: LearnedItem[] }).learned) : null;
      const secrets = savedSecrets(it.entry);
      return (
        <div class="item" data-seq={it.entry.seq}>
          <Show when={!secrets} fallback={<SecretNotice saved={secrets!} text={it.entry.text} sessionId={props.session.info.id} />}>
            <Show when={learned} fallback={<NoticeLine level={it.entry.level} text={it.entry.text} />}>
              <LearnedNotice items={learned!} />
            </Show>
          </Show>
        </div>
      );
    }
    case "compaction":
      return (
        <div class="item">
          <CompactionDivider entry={it.entry} />
        </div>
      );
    case "stop":
      return (
        <div class="item">
          <StopLine entry={it.entry} />
        </div>
      );
    case "board":
      return (
        <div class="item">
          <BoardCallCard session={props.session} callId={it.call.id} args={it.call.arguments} />
        </div>
      );
    case "file":
      return (
        <div class="item">
          <SharedFileCall session={props.session} callId={it.call.id} args={it.call.arguments} />
        </div>
      );
    case "note":
      return (
        <div class="item">
          <NoteCallCard session={props.session} callId={it.call.id} args={it.call.arguments} />
        </div>
      );
    case "orphan":
      return (
        <div class="item">
          <ToolCard view={toolView(props.session, it.entry.call_id, it.entry.tool, {})} session={props.session} />
        </div>
      );
    case "turn":
      return (
        <div class="item item-foot">
          <TurnFooter entry={it.entry} first={it.first} session={props.session} last={props.lastTurn} check={it.check} />
        </div>
      );
  }
}

/**
 * A part of the reply being streamed: what the stream has so far, then (same
 * components, same DOM) the committed item it became.
 */
function LiveRowView(props: { row: LiveRow; session: SessionState }) {
  const row = props.row;
  const s = () => props.session;
  const st = () => s().streaming;
  const done = row.item;
  const part = () => (done() ? undefined : st()?.content[row.index]);
  const text = () => {
    const d = done();
    if (d && (d.k === "text" || d.k === "narr" || d.k === "thinking")) return d.text;
    const p = part();
    return p && p.type !== "tool_call" ? p.text : "";
  };
  const seq = () => {
    const d = done();
    return d && "entry" in d ? d.entry.seq : undefined;
  };
  switch (row.part) {
    case "speaker": {
      const author = () => {
        const d = done();
        return d?.k === "speaker" ? d.author : (st()?.author ?? s().info.bot_id ?? "");
      };
      const typing = () => !done() && !!st() && !st()!.ended && !st()!.content.some((p) => p?.type === "text" && p.text);
      return (
        <div class="item item-speaker" data-seq={seq()}>
          <Speaker author={author()} ts={done()?.k === "speaker" ? (done() as { ts: number }).ts : undefined} typing={typing()} />
        </div>
      );
    }
    case "thinking": {
      const ms = () => {
        const d = st();
        if (done()) return s().thinkMs[seq()!];
        return d?.thinkEnd && d.thinkStart ? d.thinkEnd - d.thinkStart : undefined;
      };
      // A run of reasoning parts shows as one block, on the run's first row (as deriveItems does once committed).
      const lead = () => !!done() || !continuesThinking(st()?.content ?? [], row.index);
      const shown = () => (done() ? text() : thinkingRun(st()?.content ?? [], row.index));
      return (
        <Show when={lead() && shown()}>
          <div class="item">
            <ThinkingBlock text={shown()} live={!done() && !!st() && !st()!.ended && !st()!.thinkEnd} startedAt={done() ? undefined : st()?.thinkStart} ms={ms()} />
          </div>
        </Show>
      );
    }
    case "text":
      return (
        <div class="item">
          <AssistantText text={text()} seq={seq()} streaming={!done() && !!st() && !st()!.ended} />
        </div>
      );
    case "tool": {
      const view = () => {
        const d = done();
        if (d?.k === "tool") return toolView(s(), d.call.id, d.call.name, d.call.arguments);
        const p = part();
        const c = p?.type === "tool_call" ? p : undefined;
        return toolView(s(), c?.id ?? row.callId ?? "", c?.name ?? "", c?.arguments ?? {}, true);
      };
      return (
        <div class="item">
          <ToolCard view={view()} session={s()} ts={done()?.k === "tool" ? (done() as { entry: { ts: number } }).entry.ts : undefined} />
        </div>
      );
    }
  }
}
