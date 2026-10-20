// The bot editor's "Machines": what this bot may do on each of the user's
// computers turned on as machines (Settings → Devices), including a desktop
// app turned on as one and the web views it can host. Never more than the
// machine itself allows: the server checks both. New bots start with no
// access; bots made before this setting use each machine as it allows until
// they're given less here.
import { For, Show, createResource, onCleanup } from "solid-js";
import type { BotMachines as Access } from "../../proto/BotMachines";
import type { MachineAccess } from "../../proto/MachineAccess";
import { listMachines, onMachinesChanged, type MachineInfo } from "../../lib/machines";
import { presetInfo } from "../../lib/machine-settings";
import { ACCESS_CHOICES, accessHelp, machineLetsBot } from "./presets";
import { Icon } from "../icons";
import { Segmented } from "../primitives";

export function BotMachines(props: {
  /** Undefined: made before the setting (each machine as it allows). */
  value: Access | undefined;
  onChange: (v: Access) => void;
  botId: string | null;
  handle: string;
  name: string;
}) {
  const [list, { refetch }] = createResource(() => listMachines().catch(() => [] as MachineInfo[]));
  onCleanup(onMachinesChanged(() => void refetch()));
  const machines = () => [...(list() ?? [])].sort((a, b) => a.name.localeCompare(b.name));
  const legacy = () => !props.value;
  /** The setting as it stands (older bots: as each machine allows). */
  const current = (): Access => props.value ?? { default: "allowed", each: {} };
  const levelOf = (id: string): MachineAccess => current().each?.[id] ?? current().default;
  const set = (id: string, level: MachineAccess) => props.onChange({ ...current(), each: { ...(current().each ?? {}), [id]: level } });
  const setDefault = (level: MachineAccess) => props.onChange({ ...current(), default: level });
  return (
    <section class="be-sec be-machines">
      <h3 class="be-h">
        <Icon name="cpu" size={16} /> Machines
      </h3>
      <p class="muted small be-note">
        What {props.name} may do on your computers turned on as machines. Never more than a machine allows itself; set that on the computer.
      </p>
      <Show when={legacy()}>
        <p class="small be-mach-legacy">
          <Icon name="info" size={13} /> Made before this setting: it may use each machine as that machine allows. Choose less here.
        </p>
      </Show>
      <div class="be-mach-list" role="group" aria-label={`${props.name}'s access to machines`}>
        <For each={machines()}>
          {(m) => {
            const closed = () => !machineLetsBot(m.scope, props.botId, props.handle);
            return (
              <div class="be-mach" data-machine={m.id}>
                <div class="be-mach-head">
                  <Icon name="monitor" size={15} />
                  <span class="be-mach-name">{m.name}</span>
                  <span class="be-mach-preset">{m.paused ? "Paused" : presetInfo(m.preset).label}</span>
                </div>
                <Segmented<MachineAccess> label={`${props.name} on ${m.name}`} value={levelOf(m.id)} onChange={(v) => set(m.id, v)} options={ACCESS_CHOICES} class="be-mach-seg" />
                <span class="field-help" classList={{ "warn-text": closed() && levelOf(m.id) !== "no_access" }}>
                  {closed() && levelOf(m.id) !== "no_access"
                    ? `${m.name} itself lets only certain bots use it, and not this one: it can't use it.`
                    : accessHelp(levelOf(m.id), m.name)}
                </span>
              </div>
            );
          }}
        </For>
        <div class="be-mach be-mach-other" data-machine="">
          <div class="be-mach-head">
            <Icon name="grid" size={15} />
            <span class="be-mach-name">{machines().length ? "Other machines" : "Machines"}</span>
            <span class="be-mach-preset">{machines().length ? "and ones turned on later" : "turned on later"}</span>
          </div>
          <Segmented<MachineAccess> label={`${props.name} on other machines`} value={current().default} onChange={setDefault} options={ACCESS_CHOICES} class="be-mach-seg" />
          <span class="field-help">{accessHelp(current().default, null)}</span>
        </div>
      </div>
      <Show when={list() && !machines().length}>
        <p class="muted small">No machines yet: turn a computer into one in Settings → Devices, on that computer.</p>
      </Show>
    </section>
  );
}
