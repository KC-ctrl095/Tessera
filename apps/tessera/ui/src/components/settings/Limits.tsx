// Settings → Usage: a subscription's usage limits (its 5-hour and weekly
// windows, and a model's own when one comes up), as the provider last
// reported them along with a reply. Nothing is fetched just to show them.
import { For, Show } from "solid-js";
import type { PlanLimit } from "../../proto/PlanLimit";
import { relTime } from "../../lib/util";
import { Icon } from "../icons";

/** Close to a limit from this share on (when the provider gives no status). */
const WARN_AT = 80;

type Level = "ok" | "warning" | "reached";

function levelOf(l: PlanLimit): Level {
  const p = l.used_percent;
  if (l.status === "reached" || (p != null && p >= 100)) return "reached";
  if (l.status === "warning" || (p != null && p >= WARN_AT)) return "warning";
  return "ok";
}

/** "in 35 min", "in 2 h 10 min", "Tue 9:00 AM", "Oct 14, 9:00 AM". */
function resetsIn(at: number, now: number): string {
  const min = Math.max(1, Math.round((at - now) / 60_000));
  if (min < 60) return `in ${min} min`;
  if (min < 24 * 60) return `in ${Math.floor(min / 60)} h${min % 60 ? ` ${min % 60} min` : ""}`;
  const time = new Date(at).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  if (min < 6 * 24 * 60) return `${new Date(at).toLocaleDateString(undefined, { weekday: "short" })} ${time}`;
  return `${new Date(at).toLocaleDateString(undefined, { month: "short", day: "numeric" })}, ${time}`;
}

/** How fresh the numbers are: "just now", "12m ago". */
export const asOf = (ts: number, now: number) => (now - ts < 60_000 ? "just now" : relTime(ts, now));

function Meter(props: { limit: PlanLimit; now: number }) {
  const l = () => props.limit;
  /** Its window started over since it was reported: the numbers are from before. */
  const over = () => l().resets_at != null && l().resets_at! <= props.now;
  const level = () => (over() ? "ok" : levelOf(l()));
  const pct = () => l().used_percent;
  const fill = () => (over() ? 0 : pct() != null ? Math.min(100, Math.max(0, pct()!)) : level() === "reached" ? 100 : 0);
  const value = () => {
    if (over()) return "Started over";
    if (level() === "reached") return "Reached";
    if (pct() != null) return `${Math.round(pct()!)}% used`;
    return level() === "warning" ? "Almost used up" : "Within the limit";
  };
  const reset = () => {
    const at = l().resets_at;
    if (at == null) return "";
    return over() ? `Reset ${relTime(at, props.now)}` : `Resets ${resetsIn(at, props.now)}`;
  };
  return (
    <div class="usage-limit" classList={{ warn: level() === "warning", full: level() === "reached", over: over() }}>
      <div class="usage-limit-line">
        <span class="usage-limit-name">{l().label}</span>
        <span class="usage-limit-value">
          <Show when={level() !== "ok"}>
            <Icon name="alertCircle" size={13} />
          </Show>
          {value()}
        </span>
      </div>
      <div
        class="usage-limit-track"
        role="meter"
        aria-label={l().label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(fill())}
        aria-valuetext={[value(), reset()].filter(Boolean).join(", ")}
      >
        <span class="usage-limit-fill" style={{ width: `${fill()}%` }} />
      </div>
      <Show when={reset()}>
        <div class="usage-limit-reset">{reset()}</div>
      </Show>
    </div>
  );
}

/** A plan's limits as meters, with how fresh they are; a line saying when they come, while none did
 * (nothing from older servers, which don't report them). */
export function PlanLimits(props: { limits?: PlanLimit[]; now: number }) {
  const list = () => props.limits ?? [];
  const latest = () => Math.max(...list().map((l) => l.updated_at));
  return (
    <Show when={props.limits}>
      <Show when={list().length} fallback={<p class="usage-limits-none">Its 5-hour and weekly limits show here after its next reply.</p>}>
        <div class="usage-limits">
          <For each={list()}>{(l) => <Meter limit={l} now={props.now} />}</For>
        </div>
        <p class="usage-limits-asof" title="Tessera never asks just to check.">
          <Icon name="clock" size={12} />
          Updated {asOf(latest(), props.now)} · comes with its replies
        </p>
      </Show>
    </Show>
  );
}
