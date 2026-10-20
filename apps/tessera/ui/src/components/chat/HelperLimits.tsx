// Helper limits: how many helpers work at once, and how long each may work. One chat's own (Chat
// options → Helpers, loaded when the options open) and the defaults of chats that set none
// (Settings → Models). Plain number fields or an explicit "No limit".
import { Show, createSignal, onMount } from "solid-js";
import type { HelperLimits } from "../../proto/HelperLimits";
import type { HelperSettings } from "../../proto/HelperSettings";
import type { SessionInfo } from "../../proto/SessionInfo";
import { api } from "../../lib/store";
import { toast, toastError } from "../../lib/ui";
import "../../styles/helper-limits.css";

/** The largest number the server takes (`HelperSettings.max`; older servers said less). */
export const LIMIT_MAX = 0xffff_ffff;

type Key = "at_once" | "steps" | "minutes";
const FIELDS: { key: Key; label: string; unit: string }[] = [
  { key: "at_once", label: "At once", unit: "helpers" },
  { key: "steps", label: "Steps each", unit: "steps" },
  { key: "minutes", label: "Time each", unit: "min" },
];

/** "" → null (the default), a whole number from 1 to `max` → it, anything else → undefined. */
export function parseLimit(text: string, max = LIMIT_MAX): number | null | undefined {
  const t = text.trim();
  if (!t) return null;
  const n = Number(t);
  return Number.isInteger(n) && n >= 1 && n <= max ? n : undefined;
}

/** One limit: a number field, its unit, and (for a chat) "No limit". */
export function LimitField(props: {
  label: string;
  unit: string;
  max?: number;
  value: string;
  placeholder?: string;
  none?: boolean;
  onInput: (v: string) => void;
  onNone?: (on: boolean) => void;
  onEnter?: () => void;
}) {
  return (
    <div class="hl-row" classList={{ none: !!props.none }}>
      <label class="hl-label">
        <span>{props.label}</span>
        <input
          class="input hl-num"
          type="number"
          inputmode="numeric"
          min="1"
          max={props.max ?? LIMIT_MAX}
          step="1"
          aria-label={props.label}
          placeholder={props.none ? "" : props.placeholder}
          value={props.none ? "" : props.value}
          disabled={props.none}
          onInput={(e) => props.onInput(e.currentTarget.value)}
          onKeyDown={(e) => e.key === "Enter" && props.onEnter?.()}
        />
        <span class="hl-unit">{props.unit}</span>
      </label>
      <Show when={props.onNone}>
        <label class="hl-none">
          <input type="checkbox" checked={!!props.none} onChange={(e) => props.onNone!(e.currentTarget.checked)} />
          <span>No limit</span>
        </label>
      </Show>
    </div>
  );
}

let known: HelperSettings | null = null;
/** The server's defaults, fetched once per visit (Settings refreshes it when it saves). */
export async function helperDefaults(): Promise<HelperSettings | null> {
  try {
    known = await api.getHelpers();
  } catch {
    // Never reuse defaults from a previously connected server.
    known = null;
  }
  return known;
}
export const rememberDefaults = (h: HelperSettings) => (known = h);

const part = (key: Key, n: number | undefined): string => {
  if (n === undefined) return "";
  if (key === "at_once") return n === 0 ? "any number at once" : `${n} at once`;
  if (key === "steps") return n === 0 ? "no step limit" : `${n} steps`;
  return n === 0 ? "no time limit" : `${n} min`;
};

/** "4 at once · 40 steps · 15 min": the chat's own numbers, else the server's. */
export function limitsSummary(own: HelperLimits, d: HelperSettings | null): string {
  if (d?.enabled === false) return "Disabled on this server";
  const of = (k: Key) => own[k] ?? (d ? (k === "at_once" ? d.at_once : (d[k] ?? undefined)) : undefined);
  const text = FIELDS.map((f) => part(f.key, of(f.key))).filter(Boolean).join(" · ");
  return text || "Server defaults";
}

/**
 * Chat options → Helpers → Limits: this chat's numbers in one line, Change opens three fields in
 * place. Empty follows the server's default; "No limit" is a deliberate choice with a word on cost.
 */
export function ChatHelperLimits(props: { session: SessionInfo }) {
  const [d, setD] = createSignal<HelperSettings | null>(known);
  const [open, setOpen] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [text, setText] = createSignal<Record<Key, string>>({ at_once: "", steps: "", minutes: "" });
  const [none, setNone] = createSignal<Record<Key, boolean>>({ at_once: false, steps: false, minutes: false });
  onMount(() => void helperDefaults().then(setD));
  const own = () => props.session.helper_limits ?? {};

  const edit = () => {
    const o = own();
    const t = { at_once: "", steps: "", minutes: "" };
    const n = { at_once: false, steps: false, minutes: false };
    for (const f of FIELDS) {
      const v = o[f.key];
      if (v === 0) n[f.key] = true;
      else if (v != null) t[f.key] = String(v);
    }
    setText(t);
    setNone(n);
    setOpen(true);
  };
  const placeholder = (k: Key) => {
    const v = d() ? (k === "at_once" ? d()!.at_once : d()![k]) : undefined;
    return v === 0 ? "No limit" : v != null ? `${v}` : "Default";
  };
  const save = async (limits?: HelperLimits) => {
    if (busy()) return;
    if (!limits) {
      limits = {};
      for (const f of FIELDS) {
        if (none()[f.key]) {
          limits[f.key] = 0;
          continue;
        }
        const v = parseLimit(text()[f.key]);
        if (v === undefined) {
          toast({ level: "error", message: `${f.label}: a whole number from 1 to ${LIMIT_MAX}, or No limit.` });
          return;
        }
        if (v !== null) limits[f.key] = v;
      }
    }
    setBusy(true);
    try {
      await api.setChatHelpers(props.session.id, limits);
      setOpen(false);
    } catch (e) {
      toastError(e, "Could not change the helper limits");
    } finally {
      setBusy(false);
    }
  };
  const anyNone = () => FIELDS.some((f) => none()[f.key]);

  return (
    <div class="hl-chat" classList={{ open: open() }}>
      <Show
        when={open()}
        fallback={
          <button type="button" class="opt-line hl-sum" onClick={edit} aria-label="Helper limits">
            <span class="opt-line-label">Limits</span>
            <span class="opt-line-value">{limitsSummary(own(), d())}</span>
            <span class="opt-line-action">Change</span>
          </button>
        }
      >
        <div class="hl-edit" role="group" aria-label="Helper limits for this chat">
          {FIELDS.map((f) => (
            <LimitField
              label={f.label}
              unit={f.unit}
              value={text()[f.key]}
              placeholder={placeholder(f.key)}
              none={none()[f.key]}
              onInput={(v) => setText({ ...text(), [f.key]: v })}
              onNone={(on) => setNone({ ...none(), [f.key]: on })}
              onEnter={() => void save()}
            />
          ))}
          <Show when={anyNone()}>
            <p class="hl-warn">Without a limit, helpers keep going until they're done. That can use a lot of your plan or credits.</p>
          </Show>
          <p class="hl-note">
            Empty follows the server's default.
            <Show when={d()?.capacity}> Your server runs up to {d()!.capacity} helpers at once across all chats; more wait their turn.</Show>
          </p>
          <div class="hl-acts">
            <button type="button" class="btn btn-sm btn-ghost" disabled={busy()} onClick={() => void save({})}>
              Use defaults
            </button>
            <button type="button" class="btn btn-sm btn-primary" disabled={busy()} onClick={() => void save()}>
              Save
            </button>
          </div>
        </div>
      </Show>
    </div>
  );
}
