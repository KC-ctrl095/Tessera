// "Let saved passwords be used on my machines" (`secret_machines`): off, on
// the machines picked, or on all of them, for every password that doesn't
// choose for itself. Each machine still decides what it runs; the server
// passes a value only to the command that names it, over the encrypted
// channel, and both sides remove it from what comes back.
import { For, Show } from "solid-js";
import type { SecretMachines } from "../../proto/SecretMachines";
import type { MachineInfo } from "../../lib/machines";
import { confirmDialog } from "../../lib/ui";
import { Icon } from "../icons";
import "../../styles/credentials.css";

export const MACHINE_CHOICES: { value: SecretMachines; label: string; hint: string }[] = [
  { value: "off", label: "Off", hint: "Saved passwords are used only on this server, unless you allow a machine for one of them." },
  { value: "picked", label: "Machines I pick", hint: "Commands on the machines you pick may use your saved passwords. Each machine still decides what it runs." },
  { value: "all", label: "All my machines", hint: "Commands on any of your machines, also ones you add later, may use your saved passwords. Each machine still decides what it runs." },
];

/** Where the setting lets passwords go now, in a few words: "No machines", "Desktop", "All your machines". */
export function machinesNow(mode: SecretMachines | undefined, picked: string[], machines: MachineInfo[]): string {
  if (mode === "all") return "All your machines";
  if (mode !== "picked") return "No machines";
  const names = picked.map((id) => machines.find((m) => m.id === id)?.name).filter((n): n is string => !!n);
  if (!names.length) return "No machines yet";
  return names.length <= 2 ? names.join(" and ") : `${names.slice(0, 2).join(", ")} and ${names.length - 2} more`;
}

/** Asks before saved passwords may go to more machines than now. */
export async function confirmMoreMachines(names: string[] | "all"): Promise<boolean> {
  const where = names === "all" ? "any of your machines" : names.join(", ");
  return confirmDialog({
    title: names === "all" ? "Use saved passwords on all your machines?" : `Use saved passwords on ${where}?`,
    body:
      `Agents could then use them in commands on ${where}. A password goes only to the command that names it and is removed from what comes back, ` +
      "but a program on that computer could keep it. Allow only computers you trust. Each password can still be kept to this server.",
    confirmLabel: "Allow",
  });
}

export function MachinesChoice(props: {
  value: SecretMachines;
  picked: string[];
  machines: MachineInfo[];
  busy: boolean;
  onChange: (value: SecretMachines, picked?: string[]) => void;
}) {
  const chosen = () => MACHINE_CHOICES.find((o) => o.value === props.value) ?? MACHINE_CHOICES[0];
  const rank = (v: SecretMachines) => ["off", "picked", "all"].indexOf(v);
  const choose = async (v: SecretMachines) => {
    if (v === props.value) return;
    if (rank(v) > rank(props.value)) {
      // Picking goes to the machines picked before (none yet: nowhere, so no need to ask).
      const names = props.picked.map((id) => props.machines.find((m) => m.id === id)?.name).filter((n): n is string => !!n);
      if (v === "all" && !(await confirmMoreMachines("all"))) return;
      if (v === "picked" && names.length && !(await confirmMoreMachines(names))) return;
    }
    props.onChange(v);
  };
  const toggle = async (m: MachineInfo) => {
    const on = props.picked.includes(m.id);
    if (!on && !(await confirmMoreMachines([m.name]))) return;
    props.onChange("picked", on ? props.picked.filter((x) => x !== m.id) : [...props.picked, m.id]);
  };
  return (
    <div class="cred-machines-choice">
      <div class="segmented cred-machines-seg" role="radiogroup" aria-label="Let saved passwords be used on my machines">
        <For each={MACHINE_CHOICES}>
          {(o) => (
            <button type="button" role="radio" aria-checked={props.value === o.value} classList={{ active: props.value === o.value }} disabled={props.busy} onClick={() => void choose(o.value)}>
              <span>{o.label}</span>
            </button>
          )}
        </For>
      </div>
      <p class="setting-help cred-machines-hint">{chosen().hint}</p>
      <Show when={props.value === "picked"}>
        <div class="cred-places" role="group" aria-label="Machines that may use saved passwords">
          <For each={props.machines}>
            {(m) => (
              <button type="button" class="chip cred-place" aria-pressed={props.picked.includes(m.id)} disabled={props.busy} onClick={() => void toggle(m)}>
                <Icon name="laptop" size={14} />
                <span>{m.name}</span>
                <Show when={!m.online}>
                  <span class="cred-off">offline</span>
                </Show>
                <Show when={props.picked.includes(m.id)}>
                  <Icon name="check" size={13} />
                </Show>
              </button>
            )}
          </For>
        </div>
        <Show when={!props.picked.length}>
          <span class="field-help warn-text">
            <Icon name="alert" size={13} /> Pick the machines: until then this is the same as Off.
          </span>
        </Show>
      </Show>
    </div>
  );
}
