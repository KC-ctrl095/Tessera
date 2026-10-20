import { Show, createSignal } from "solid-js";
import type { ApprovalDecision } from "../../proto/ApprovalDecision";
import type { ApprovalRequest } from "../../proto/ApprovalRequest";
import { ensureMachines, isMachineTool, machineCall, machineCallLine } from "../../lib/machines";
import { api } from "../../lib/store";
import { prettyArgs, toolMeta, toolPresentation } from "../../lib/tools";
import { toastError } from "../../lib/ui";
import { Icon, Spinner } from "../icons";
import "../../styles/machines.css";

/** Inline approval request. Keyboard (when focused): y = allow, a = allow for session, n = deny. */
export function ApprovalCard(props: { sessionId: string; request: ApprovalRequest }) {
  const [busy, setBusy] = createSignal<ApprovalDecision | null>(null);
  const [showArgs, setShowArgs] = createSignal(false);
  const r = () => props.request;
  const presentation = () => toolPresentation(r().tool, r().arguments);
  // A call on one of the user's machines names the machine first ("On Desktop: run cargo build in ~/proj").
  const machine = () => (isMachineTool(r().tool) ? machineCall(r().arguments, r().summary) : null);
  if (isMachineTool(props.request.tool)) ensureMachines();
  // In full, from the exact arguments that will run there (the server's own summary is shorter).
  const summary = () => (machine()?.machine ? machineCallLine(r().arguments, r().summary, true) : r().summary);

  const decide = async (d: ApprovalDecision) => {
    if (busy()) return;
    setBusy(d);
    try {
      await api.respondApproval(props.sessionId, r().id, d);
    } catch (e) {
      toastError(e, "Could not send decision");
      setBusy(null);
    }
  };

  const onKey = (e: KeyboardEvent) => {
    if (e.target !== e.currentTarget || e.ctrlKey || e.metaKey || e.altKey) return;
    const k = e.key.toLowerCase();
    if (k === "y") void decide("allow");
    else if (k === "a" && !r().once) void decide("allow_session");
    else if (k === "n") void decide("deny");
    else return;
    e.preventDefault();
  };

  return (
    <div
      class="approval"
      classList={{ "approval-on-machine": !!machine()?.machine }}
      tabIndex={0}
      onKeyDown={onKey}
      role="group"
      aria-label={machine()?.machine ? `Approval needed on ${machine()!.machine}` : `Approval needed for ${presentation().kind}`}
    >
      <div class="approval-head">
        <span class="approval-icon">
          <Icon name="shield" size={17} />
        </span>
        <div class="approval-title">
          <span>Approval needed</span>
          <Show
            when={machine()?.machine}
            fallback={
              <span class="approval-tool">
                <Icon name={toolMeta(presentation().kind).icon} size={13} /> {presentation().kind}
              </span>
            }
          >
            <span class="approval-tool approval-machine" title={`On your machine ${machine()!.machine}`}>
              <Icon name="laptop" size={13} /> On {machine()!.machine}
            </span>
          </Show>
        </div>
        <span class="approval-keys" aria-hidden="true">
          <kbd>Y</kbd>
          <kbd>A</kbd>
          <kbd>N</kbd>
        </span>
      </div>
      <pre class="approval-summary">{summary()}</pre>
      <Show when={r().reason}>
        <p class="approval-reason">{r().reason}</p>
      </Show>
      <button type="button" class="link-btn" onClick={() => setShowArgs(!showArgs())} aria-expanded={showArgs()}>
        {showArgs() ? "Hide arguments" : "Show arguments"}
      </button>
      <Show when={showArgs()}>
        <pre class="tool-pre">{prettyArgs(presentation().args)}</pre>
        <details class="tool-audit"><summary>Execution details</summary><pre class="tool-pre">{r().tool}{"\n"}{r().summary}{"\n"}{prettyArgs(r().arguments)}</pre></details>
      </Show>
      <div class="approval-actions">
        <button type="button" class="btn btn-primary" disabled={!!busy()} onClick={() => decide("allow")}>
          <Show when={busy() === "allow"} fallback={<Icon name="check" size={16} />}>
            <Spinner size={14} />
          </Show>
          Allow
        </button>
        {/* A machine that asks before every action (or risky ones) asks each time. */}
        <Show when={!r().once}>
          <button type="button" class="btn" disabled={!!busy()} onClick={() => decide("allow_session")}>
            <Show when={busy() === "allow_session"} fallback={<Icon name="shieldCheck" size={16} />}>
              <Spinner size={14} />
            </Show>
            Allow for this session
          </button>
        </Show>
        <button type="button" class="btn btn-ghost danger-text" disabled={!!busy()} onClick={() => decide("deny")}>
          <Show when={busy() === "deny"} fallback={<Icon name="x" size={16} />}>
            <Spinner size={14} />
          </Show>
          Deny
        </button>
      </div>
    </div>
  );
}
