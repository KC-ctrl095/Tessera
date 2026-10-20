import { For, Show, createEffect, createMemo, createSignal, onMount, untrack } from "solid-js";
import type { Entry } from "../../proto/Entry";
import type { SessionInfo } from "../../proto/SessionInfo";
import { handoffReply } from "../../lib/handoffIndex";
import { abortSession } from "../../lib/actions";
import { toastError } from "../../lib/ui";
import { api, botById, ensureLoaded, isBusy, onSessionRemoved, peekSession, plainText, state, type SessionState } from "../../lib/store";
import { argsObject, toolPresentation } from "../../lib/tools";
import { NO_BOT_ANSWER, parseReport, type BotReply, type UserEntry } from "../../lib/transcript";
import { fmtDuration } from "../../lib/util";
import { openSession } from "../../lib/workspace";
import { Icon, Tessellate } from "../icons";
import { VirtualList } from "../VirtualList";
import { currentActivity } from "./Activity";
import { Markdown } from "./Markdown";
import { useTicker } from "./Thinking";
import type { ToolView } from "./ToolCard";
import { botsWorkingFor, childName, childSessions, detailsChildren, handoffOf, refsForCall, roleLabel, sameRefs, type ChildRef } from "./SubagentGate";
import { HelperCard } from "./HelperCard";

/** Never silently turn a failed child Stop into Stop the entire parent chat. */
async function stopChild(info: SessionInfo) {
  try { await api.abort(info.id); } catch (e) { toastError(e); }
}

/**
 * Live cards for the subagents an `agent` call started: name, status, steps,
 * elapsed time and what each is doing now, with Stop and Open.
 */
export function SubagentCards(props: { session: SessionState; view: ToolView; ts?: number }) {
  const refs = createMemo(() => refsForCall(props.session, props.view, props.ts), [], { equals: sameRefs });
  return (
    <Show when={refs().length} fallback={<Show when={toolPresentation(props.view.name, props.view.args).helper === "read_only"}><MetadataHelperCard view={props.view} /></Show>}>
      <Show when={refs().length > SWARM} fallback={
        <div class="subagents" role="list" aria-label="Subagents">
          <For each={refs()}>{(r) => <SubagentCard parent={props.session} child={r} />}</For>
        </div>
      }>
        <SwarmGroup parent={props.session} refs={refs()} />
      </Show>
    </Show>
  );
}

/** More than this many subagents collapse into one summary row (a swarm). */
const SWARM = 6;

type ChildStatus = "running" | "waiting" | "ok" | "error" | "unknown";

// One incremental history index per parent, shared by every helper status and
// report card. Subscribe to history boundaries, never to every old content part.
const helperHistories = new Map<string, {
  entries: readonly Entry[]; first: Entry | undefined; length: number; version: number;
  seen: Set<string>; controls: Set<string>; reports: Map<string, string>;
}>();
onSessionRemoved((id) => helperHistories.delete(id));

function helperHistory(parent: SessionState) {
  const entries = parent.entries, length = entries.length, first = entries[0], version = parent.entriesVersion;
  const id = parent.info.id;
  let index = helperHistories.get(id);
  if (!index || index.entries !== entries || index.first !== first || length < index.length || index.version !== version) {
    index = { entries, first, length: 0, version, seen: new Set(), controls: new Set(), reports: new Map() };
    helperHistories.set(id, index);
  }
  const current = index;
  untrack(() => {
    for (let i = current.length; i < length; i++) {
      const e = entries[i];
      if (e.kind === "assistant") {
        for (const p of e.content) if (p.type === "tool_call" && !current.seen.has(p.id)) {
          current.seen.add(p.id);
          if (toolPresentation(p.name, p.arguments).helper === "controls") current.controls.add(p.id);
        }
      } else if (e.kind === "user" && e.author?.startsWith("subagent:")) {
        current.reports.set(e.author.slice("subagent:".length), parseReport(plainText(e.content)).body);
      }
    }
    current.length = length;
  });
  return current;
}

/** Where a subagent is: running, waiting for a slot (idle, no steps yet), done or failed. */
export function childStatus(parent: SessionState, r: ChildRef): ChildStatus {
  const info = r.sessionId ? state.sessions[r.sessionId]?.info : undefined;
  if (info) {
    // Waiting for its turn (its chat's limit, the model or the server's room).
    if (info.helper_queued && isBusy(info)) return "waiting";
    if (isBusy(info)) return "running";
    if (info.status === "error" || info.status === "interrupted") return "error";
    // Finished helpers never subscribe to or rescan their parent's history.
    if (info.status !== "idle" || info.steps !== 0) return "ok";
    const foregroundDone = !!info.parent_call && !!detailsChildren(parent.results[info.parent_call]?.details)?.some((c) => c.sessionId === info.id && !c.background);
    if (foregroundDone || reportOf(parent, r.sessionId!)) return "ok";
    // Legacy control-shaped Agent calls can also be Claude-owned. Keep their
    // original waiting rule without a separate full-history scan per helper.
    const native = info.helper_owner === "tessera" || !info.parent_call || helperHistory(parent).controls.has(info.parent_call);
    if (native) return "waiting";
    return "ok";
  }
  const rep = r.sessionId ? reportOf(parent, r.sessionId) : undefined;
  if (rep) return /^The subagent did not finish/m.test(rep) ? "error" : "ok";
  return "unknown";
}

/** The report a child delivered into its parent, if any (text after the header). */
function reportOf(parent: SessionState, childId: string): string | undefined {
  return helperHistory(parent).reports.get(childId);
}

function countBy(parent: SessionState, refs: ChildRef[]) {
  const c = { running: 0, waiting: 0, ok: 0, error: 0, unknown: 0 };
  for (const r of refs) c[childStatus(parent, r)]++;
  return c;
}

/** "24 subagents · 4 running · 17 waiting for a slot · 3 done" */
export function swarmSummary(total: number, c: { running: number; waiting: number; ok: number; error: number }) {
  const parts = [`${total} subagents`];
  if (c.running) parts.push(`${c.running} running`);
  if (c.waiting) parts.push(`${c.waiting} waiting for a slot`);
  if (c.ok) parts.push(`${c.ok} done`);
  if (c.error) parts.push(`${c.error} failed`);
  return parts.join(" · ");
}

/** What a running subagent is doing, in a few words ("Reading src/engine.rs"). */
function doingNow(s: SessionState | undefined): string {
  const a = s ? currentActivity(s) : null;
  return !a ? "Working" : a.kind === "thinking" ? "Thinking" : a.detail ? `${a.verb} ${a.detail}` : a.verb;
}

/** A swarm: one summary row that opens into a virtualised list of compact rows. */
function SwarmGroup(props: { parent: SessionState; refs: ChildRef[] }) {
  const [open, setOpen] = createSignal(false);
  const counts = createMemo(() => countBy(props.parent, props.refs));
  // Running first, then waiting, then finished — what's happening now on top.
  const sorted = createMemo(() => {
    const rank: Record<ChildStatus, number> = { running: 0, waiting: 1, error: 2, ok: 3, unknown: 4 };
    return [...props.refs].sort((a, b) => rank[childStatus(props.parent, a)] - rank[childStatus(props.parent, b)]);
  });
  return (
    <div class="swarm" classList={{ open: open() }}>
      <button type="button" class="swarm-head" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <span class="swarm-strip" aria-hidden="true">
          <For each={props.refs.slice(0, 40)}>{(r) => <i class={`sq sq-${statusSq(childStatus(props.parent, r))}`} />}</For>
        </span>
        <Show when={counts().running}>
          <Tessellate size={9} />
        </Show>
        <span class="swarm-sum">{swarmSummary(props.refs.length, counts())}</span>
        <Icon name="chevronDown" size={13} class="chev" />
      </button>
      <Show when={open()}>
        <VirtualList items={sorted()} rowHeight={40} maxHeight={320} class="swarm-list" label="Helpers">
          {(r) => <SubagentRow parent={props.parent} child={r} />}
        </VirtualList>
      </Show>
    </div>
  );
}

const statusSq = (s: ChildStatus) => (s === "running" ? "running" : s === "waiting" ? "pending" : s === "ok" ? "ok" : s === "error" ? "error" : "unknown");

/** One subagent in a swarm list: square, name, steps · time, what it's doing, Open. */
function SubagentRow(props: { parent: SessionState; child: ChildRef }) {
  const info = () => (props.child.sessionId ? state.sessions[props.child.sessionId]?.info : undefined);
  const st = () => childStatus(props.parent, props.child);
  onMount(() => {
    if (props.child.sessionId && st() === "running") void ensureLoaded(props.child.sessionId).catch(() => {});
  });
  const now = useTicker(() => st() === "running");
  const activity = () => {
    const s = props.child.sessionId ? state.sessions[props.child.sessionId] : undefined;
    if (st() === "waiting") return "Waiting for a slot";
    if (st() !== "running" || !s) return st() === "error" ? "Did not finish" : st() === "ok" ? "Done" : "";
    return doingNow(s);
  };
  return (
    <div class="swarm-row" role="listitem" data-status={st()}>
      <span class="swarm-row-st">
        <Show when={st() === "running"} fallback={<span class={`sq sq-${statusSq(st())}`} />}>
          <Tessellate size={8} />
        </Show>
      </span>
      <span class="swarm-row-name">{childName(info(), props.child.name)}</span>
      <span class="swarm-row-act">{activity()}</span>
      <span class="swarm-row-stats">
        <Show when={info()}>
          {info()!.steps} · {fmtDuration(Math.max(0, (st() === "running" ? now() : info()!.updated_at) - info()!.created_at))}
        </Show>
      </span>
      <Show when={props.child.sessionId}>
        <button type="button" class="btn btn-xs btn-ghost" onClick={() => openSession(props.child.sessionId!, { newPane: true })}>
          Open
        </button>
      </Show>
    </div>
  );
}

function SubagentCard(props: { parent: SessionState; child: ChildRef }) {
  const s = () => (props.child.sessionId ? state.sessions[props.child.sessionId] : undefined);
  const info = () => s()?.info;
  const status = createMemo(() => childStatus(props.parent, props.child));
  const running = () => status() === "running";
  onMount(() => {
    if (props.child.sessionId && !running()) peekSession(props.child.sessionId);
  });
  createEffect(() => {
    if (props.child.sessionId && running()) void ensureLoaded(props.child.sessionId).catch(() => {});
  });
  const now = useTicker(running);
  const elapsed = () => {
    const i = info();
    if (!i) return 0;
    return Math.max(0, (running() ? now() : i.updated_at) - i.created_at);
  };
  const activity = createMemo(() => {
    const st = s();
    if (!st) return status() === "ok" ? "Finished · report delivered" : status() === "error" ? "Did not finish" : "Outcome unknown";
    if (status() === "waiting") return "Waiting for a slot";
    if (!running()) return st.info.status === "interrupted" ? "Stopped" : st.info.status === "error" ? "Failed" : "Finished";
    return doingNow(st);
  });
  const name = () => childName(info(), props.child.name);
  const role = () => roleLabel(info()?.role);
  return <HelperCard name={name()} role={role()} status={status()} steps={info() && status() !== "waiting" ? info()!.steps : undefined} elapsed={status() !== "waiting" ? elapsed() : undefined} activity={activity()} stop={(running() || status() === "waiting") && info() ? () => void stopChild(info()!) : undefined} open={props.child.sessionId ? () => openSession(props.child.sessionId!, { newPane: true }) : undefined} />;
}

/** No independent session means no Open/Stop/Continue controls. */
function MetadataHelperCard(props: { view: ToolView }) {
  const args = () => argsObject(props.view.args) ?? {};
  const run = () => (props.view.result?.details as { subagent?: { type?: string; description?: string; tool_uses?: number; duration_ms?: number; status?: string; background?: boolean } } | undefined)?.subagent;
  const tasks = () => { const list = argsObject(toolPresentation(props.view.name, props.view.args).args)?.tasks;
    return Array.isArray(list) && list.length ? list as { name?: string; agent?: string }[] : [{ name: run()?.description ?? "Helper" }]; };
  const running = () => props.view.status === "running";
  const now = useTicker(running);
  const status = (): ChildStatus => running() ? "running" : props.view.status === "error" || props.view.status === "stopped" ? "error" : props.view.status === "preparing" || props.view.status === "pending" || props.view.status === "approval" ? "waiting" : props.view.status === "ok" && !run()?.background ? "ok" : "unknown";
  const activity = () => running() ? (props.view.live?.output ?? "").split("\n").filter(Boolean).at(-1)?.replace(/^[→·]\s*/, "") || "Working" : props.view.status === "approval" ? "Needs approval" : status() === "waiting" ? "Waiting for a slot" : props.view.status === "stopped" ? "Stopped" : status() === "error" ? "Failed" : status() === "ok" ? "Finished" : run()?.background ? "Runs in the background · outcome unknown" : "Outcome unknown";
  return <div class="subagents" role="list" aria-label="Subagents"><For each={tasks()}>{(task) => <HelperCard name={task.name ?? "Helper"} role={roleLabel((args().subagent_type as string | undefined) ?? task.agent ?? run()?.type)} status={status()} steps={run()?.tool_uses} elapsed={running() && props.view.live ? Math.max(0, now() - props.view.live!.startedAt) : run()?.duration_ms} activity={activity()} />}</For></div>;
}

const DID_NOT_FINISH = /^The subagent did not finish/m;

/** The report as one plain line (no markdown marks, no code): the collapsed card's second line. */
function gistOf(md: string): string {
  const t = md
    .replace(/```[\s\S]*?(```|$)/g, " ")
    .replace(/^\s{0,3}#{1,6}\s+/gm, "")
    .replace(/^\s*(?:[-*+]|\d+[.)])\s+/gm, "")
    .replace(/^\s*>\s?/gm, "")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\*\*|__|`/g, "")
    .replace(/\s+/g, " ")
    .trim();
  return t.length > 240 ? t.slice(0, 240) : t;
}

/** What a report says in full, with a way into the subagent's own chat. */
function ReportBody(props: { text: string; childId: string }) {
  return (
    <div class="report-body">
      <Markdown text={props.text} />
      <button type="button" class="btn btn-xs btn-ghost report-open" onClick={() => openSession(props.childId, { newPane: true })}>
        Open task
        <Icon name="chevronRight" size={12} />
      </button>
    </div>
  );
}

/**
 * A background subagent's report, as two quiet lines: "↳ name · 5 steps · 48 s"
 * and the first words of what it found. Tap to read all of it.
 */
export function ReportCard(props: { entry: UserEntry; childId: string }) {
  const report = createMemo(() => parseReport(plainText(props.entry.content)));
  const [open, setOpen] = createSignal(false);
  const name = () => state.sessions[props.childId]?.info.title?.replace(/^↳\s*/, "") || report().name;
  const failed = () => DID_NOT_FINISH.test(report().body);
  const meta = () => [report().steps !== null ? `${report().steps} steps` : "", report().took ?? ""].filter(Boolean).join(" · ");
  const gist = createMemo(() => gistOf(report().body));
  return (
    <section class="report" classList={{ open: open(), failed: failed() }} aria-label={`Report from ${report().name}`}>
      <button type="button" class="report-head" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <span class="report-line">
          <span class={`sq ${failed() ? "sq-error" : "sq-ok"}`} />
          <span class="report-name">
            <span class="subagent-arrow" aria-hidden="true">
              ↳
            </span>
            {name()}
          </span>
          <span class="report-meta">{meta()}</span>
          <Icon name="chevronDown" size={13} class="chev" />
        </span>
        <Show when={!open() && gist()}>
          <span class="report-gist">{gist()}</span>
        </Show>
      </button>
      <Show when={open()}>
        <ReportBody text={report().body} childId={props.childId} />
      </Show>
    </section>
  );
}


/** A subagent's row height in the bar, and how many show before the list scrolls. */
const BAR_ROW = 32;
const BAR_ROWS = 4;

/**
 * The subagents at work, pinned under the header while any are running (or
 * waiting for a slot): one slim line that opens into a row each — name, what
 * it is doing, how long — and a tap on a row opens that subagent's chat.
 * Nothing renders (and nothing ticks) when none are active.
 */
export function SubagentBar(props: { session: SessionState }) {
  const active = createMemo(
    () =>
      childSessions(props.session.info.id)
        .map((i): ChildRef => ({ name: childName(i, "Helper"), sessionId: i.id, background: true }))
        .filter((r) => {
          const st = childStatus(props.session, r);
          return st === "running" || st === "waiting";
        })
        // What is happening now first; the order of starting otherwise.
        .sort((a, b) => Number(childStatus(props.session, a) === "waiting") - Number(childStatus(props.session, b) === "waiting")),
    [],
    { equals: sameRefs },
  );
  // Bots working on something this chat handed them: in the same bar, with Stop.
  const bots = createMemo(() => botsWorkingFor(props.session.info.id).map((i): ChildRef => ({ name: botLabel(i), sessionId: i.id, background: true })), [], { equals: sameRefs });
  return (
    <Show when={active().length || bots().length}>
      <SubagentBarBody session={props.session} refs={active()} bots={bots()} />
    </Show>
  );
}

function SubagentBarBody(props: { session: SessionState; refs: ChildRef[]; bots: ChildRef[] }) {
  // It starts folded, as one line of names, so it never pushes the chat down by itself. Once you open it, it stays open.
  const [pref, setPref] = createSignal<boolean | null>(null);
  const open = () => pref() ?? false;
  const counts = createMemo(() => countBy(props.session, props.refs));
  const summary = () =>
    [counts().running ? `${counts().running} working` : "", counts().waiting ? `${counts().waiting} waiting` : "", props.bots.length ? `${props.bots.length} ${props.bots.length === 1 ? "bot" : "bots"} working` : ""]
      .filter(Boolean)
      .join(" · ");
  const all = createMemo(() => [...props.bots, ...props.refs]);
  return (
    <section class="agents-bar" classList={{ open: open() }} aria-label="Helpers at work">
      <div class="agents-in">
        <button type="button" class="agents-head" aria-expanded={open()} onClick={() => setPref(!open())}>
          <Tessellate size={9} />
          <span class="agents-label">Helpers</span>
          <span class="agents-sum">{summary()}</span>
          <Show when={!open()}>
            <span class="agents-names">{all().map((r) => r.name).join(" · ")}</span>
          </Show>
          <span class="spacer" />
          <Icon name="chevronDown" size={13} class="chev" />
        </button>
        <Show when={open()}>
          <VirtualList items={all()} rowHeight={BAR_ROW} maxHeight={BAR_ROW * BAR_ROWS + BAR_ROW / 2} class="agents-rows" label="Helpers at work">
            {(r) => (props.bots.includes(r) ? <BotRow child={r} /> : <AgentRow parent={props.session} child={r} />)}
          </VirtualList>
        </Show>
      </div>
    </section>
  );
}

/** One active subagent: its state, name, what it is doing, elapsed; the whole row opens its chat. */
function AgentRow(props: { parent: SessionState; child: ChildRef }) {
  const id = () => props.child.sessionId;
  const s = () => (id() ? state.sessions[id()!] : undefined);
  const st = () => childStatus(props.parent, props.child);
  // Its tool calls and stream reach the row once it is loaded (also when it starts after we showed it waiting).
  createEffect(() => {
    if (id() && st() === "running") void ensureLoaded(id()!).catch(() => {});
  });
  const now = useTicker(() => st() === "running");
  const elapsed = () => {
    const i = s()?.info;
    return i && st() === "running" ? fmtDuration(Math.max(0, now() - i.created_at)) : "";
  };
  return (
    <div class="agent-row bot-row helper-row" data-status={st()}>
      <button type="button" class="bot-row-open" disabled={!id()} onClick={() => id() && openSession(id()!, { newPane: true })}>
      <span class="agent-st">
        <Show when={st() === "running"} fallback={<span class="sq sq-pending" />}>
          <Tessellate size={8} />
        </Show>
      </span>
      <span class="agent-name">{childName(s()?.info, props.child.name)}</span>
      <Show when={roleLabel(s()?.info.role)}>{(r) => <span class="badge agent-role">{r()}</span>}</Show>
      <span class="agent-doing">{st() === "waiting" ? "Waiting for a slot" : doingNow(s())}</span>
      <span class="agent-time">{elapsed()}</span>
      <Icon name="chevronRight" size={12} class="agent-go" />
      </button>
      <Show when={s()?.info}><button type="button" class="icon-btn sm" aria-label={`Stop ${childName(s()?.info, props.child.name)}`} data-tip="Stop" onClick={() => void stopChild(s()!.info)}><Icon name="stop" size={12} /></button></Show>
    </div>
  );
}

/** Many reports in a row (a swarm finishing): one group, each report a row that opens. */
export function ReportGroup(props: { items: { entry: UserEntry; childId: string }[] }) {
  const [open, setOpen] = createSignal(false);
  const [shown, setShown] = createSignal<number | null>(null);
  const reports = createMemo(() => props.items.map((it) => ({ ...it, r: parseReport(plainText(it.entry.content)) })));
  const failed = () => reports().filter((x) => DID_NOT_FINISH.test(x.r.body)).length;
  return (
    <section class="report-group" classList={{ open: open() }} aria-label={`${props.items.length} reports`}>
      <button type="button" class="report-group-head" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <span class="swarm-strip" aria-hidden="true">
          <For each={reports().slice(0, 40)}>{(x) => <i class={`sq ${DID_NOT_FINISH.test(x.r.body) ? "sq-error" : "sq-ok"}`} />}</For>
        </span>
        <span class="report-label">Reports</span>
        <span class="report-group-sum">
          {props.items.length} from subagents
          <Show when={failed()}> · {failed()} did not finish</Show>
        </span>
        <Icon name="chevronDown" size={13} class="chev" />
      </button>
      <Show when={open()}>
        <ol class="report-rows">
          <For each={reports()}>
            {(x, i) => (
              <li>
                <button type="button" class="report-row" aria-expanded={shown() === i()} onClick={() => setShown(shown() === i() ? null : i())}>
                  <span class={`sq ${DID_NOT_FINISH.test(x.r.body) ? "sq-error" : "sq-ok"}`} />
                  <span class="report-row-name">
                    <span class="subagent-arrow" aria-hidden="true">
                      ↳
                    </span>
                    {x.r.name}
                  </span>
                  <span class="report-row-meta">{[x.r.steps !== null ? `${x.r.steps} steps` : "", x.r.took ?? ""].filter(Boolean).join(" · ")}</span>
                  <Icon name="chevronDown" size={12} class="chev" />
                </button>
                <Show when={shown() === i()}>
                  <ReportBody text={x.r.body} childId={x.childId} />
                </Show>
              </li>
            )}
          </For>
        </ol>
      </Show>
    </section>
  );
}

// ── bots working for this chat (`message_bot` in the background) ──

/** "@orion" for a bot's conversation (its handle, else its title). */
function botLabel(i: SessionInfo | undefined): string {
  const b = i?.bot_id ? botById(i.bot_id) : undefined;
  return b ? `@${b.handle}` : i?.title?.replace(/^Your chats → /, "") || "A bot";
}

/** Stop a bot's work for this chat: its reply then says it was stopped, and the chat waits for you. */
function stopBot(id: string) {
  void abortSession(id);
}

/** A bot at work for this chat, in the bar: what it does, how long; the row opens its conversation, the square stops it. */
function BotRow(props: { child: ChildRef }) {
  const id = () => props.child.sessionId!;
  const s = () => state.sessions[id()];
  createEffect(() => {
    if (isBusy(s()?.info)) void ensureLoaded(id()).catch(() => {});
  });
  const now = useTicker(() => isBusy(s()?.info));
  const since = () => s()?.info.working_for?.since;
  return (
    <div class="agent-row bot-row" data-status="running">
      <button type="button" class="bot-row-open" onClick={() => openSession(id(), { newPane: true })}>
        <span class="agent-st">
          <Tessellate size={8} />
        </span>
        <span class="agent-name">{props.child.name}</span>
        <span class="agent-doing">{doingNow(s())}</span>
        <span class="agent-time">{since() ? fmtDuration(Math.max(0, now() - since()!)) : ""}</span>
      </button>
      <button type="button" class="icon-btn sm" aria-label={`Stop ${props.child.name}`} data-tip="Stop" onClick={() => stopBot(id())}>
        <Icon name="stop" size={12} />
      </button>
    </div>
  );
}


/**
 * A `message_bot` call that handed the work over in the background: the bot at work on it (what it is
 * doing, how long, Stop, Open), and once it replied, that it did (the reply itself comes below as a message).
 */
export function BotHandoffCard(props: { session: SessionState; view: ToolView }) {
  const h = createMemo(() => handoffOf(props.view.result?.details));
  return <Show when={h()}>{(x) => <HandoffRow session={props.session} callId={props.view.id} bot={x().bot} dmId={x().sessionId} />}</Show>;
}

function HandoffRow(props: { session: SessionState; callId: string; bot: string; dmId: string }) {
  const dm = () => state.sessions[props.dmId];
  const working = () => {
    const i = dm()?.info;
    return !!i && isBusy(i) && i.working_for?.call_id === props.callId;
  };
  onMount(() => {
    if (working()) void ensureLoaded(props.dmId).catch(() => {});
    else peekSession(props.dmId);
  });
  createEffect(() => {
    if (working()) void ensureLoaded(props.dmId).catch(() => {});
  });
  const reply = createMemo(() => handoffReply(props.session, props.callId, props.bot, props.dmId));
  const failed = () => !!reply() && NO_BOT_ANSWER.test(reply()!.reply.body);
  const status = () => (working() ? "running" : reply() ? (failed() ? "error" : "ok") : "unknown");
  const now = useTicker(working);
  const since = () => dm()?.info.working_for?.since;
  const activity = () => {
    if (working()) return doingNow(dm());
    const r = reply();
    if (!r) return "Handed over; its reply comes here";
    if (failed()) return /stopped/.test(r.reply.body) ? "Stopped" : "No reply";
    return "Replied";
  };
  return (
    <div class="subagents bot-handoff" role="list" aria-label={`Work handed to ${props.bot}`}>
      <div class="subagent" role="listitem" data-status={status()}>
        <span class="subagent-st">
          <Show when={working()} fallback={<span class={`sq sq-${status()}`} />}>
            <Tessellate size={9} label="Working" />
          </Show>
        </span>
        <div class="subagent-main">
          <div class="subagent-line">
            <span class="subagent-name">{props.bot}</span>
            <span class="subagent-stats">
              <Show when={working() && since()} fallback={reply()?.reply.took ?? ""}>
                {fmtDuration(Math.max(0, now() - since()!))}
              </Show>
            </span>
          </div>
          <div class="subagent-activity" title={activity()}>
            {working() ? `Working on it · ${activity()}` : activity()}
          </div>
        </div>
        <div class="subagent-actions">
          <Show when={working()}>
            <button type="button" class="icon-btn sm" aria-label={`Stop ${props.bot}`} data-tip="Stop" onClick={() => stopBot(props.dmId)}>
              <Icon name="stop" size={13} />
            </button>
          </Show>
          <button type="button" class="btn btn-xs btn-ghost" onClick={() => openSession(props.dmId, { newPane: true })}>
            Open
          </button>
        </div>
      </div>
    </div>
  );
}

/**
 * A bot's reply to work this chat handed it, as two quiet lines like a helper's report: "@orion replied ·
 * 4 min" and its first words. Tap to read all of it; Open shows the bot's conversation.
 */
export function BotReplyCard(props: { entry: UserEntry; author: string; reply: BotReply }) {
  const [open, setOpen] = createSignal(false);
  const failed = () => NO_BOT_ANSWER.test(props.reply.body);
  const gist = createMemo(() => gistOf(props.reply.body));
  const who = () => {
    const b = botById(props.author);
    return b ? `@${b.handle}` : `@${props.reply.handle}`;
  };
  return (
    <section class="report bot-reply" classList={{ open: open(), failed: failed() }} aria-label={`Reply from ${who()}`} data-seq={props.entry.seq}>
      <button type="button" class="report-head" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <span class="report-line">
          <span class={`sq ${failed() ? "sq-error" : "sq-ok"}`} />
          <span class="report-name">{failed() ? `${who()} didn't reply` : `${who()} replied`}</span>
          <span class="report-meta">{props.reply.took ?? ""}</span>
          <Icon name="chevronDown" size={13} class="chev" />
        </span>
        <Show when={!open() && gist()}>
          <span class="report-gist">{gist()}</span>
        </Show>
      </button>
      <Show when={open()}>
        <div class="report-body">
          <Markdown text={props.reply.body} />
          <Show when={props.reply.sessionId}>
            <button type="button" class="btn btn-xs btn-ghost report-open" onClick={() => openSession(props.reply.sessionId!, { newPane: true })}>
              Open conversation
              <Icon name="chevronRight" size={12} />
            </button>
          </Show>
        </div>
      </Show>
    </section>
  );
}
