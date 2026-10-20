// Shared write-only input for the vault editor and secure chat entry. Values
// are local form state only, never fetched back from the vault.
import { Show, createSignal } from "solid-js";
import { toast } from "../lib/ui";
import { Icon } from "./icons";

function strength(v: string): { label: string; tone: "weak" | "fair" | "strong" } | null {
  if (!v) return null;
  let pool = 0;
  if (/[a-z]/.test(v)) pool += 26;
  if (/[A-Z]/.test(v)) pool += 26;
  if (/\d/.test(v)) pool += 10;
  if (/[^a-zA-Z0-9]/.test(v)) pool += 32;
  const bits = v.length * Math.log2(Math.max(pool, 2));
  if (bits < 45) return { label: "Weak — easy to guess", tone: "weak" };
  if (bits < 70) return { label: "Fair", tone: "fair" };
  return { label: "Strong", tone: "strong" };
}

export function SecretInput(props: {
  value: string;
  onInput: (v: string) => void;
  secret?: boolean;
  placeholder?: string;
  type?: string;
  label: string;
  strength?: boolean;
  mono?: boolean;
  autofocus?: boolean;
}) {
  const [show, setShow] = createSignal(false);
  const s = () => (props.strength ? strength(props.value) : null);
  const paste = async () => {
    try {
      const t = await navigator.clipboard.readText();
      if (t) props.onInput(t.trim());
    } catch {
      toast({ level: "info", message: "Paste isn't allowed here — use the keyboard (Ctrl+V or long-press)." });
    }
  };
  return (
    <div class="sv">
      <div class="sv-row">
        <input
          class="input"
          classList={{ mono: props.mono || props.secret }}
          type={props.secret && !show() ? "password" : props.type ?? "text"}
          value={props.value}
          placeholder={props.placeholder}
          aria-label={props.label}
          autocomplete={props.secret ? "new-password" : "off"}
          autocapitalize="off"
          autocorrect="off"
          spellcheck={false}
          autofocus={props.autofocus}
          data-1p-ignore
          data-lpignore="true"
          onInput={(e) => props.onInput(e.currentTarget.value)}
        />
        <Show when={props.secret}>
          <button type="button" class="icon-btn sm" aria-label={show() ? `Hide ${props.label}` : `Show ${props.label}`} data-tip={show() ? "Hide" : "Show what you're typing"} onClick={() => setShow(!show())}>
            <Icon name={show() ? "eyeOff" : "eye"} size={15} />
          </button>
        </Show>
        <Show when={typeof navigator !== "undefined" && !!navigator.clipboard?.readText}>
          <button type="button" class="icon-btn sm" aria-label={`Paste ${props.label}`} data-tip="Paste" onClick={() => void paste()}>
            <Icon name="copy" size={15} />
          </button>
        </Show>
      </div>
      <Show when={s()}>
        <span class={`sv-strength ${s()!.tone}`}><i /><i /><i />{s()!.label}</span>
      </Show>
    </div>
  );
}
