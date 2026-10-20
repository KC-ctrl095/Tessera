// Settings → Devices: one entry per computer, phone or browser paired with
// this server. A computer's entry gathers all its sign-ins (its Tessera app,
// its terminal) and its machine (what agents may use there), shows plainly
// what it is used for, and keeps each sign-in's own actions inside it. The
// server says which devices share a computer (lib/devices.ts), so every
// device shows the same list. "Add…" pairs a device or adds a machine.
//
// Any device can rename an entry (its sign-ins) or sign one out. A computer has
// one name: renaming it on that computer itself renames what agents call it too
// (its machine). A machine's name and what it allows change only on that
// computer: other devices can only pause it or revoke its access. In the
// desktop app, this computer's entry, pinned first, has its local controls
// (ThisComputerPanel).
import { For, Show, createComputed, createMemo, createResource, createSignal, onCleanup, type JSX } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import type { PairingCode } from "../../proto/PairingCode";
import { installCommandHere } from "../../lib/repo";
import { rememberDeviceName } from "../../lib/app";
import { ROLE, clientOf, computers, renameDevice, usedFor, type Computer } from "../../lib/devices";
import {
  INSTALL_COMMAND,
  INSTALL_COMMAND_WINDOWS,
  INSTALL_URL,
  enableCommand,
  listMachines,
  nodeEnable,
  nodeName,
  nodeStatus,
  onMachinesChanged,
  osLabel,
  rememberMachines,
  revokeMachine,
  setMachinePaused,
  type DeviceInfo,
  type MachineInfo,
  type ThisComputer,
} from "../../lib/machines";
import { presetInfo, presetsFor, scopeLines } from "../../lib/machine-settings";
import { isDesktopApp } from "../../lib/native";
import { api } from "../../lib/store";
import { RpcError } from "../../lib/transport";
import { confirmDialog, errorMessage, promptDialog, setSettingsTab, toast, toastError } from "../../lib/ui";
import { copyText, fmtDateTime, openExternal, relTime } from "../../lib/util";
import { Icon, Spinner } from "../icons";
import { CopyButton, Segmented, openMenu } from "../primitives";
import { PairPanel } from "./PairPanel";
import { machineBots } from "../bot/presets";
import { ACCESS_WORDS, accessLabel } from "./AccessPicker";
import { ThisComputerPanel, showActivity } from "./ThisComputerPanel";
import { KeepRunning } from "./KeepRunning";
import { PageRow } from "./kit";
import "../../styles/machines.css";

/** An older server: no machines there. */
const oldServer = (e: unknown) => e instanceof RpcError && (e.code === "bad_request" || e.code === "not_found") && /unknown|variant|method/i.test(e.message);

export function DevicesTab(props: { focus?: "this" | "add-machine" }) {
  const [devices, { refetch: refetchDevices, mutate: mutateDevices }] = createResource(() => api.listDevices());
  const [machines, { refetch: refetchMachines, mutate: mutateMachines }] = createResource(async () => {
    try {
      const list = await listMachines();
      rememberMachines(list);
      return list;
    } catch (e) {
      if (oldServer(e)) return [] as MachineInfo[];
      throw e;
    }
  });
  // This computer's own node (the desktop app only; `null` elsewhere).
  const [local, { refetch: refetchLocal, mutate: setLocal }] = createResource(async () => (isDesktopApp ? ((await nodeStatus().catch(() => null)) ?? null) : null));
  onCleanup(
    onMachinesChanged(() => {
      void refetchMachines();
      void refetchDevices();
      if (isDesktopApp) void refetchLocal();
    }),
  );

  const [adding, setAdding] = createSignal<"pair" | "machine" | null>(props.focus === "add-machine" ? "machine" : null);
  /** The entry shown open (one at a time; "this" is this device's). */
  const [open, setOpen] = createSignal<string | null>(props.focus === "this" ? "this" : null);

  // Keep entries and their sign-ins and machines by id: fresh replies must not replace open panels.
  // Keep the last reply visible during refreshes instead of returning to the loading screen.
  const [entryList, setEntries] = createStore<Computer[]>([]);
  createComputed(() => setEntries(reconcile(computers(devices.latest ?? [], machines.latest ?? []))));
  const entries = () => entryList;
  // Local folder controls also need their identity when the shell returns a fresh status.
  const [localView, setLocalView] = createStore<{ info: ThisComputer | null }>({ info: null });
  createComputed(() => setLocalView(reconcile({ info: local.latest ?? null }, { key: "path" })));
  /** Servers from before renaming say nothing of what each device is. */
  const canRename = () => (devices.latest ?? []).some((d) => d.client !== undefined);
  const isOpen = (c: Computer) => open() === c.id || (c.here && open() === "this");
  const toggle = (c: Computer) => setOpen(isOpen(c) ? null : c.id);
  /** The desktop app's own node, as the server sees it: in this computer's entry, else (an older node) by its name. */
  const localMachine = createMemo(() => {
    const n = localView.info?.node;
    if (!n?.enabled) return null;
    const here = entries().find((c) => c.here);
    return here?.machines.find((m) => m.name === n.name) ?? here?.machines[0] ?? (machines.latest ?? []).find((m) => m.name === n.name) ?? null;
  });

  const replaceMachine = (m: MachineInfo) => mutateMachines((l) => l?.map((x) => (x.id === m.id ? m : x)));
  const dropDevice = (id: string) => {
    mutateDevices((l) => l?.filter((d) => d.id !== id));
    mutateMachines((l) => l?.filter((m) => m.id !== id));
  };
  const dropMachine = (id: string) => mutateMachines((l) => l?.filter((m) => m.id !== id));

  const addMenu = (e: MouseEvent) =>
    openMenu(
      e.currentTarget as HTMLElement,
      [
        { label: "Pair a device", description: "A phone, tablet, laptop or browser that uses Tessera", icon: "phone", onSelect: () => setAdding("pair") },
        { label: "Add a machine", description: "Another computer your agents may use", icon: "laptop", onSelect: () => setAdding("machine") },
      ],
      "bottom-end",
      { sheet: false },
    );

  return (
    <div class="stack dev">
      <div class="dev-top">
        <p class="muted grow">One entry per device. A computer can also be a machine: your agents may use it, as far as that computer allows.</p>
        <button type="button" class="btn btn-primary dev-add-btn" aria-haspopup="menu" onClick={addMenu}>
          <Icon name="plus" size={15} /> Add…
        </button>
      </div>

      <Show when={adding() === "pair"}>
        <AddPanel title="Pair a device" onClose={() => setAdding(null)}>
          <PairPanel autostart />
        </AddPanel>
      </Show>
      <Show when={adding() === "machine"}>
        <AddPanel title="Add a machine" onClose={() => setAdding(null)}>
          <AddMachine
            known={() => machines.latest ?? []}
            local={localView.info}
            onLocal={async () => {
              try {
                const s = await nodeEnable(true);
                if (s) setLocal(s);
                setAdding(null);
                setOpen("this");
              } catch (e) {
                toastError(e, "Couldn't turn this computer on");
              }
            }}
          />
        </AddPanel>
      </Show>

      <Show
        when={devices.latest && machines.latest}
        fallback={
          <div class="empty-state">
            <Show when={devices.error || machines.error} fallback={<Spinner size={20} />}>
              <span class="empty-icon">
                <Icon name="alertCircle" size={20} />
              </span>
              <p>{errorMessage(devices.error || machines.error)}</p>
            </Show>
          </div>
        }
      >
        <ul class="device-list dev-list">
          <For each={entries()}>
            {(c) => (
              <Entry
                c={c}
                open={isOpen(c)}
                onToggle={() => toggle(c)}
                local={c.here && isDesktopApp ? localView.info : null}
                localMachine={c.here ? localMachine() : null}
                setLocal={(s) => setLocal(s)}
                refetchLocal={() => void refetchLocal()}
                onChanged={replaceMachine}
                onGone={dropDevice}
                onMachineGone={dropMachine}
                onRenamed={(list) => mutateDevices(list)}
                canRename={canRename()}
              />
            )}
          </For>
        </ul>
        <Show when={entries().length < 2}>
          <p class="muted small dev-empty">No other devices yet. Use Add… to pair your phone, or to let your agents use another computer.</p>
        </Show>
      </Show>
      <PageRow icon="listChecks" title="Activity" help="What your agents did on your computers, newest first." onOpen={() => setSettingsTab("activity")} />
    </div>
  );
}

function AddPanel(props: { title: string; onClose: () => void; children: JSX.Element }) {
  return (
    <section class="card dev-add" aria-label={props.title}>
      <header class="dev-add-head">
        <div class="setting-title grow">{props.title}</div>
        <button type="button" class="icon-btn sm" aria-label="Close" data-tip="Close" onClick={props.onClose}>
          <Icon name="x" size={15} />
        </button>
      </header>
      {props.children}
    </section>
  );
}

/** "Machine · Work in folders", paused, or stopped there by its user. */
function MachineBadge(props: { paused: boolean; stopped?: boolean; preset: MachineInfo["preset"] }) {
  return (
    <span class={`badge dev-mbadge ${props.paused || props.stopped ? "badge-warn" : "badge-accent"}`}>
      <Icon name={props.stopped ? "hand" : props.paused ? "pause" : "cpu"} size={11} />
      Machine · {props.stopped ? "Stopped there" : props.paused ? "Paused" : presetInfo(props.preset).label}
    </span>
  );
}

/** One computer, phone or browser: its name, what it's used for, and inside, each sign-in and its machine. */
function Entry(props: {
  c: Computer;
  open: boolean;
  onToggle: () => void;
  /** The desktop app's own computer: its node, for the local controls. */
  local: ThisComputer | null;
  localMachine: MachineInfo | null;
  setLocal: (s: ThisComputer) => void;
  refetchLocal: () => void;
  onChanged: (m: MachineInfo) => void;
  onGone: (id: string) => void;
  onMachineGone: (id: string) => void;
  onRenamed: (list: DeviceInfo[]) => void;
  canRename: boolean;
}) {
  const c = () => props.c;
  /** The node here, when this is the desktop app's own computer. */
  const node = () => props.local?.node ?? null;
  const localOn = () => !!node()?.enabled && !node()!.revoked;
  /** Machines shown from the server's side; on this very computer its local controls stand for them. */
  const remote = () => (node() ? [] : c().machines);
  const badge = () => {
    const m = node() ? (localOn() ? node()! : null) : (c().machines[0] ?? null);
    return m ? { paused: m.paused, stopped: !!m.scope.stopped, preset: m.preset } : null;
  };
  const online = () => c().online || (localOn() && !!node()!.connected);
  const state = () => (badge()?.paused ? "paused" : online() ? "online" : "offline");
  const meta = () => {
    const parts = [c().here ? "This device" : "", usedFor(c())];
    if (!c().here) parts.push(online() ? "online" : c().lastSeen ? `seen ${relTime(c().lastSeen)}` : "never seen");
    const os = c().machines[0]?.os;
    if (os && !c().here) parts.splice(2, 0, osLabel(os));
    return parts.filter(Boolean).join(" · ");
  };
  /** What agents call it, when that isn't the entry's name. */
  const agentName = () => {
    const n = node() ? (localOn() ? node()!.name : null) : c().machines[0]?.name;
    return n && n !== c().name ? n : null;
  };

  const [renaming, setRenaming] = createSignal(false);
  /** On the computer itself, one Rename gives its sign-ins and its machine the same name. */
  const canRenameHere = () => (c().renamable.length > 0 && props.canRename) || (c().here && !!node());
  const rename = async () => {
    const target = c().renamable[0];
    const local = c().here ? node() : null;
    if (!target && !local) return;
    const name = (await promptDialog({ title: `Rename ${c().name}`, label: "Name", value: c().name, placeholder: "e.g. Kitchen laptop", confirmLabel: "Rename" }))?.trim();
    if (!name || (name === c().name && (!local || local.name === name))) return;
    setRenaming(true);
    try {
      if (target && props.canRename && name !== c().name) {
        const list = await renameDevice(target.id, name);
        props.onRenamed(list);
        if (c().here) void rememberDeviceName(name);
      }
      // What agents call this computer: changed here, on the computer itself (never through the server).
      if (local && local.name !== name) {
        const s = await nodeName(name);
        if (s) props.setLocal(s);
      }
      toast({ level: "success", message: `Renamed to “${name}”.`, timeout: 2500 });
    } catch (e) {
      toastError(e, "Couldn't rename it");
    } finally {
      setRenaming(false);
    }
  };

  return (
    <li class="card dev-item" data-state={state()} data-entry={c().id} classList={{ open: props.open, here: c().here, "dev-this": !!node() }}>
      <button type="button" class="dev-row" classList={{ tagged: !!badge() }} aria-expanded={props.open} onClick={props.onToggle}>
        <span class="device-icon">
          <Icon name={c().icon} size={17} />
        </span>
        <span class="dev-id">
          <span class="dev-name">{c().name}</span>
          <span class="dev-meta">{meta()}</span>
        </span>
        <span class="dev-tags">
          <Show when={badge()}>{(b) => <MachineBadge paused={b().paused} stopped={b().stopped} preset={b().preset} />}</Show>
          <span class="dev-dot" classList={{ on: online() }} aria-label={online() ? "Online" : "Offline"} />
        </span>
        <Icon name="chevronDown" size={15} class="dev-chev" />
      </button>
      <Show when={props.open}>
        <div class="dev-details">
          <div class="dev-sec dev-naming">
            <div class="dev-naming-row">
              <span class="grow dev-naming-text">
                <Show
                  when={canRenameHere()}
                  fallback={<>Its name changes only on that computer (in its Tessera app, or <code>tessera node name</code>).</>}
                >
                  Called “{c().name}” on all your devices{node() && localOn() ? ", and by your agents" : ""}.
                </Show>
              </span>
              <Show when={canRenameHere()}>
                <button type="button" class="btn btn-sm" disabled={renaming()} onClick={() => void rename()} aria-label={`Rename ${c().name}`}>
                  <Icon name="pencil" size={13} /> Rename
                </button>
              </Show>
            </div>
            <Show when={agentName()}>
              {(n) => (
                <p class="mach-fine dev-agentname">
                  <Icon name="cpu" size={13} />
                  <span>
                    Agents call it <b>{n()}</b>.{" "}
                    <Show when={node()} fallback={<>That name changes only on {n()} itself (in its Tessera app, or <code>tessera node name</code>).</>}>
                      Rename it here to give it one name everywhere.
                    </Show>
                  </span>
                </p>
              )}
            </Show>
          </div>
          <For each={c().devices}>
            {(d) => <SignIn d={d} entry={c()} carries={c().machines.find((m) => m.id === d.id) ?? (props.localMachine?.id === d.id ? props.localMachine : null)} onGone={props.onGone} />}
          </For>
          <Show when={c().here && isDesktopApp}>
            <KeepRunning />
          </Show>
          <Show when={node()}>
            <div class="dev-sec">
              <div class="dev-sec-title">Machine for agents · this computer</div>
              <ThisComputerPanel info={props.local!} setInfo={props.setLocal} refetch={props.refetchLocal} machine={props.localMachine} />
            </div>
          </Show>
          <For each={remote()}>
            {(m) => (
              <div class="dev-sec">
                <div class="dev-sec-title">Machine for agents{c().machines.length > 1 ? ` · ${m.name}` : ""}</div>
                <MachineDetails m={m} onChanged={props.onChanged} onGone={props.onMachineGone} />
              </div>
            )}
          </For>
        </div>
      </Show>
    </li>
  );
}

/** One way this device is signed in (its app, its terminal…): when, and Sign out. */
function SignIn(props: { d: DeviceInfo; entry: Computer; carries: MachineInfo | null; onGone: (id: string) => void }) {
  const d = () => props.d;
  const role = () => {
    const k = clientOf(d());
    return k === "unknown" && props.entry.kind === "phone" ? ROLE.phone : ROLE[k];
  };
  const machineOnly = () => clientOf(d()) === "machine";
  const [busy, setBusy] = createSignal(false);
  const signOut = async () => {
    const where = props.entry.name;
    const m = props.carries;
    const ok = await confirmDialog({
      title: machineOnly() ? `Sign out the machine on ${where}?` : `Sign out ${role().label.toLowerCase().replace(/ \(.*\)$/, "")} on ${where}?`,
      body: machineOnly()
        ? `It is disconnected right away and agents can't use it any more. To use it again, pair it again on that computer. To stop agents for a while, use Pause instead.`
        : `It is disconnected right away and must be paired again to come back.` +
          (m ? ` ${m.name}'s machine connection uses this sign-in, so agents can't use ${m.name} either until it's turned on again on that computer.` : ""),
      confirmLabel: "Sign out",
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    try {
      await api.revokeDevice(d().id);
      props.onGone(d().id);
    } catch (e) {
      toastError(e, `Couldn't sign out ${d().name}`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div class="dev-sec dev-device" data-client={clientOf(d())}>
      <Icon name={role().icon} size={15} class="dev-role-icon" />
      <span class="grow dev-signin">
        <span class="dev-role">
          {role().label}
          <Show when={d().current}> · this device</Show>
          <Show when={props.carries && !machineOnly()}> · the machine uses it</Show>
        </span>
        <span class="muted small">
          <Show when={d().name !== props.entry.name}>“{d().name}” · </Show>
          <Show when={d().model && d().model !== d().name}>{d().model} · </Show>
          paired {fmtDateTime(d().created_at)}
          <Show when={d().last_seen}> · active {relTime(d().last_seen!)}</Show>
        </span>
      </span>
      <Show when={!d().current}>
        <button type="button" class="btn btn-sm btn-ghost danger-text" disabled={busy()} onClick={() => void signOut()} aria-label={`Sign out ${d().name}`}>
          <Icon name="logOut" size={14} /> Sign out
        </button>
      </Show>
    </div>
  );
}

/** A machine as any device sees it: what it allows, who can use it, what it did; pause or revoke. */
function MachineDetails(props: { m: MachineInfo; onChanged: (m: MachineInfo) => void; onGone: (id: string) => void }) {
  const m = () => props.m;
  const [busy, setBusy] = createSignal(false);
  const p = () => presetsFor(m().os, m().features, m().sandbox_ok).find((p) => p.value === m().preset) ?? presetInfo(m().preset, m().os);
  const togglePause = async () => {
    setBusy(true);
    try {
      props.onChanged(await setMachinePaused(m().id, !m().paused));
    } catch (e) {
      toastError(e, m().paused ? `Couldn't resume ${m().name}` : `Couldn't pause ${m().name}`);
    } finally {
      setBusy(false);
    }
  };
  const revoke = async () => {
    const ok = await confirmDialog({
      title: `Stop agents using ${m().name}?`,
      body: `Agents lose access to ${m().name} right away and anything running there stops. It stays paired; to use it again, turn it on again on that computer.`,
      confirmLabel: "Stop agents using it",
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    try {
      await revokeMachine(m().id);
      props.onGone(m().id);
    } catch (e) {
      toastError(e, `Couldn't stop agents using ${m().name}`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div class="dev-machine">
      <div class="dev-sec">
        <div class="dev-preset" classList={{ warn: !!p().warn, danger: !!p().danger }}>
          <Icon name={p().icon} size={15} />
          <span>
            <b>{p().label}.</b> {p().sentence}
          </span>
        </div>
        <ul class="mach-scope" aria-label={`What ${m().name} allows`}>
          <For each={scopeLines(m().scope, m().sandbox_ok, m().home, m().os)}>
            {(l) => (
              <li classList={{ warn: !!l.warn }}>
                <Icon name={l.icon} size={13} />
                <span>{l.text}</span>
              </li>
            )}
          </For>
          <li>
            <Icon name="users" size={13} />
            <span>
              {ACCESS_WORDS.title}: {accessLabel(m().scope.bots ?? null)}
            </span>
          </li>
          <Show when={machineBots(m())}>
            {(bots) => (
              <li class="mach-bots">
                <Icon name="bot" size={13} />
                <span>Bots that may use it: {bots()}. Set per bot in its settings.</span>
              </li>
            )}
          </Show>
        </ul>
        <p class="mach-fine">
          <Icon name="lock" size={13} />
          <span>Change what it allows, and its name, on that computer. From here you can only pause it or stop agents using it.</span>
        </p>
      </div>
      <Show when={m().paused}>
        <p class="mach-note">Agents can't use it until you resume it. Resuming never gives more access than {m().name} allows.</p>
      </Show>
      <Show when={m().scope.stopped}>
        <p class="mach-note">Stopped on {m().name} (its Stop bar or Stop key): agents can't use it until someone resumes it on that computer.</p>
      </Show>
      <div class="mach-actions">
        <Show when={m().recent.length}>
          <button type="button" class="btn btn-sm" onClick={() => showActivity(m().id)} aria-label={`What agents did on ${m().name}`}>
            <Icon name="listChecks" size={14} /> Activity
          </button>
        </Show>
        <button type="button" class="btn btn-sm" aria-disabled={busy()} onClick={() => !busy() && void togglePause()} aria-label={m().paused ? `Resume ${m().name}` : `Pause ${m().name}`}>
          <Show when={busy()} fallback={<Icon name={m().paused ? "play" : "pause"} size={14} />}>
            <Spinner size={13} />
          </Show>
          {m().paused ? "Resume" : "Pause"}
        </button>
        <button type="button" class="btn btn-sm btn-ghost danger-text" disabled={busy()} onClick={() => void revoke()} aria-label={`Stop agents using ${m().name}`}>
          Stop agents using it
        </button>
      </div>
    </div>
  );
}

/**
 * Add a machine: in the desktop app, this computer in one click; any other
 * computer with one command run there (it pairs that computer and turns it
 * on), then it shows up here by itself.
 */
function AddMachine(props: { known: () => MachineInfo[]; local: ThisComputer | null; onLocal: () => Promise<void> }) {
  const [os, setOs] = createSignal<"linux" | "macos" | "windows">(/mac/i.test(navigator.platform) ? "macos" : /win/i.test(navigator.platform) ? "windows" : "linux");
  const install = () => (os() === "windows" ? INSTALL_COMMAND_WINDOWS : INSTALL_COMMAND);
  const [code, setCode] = createSignal<PairingCode | null>(null);
  const [failed, setFailed] = createSignal<unknown>(null);
  const [now, setNow] = createSignal(Date.now());
  const before = new Set(props.known().map((m) => m.id));
  const joined = () => props.known().find((m) => !before.has(m.id)) ?? null;
  const tick = setInterval(() => setNow(Date.now()), 1000);
  onCleanup(() => clearInterval(tick));
  const fresh = async () => {
    setFailed(null);
    try {
      setCode(await api.createPairing());
    } catch (e) {
      setFailed(e);
    }
  };
  void fresh();
  const left = () => Math.max(0, (code()?.expires_at ?? 0) - now());
  const command = () => (code()?.link ? enableCommand(code()!.link!) : `tessera connect <your server's address> --code ${code()?.code ?? ""} && tessera node enable`);
  /** On screen the one-time code stays out of view; copying gives the whole command. */
  const shown = () => (code()?.link ? "tessera node enable 'tessera://pair…'" : command());
  const copy = async () => {
    if (await copyText(command())) toast({ level: "success", message: "Command copied. It holds a one-time code: run it only on that computer.", timeout: 3500 });
  };
  const localOn = () => !!props.local?.node?.enabled;
  return (
    <div class="dev-addm">
      <Show when={isDesktopApp && props.local?.node && !localOn()}>
        <div class="dev-addm-local">
          <Icon name="monitor" size={16} />
          <span class="grow">
            <span class="strong">This computer</span>
            <span class="muted small"> · let your agents use {props.local!.node!.name}. You choose what they may do next.</span>
          </span>
          <button type="button" class="btn btn-sm btn-primary" onClick={() => void props.onLocal()}>
            Use this computer
          </button>
        </div>
      </Show>
      <Show when={isDesktopApp && props.local && !props.local.node}>
        <p class="mach-fine">
          <Icon name="info" size={13} />
          <span>
            To let agents use this computer, install the <code>tessera</code> program here first (in a terminal: <code>{installCommandHere()}</code>), then come back.
          </span>
        </p>
      </Show>
      <div class="dev-addm-other">
        <div class="setting-title">{isDesktopApp ? "Another computer" : "A computer"}</div>
        <Segmented
          value={os()}
          options={[
            { value: "linux", label: "Linux" },
            { value: "macos", label: "macOS" },
            { value: "windows", label: "Windows" },
          ]}
          onChange={setOs}
          label="Its system"
          class="sm"
        />
        <ol class="dev-steps">
          <li>
            <span>
              Open {os() === "windows" ? "PowerShell" : "a terminal"} on that computer. Tessera not installed there yet?{" "}
              <button type="button" class="link-btn" onClick={() => void openExternal(INSTALL_URL)}>
                Install it first
              </button>
              :
            </span>
            <span class="mach-cmd">
              <code>{install()}</code>
              <CopyButton text={install} label="Copy install command" size={13} class="sm" />
            </span>
          </li>
          <li>
            <span>Run this there. It connects that computer to this server and asks what agents may do on it:</span>
            <Show when={code()} fallback={<Show when={failed()} fallback={<Spinner size={16} />}>{<span class="danger-text small">{errorMessage(failed())}</span>}</Show>}>
              <Show
                when={left() > 0}
                fallback={
                  <button type="button" class="btn btn-sm" onClick={() => void fresh()}>
                    <Icon name="refresh" size={13} /> New command
                  </button>
                }
              >
                <span class="mach-cmd dev-enable-cmd">
                  <code>{shown()}</code>
                  <button type="button" class="icon-btn sm" aria-label="Copy command" data-tip="Copy" onClick={() => void copy()}>
                    <Icon name="copy" size={13} />
                  </button>
                </span>
                <span class="muted small tabular">
                  Works once, for {Math.floor(left() / 60000)}:{String(Math.floor((left() % 60000) / 1000)).padStart(2, "0")} more
                </span>
              </Show>
            </Show>
            <Show when={os() === "macos"}>
              <span class="muted small">On macOS agents can read and change files; running commands needs Linux.</span>
            </Show>
            <Show when={os() === "windows"}>
              <span class="muted small">On Windows agents can read and change files; commands run only with Full access, without a sandbox (Tessera has none on Windows yet).</span>
            </Show>
          </li>
          <li>
            <Show
              when={joined()}
              fallback={
                <span class="dev-wait">
                  <Spinner size={13} /> Waiting for it to connect…
                </span>
              }
            >
              <span class="dev-joined">
                <Icon name="circleCheck" size={15} /> {joined()!.name} is connected as a machine ({presetInfo(joined()!.preset).label}). Change what it allows on that computer.
              </span>
            </Show>
          </li>
        </ol>
        <p class="mach-fine">
          <Icon name="lock" size={13} />
          <span>Only that computer can turn itself on or let agents do more there. From here you can pause it or stop agents using it.</span>
        </p>
      </div>
    </div>
  );
}
