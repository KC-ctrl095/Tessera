import { For, Show, Suspense, lazy, createContext, createEffect, createMemo, createSignal, onCleanup, onMount, useContext, type JSX } from "solid-js";
import type { ApprovalRequest } from "../../proto/ApprovalRequest";
import { isBusy, plainText, type LiveTool, type SessionState, type ToolResultEntry } from "../../lib/store";
import { argsObject, detailsDiff, detailsExitCode, diffStats, groupSummary, prettyArgs, quietTool, shellOutput, toolMeta, toolPresentation, toolSummary } from "../../lib/tools";
import type { StepItem } from "../../lib/transcript";
import { Follow } from "../../lib/follow";
import { fmtDuration } from "../../lib/util";
import { Icon, Tessellate } from "../icons";
import { CopyButton } from "../primitives";
const DiffView = lazy(() => import("./Diff").then((m) => ({ default: m.DiffView })));
import { Markdown } from "./Markdown";
import { BotHandoffSlot, SubagentCardsSlot, handoffOf, isHelperStart } from "./SubagentGate";
import { ThinkingBlock, useTicker } from "./Thinking";
import { isBoardTool, watchTab, watchedBoard } from "../../lib/boards";
import { ensureMachines, isMachineTool, machineCall } from "../../lib/machines";
import "../../styles/machines.css";
import { BoardCallCard, showsBoard } from "../board/BoardCard";
import { state } from "../../lib/store";
import { openBoard, paneShowingBoard, workspace } from "../../lib/workspace";
import { onDemand } from "../../lib/onDemand";
import { NoteCallCard, NoteOpenButton, isNoteTool, makesNote } from "./NoteCard";

const SecretEntry = lazy(() => import("./SecretEntry"));
import { PartError } from "../PartError";
const recallCard = onDemand(() => import("./RecallCard"), "RecallCard");

export type ToolStatus = "preparing" | "pending" | "approval" | "running" | "ok" | "error" | "unknown" | "stopped";

export interface ToolView {
  id: string;
  name: string;
  /** Parsed object (persisted) or partial JSON string (streaming). */
  args: unknown;
  result?: ToolResultEntry;
  live?: LiveTool;
  approval?: ApprovalRequest;
  status: ToolStatus;
}

/** Tool durations observed live or persisted with tool results. */
export const DurationContext = createContext<(callId: string) => number | undefined>(() => undefined);

export function toolView(s: SessionState, id: string, name: string, args: unknown, preparing = false): ToolView {
  const result = s.results[id];
  const live = s.running[id];
  const approval = s.approvals.find((a) => a.call_id === id);
  const details = result?.details as { interrupted?: boolean; stopped?: boolean; status?: string; subagent?: { status?: string } } | undefined;
  const status: ToolStatus = preparing
    ? "preparing"
    : result
      ? details?.stopped || details?.status === "stopped" || details?.subagent?.status === "stopped"
        ? "stopped"
        : details?.interrupted
          ? "unknown"
          : result.is_error
        ? "error"
        : "ok"
      : approval
        ? "approval"
        : live
          ? "running"
          : isBusy(s.info)
            ? "pending"
            : "unknown";
  return { id, name, args, result, live, approval, status };
}

const STATUS_LABEL: Record<ToolStatus, string> = {
  preparing: "Preparing",
  pending: "Waiting",
  approval: "Needs approval",
  running: "Running",
  ok: "Done",
  error: "Failed",
  stopped: "Stopped",
  unknown: "Outcome unknown",
};

/** A tessera: working (the indicator), done (verdigris), failed (terracotta), waiting (hollow). */
export function StatusSquare(props: { status: ToolStatus }) {
  return (
    <Show when={props.status === "running" || props.status === "preparing"} fallback={<span class={`sq sq-${props.status === "stopped" ? "error" : props.status}`} role="img" aria-label={STATUS_LABEL[props.status]} />}>
      <Tessellate size={9} label={STATUS_LABEL[props.status]} />
    </Show>
  );
}

/**
 * Output that follows its own tail. New output is appended to the text node
 * (no rebuilding the whole text per chunk), and the scroll to the end happens
 * when the text's box has grown, after layout: never a forced layout. The
 * reader scrolling inside it stops that (lib/follow.ts).
 */
function LiveOutput(props: { text: string; class?: string }) {
  let el!: HTMLPreElement;
  let body!: HTMLSpanElement;
  const node = document.createTextNode(" ");
  let shown = " ";
  createEffect(() => {
    const t = props.text || " ";
    if (t.length > shown.length && shown !== " " && t.startsWith(shown)) node.appendData(t.slice(shown.length));
    else if (t !== shown) node.data = t;
    shown = t;
  });
  onMount(() => {
    // Follows its end until the reader scrolls inside it; back at its end, it follows again.
    const follow = new Follow(el);
    const ro = new ResizeObserver(() => follow.stick());
    ro.observe(body);
    onCleanup(() => {
      ro.disconnect();
      follow.dispose();
    });
  });
  return (
    <pre ref={el} class={"tool-live" + (props.class ? " " + props.class : "")}>
      <span ref={body} class="tool-live-text">
        {node}
      </span>
    </pre>
  );
}

/** The last `lines` lines of `s` (without its final line break), found from the end. */
function tail(s: string, lines: number): string {
  const end = s.endsWith("\n") ? s.length - 1 : s.length;
  let start = end;
  for (let k = 0; k < lines; k++) {
    const nl = start > 0 ? s.lastIndexOf("\n", start - 1) : -1;
    if (nl < 0) return s.slice(0, end);
    start = nl;
  }
  return s.slice(start + 1, end);
}

export function ToolCard(props: { view: ToolView; defaultOpen?: boolean; flat?: boolean; session?: SessionState; ts?: number }) {
  // A whiteboard being put up streams in as its card, not as a tool row.
  if (props.session && isBoardTool(props.view.name) && showsBoard(props.view.args) && !props.flat)
    return <BoardCallCard session={props.session} callId={props.view.id} args={props.view.args} preparing={props.view.status === "preparing"} />;
  // So is a note an agent writes (a card with "Open"; no tile pops up).
  if (props.session && makesNote(props.view.name, props.view.args) && !props.flat)
    return <NoteCallCard session={props.session} callId={props.view.id} args={props.view.args} preparing={props.view.status === "preparing"} />;
  // Keep the ordinary tool controls usable while recall's code and styles load.
  if (props.view.name === "recall") {
    const [open, setOpen] = createSignal(!!props.defaultOpen);
    if (!recallCard.error()) void recallCard.load();
    return <>
      <Show when={recallCard.get()} fallback={<GenericToolCard {...props} open={open()} onOpenChange={setOpen} />}>
        {(m) => {
          const Card = m().RecallCard;
          return <Card {...props} open={open()} onOpenChange={setOpen} />;
        }}
      </Show>
      <Show when={!recallCard.get()}>
        <Show when={recallCard.error()} fallback={<p class="tool-note" role="status" aria-busy="true">Loading recall controls…</p>}>
          <PartError error={recallCard.error()} retry={recallCard.retry} />
        </Show>
      </Show>
    </>;
  }
  return <GenericToolCard {...props} />;
}

function GenericToolCard(props: { view: ToolView; defaultOpen?: boolean; flat?: boolean; session?: SessionState; ts?: number; open?: boolean; onOpenChange?: (open: boolean) => void }) {
  const v = () => props.view;
  const presentation = createMemo(() => toolPresentation(v().name, v().args));
  const meta = createMemo(() => toolMeta(presentation().kind));
  const summary = createMemo(() => toolSummary(v().name, v().args));
  const [localOpen, setLocalOpen] = createSignal(!!props.defaultOpen);
  const open = () => props.open ?? localOpen();
  const setOpen = (value: boolean) => props.onOpenChange ? props.onOpenChange(value) : setLocalOpen(value);
  const diff = createMemo(() => detailsDiff(v().result?.details));
  const stats = createMemo(() => (diff() ? diffStats(diff()!) : null));
  const exit = createMemo(() => detailsExitCode(v().result?.details));
  const bash = () => presentation().kind === "bash";
  const rawOutput = createMemo(() => (v().result ? plainText(v().result!.content) : v().live?.output ?? ""));
  const shell = () => shellOutput(rawOutput(), v().result?.details);
  const output = () => bash() ? shell().output : rawOutput();
  // A call on one of the user's machines: the machine's name where the tool's would be.
  const machine = createMemo(() => (isMachineTool(v().name) ? machineCall(v().args) : null));
  if (isMachineTool(props.view.name)) ensureMachines();
  const isShell = () => meta().icon === "terminal" || machine()?.action === "run";
  const command = createMemo(() => {
    const o = argsObject(presentation().args);
    const c = o?.command ?? o?.cmd;
    return typeof c === "string" ? c : undefined;
  });

  const durationOf = useContext(DurationContext);
  const now = useTicker(() => v().status === "running");
  const elapsed = () => (v().live ? Math.max(0, now() - v().live!.startedAt) : 0);
  const done = () => durationOf(v().id);
  const showTail = () => v().status === "running" && !open() && !!v().live?.output && !isAgent();
  const isAgent = () => isHelperStart(v().name, v().args);

  const browser = () => /^browser$/i.test(v().name);
  // Only the chat's latest browser step offers its tile (one button, not one per step).
  const latest = () => !!props.session && (open() || lastBrowserCall(props.session) === v().id);
  const tile = createMemo(() => (browser() && props.session && latest() ? browserBoard(props.session, v()) : null));
  // The agent works in background tabs: its step shows a small picture of
  // the page, and the latest one lets you watch it live (BrowserPeek).
  const step = createMemo(() => (browser() ? browserStepDetails(v().result?.details) : null));
  const entry = createMemo(() => {
    const d = v().result?.details as Record<string, unknown> | undefined;
    const e = d?.secret_entry;
    return v().name === "save_secret" && !v().result?.is_error && e && typeof e === "object" ? e as Record<string, unknown> : null;
  });

  return (
    <div class="tool" data-status={v().status} classList={{ open: open(), flat: !!props.flat, "tool-browser": browser() }}>
      <div class="tool-row">
      <button type="button" class="tool-head" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <span class="tool-st">
          <StatusSquare status={v().status} />
        </span>
        <Show
          when={browser()}
          fallback={
            <Show when={machine()?.machine} fallback={<span class="tool-name">{presentation().kind || "tool"}</span>}>
              <span class="tool-name tool-machine" title={`On your machine ${machine()!.machine}`}>
                <Icon name="laptop" size={12} />
                {machine()!.machine}
              </span>
            </Show>
          }
        >
          <span class="tool-name tool-name-icon" aria-label="browser" data-tip="The browser">
            <Icon name="globe" size={13} />
          </span>
        </Show>
        <span class="tool-sum" title={machine()?.machine ? `On ${machine()!.machine}: ${machine()!.text}` : summary()}>
          {(machine()?.machine ? machine()!.text : summary()) || (v().status === "preparing" ? "preparing…" : "")}
        </span>
        <span class="tool-meta">
          <Show when={stats()}>
            <span class="diffstat">
              <span class="add">+{stats()!.add}</span>
              <span class="del">−{stats()!.del}</span>
            </span>
          </Show>
          <Show when={exit() !== undefined && exit() !== 0}>
            <span class="badge badge-err">exit {exit()}</span>
          </Show>
          <Show when={v().status === "approval" && !props.session?.approvals.some((a) => a.call_id === v().id)}>
            <span class="badge badge-warn">needs approval</span>
          </Show>
          <Show when={v().status === "running" && elapsed() > 1500}>
            <span class="tool-time live">{fmtDuration(elapsed())}</span>
          </Show>
          <Show when={(v().status === "ok" || v().status === "error") && done() !== undefined && done()! >= 1000}>
            <span class="tool-time">{fmtDuration(done()!)}</span>
          </Show>
        </span>
        <Icon name="chevronDown" size={13} class="chev" />
      </button>
      <Show when={props.session && step()?.tab}>
        <BrowserPeek session={props.session!} callId={v().id} always={open()} onThumb={() => setOpen(!open())} />
      </Show>
      {/* The browser's tab is a tile: one tap to see it (or what the agent is doing there). */}
      <Show when={tile() && !tile()!.onScreen}>
        <ShowTile id={tile()!.id} />
      </Show>
      <Show when={isNoteTool(v().name) && v().result && !v().result!.is_error}>
        <NoteOpenButton details={v().result!.details} />
      </Show>
      </div>
      <Show when={props.session && entry()}>
        <Suspense fallback={<p class="tool-note" role="status">Loading secure entry…</p>}><SecretEntry sessionId={props.session!.info.id} callId={v().id} entry={entry()!} /></Suspense>
      </Show>

      {/* Work handed to a bot in the background: the bot at work on it, Stop and Open. */}
      <Show when={props.session && v().name === "message_bot" && handoffOf(v().result?.details)}>
        <BotHandoffSlot session={props.session!} view={v()} />
      </Show>

      <Show when={isAgent() && props.session}>
        <SubagentCardsSlot session={props.session!} view={v()} ts={props.ts} />
      </Show>

      <Show when={showTail()}>
        <LiveOutput text={tail(v().live!.output, 6)} class="tail" />
      </Show>

      <Show when={open()}>
        <div class="tool-body">
          <Show
            when={isShell() && command()}
            fallback={
              <Show when={!diff() || !v().result}>
                <Section title="Input" copy={() => prettyArgs(presentation().args)}>
                  <pre class="tool-pre">{prettyArgs(presentation().args)}</pre>
                </Section>
              </Show>
            }
          >
            <Section title="Command" copy={() => command()!}>
              <pre class="tool-pre cmd">
                <span class="prompt">$ </span>
                {command()}
              </pre>
            </Section>
          </Show>
          <details class="tool-audit">
            <summary>Execution details</summary>
            <Section title={v().name} copy={() => prettyArgs(v().args)}><pre class="tool-pre">{prettyArgs(v().args)}</pre></Section>
            <Show when={rawOutput() !== output()}><Section title="Raw result"><pre class="tool-pre">{rawOutput()}</pre></Section></Show>
          </details>
          <Show when={bash() && shell().status}><div class="tool-meta">{shell().status}</div></Show>
          <Show when={diff()}>
            <Section title="Changes" copy={() => diff()!}>
              <Suspense fallback={<pre class="tool-output">{diff()}</pre>}><DiffView diff={diff()!} /></Suspense>
            </Section>
          </Show>
          <Show when={v().status === "running"}>
            <Section title="Output · live">
              <LiveOutput text={v().live?.output ?? ""} />
            </Section>
          </Show>
          <Show when={v().result && (!diff() || output().length < 400)}>
            <Section title={v().result!.is_error ? "Error" : "Output"} copy={output}>
              <ResultText text={output()} error={v().result!.is_error} />
              <Show when={v().result!.artifact}>
                <div class="tool-note">Output truncated · full output id {v().result!.artifact}</div>
              </Show>
            </Section>
          </Show>
          <Show when={step()?.shot}>
            {(shot) => (
              <div class="tool-images">
                <img class="tool-shot" src={`data:${shot().mime};base64,${shot().data}`} alt="The page after this step" />
              </div>
            )}
          </Show>
          <Show when={v().result?.content.some((c) => c.type === "image")}>
            <div class="tool-images">
              <For each={v().result!.content.filter((c) => c.type === "image")}>
                {(img) => img.type === "image" && <img src={`data:${img.mime};base64,${img.data}`} alt="Tool output image" decoding="async" />}
              </For>
            </div>
          </Show>
        </div>
      </Show>
    </div>
  );
}

function ResultText(props: { text: string; error: boolean }) {
  const LIMIT = 6000;
  const [all, setAll] = createSignal(false);
  const shown = () => (all() || props.text.length <= LIMIT ? props.text : props.text.slice(0, LIMIT));
  return (
    <>
      <pre class="tool-pre" classList={{ err: props.error }}>
        {shown() || "(no output)"}
      </pre>
      <Show when={!all() && props.text.length > LIMIT}>
        <button type="button" class="link-btn" onClick={() => setAll(true)}>
          Show all ({Math.round(props.text.length / 1000)}k characters)
        </button>
      </Show>
    </>
  );
}

function Section(props: { title: string; copy?: () => string; children: JSX.Element }) {
  return (
    <div class="tool-sec">
      <div class="tool-sec-head">
        <span>{props.title}</span>
        <Show when={props.copy}>
          <CopyButton text={props.copy!} label={`Copy ${props.title.toLowerCase()}`} size={13} class="sm" />
        </Show>
      </div>
      {props.children}
    </div>
  );
}

/** A finished run of steps, collapsed to "7 steps · 14s" with a strip of tesserae; expands into the rows. */
export function StepsGroup(props: { items: StepItem[]; tools: string[]; session: SessionState }) {
  const [open, setOpen] = createSignal(false);
  const durationOf = useContext(DurationContext);
  const visible = createMemo(() => props.items.filter((it) => it.k !== "tool" || !quietTool(it.call.name, it.call.arguments, props.session.results[it.call.id])));
  const names = () => visible().filter((it) => it.k === "tool").map((it) => it.call.name);
  const statuses = createMemo(() =>
    visible().map((it) => (it.k === "tool" ? toolView(props.session, it.call.id, it.call.name, it.call.arguments).status : it.k === "thinking" ? "think" : "text")),
  );
  const failed = createMemo(() => statuses().filter((s) => s === "error").length);
  // The run's latest browser step that finished in a tab, if any.
  const browsed = createMemo(() => {
    for (let i = props.items.length - 1; i >= 0; i--) {
      const it = props.items[i];
      if (it.k === "tool" && /^browser$/i.test(it.call.name) && browserStepDetails(props.session.results[it.call.id]?.details)?.tab) return it.call.id;
    }
    return null;
  });
  const total = createMemo(() => {
    let ms = 0;
    let known = false;
    for (const it of visible()) {
      if (it.k === "tool") {
        const d = durationOf(it.call.id);
        if (d !== undefined) {
          ms += d;
          known = true;
        }
      } else if (it.k === "thinking") {
        const t = props.session.thinkMs[it.entry.seq];
        if (t) {
          ms += t;
          known = true;
        }
      }
    }
    return known ? ms : undefined;
  });
  const n = () => names().length;
  return (
    <div class="steps" classList={{ open: open() }}>
      <div class="steps-row">
      <button type="button" class="steps-head" aria-expanded={open()} onClick={() => setOpen(!open())} title={groupSummary(names())}>
        <span class="steps-strip" aria-hidden="true">
          <For each={statuses().slice(0, 24)}>{(st) => <i class={`sq sq-${st === "stopped" ? "error" : st}`} />}</For>
        </span>
        <span class="steps-sum">
          {n() ? `${n()} ${n() === 1 ? "step" : "steps"}` : "Work state"}
          <Show when={total() !== undefined && total()! >= 1000}>
            <span class="steps-time"> · {fmtDuration(total()!)}</span>
          </Show>
        </span>
        <span class="steps-detail">{groupSummary(names())}</span>
        <Show when={failed() > 0}>
          <span class="badge badge-err">{failed()} failed</span>
        </Show>
        <Icon name="chevronDown" size={13} class="chev" />
      </button>
      {/* Folded browsing keeps its latest picture and Watch in view. */}
      <Show when={!open() && browsed()}>
        {(id) => <BrowserPeek session={props.session} callId={id()} tile onThumb={() => setOpen(true)} />}
      </Show>
      </div>
      <Show when={open()}>
        <ol class="timeline">
          <For each={props.items}>
            {(it) => (
              <li class="tl-item" data-k={it.k}>
                {it.k === "thinking" ? (
                  <ThinkingBlock text={it.text} ms={props.session.thinkMs[it.entry.seq]} />
                ) : it.k === "narr" ? (
                  <div class="msg-assistant narr">
                    <Markdown text={it.text} />
                  </div>
                ) : (
                  <ToolCard view={toolView(props.session, it.call.id, it.call.name, it.call.arguments)} flat session={props.session} ts={it.entry.ts} />
                )}
              </li>
            )}
          </For>
        </ol>
      </Show>
    </div>
  );
}

/**
 * A browser step's small picture of the page and, on the chat's latest step
 * that finished in a background tab (or `always`), "Watch": the tab becomes
 * a web view of yours, opened where you are. The picture watches too, or
 * calls `onThumb` where there is nothing to watch.
 */
function BrowserPeek(props: { session: SessionState; callId: string; always?: boolean; tile?: boolean; onThumb?: () => void }) {
  const view = () => toolView(props.session, props.callId, "browser", {});
  const st = createMemo(() => browserStepDetails(props.session.results[props.callId]?.details));
  // `tile` (a folded run): its web view, once there is one, comes back from here too.
  const board = createMemo(() => (props.tile && lastBrowserCall(props.session, true) === props.callId ? browserBoard(props.session, view()) : null));
  // Stays on the latest finished step while the next one runs (no flicker between steps).
  const canWatch = () =>
    !props.session.info.private && !!st()?.tab && (props.always || lastBrowserCall(props.session, true) === props.callId) && !browserBoard(props.session, view());
  const [busy, setBusy] = createSignal(false);
  const watch = async () => {
    const d = st();
    if (!d?.tab || busy()) return;
    setBusy(true);
    try {
      await watchTab(props.session.info.id, d.tab, d.url ? { url: d.url, title: d.title } : undefined);
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <Show when={st()?.shot}>
        {(shot) => (
          <button
            type="button"
            class="tool-thumb"
            aria-label={canWatch() ? "Watch the page live" : "What the page looked like"}
            data-tip={canWatch() ? "Watch live" : undefined}
            onClick={() => (canWatch() ? void watch() : props.onThumb?.())}
          >
            <img src={`data:${shot().mime};base64,${shot().data}`} alt="" />
          </button>
        )}
      </Show>
      <Show when={canWatch()}>
        {/* On narrow rows only the eye shows (the label goes, see .tool-show-label). */}
        <button type="button" class="tool-show tool-watch" aria-label="Watch" data-tip="Watch the page live" disabled={busy()} onClick={() => void watch()}>
          <Icon name="eye" size={12} />
          <span class="tool-show-label">Watch</span>
        </button>
      </Show>
      <Show when={board() && !board()!.onScreen}>
        <ShowTile id={board()!.id} />
      </Show>
    </>
  );
}

/** "Show tile" for a browser step's web view (just the globe on narrow rows). */
function ShowTile(props: { id: string }) {
  return (
    <button type="button" class="tool-show" aria-label="Show tile" data-tip="Show its tile" onClick={() => openBoard(props.id)}>
      <Icon name="globe" size={12} />
      <span class="tool-show-label">Show tile</span>
    </button>
  );
}

/** What a browser step's result says about its tab (`details`). */
function browserStepDetails(details: unknown): { tab?: string; boardId?: string; url?: string; title?: string; shot?: { mime: string; data: string } } | null {
  const d = details as Record<string, unknown> | null | undefined;
  if (!d || typeof d !== "object") return null;
  const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);
  const shot = d.shot as { mime?: unknown; data?: unknown } | undefined;
  return {
    tab: str(d.tab),
    boardId: str(d.board_id),
    url: str(d.url),
    title: str(d.title),
    shot: shot && typeof shot.mime === "string" && typeof shot.data === "string" ? { mime: shot.mime, data: shot.data } : undefined,
  };
}

/**
 * The web view a browser step happened in: the one its result names (or the
 * one its background tab became when you watched it), else (older results
 * that don't say) the chat's web view showing the address it opened, else
 * the chat's latest. A step in a background tab has none.
 */
function browserBoard(session: SessionState, view: ToolView): { id: string; onScreen: boolean } | null {
  const d = (view.result?.details ?? null) as Record<string, unknown> | null;
  const named = (typeof d?.board_id === "string" && d.board_id) || (d?.board && typeof (d.board as { id?: unknown }).id === "string" ? (d.board as { id: string }).id : "") || (typeof d?.tab === "string" ? watchedBoard(d.tab) ?? "" : "");
  if (!named && typeof d?.tab === "string") return null;
  const mine = Object.values(state.boards).filter((b) => (b.content.type === "url" || b.content.type === "local") && b.session_id === session.info.id);
  const args = argsObject(view.args);
  const url = typeof args?.url === "string" ? args.url : "";
  const b =
    (named && state.boards[named]) ||
    (url && mine.find((x) => x.content.type === "url" && x.content.url.replace(/\/$/, "") === url.replace(/\/$/, ""))) ||
    mine.sort((x, y) => y.updated_at - x.updated_at)[0];
  if (!b) return null;
  const pane = paneShowingBoard(b.id);
  return { id: b.id, onScreen: !!pane && workspace.paneWorkspace(pane) === workspace.active() };
}

/** The id of the chat's most recent browser step (`done`: the latest that finished in a tab). */
function lastBrowserCall(session: SessionState, done = false): string | null {
  const ok = (id: string) => !done || typeof (session.results[id]?.details as { tab?: unknown } | null | undefined)?.tab === "string";
  const live = session.streaming?.content;
  if (live) for (let i = live.length - 1; i >= 0; i--) {
    const p = live[i];
    if (p.type === "tool_call" && /^browser$/i.test(p.name) && ok(p.id)) return p.id;
  }
  for (let i = session.entries.length - 1; i >= 0; i--) {
    const e = session.entries[i];
    if (e.kind !== "assistant") continue;
    for (let j = e.content.length - 1; j >= 0; j--) {
      const p = e.content[j];
      if (p.type === "tool_call" && /^browser$/i.test(p.name) && ok(p.id)) return p.id;
    }
  }
  return null;
}
