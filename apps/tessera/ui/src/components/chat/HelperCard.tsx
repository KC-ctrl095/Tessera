import { Show } from "solid-js";
import { fmtDuration } from "../../lib/util";
import { Icon, Tessellate } from "../icons";

/** Shared helper presentation. Only render actions backed by a real child session. */
export function HelperCard(props: {
  name: string; role?: string; roleClass?: string;
  status: "running" | "waiting" | "ok" | "error" | "unknown";
  steps?: number; elapsed?: number; activity: string;
  stop?: () => void; open?: () => void;
}) {
  return (
    <div class="subagent" role="listitem" data-status={props.status}>
      <span class="subagent-st">
        <Show when={props.status === "running"} fallback={<span class={`sq sq-${props.status === "waiting" ? "pending" : props.status}`} />}>
          <Tessellate size={9} label="Working" />
        </Show>
      </span>
      <div class="subagent-main">
        <div class="subagent-line">
          <span class="subagent-name" title={props.name}><span class="subagent-arrow" aria-hidden="true">↳</span>{props.name}</span>
          <Show when={props.role}><span class={`badge subagent-role ${props.roleClass ?? ""}`}>{props.role}</span></Show>
          <span class="subagent-stats">
            <Show when={props.steps !== undefined}>{props.steps} {props.steps === 1 ? "step" : "steps"}</Show>
            <Show when={(props.elapsed ?? 0) >= 1000}>{props.steps !== undefined ? " · " : ""}{fmtDuration(props.elapsed!)}</Show>
          </span>
        </div>
        <div class="subagent-activity" title={props.activity}>{props.activity}</div>
      </div>
      <Show when={props.stop || props.open}>
        <div class="subagent-actions">
          <Show when={props.stop}><button type="button" class="icon-btn sm" aria-label={`Stop ${props.name}`} data-tip="Stop" onClick={props.stop}><Icon name="stop" size={13} /></button></Show>
          <Show when={props.open}><button type="button" class="btn btn-xs btn-ghost" onClick={props.open}>Open</button></Show>
        </div>
      </Show>
    </div>
  );
}
