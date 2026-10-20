import { Show, createMemo, createSignal, onCleanup } from "solid-js";
import { watchLiveness } from "../../lib/liveness";
import { botById, isBusy, type SessionState } from "../../lib/store";
import { toolActivity, toolMeta } from "../../lib/tools";
import { fmtDuration } from "../../lib/util";
import { Icon, Tessellate } from "../icons";

export type ActivityKind = "compacting" | "approval" | "tool" | "preparing" | "thinking" | "writing" | "ready" | "channel" | "working";

export interface Activity {
  kind: ActivityKind;
  verb: string;
  detail?: string;
  since: number | null;
  tone?: "warn";
}

/** Find a tool call's arguments in the stream or the persisted history. */
function callArgs(s: SessionState, id: string): unknown {
  for (const p of s.streaming?.content ?? []) if (p && p.type === "tool_call" && p.id === id) return p.arguments;
  for (let i = s.entries.length - 1; i >= 0 && i >= s.entries.length - 40; i--) {
    const e = s.entries[i];
    if (e.kind !== "assistant") continue;
    for (const p of e.content) if (p.type === "tool_call" && p.id === id) return p.arguments;
  }
  return undefined;
}

/** What the agent is doing right now, in one line. */
export function currentActivity(s: SessionState): Activity | null {
  // Nothing streams once the turn is over: a stream that never heard its end
  // (events lost on the way) must not keep the line up.
  if (!isBusy(s.info)) return null;
  const st = s.streaming && !s.streaming.ended ? s.streaming : null;
  // Since compacting began, not since the turn did: a 3-minute compaction
  // late in a 30-minute turn must not read "Compacting context · 30m".
  if (s.info.status === "compacting") return { kind: "compacting", verb: "Compacting context", since: s.statusSince ?? s.turnStart };
  if (s.info.status === "awaiting_approval" || s.approvals.length) {
    const a = s.approvals[0];
    return { kind: "approval", verb: "Waiting for your approval", detail: a?.summary, since: null, tone: "warn" };
  }
  const running = Object.values(s.running).sort((a, b) => b.startedAt - a.startedAt)[0];
  if (running) {
    const { verb, detail } = toolActivity(running.name, callArgs(s, running.call_id));
    const waitingOnBot = /^(message_bot|message|ask_bot|dm)/i.test(running.name);
    return { kind: "tool", verb: waitingOnBot ? "Waiting for" : verb, detail, since: running.startedAt };
  }
  if (st) {
    const who = st.author && s.info.kind === "channel" ? botById(st.author) : undefined;
    const parts = st.content.filter(Boolean);
    const last = parts[parts.length - 1];
    if (!last) return { kind: "ready", verb: who ? `@${who.handle} is working` : "Working", since: st.startedAt };
    if (last.type === "thinking" && !st.thinkEnd) return { kind: "thinking", verb: who ? `@${who.handle} is thinking` : "Thinking", since: st.thinkStart ?? st.startedAt };
    if (last.type === "tool_call") {
      const m = toolMeta(last.name);
      const { detail } = toolActivity(last.name, last.arguments);
      return { kind: "preparing", verb: `Preparing ${m.noun[0] === "command" ? "a command" : m.doing.toLowerCase()}`, detail, since: st.startedAt };
    }
    return { kind: "writing", verb: who ? `@${who.handle} is replying` : "Writing", since: st.startedAt };
  }
  if (s.info.kind === "channel") return { kind: "channel", verb: "Waiting for the next speaker", since: s.turnStart };
  return { kind: "working", verb: "Working", since: s.turnStart };
}

/** Now, every second while `on`, and at once when the app is back in front (timers sleep in the background). */
function useClock(on: () => boolean) {
  const [now, setNow] = createSignal(Date.now());
  const tick = () => {
    if (on()) setNow(Date.now());
  };
  const t = setInterval(tick, 1000);
  document.addEventListener("visibilitychange", tick);
  onCleanup(() => {
    clearInterval(t);
    document.removeEventListener("visibilitychange", tick);
  });
  return now;
}

/**
 * Kinds that need no line of their own: the transcript's live tail already
 * shows them (a thinking row, streaming text, a tool row, an approval card),
 * and the moment before the first word has nothing to say.
 */
const SHOWN_IN_TAIL = new Set<ActivityKind>(["ready", "thinking", "writing", "preparing", "tool", "approval"]);

/**
 * The live status: a slim line at the top of the composer ("Running cargo
 * test · 12s"). Hidden when it would repeat the last thing in the transcript,
 * unless the reader has scrolled away from the live tail (then it doubles as
 * the way back).
 */
export function ActivityLine(props: { session: SessionState; detached?: boolean; onJump?: () => void }) {
  watchLiveness();
  const act = createMemo(() => currentActivity(props.session));
  const shown = () => {
    const a = act();
    return !!a && (!!props.detached || !SHOWN_IN_TAIL.has(a.kind));
  };
  const now = useClock(shown);
  const elapsed = () => {
    const a = act();
    return a?.since ? Math.max(0, now() - a.since) : 0;
  };
  // While the agent works the line keeps its place even when it has nothing
  // to add, so the transcript above never shifts between steps.
  return (
    <Show when={shown() && act()} fallback={<Show when={act()}>{<div class="activity-slot" aria-hidden="true" />}</Show>}>
      {(a) => (
        <div class="activity" classList={{ warn: a().tone === "warn", away: !!props.detached }} role="status" aria-live="polite">
          <span class="activity-icon">
            <Show when={a().tone !== "warn"} fallback={<Icon name="shield" size={13} />}>
              <Tessellate size={9} />
            </Show>
          </span>
          <span class="activity-verb">{a().verb}</span>
          <Show when={a().detail}>
            <span class="activity-detail">{a().detail}</span>
          </Show>
          <Show when={elapsed() >= 1000}>
            <span class="activity-time">{fmtDuration(elapsed())}</span>
          </Show>
          <Show when={props.detached && props.onJump}>
            <button type="button" class="activity-jump" onClick={() => props.onJump!()}>
              Jump to live
              <Icon name="arrowDown" size={12} />
            </button>
          </Show>
        </div>
      )}
    </Show>
  );
}
