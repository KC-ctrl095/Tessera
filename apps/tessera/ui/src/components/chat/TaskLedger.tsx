import { For, Show, createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import type { PlanStatus } from "../../proto/PlanStatus";
import type { SessionInfo } from "../../proto/SessionInfo";
import type { CriterionState } from "../../proto/CriterionState";
import type { CriterionStatus } from "../../proto/CriterionStatus";
import { setSessionGoal } from "../../lib/actions";
import { Icon, type IconName } from "../icons";

const STATUS: Record<PlanStatus, { icon: IconName; label: string }> = {
  done: { icon: "circleCheck", label: "Done" },
  in_progress: { icon: "circleHalf", label: "In progress" },
  pending: { icon: "circle", label: "Pending" },
  blocked: { icon: "circleX", label: "Blocked" },
  skipped: { icon: "circleMinus", label: "Skipped" },
};

/**
 * The goal of a chat, as a slim inscription just above the composer (set
 * with `/goal`; × clears it). When the agent keeps a plan, its progress sits
 * on the right and the strip opens the full ledger upward, over the chat.
 */
export function GoalStrip(props: { info: SessionInfo }) {
  const [open, setOpen] = createSignal(false);
  let root!: HTMLElement;
  const t = () => props.info.task;
  const goal = () => t()?.goal?.trim() ?? "";
  const plan = () => t()?.plan ?? [];
  const counts = createMemo(() => {
    const p = plan();
    const done = p.filter((x) => x.status === "done" || x.status === "skipped").length;
    return { done, total: p.length };
  });
  const current = createMemo(() => plan().find((p) => p.status === "in_progress"));
  const more = () => {
    const x = t();
    return !!x && (x.plan.length > 0 || x.open_questions.length > 0 || x.decisions.length > 0 || x.constraints.length > 0 || x.done_criteria.length > 0 || x.artifacts.length > 0 || x.notes.length > 0);
  };
  const shown = () => !!goal() || plan().length > 0;
  createEffect(() => {
    if (!shown()) setOpen(false);
  });

  // The ledger floats over the chat: a press outside it, or Escape from inside, puts it away.
  createEffect(() => {
    if (!open()) return;
    const down = (e: PointerEvent) => {
      if (!root.contains(e.target as Node)) setOpen(false);
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape" && root.contains(document.activeElement)) {
        e.stopPropagation();
        setOpen(false);
      }
    };
    document.addEventListener("pointerdown", down, true);
    document.addEventListener("keydown", key, true);
    onCleanup(() => {
      document.removeEventListener("pointerdown", down, true);
      document.removeEventListener("keydown", key, true);
    });
  });

  return (
    <Show when={shown()}>
      <section class="goal" ref={root} classList={{ open: open() }} aria-label="Goal">
        <div class="goal-row">
          <span class="goal-label">{goal() ? "Goal" : "Plan"}</span>
          {/* Read-only: a goal is set with /goal (the plan opens from here). */}
          <span
            class="goal-text"
            classList={{ opens: more() }}
            title={goal() || current()?.text || undefined}
            onClick={() => more() && setOpen(!open())}
          >
            {goal() || current()?.text || plan()[0]?.text || ""}
          </span>
          <Show when={counts().total}>
            <span class="goal-progress" aria-label={`${counts().done} of ${counts().total} steps done`}>
              <span class="goal-squares" aria-hidden="true">
                <For each={plan().slice(0, 12)}>{(p) => <i class={`sq sq-plan-${p.status}`} />}</For>
              </span>
              <span class="goal-count">
                {counts().done}/{counts().total}
              </span>
            </span>
          </Show>
          <Show when={more()}>
            <button type="button" class="icon-btn sm goal-chev" aria-expanded={open()} aria-label={open() ? "Hide the plan" : "Show the plan"} onClick={() => setOpen(!open())}>
              <Icon name="chevronDown" size={13} class="chev" />
            </button>
          </Show>
          <Show when={goal()}>
            <button type="button" class="icon-btn sm goal-x" aria-label="Clear the goal" data-tip="Clear the goal" onClick={() => void setSessionGoal(props.info.id, "")}>
              <Icon name="x" size={12} />
            </button>
          </Show>
        </div>
        <Show when={open() && t()}>
          <div class="task-body">
            <Show when={plan().length}>
              <ol class="plan">
                <For each={plan()}>
                  {(p) => (
                    <li class={`plan-item st-${p.status}`}>
                      <Icon name={STATUS[p.status].icon} size={14} aria-label={STATUS[p.status].label} />
                      <span>{p.text}</span>
                    </li>
                  )}
                </For>
              </ol>
            </Show>
            <LedgerList title="Open questions" items={t()!.open_questions} icon="help" />
            <LedgerList title="Decisions" items={t()!.decisions} icon="check" />
            <LedgerList title="Constraints" items={t()!.constraints} icon="lock" />
            <DoneWhen items={t()!.done_criteria} criteria={t()!.criteria ?? []} />
            <LedgerList title="Artifacts" items={t()!.artifacts} icon="file" />
            <LedgerList title="Notes" items={t()!.notes} icon="bookmark" />
          </div>
        </Show>
      </section>
    </Show>
  );
}


const CRITERION: Record<CriterionStatus, { icon: IconName; label: string }> = {
  unchecked: { icon: "circle", label: "Not checked yet" },
  met: { icon: "circleCheck", label: "Met" },
  unmet: { icon: "circleX", label: "Not met" },
  unverifiable: { icon: "help", label: "Can't be verified" },
};

/** "Done when" items with how each was checked (a tick, a cross…) and the evidence. */
function DoneWhen(props: { items: string[]; criteria: CriterionState[] }) {
  const checked = () => props.criteria.filter((c) => c && c.status === "met").length;
  return (
    <Show when={props.items.length}>
      <div class="ledger-sec">
        <div class="ledger-title">
          Done when
          <Show when={props.criteria.some((c) => c && c.status !== "unchecked")}>
            <span class="ledger-count">
              {checked()}/{props.items.length} met
            </span>
          </Show>
        </div>
        <ul class="criteria">
          <For each={props.items}>
            {(x, i) => {
              const c = () => props.criteria[i()] ?? { status: "unchecked" as const, evidence: "" };
              return (
                <li class={`crit crit-${c().status}`}>
                  <Icon name={CRITERION[c().status]?.icon ?? "circle"} size={13} aria-label={CRITERION[c().status]?.label} />
                  <span class="crit-text">
                    {x}
                    <Show when={c().evidence.trim()}>
                      <span class="crit-evidence">{c().evidence}</span>
                    </Show>
                  </span>
                </li>
              );
            }}
          </For>
        </ul>
      </div>
    </Show>
  );
}

function LedgerList(props: { title: string; items: string[]; icon: IconName }) {
  return (
    <Show when={props.items.length}>
      <div class="ledger-sec">
        <div class="ledger-title">{props.title}</div>
        <ul>
          <For each={props.items}>
            {(x) => (
              <li>
                <Icon name={props.icon} size={13} />
                <span>{x}</span>
              </li>
            )}
          </For>
        </ul>
      </div>
    </Show>
  );
}
