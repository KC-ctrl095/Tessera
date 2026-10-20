// Settings → Devices → Activity: what agents did on your machines, newest first. Each machine's latest
// actions as the server saw them (any device can look); in the desktop app, also everything
// done on this computer as it recorded it itself. Devices links here instead of listing it.
import { For, Show, createMemo, createResource, createSignal, onCleanup } from "solid-js";
import { activityFocus, listMachines, nodeStatus, onMachinesChanged, outcomeInfo, setActivityFocus, shortPath, type MachineActivity, type MachineInfo } from "../../lib/machines";
import { isDesktopApp } from "../../lib/native";
import { RpcError } from "../../lib/transport";
import { errorMessage } from "../../lib/ui";
import { fmtDateTime, relTime, truncate } from "../../lib/util";
import { Icon, Spinner } from "../icons";
import { ActivityLog } from "./ThisComputerPanel";
import "../../styles/machines.css";

/** An older server: no machines there. */
const oldServer = (e: unknown) => e instanceof RpcError && (e.code === "bad_request" || e.code === "not_found") && /unknown|variant|method/i.test(e.message);

type Row = { m: MachineInfo; a: MachineActivity; key: string };

const VERB: Record<string, string> = { run: "Ran", "run as administrator": "Ran as administrator", read: "Read", list: "Listed", write: "Wrote", edit: "Edited" };

export function ActivityTab() {
  const [machines, { refetch }] = createResource(async () => {
    try {
      return await listMachines();
    } catch (e) {
      if (oldServer(e)) return [] as MachineInfo[];
      throw e;
    }
  });
  onCleanup(onMachinesChanged(() => void refetch()));
  const [local] = createResource(async () => (isDesktopApp ? ((await nodeStatus().catch(() => null)) ?? null) : null));

  // Opened from a machine in Devices: that machine first.
  const [which, setWhich] = createSignal<string | null>(activityFocus());
  setActivityFocus(null);

  const withActivity = createMemo(() => (machines() ?? []).filter((m) => m.recent.length));
  const rows = createMemo<Row[]>(() =>
    withActivity()
      .filter((m) => !which() || m.id === which())
      .flatMap((m) => m.recent.map((a, i) => ({ m, a, key: `${m.id}:${a.ts}:${i}` })))
      .sort((x, y) => y.a.ts - x.a.ts),
  );

  return (
    <div class="stack act">
      <Show
        when={machines()}
        fallback={
          <div class="empty-state">
            <Show when={machines.error} fallback={<Spinner size={20} />}>
              <span class="empty-icon">
                <Icon name="alertCircle" size={20} />
              </span>
              <p>{errorMessage(machines.error)}</p>
            </Show>
          </div>
        }
      >
        <Show when={withActivity().length > 1}>
          <div class="act-filter" role="radiogroup" aria-label="Machine">
            <button type="button" role="radio" class="act-chip" aria-checked={!which()} classList={{ on: !which() }} onClick={() => setWhich(null)}>
              All
            </button>
            <For each={withActivity()}>
              {(m) => (
                <button type="button" role="radio" class="act-chip" aria-checked={which() === m.id} classList={{ on: which() === m.id }} onClick={() => setWhich(m.id)}>
                  {m.name}
                </button>
              )}
            </For>
          </div>
        </Show>
        <Show
          when={rows().length}
          fallback={<p class="muted small act-empty">{(machines() ?? []).length ? "Nothing yet. What agents do on your machines shows up here." : "No machines yet. Add one in Devices; what agents do on it shows up here."}</p>}
        >
          <ul class="act-list" aria-label="What agents did">
            <For each={rows()}>{(r) => <ActRow row={r} many={withActivity().length > 1 && !which()} />}</For>
          </ul>
          <p class="muted small act-fine">The latest actions on each machine, as this server saw them. Each computer keeps its whole log itself.</p>
        </Show>
      </Show>
      <Show when={isDesktopApp && local()?.node?.enabled}>
        <ActivityLog />
      </Show>
    </div>
  );
}

/** One action: what it did (two lines; a tap shows all of it), where, who, when, how it ended. */
function ActRow(props: { row: Row; many: boolean }) {
  const [open, setOpen] = createSignal(false);
  const a = () => props.row.a;
  const o = () => outcomeInfo(a().outcome);
  const target = () => (a().action === "run" ? a().target : shortPath(a().target, props.row.m.home));
  const meta = () =>
    [props.many ? props.row.m.name : "", relTime(a().ts), a().bot ?? "", a().chat ? `in “${truncate(a().chat, 60)}”` : ""].filter(Boolean).join(" · ");
  return (
    <li>
      <button type="button" class="act-row" classList={{ open: open() }} aria-expanded={open()} title={fmtDateTime(a().ts)} onClick={() => setOpen(!open())}>
        <span class="act-what">
          <span class="act-line">
            <span class="act-verb">{VERB[a().action] ?? a().action}</span> <span class="mono act-target">{target()}</span>
          </span>
          <span class="act-meta">{meta()}</span>
        </span>
        <span class={`badge ${{ ok: "badge-ok", warn: "badge-warn", err: "badge-err", "": "" }[o().tone]}`}>{o().label}</span>
      </button>
    </li>
  );
}
