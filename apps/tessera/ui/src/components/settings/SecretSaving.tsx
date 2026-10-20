// How passwords and keys typed in a chat are saved (`secret_saving`): at
// once, after asking, or not at all (kept for that chat). On Passwords & keys,
// under the switch that takes them out of chats.
import { For } from "solid-js";
import type { SecretSaving } from "../../proto/SecretSaving";
import "../../styles/credentials.css";

/** The choices for `secret_saving`, in plain words. */
export const SAVING: { value: SecretSaving; label: string; hint: string }[] = [
  {
    value: "auto",
    label: "Save automatically",
    hint: "They go to Passwords & keys right away, for any chat to use (asking you before the first use). Agents save a login you type without asking.",
  },
  {
    value: "ask",
    label: "Ask me first",
    hint: "Each one is kept for that chat only, and the chat asks whether to save it to Passwords & keys. Agents ask you before they save one.",
  },
  {
    value: "chat_only",
    label: "Don't save",
    hint: "They stay with the chat they were typed in and are deleted with it; nothing is added to Passwords & keys.",
  },
];

/** The three-way choice and what the chosen one does. `on`: the switch that takes them out of chats is on. */
export function SavingChoice(props: { value: SecretSaving; on: boolean; busy: boolean; onChange: (v: SecretSaving) => void }) {
  const chosen = () => SAVING.find((o) => o.value === props.value) ?? SAVING[0];
  return (
    <div class="cred-saving-choice" classList={{ off: !props.on }}>
      <div class="segmented cred-saving-seg" role="radiogroup" aria-label="Passwords and keys you type in a chat">
        <For each={SAVING}>
          {(o) => (
            <button
              type="button"
              role="radio"
              aria-checked={props.value === o.value}
              classList={{ active: props.value === o.value }}
              disabled={props.busy || !props.on}
              onClick={() => props.value !== o.value && props.onChange(o.value)}
            >
              <span>{o.label}</span>
            </button>
          )}
        </For>
      </div>
      <p class="setting-help cred-saving-hint">{chosen().hint}</p>
    </div>
  );
}
