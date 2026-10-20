// "Who can use it": any chat, or only some bots (their chats, scheduled
// duties and the helpers they start). Shared by Passwords & keys and
// Machines, so both say it the same way. The value is `null` for any chat,
// or the ids of the bots that may (an empty list: nobody yet).
import { For, Show } from "solid-js";
import "../../styles/access.css";
import { botById, state } from "../../lib/store";
import { openBotEditor } from "../../lib/ui";
import { BotAvatar } from "../bots";
import { Icon } from "../icons";

/** The words both places use. */
export const ACCESS_WORDS = {
  title: "Who can use it",
  any: "Any chat",
  anyAsks: "Any chat (asks first)",
  only: "Only these bots",
};

/** One line for a list row: "Any chat", "Only Sentinel", "Only Sentinel and Forge", "Nobody yet". */
export function accessLabel(bots: string[] | null | undefined, any = ACCESS_WORDS.any): string {
  if (bots == null) return any;
  const names = bots.map((id) => botById(id)?.name).filter((n): n is string => !!n);
  if (!names.length) return "Nobody yet";
  if (names.length <= 2) return `Only ${names.join(" and ")}`;
  return `Only ${names.slice(0, 2).join(", ")} and ${names.length - 2} more`;
}

export function AccessPicker(props: {
  value: string[] | null;
  onChange: (v: string[] | null) => void;
  /** Heading; `""` hides it. Default "Who can use it". */
  label?: string;
  /** Default "Any chat (asks first)". */
  anyLabel?: string;
  /** Default "Only these bots". */
  onlyLabel?: string;
  /** One line under the choice. */
  anyHelp?: string;
  onlyHelp?: string;
  disabled?: boolean;
}) {
  const only = () => props.value != null;
  // Bots that no longer exist are dropped from the list on the next change.
  const picked = () => (props.value ?? []).filter((id) => botById(id));
  const bots = () => [...state.bots].sort((a, b) => a.name.localeCompare(b.name));
  const toggle = (id: string) => {
    const cur = picked();
    props.onChange(cur.includes(id) ? cur.filter((x) => x !== id) : [...cur, id]);
  };
  const label = () => props.label ?? ACCESS_WORDS.title;
  return (
    <div class="access" classList={{ disabled: props.disabled }}>
      <Show when={label()}>
        <span class="field-label">{label()}</span>
      </Show>
      <div class="access-modes" role="radiogroup" aria-label={label() || ACCESS_WORDS.title}>
        <button type="button" role="radio" class="access-mode" aria-checked={!only()} disabled={props.disabled} onClick={() => props.onChange(null)}>
          <Icon name="chat" size={15} />
          <span>{props.anyLabel ?? ACCESS_WORDS.anyAsks}</span>
        </button>
        <button type="button" role="radio" class="access-mode" aria-checked={only()} disabled={props.disabled} onClick={() => !only() && props.onChange([])}>
          <Icon name="bot" size={15} />
          <span>{props.onlyLabel ?? ACCESS_WORDS.only}</span>
        </button>
      </div>
      <span class="field-help">
        {only()
          ? (props.onlyHelp ?? "Only the bots you pick, in their chats, scheduled duties and helpers, without asking. Other chats don't see it.")
          : (props.anyHelp ?? "Every chat can use it, and asks you first.")}
      </span>
      <Show when={only()}>
        <div class="access-bots" role="group" aria-label="Bots">
          <For each={bots()}>
            {(b) => (
              <button type="button" class="chip access-bot" aria-pressed={picked().includes(b.id)} disabled={props.disabled} onClick={() => toggle(b.id)}>
                <BotAvatar bot={b} size={18} />
                <span>{b.name}</span>
                <Show when={picked().includes(b.id)}>
                  <Icon name="check" size={13} />
                </Show>
              </button>
            )}
          </For>
          <Show when={!bots().length}>
            <span class="field-help">
              No bots yet.{" "}
              <button type="button" class="link-btn" onClick={() => openBotEditor()}>
                Create one
              </button>
            </span>
          </Show>
        </div>
        <Show when={bots().length && !picked().length}>
          <span class="field-help warn-text">
            <Icon name="alert" size={13} /> Pick at least one bot: until then nothing can use it.
          </span>
        </Show>
      </Show>
    </div>
  );
}
