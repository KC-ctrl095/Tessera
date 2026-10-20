// What the app shell needs to know about a chat's helpers (subagents), without
// their cards: the cards, the live bar and the reports (Subagents.tsx) load the
// first time a chat has helpers.
import { Show, createEffect } from "solid-js";
import type { SessionInfo } from "../../proto/SessionInfo";
import { isBusy, state, type SessionState } from "../../lib/store";
import { onDemand } from "../../lib/onDemand";
import { argsObject, toolPresentation } from "../../lib/tools";
import type { BotReply, UserEntry } from "../../lib/transcript";
import type { ToolView } from "./ToolCard";

export interface ChildRef {
  name: string;
  sessionId: string | null;
  background: boolean;
}

/** `details.subagents` of a finished `agent` call. */
export function detailsChildren(details: unknown): ChildRef[] | null {
  const list = (details as { subagents?: unknown } | undefined)?.subagents;
  if (!Array.isArray(list)) return null;
  const seen = new Set<string>();
  return list.flatMap((x) => {
    const o = (x ?? {}) as { name?: unknown; session_id?: unknown; background?: unknown };
    const sessionId = typeof o.session_id === "string" && o.session_id ? o.session_id : null;
    if (sessionId && seen.has(sessionId)) return [];
    if (sessionId) seen.add(sessionId);
    return [{ name: typeof o.name === "string" ? o.name : "Helper", sessionId, background: !!o.background }];
  });
}

/**
 * Subagent sessions of `parent` (they reach us through `session_updated`).
 * The harness's own helpers (the memory review) are not subagents the user
 * started: they are never listed.
 */
export function childSessions(parentId: string): SessionInfo[] {
  return Object.values(state.sessions)
    .map((s) => s.info)
    .filter((i) => i.parent_id === parentId && i.kind === "subagent" && !i.internal)
    .sort((a, b) => a.created_at - b.created_at);
}

/** Bots working on something this chat handed them in the background (`message_bot`; `working_for`). */
export function botsWorkingFor(parentId: string): SessionInfo[] {
  return Object.values(state.sessions)
    .map((s) => s.info)
    .filter((i) => i.working_for?.session_id === parentId && isBusy(i))
    .sort((a, b) => (a.working_for?.since ?? 0) - (b.working_for?.since ?? 0));
}

/** `details` of a `message_bot` call that handed the work over in the background. */
export function handoffOf(details: unknown): { bot: string; sessionId: string } | null {
  const d = details as { bot?: unknown; session_id?: unknown; background?: unknown } | undefined;
  return d && d.background === true && typeof d.session_id === "string" && typeof d.bot === "string" ? { bot: d.bot, sessionId: d.session_id } : null;
}

/** The harness is reviewing this chat for memory right now (an internal helper is at work). */
export const reviewingMemory = (parentId: string): boolean =>
  Object.values(state.sessions).some((s) => s.info.internal && s.info.parent_id === parentId && isBusy(s.info));

/** Session ids some other `agent` call of this chat already reported. */
export function claimedElsewhere(s: SessionState, callId: string): Set<string> {
  const out = new Set<string>();
  for (const [id, r] of Object.entries(s.results)) {
    if (id === callId) continue;
    for (const c of detailsChildren(r.details) ?? []) if (c.sessionId) out.add(c.sessionId);
  }
  return out;
}

export const childName = (i: SessionInfo | undefined, fallback: string) => i?.title?.replace(/^↳\s*/, "").trim() || fallback;

export const sameRefs = (a: ChildRef[], b: ChildRef[]) => a.length === b.length && a.every((r, i) => r.sessionId === b[i].sessionId && r.name === b[i].name && r.background === b[i].background);

/** Only start calls can claim children; agents list/message/apply must not repeat their cards. */
export function isHelperStart(name: string, args: unknown): boolean {
  return toolPresentation(name, args).kind === "agent" || /^(agent|subagent)$/i.test(name);
}

/** Existing wire facts only: details, parent_call, then bounded legacy native call matching. */
export function refsForCall(s: SessionState, view: ToolView, ts?: number): ChildRef[] {
  const detailed = detailsChildren(view.result?.details);
  const refs = [...(detailed ?? [])];
  for (const i of helpersOf(s.info.id, view.id)) {
    if (!refs.some((r) => r.sessionId === i.id)) refs.push({ name: "Helper", sessionId: i.id, background: true });
  }
  const cliCall = /^(Task|Agent)$/.test(view.name) && !Array.isArray(argsObject(view.args)?.tasks);
  if (refs.length || detailed || view.status === "preparing" || cliCall) return refs;
  const calls = s.entries.flatMap((e) => e.kind === "assistant" ? e.content.flatMap((p) => p.type === "tool_call" && isHelperStart(p.name, p.arguments) && !(/^(Task|Agent)$/.test(p.name) && !Array.isArray(argsObject(p.arguments)?.tasks)) ? [{ id: p.id, ts: e.ts, args: p.arguments }] : []) : []);
  if (!calls.some((c) => c.id === view.id)) calls.push({ id: view.id, ts: ts ?? 0, args: view.args });
  const names = (args: unknown): string[] => {
    const tasks = argsObject(args)?.tasks;
    return Array.isArray(tasks) ? tasks.flatMap((t) => typeof t?.name === "string" ? [t.name] : []) : [];
  };
  const taken = claimedElsewhere(s, view.id);
  for (const i of childSessions(s.info.id)) {
    if (i.parent_call || taken.has(i.id)) continue;
    const candidates = calls.filter((c) => c.ts <= i.created_at);
    const named = candidates.filter((c) => names(c.args).includes(childName(i, "")));
    // Names are not unique, and several calls can share one assistant timestamp.
    // Infer only a unique eligible match; ambiguous children stay in the top bar
    // until parent_call/result metadata supplies their real call association.
    const matches = named.length ? named : candidates;
    const owner = matches.length === 1 ? matches[0] : undefined;
    if (owner?.id === view.id) refs.push({ name: childName(i, "Helper"), sessionId: i.id, background: true });
  }
  return refs;
}

/**
 * What a subagent is for, as a label (`Explore`, `Plan`, a defined agent's name). The default kind
 * (`general-purpose`) says nothing: no label, it is just a subagent.
 */
export const roleLabel = (role: string | null | undefined): string => {
  const r = role?.trim().replace(/[-_]+/g, " ") ?? "";
  if (!r || /^general( purpose)?$/i.test(r)) return "";
  return r[0].toUpperCase() + r.slice(1);
};

/** Helpers linked to a call, independent of engine/provider. */
export const helpersOf = (parentId: string, callId: string) => childSessions(parentId).filter((i) => i.parent_call === callId);


/** The harness is reviewing this chat for memory: one barely-there line above the composer. */
export function MemoryReview(props: { session: SessionState }) {
  return (
    <Show when={!isBusy(props.session.info) && reviewingMemory(props.session.info.id)}>
      <div class="memory-review" role="status">
        <span class="learned-glyph" aria-hidden="true">
          <i />
          <i />
          <i />
          <i />
        </span>
        <span>Looking for things to remember…</span>
      </div>
    </Show>
  );
}

/** The cards, bar and reports themselves. */
export const subagentParts = onDemand(() => import("./Subagents"));

/** The helpers at work, pinned under the header: loaded once this chat has any. */
export function SubagentBarSlot(props: { session: SessionState }) {
  const has = () => childSessions(props.session.info.id).length > 0 || botsWorkingFor(props.session.info.id).length > 0;
  createEffect(() => {
    if (has()) void subagentParts.load();
  });
  return (
    <Show when={has() && subagentParts.get()}>
      {(m) => {
        const Bar = m().SubagentBar;
        return <Bar session={props.session} />;
      }}
    </Show>
  );
}

/** Live cards for the helpers an `agent` call started. */
export function SubagentCardsSlot(props: { session: SessionState; view: ToolView; ts?: number }) {
  void subagentParts.load();
  return (
    <Show when={subagentParts.get()}>
      {(m) => {
        const Cards = m().SubagentCards;
        return <Cards session={props.session} view={props.view} ts={props.ts} />;
      }}
    </Show>
  );
}

/** A helper's report in the transcript. */
export function ReportCardSlot(props: { entry: UserEntry; childId: string }) {
  void subagentParts.load();
  return (
    <Show when={subagentParts.get()} fallback={<section class="report" />}>
      {(m) => {
        const Card = m().ReportCard;
        return <Card entry={props.entry} childId={props.childId} />;
      }}
    </Show>
  );
}

/** Many reports in a row. */
export function ReportGroupSlot(props: { items: { entry: UserEntry; childId: string }[] }) {
  void subagentParts.load();
  return (
    <Show when={subagentParts.get()} fallback={<section class="report-group" />}>
      {(m) => {
        const Group = m().ReportGroup;
        return <Group items={props.items} />;
      }}
    </Show>
  );
}

/** A `message_bot` hand-off: the bot at work on it (Stop, Open), or what came of it. */
export function BotHandoffSlot(props: { session: SessionState; view: ToolView }) {
  void subagentParts.load();
  return (
    <Show when={subagentParts.get()}>
      {(m) => {
        const Card = m().BotHandoffCard;
        return <Card session={props.session} view={props.view} />;
      }}
    </Show>
  );
}

/** A bot's reply to a hand-off, delivered into the chat. */
export function BotReplySlot(props: { entry: UserEntry; author: string; reply: BotReply }) {
  void subagentParts.load();
  return (
    <Show when={subagentParts.get()} fallback={<section class="report" />}>
      {(m) => {
        const Card = m().BotReplyCard;
        return <Card entry={props.entry} author={props.author} reply={props.reply} />;
      }}
    </Show>
  );
}
