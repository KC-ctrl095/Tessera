// Settings → Passwords & keys: logins (username, password, one-time
// codes…), API keys and custom sets of values the agents can use in
// commands without ever seeing them. Values are write-only: after saving
// they can be replaced, never shown (not even how long they are). Each one
// also says who can use it (any chat, or only some bots) and where (this
// server, some machines); the server enforces both whenever a command asks.
// On top: whether ones typed in a chat are saved at once, asked about, or
// kept for that chat only (`secret_saving`), and whether saved ones may be
// used on the user's machines (`secret_machines`: each password follows it
// unless it chooses for itself); those kept for a chat are listed apart,
// with Save. Titles that say little ("saved from a chat") get suggestions.
import { For, Show, createMemo, createResource, createSignal } from "solid-js";
import { createStore } from "solid-js/store";
import "../styles/credentials.css";
import type { SecretInfo } from "../proto/SecretInfo";
import type { SecretKind } from "../proto/SecretKind";
import type { SecretTitle } from "../proto/SecretTitle";
import type { SecurityInfo } from "../proto/SecurityInfo";
import { listMachines, type MachineInfo } from "../lib/machines";
import { api, call, state } from "../lib/store";
import { confirmDialog, errorMessage, setSettingsTab, toast, toastError } from "../lib/ui";
import { relTime } from "../lib/util";
import { openSession } from "../lib/workspace";
import { sessionLabel } from "./bots";
import { Icon, Spinner, type IconName } from "./icons";
import { Toggle, openMenu } from "./primitives";
import { AccessPicker, ACCESS_WORDS, accessLabel } from "./settings/AccessPicker";
import { SavingChoice } from "./settings/SecretSaving";
import { MachinesChoice } from "./settings/SecretMachines";
import { SecretInput } from "./SecretInput";

const SECRET_NAME = /^[A-Z_][A-Z0-9_]*$/;
const FIELD_NAME = /^[a-z0-9_]{1,32}$/;
const BASE32 = /^[A-Z2-7]{16,}=*$/;

/** The fields a login offers up front, in order. */
const LOGIN_FIELDS: { key: string; label: string; secret?: boolean; placeholder?: string; type?: string }[] = [
  { key: "username", label: "Username", placeholder: "octocat" },
  { key: "email", label: "Email", placeholder: "you@example.com", type: "email" },
  { key: "password", label: "Password", secret: true },
  { key: "url", label: "Sign-in page", placeholder: "https://github.com/login", type: "url" },
  { key: "totp", label: "Two-factor setup key", secret: true, placeholder: "JBSW Y3DP EHPK 3PXP" },
];

const KIND_ICON: Record<SecretKind, IconName> = { token: "key", login: "user", custom: "variable" };
const KIND_LABEL: Record<SecretKind, string> = { token: "Key", login: "Login", custom: "Values" };

/** "GitHub" → GITHUB, "My bank (EU)" → MY_BANK_EU. */
export function envName(description: string): string {
  return description
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .toUpperCase()
    .replace(/[^A-Z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .replace(/^(\d)/, "_$1")
    .slice(0, 48);
}

/** The environment variables a secret gives a command. */
export function secretVars(name: string, kind: SecretKind, fields: string[]): string[] {
  if (kind === "token") return [name];
  return fields.map((f) => (f === "totp" ? `${name}_OTP` : `${name}_${f.toUpperCase()}`));
}

/** "This server", "Desktop", "This server and Desktop", "Nowhere yet". */
export function placesLabel(onServer: boolean, onMachines: string[], machines: MachineInfo[]): string {
  const names = onMachines.map((id) => machines.find((m) => m.id === id)?.name).filter((n): n is string => !!n);
  const all = [...(onServer ? ["This server"] : []), ...names];
  if (!all.length) return "Nowhere yet";
  return all.length <= 2 ? all.join(" and ") : `${all.slice(0, -1).join(", ")} and ${all[all.length - 1]}`;
}

/** The machines a password may be used on: its own, or what the setting for all of them allows ("all" for every machine). */
export function machinesOf(onMachines: string[], follows: boolean | undefined, security: SecurityInfo | null | undefined): string[] | "all" {
  // Older servers have no setting: a password's own list is all there is.
  if (!follows || security?.secret_machines === undefined) return onMachines;
  if (security.secret_machines === "all") return "all";
  return security.secret_machines === "picked" ? security.secret_machines_picked ?? [] : [];
}

/** A list row's "where": "This server", "This server and Desktop", "This server and all your machines". */
export function whereLabel(s: Pick<SecretInfo, "on_server" | "on_machines" | "machines_default">, security: SecurityInfo | null | undefined, machines: MachineInfo[]): string {
  const onServer = s.on_server ?? true;
  const ids = machinesOf(s.on_machines ?? [], s.machines_default, security);
  if (ids === "all") return machines.length ? (onServer ? "This server and all your machines" : "All your machines") : onServer ? "This server" : "Nowhere yet";
  return placesLabel(onServer, ids, machines);
}


interface Draft {
  kind: SecretKind;
  description: string;
  name: string;
  /** The user typed the name themselves (stop deriving it). */
  nameTouched: boolean;
  /** New values typed (never the stored ones, which are never sent here). */
  values: Record<string, string>;
  /** Stored fields being replaced (an input is open for them). */
  replacing: string[];
  /** Extra fields beyond the standard login ones, in order. */
  extra: string[];
  remove: string[];
  ask: boolean;
  hosts: string;
  /** Who can use it: null = any chat; ids = only these bots. */
  bots: string[] | null;
  onServer: boolean;
  onMachines: string[];
  /** Its machines follow the setting for all passwords (`machines_default`). */
  machinesDefault: boolean;
  /** The user changed which machines (only then are they sent: an unloaded machine list never clears them). */
  machinesTouched: boolean;
}

const blank = (kind: SecretKind): Draft => ({
  kind,
  description: "",
  name: "",
  nameTouched: false,
  values: {},
  replacing: [],
  extra: [],
  remove: [],
  ask: true,
  hosts: "",
  bots: null,
  onServer: true,
  onMachines: [],
  machinesDefault: true,
  machinesTouched: false,
});

/** Add one, or edit one (`existing`: its values are never loaded, only replaced). */
function SecretEditor(props: {
  existing?: SecretInfo;
  kind: SecretKind;
  taken: string[];
  machines: MachineInfo[];
  security?: SecurityInfo | null;
  onDone: (saved: boolean) => void;
  onDelete?: () => void;
}) {
  const ex = props.existing;
  const [d, setD] = createStore<Draft>(
    ex
      ? {
          kind: ex.kind,
          description: ex.description,
          name: ex.name,
          nameTouched: true,
          values: {},
          replacing: [],
          extra: ex.kind === "token" ? [] : ex.fields.filter((f) => ex.kind === "custom" || !LOGIN_FIELDS.some((l) => l.key === f)),
          remove: [],
          ask: ex.ask,
          hosts: ex.hosts.join(", "),
          bots: ex.bots ?? null,
          onServer: ex.on_server ?? true,
          onMachines: [...(ex.on_machines ?? [])],
          // Older servers send nothing: its own list is all there is.
          machinesDefault: ex.machines_default ?? false,
          machinesTouched: false,
        }
      : blank(props.kind),
  );
  const [newField, setNewField] = createSignal("");
  /** The new-field input is out: a set of values starts with it, a login offers it behind a link. */
  const [addOpen, setAddOpen] = createSignal(!ex && props.kind === "custom");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  /** A value is stored for this field (and isn't being removed). */
  const stored = (f: string) => !!ex && (ex.kind === "token" ? f === "value" : ex.fields.includes(f)) && !d.remove.includes(f);
  const renamed = () => !!ex && d.name !== ex.name;

  const nameError = () => {
    if (!d.name) return null;
    if (!SECRET_NAME.test(d.name)) return "Capital letters, digits and _ only, not starting with a digit.";
    if ((!ex || renamed()) && props.taken.includes(d.name)) return `${d.name} is taken — pick another name.`;
    return null;
  };
  const totpError = () => {
    const v = (d.values.totp ?? "").replace(/\s+/g, "").toUpperCase();
    return v && !BASE32.test(v) ? "That doesn't look like a two-factor setup key: it's the text shown next to the QR code when you set up two-factor login." : null;
  };
  /** Fields it will have after saving (for the variables preview). */
  const fieldsAfter = createMemo(() => {
    if (d.kind === "token") return [];
    const keys = new Set<string>();
    for (const f of ex?.fields ?? []) if (!d.remove.includes(f)) keys.add(f);
    for (const [k, v] of Object.entries(d.values)) if (v) keys.add(k);
    const order = [...LOGIN_FIELDS.map((l) => l.key), ...d.extra];
    return [...keys].sort((a, b) => (order.indexOf(a) + 1 || 99) - (order.indexOf(b) + 1 || 99));
  });
  const vars = () => (d.name && !nameError() ? secretVars(d.name, d.kind, fieldsAfter()) : []);
  const oldVars = () => (ex ? secretVars(ex.name, ex.kind, ex.fields.filter((f) => !d.remove.includes(f))) : []);
  const showWhere = () => props.machines.length > 0 || !d.onServer || d.onMachines.length > 0;
  /** The server has the setting for all passwords, so a password can follow it. */
  const hasDefault = () => props.security?.secret_machines !== undefined;
  const following = () => hasDefault() && d.machinesDefault;
  const machinesAfter = () => {
    const ids = machinesOf(d.onMachines, following(), props.security);
    return ids === "all" ? props.machines.map((m) => m.id) : ids.filter((id) => props.machines.some((m) => m.id === id));
  };
  const nowhere = () => !d.onServer && !machinesAfter().length;
  /** Where "Like my other passwords" puts it now. */
  const followText = () => {
    const sec = props.security;
    if (sec?.secret_machines === "all") return "On all your machines, as set at the top of this page.";
    const names = machinesAfter()
      .map((id) => props.machines.find((m) => m.id === id)?.name)
      .filter((n): n is string => !!n);
    return names.length ? `On ${names.join(", ")}, as set at the top of this page.` : "On none of your machines, as set at the top of this page.";
  };

  const setDescription = (v: string) => {
    setD("description", v);
    if (!d.nameTouched && !ex) setD("name", envName(v) + (d.kind === "token" && envName(v) && !/_(TOKEN|KEY|SECRET)$/.test(envName(v)) ? "_TOKEN" : ""));
  };
  const addField = () => {
    const f = newField().trim().toLowerCase().replace(/[\s-]+/g, "_");
    if (!FIELD_NAME.test(f)) return setError("Field names are lowercase letters, digits and _ (like api_key).");
    if (d.extra.includes(f) || (d.kind === "login" && LOGIN_FIELDS.some((l) => l.key === f))) return setError(`There is already a ${f} field.`);
    setError(null);
    setD("extra", (l) => [...l, f]);
    setNewField("");
  };
  const toggleMachine = (id: string) => {
    setD("onMachines", (l) => (l.includes(id) ? l.filter((x) => x !== id) : [...l, id]));
    setD("machinesTouched", true);
  };
  const setFollowing = (v: boolean) => {
    setD("machinesDefault", v);
    setD("machinesTouched", true);
  };

  const save = async (e: Event) => {
    e.preventDefault();
    setError(null);
    if (!d.name) return setError("Give it a name (it becomes the variable name).");
    if (nameError()) return setError(nameError());
    if (totpError()) return setError(totpError());
    const fields: Record<string, string> = {};
    // Only what was typed: a blank field keeps the saved value.
    for (const [k, v] of Object.entries(d.values)) if (v) fields[k] = k === "totp" ? v.replace(/\s+/g, "").toUpperCase() : v;
    if (d.kind === "token") {
      if (!fields.value && !ex) return setError("Paste the key's value.");
    } else if (!fieldsAfter().length) return setError("Fill in at least one field.");
    setBusy(true);
    try {
      const saved = await api.saveSecret({
        name: ex?.name ?? d.name,
        kind: d.kind,
        description: d.description.trim(),
        fields,
        remove_fields: d.remove,
        ask: d.ask,
        hosts: d.hosts
          .split(/[\s,]+/)
          .map((h) => h.trim())
          .filter(Boolean),
        bots: d.bots,
        on_server: d.onServer,
        // Only what the user changed: a machine list that didn't load never clears them.
        ...(d.machinesTouched
          ? {
              on_machines: d.onMachines.filter((id) => props.machines.some((m) => m.id === id)),
              ...(hasDefault() ? { machines_default: d.machinesDefault } : {}),
            }
          : {}),
        ...(renamed() ? { new_name: d.name } : {}),
      });
      const label = d.description.trim() || saved.name;
      toast({
        level: "success",
        message: ex ? `${label} updated${renamed() ? ` — it's ${saved.name} now` : ""}` : `${label} saved — its values can't be shown again`,
        timeout: 2600,
      });
      props.onDone(true);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  /** One value: an input when new or being replaced, else "Saved" with Replace…. */
  const ValueBox = (p: { k: string; label: string; secret?: boolean; placeholder?: string; type?: string; first?: boolean }) => (
    <Show
      when={!stored(p.k) || d.replacing.includes(p.k)}
      fallback={
        <div class="cred-saved">
          <span class="stored-badge">
            <Icon name="lock" size={10} /> saved
          </span>
          <span class="cred-saved-note">Can't be shown again</span>
          <button type="button" class="btn btn-sm cred-replace" onClick={() => setD("replacing", (l) => [...l, p.k])}>
            Replace…
          </button>
        </div>
      }
    >
      <SecretInput
        value={d.values[p.k] ?? ""}
        onInput={(v) => setD("values", p.k, v)}
        secret={p.secret}
        type={p.type}
        label={stored(p.k) ? `New ${p.label.toLowerCase()}` : p.label}
        placeholder={stored(p.k) ? `New ${p.label.toLowerCase()} (empty keeps the saved one)` : p.placeholder}
        strength={p.k === "password"}
        mono={p.k === "totp"}
        autofocus={stored(p.k)}
      />
      <Show when={stored(p.k)}>
        <button
          type="button"
          class="link-btn small-link cred-keep"
          onClick={() => {
            setD("replacing", (l) => l.filter((x) => x !== p.k));
            setD("values", p.k, undefined!);
          }}
        >
          Keep the saved one
        </button>
      </Show>
    </Show>
  );

  const FieldRow = (p: { k: string; label: string; secret?: boolean; placeholder?: string; type?: string; removable?: boolean }) => (
    <div class="field sfield" classList={{ removed: d.remove.includes(p.k) }} data-field={p.k}>
      <span class="field-label">
        {p.label}
        <span class="spacer" />
        <Show when={stored(p.k) || d.remove.includes(p.k) || p.removable}>
          <button
            type="button"
            class="link-btn small-link"
            onClick={() => {
              if (d.remove.includes(p.k)) setD("remove", (l) => l.filter((x) => x !== p.k));
              else if (ex?.fields.includes(p.k)) setD("remove", (l) => [...l, p.k]);
              else {
                setD("extra", (l) => l.filter((x) => x !== p.k));
                setD("values", p.k, undefined!);
              }
            }}
          >
            {d.remove.includes(p.k) ? "Keep" : "Remove"}
          </button>
        </Show>
      </span>
      <Show when={!d.remove.includes(p.k)} fallback={<span class="field-help">Will be removed when you save.</span>}>
        <ValueBox k={p.k} label={p.label} secret={p.secret} placeholder={p.placeholder} type={p.type} />
        <Show when={p.k === "totp" && totpError()}>
          <span class="field-help danger-text">{totpError()}</span>
        </Show>
      </Show>
    </div>
  );

  /** One chip per machine: this password may be used there. */
  const MachineChips = () => (
    <For each={props.machines}>
      {(m) => (
        <button type="button" class="chip cred-place" aria-pressed={d.onMachines.includes(m.id)} onClick={() => toggleMachine(m.id)}>
          <Icon name="laptop" size={14} />
          <span>{m.name}</span>
          <Show when={!m.online}>
            <span class="cred-off">offline</span>
          </Show>
          <Show when={d.onMachines.includes(m.id)}>
            <Icon name="check" size={13} />
          </Show>
        </button>
      )}
    </For>
  );

  return (
    <form class="secret-editor stack" onSubmit={save} autocomplete="off">
      <div class="grid2">
        <label class="field">
          <span class="field-label">Title</span>
          <input
            class="input"
            value={d.description}
            placeholder={d.kind === "token" ? "OpenWeather API key" : d.kind === "login" ? "GitHub login" : "Stripe (test mode)"}
            autocomplete="off"
            spellcheck={false}
            autofocus={!ex}
            onInput={(e) => setDescription(e.currentTarget.value)}
          />
        </label>
        <label class="field">
          <span class="field-label">Name agents use</span>
          <input
            class="input mono"
            classList={{ invalid: !!nameError() }}
            value={d.name}
            placeholder={d.kind === "token" ? "OPENWEATHER_TOKEN" : "GITHUB"}
            aria-label="Name"
            autocomplete="off"
            spellcheck={false}
            onInput={(e) => {
              setD("nameTouched", true);
              setD("name", e.currentTarget.value.toUpperCase().replace(/[\s-]+/g, "_"));
            }}
          />
          <span class="field-help" classList={{ "danger-text": !!nameError() }}>
            {nameError() ?? "What agents type to use it. They never see the value."}
          </span>
        </label>
      </div>
      <Show when={renamed() && !nameError() && d.name}>
        <div class="form-error warn cred-rename">
          <Icon name="alert" size={14} />
          <span>
            Renaming changes what agents type: {oldVars().map((v) => "$" + v).join(", ")} becomes {vars().map((v) => "$" + v).join(", ")}. Saved commands, skills, scheduled duties and notes that use
            the old name stop getting it until you change them too.
          </span>
        </div>
      </Show>

      <Show
        when={d.kind !== "token"}
        fallback={
          <div class="field sfield" data-field="value">
            <span class="field-label">Value</span>
            <ValueBox k="value" label="Value" secret placeholder="Paste the key" />
          </div>
        }
      >
        <div class="sfields">
          <Show when={d.kind === "login"}>
            <For each={LOGIN_FIELDS}>{(f) => <FieldRow k={f.key} label={f.label} secret={f.secret} placeholder={f.placeholder} type={f.type} />}</For>
          </Show>
          <For each={d.extra}>{(f) => <FieldRow k={f} label={f} secret removable />}</For>
          <Show
            when={addOpen()}
            fallback={
              <button type="button" class="link-btn small-link cred-more" onClick={() => setAddOpen(true)}>
                <Icon name="plus" size={13} /> Add a field
              </button>
            }
          >
            <div class="add-field">
              <input
                class="input mono"
                value={newField()}
                placeholder="field_name"
                aria-label="New field name"
                autocomplete="off"
                spellcheck={false}
                autofocus={d.kind !== "custom" || !!ex}
                onInput={(e) => setNewField(e.currentTarget.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    addField();
                  }
                }}
              />
              <button type="button" class="btn btn-sm" onClick={addField}>
                <Icon name="plus" size={14} /> Add field
              </button>
            </div>
          </Show>
        </div>
      </Show>

      <Show when={vars().length}>
        <div class="var-preview">
          <span class="var-preview-label">The agent uses</span>
          <span class="var-chips">
            <For each={vars()}>{(v) => <code class="var-chip">${v}</code>}</For>
          </span>
          <p class="field-help">The AI never sees these values; a command gets one only when it names it, and it's scrubbed from the output. The program can send the value anywhere; listed sites are not a network boundary.</p>
        </div>
      </Show>

      <div class="cred-sec">
        <AccessPicker
          value={d.bots}
          onChange={(v) => setD("bots", v)}
          onlyHelp="Only the bots you pick can use it in their chats, scheduled duties and helpers without asking you. This gives their commands the value, which a program can send anywhere, not just the listed sites. Other chats don't even see it."
        />
        <Show when={d.bots === null}>
          <div class="setting-row cred-ask">
            <div class="setting-text">
              <div class="setting-title">Ask me before it's first used in a chat</div>
              <div class="setting-help">You approve the first command in each chat that uses it. Chats set to run without asking (Auto) don't ask.</div>
            </div>
            <Toggle checked={d.ask} onChange={(v) => setD("ask", v)} label="Ask before first use" />
          </div>
          <Show when={!d.ask}>
            <div class="form-error warn">
              <Icon name="alert" size={14} />
              <span>Any command in any chat can then use it without asking you. A program can send the value anywhere. Only turn this off for low-risk keys.</span>
            </div>
          </Show>
        </Show>
      </div>

      <Show when={showWhere()}>
        <div class="cred-sec field cred-where-sec">
          <span class="field-label">Where it can be used</span>
          <div class="cred-places" role="group" aria-label="Where it can be used">
            <button type="button" class="chip cred-place" aria-pressed={d.onServer} onClick={() => setD("onServer", !d.onServer)}>
              <Icon name="server" size={14} />
              <span>This server</span>
              <Show when={d.onServer}>
                <Icon name="check" size={13} />
              </Show>
            </button>
            <Show when={!hasDefault()}>
              <MachineChips />
            </Show>
          </div>
          <Show when={hasDefault() && props.machines.length > 0}>
            <div class="cred-machines-mode">
              <span class="cred-machines-label">On your machines</span>
              <div class="segmented cred-where-mode" role="radiogroup" aria-label="On my machines">
                <button type="button" role="radio" aria-checked={d.machinesDefault} classList={{ active: d.machinesDefault }} onClick={() => setFollowing(true)}>
                  <span>Like my other passwords</span>
                </button>
                <button type="button" role="radio" aria-checked={!d.machinesDefault} classList={{ active: !d.machinesDefault }} onClick={() => setFollowing(false)}>
                  <span>Choose for this one</span>
                </button>
              </div>
              <Show
                when={!d.machinesDefault}
                fallback={
                  <span class="field-help cred-follow">
                    <Icon name="laptop" size={13} /> {followText()}
                  </span>
                }
              >
                <div class="cred-places" role="group" aria-label="Machines that may use it">
                  <MachineChips />
                </div>
              </Show>
            </div>
          </Show>
          <span class="field-help">This server: commands Tessera runs here, and its browser. A machine: commands an agent runs on that computer, which still decides what it runs.</span>
          <Show when={nowhere()}>
            <span class="field-help warn-text">
              <Icon name="alert" size={13} /> Pick at least one place: until then nothing can use it.
            </span>
          </Show>
        </div>
      </Show>

      <label class="field">
        <span class="field-label">
          Only for these sites <span class="opt">optional</span>
        </span>
        <input class="input mono" value={d.hosts} placeholder="github.com, api.github.com" autocomplete="off" spellcheck={false} onInput={(e) => setD("hosts", e.currentTarget.value)} />
        <span class="field-help">Shown in approvals, so you notice when it's about to go somewhere else. The browser types it only on these sites.</span>
      </label>

      <Show when={error()}>
        <div class="form-error">{error()}</div>
      </Show>
      <div class="form-actions">
        <Show when={ex && props.onDelete}>
          <button type="button" class="btn btn-ghost danger-text cred-delete" onClick={() => props.onDelete!()}>
            <Icon name="trash" size={14} /> Delete
          </button>
          <span class="spacer" />
        </Show>
        <button type="button" class="btn btn-ghost" onClick={() => props.onDone(false)}>
          Cancel
        </button>
        <button type="submit" class="btn btn-primary" disabled={busy()}>
          <Show when={busy()} fallback={<Icon name="lock" size={14} />}>
            <Spinner size={14} />
          </Show>
          {ex ? "Save changes" : "Save"}
        </button>
      </div>
    </form>
  );
}

// ── how credentials typed in a chat are saved ─────────────────────────────

type SecurityPatch = Partial<Pick<SecurityInfo, "save_secrets_from_chat" | "secret_saving" | "secret_machines" | "secret_machines_picked">>;

/**
 * Top of the page: whether passwords and keys typed in a chat are taken out of
 * it (the AI never sees them), and then what happens to them. Turning the
 * protection off asks first.
 */
function SavingCard(props: { info: SecurityInfo; busy: boolean; onSet: (patch: SecurityPatch) => void }) {
  const setOut = async (on: boolean) => {
    if (
      !on &&
      !(await confirmDialog({
        title: "Leave passwords in the chat?",
        body: "API keys and logins you type into a chat would stay in the conversation and be sent to the AI.",
        confirmLabel: "Leave them in the chat",
        danger: true,
      }))
    )
      return;
    props.onSet({ save_secrets_from_chat: on });
  };
  return (
    <div class="card cred-saving" data-saving={props.info.secret_saving ?? "off"}>
      <div class="cred-saving-head">
        <span class="cred-saving-icon">
          <Icon name="shieldCheck" size={16} />
        </span>
        <div class="setting-text grow">
          <div class="setting-title">Keep passwords and keys out of chats</div>
          <div class="setting-help">Ones you type into a chat are taken out before the AI sees them; the chat keeps only a name like $GITHUB_TOKEN. Off: they stay in the conversation and are sent to the AI.</div>
        </div>
        <Toggle checked={props.info.save_secrets_from_chat} disabled={props.busy} onChange={(v) => void setOut(v)} label="Keep passwords and keys out of chats" />
      </div>
      <Show when={props.info.secret_saving && props.info.save_secrets_from_chat}>
        <SavingChoice value={props.info.secret_saving!} on={props.info.save_secrets_from_chat} busy={props.busy} onChange={(x) => props.onSet({ secret_saving: x })} />
      </Show>
      <Show when={!props.info.save_secrets_from_chat}>
        <div class="form-error warn cred-saving-off">
          <Icon name="alert" size={14} />
          <span>What you type stays in the chat and reaches the AI.</span>
        </div>
      </Show>
    </div>
  );
}

/** "Let saved passwords be used on my machines". Only on servers with the setting, once there is a machine. */
function MachinesCard(props: { info: SecurityInfo; machines: MachineInfo[]; busy: boolean; onSet: (patch: SecurityPatch) => void }) {
  return (
    <Show when={props.info.secret_machines !== undefined && props.machines.length > 0}>
      <div class="card cred-machines" data-mode={props.info.secret_machines}>
        <div class="cred-machines-head">
          <span class="cred-machines-icon" classList={{ loose: props.info.secret_machines !== "off" }}>
            <Icon name="laptop" size={16} />
          </span>
          <div class="setting-text">
            <div class="setting-title">Let saved passwords be used on my machines</div>
            <div class="setting-help">Your other computers that agents can use. A password still goes only to a command that names it.</div>
          </div>
        </div>
        <MachinesChoice
          value={props.info.secret_machines ?? "off"}
          picked={props.info.secret_machines_picked ?? []}
          machines={props.machines}
          busy={props.busy}
          onChange={(secret_machines, picked) => props.onSet({ secret_machines, ...(picked ? { secret_machines_picked: picked } : {}) })}
        />
      </div>
    </Show>
  );
}

/** Saved ones whose titles say little ("saved from a chat"): clearer ones to apply, each editable. */
function TitlesCard(props: { list: SecretTitle[]; onDone: () => void }) {
  const [open, setOpen] = createSignal(false);
  const [titles, setTitles] = createStore<Record<string, string>>({});
  const [skip, setSkip] = createStore<Record<string, boolean>>({});
  const [busy, setBusy] = createSignal(false);
  const chosen = () => props.list.filter((t) => !skip[t.name] && (titles[t.name] ?? t.suggested).trim());
  const apply = async () => {
    setBusy(true);
    let failed = 0;
    for (const t of chosen()) {
      try {
        await call<SecretInfo>({ method: "set_secret_title", params: { name: t.name, title: (titles[t.name] ?? t.suggested).trim() } });
      } catch {
        failed++;
      }
    }
    setBusy(false);
    setOpen(false);
    if (failed) toast({ level: "error", message: `${failed} couldn't be renamed` });
    else toast({ level: "success", message: "Titles updated", timeout: 2000 });
    props.onDone();
  };
  return (
    <Show when={props.list.length}>
      <div class="card cred-titles" classList={{ open: open() }}>
        <div class="cred-titles-head">
          <Icon name="pencil" size={15} />
          <span class="grow">
            {props.list.length === 1 ? "1 title doesn't say what it's for" : `${props.list.length} titles don't say what they're for`}
          </span>
          <Show when={!open()}>
            <button type="button" class="btn btn-sm cred-titles-open" onClick={() => setOpen(true)}>
              Suggest titles
            </button>
          </Show>
        </div>
        <Show when={open()}>
          <ul class="cred-titles-list">
            <For each={props.list}>
              {(t) => (
                <li class="cred-title-row" data-name={t.name}>
                  <input type="checkbox" checked={!skip[t.name]} aria-label={`Rename ${t.name}`} onChange={(e) => setSkip(t.name, !e.currentTarget.checked)} />
                  <div class="cred-title-fields">
                    <input class="input" value={titles[t.name] ?? t.suggested} aria-label={`New title for ${t.name}`} disabled={!!skip[t.name]} onInput={(e) => setTitles(t.name, e.currentTarget.value)} />
                    <span class="cred-title-was">
                      <code class="var-chip">${t.name}</code> {t.title ? `was “${t.title}”` : "had no title"}
                    </span>
                  </div>
                </li>
              )}
            </For>
          </ul>
          <div class="form-actions">
            <button type="button" class="btn btn-ghost" onClick={() => setOpen(false)}>
              Not now
            </button>
            <button type="button" class="btn btn-primary cred-titles-apply" disabled={busy() || !chosen().length} onClick={() => void apply()}>
              <Show when={busy()}>
                <Spinner size={14} />
              </Show>
              Rename {chosen().length}
            </button>
          </div>
        </Show>
      </div>
    </Show>
  );
}

// ── the list ─────────────────────────────────────────────────────────────

type Group = "login" | "key" | "twofactor";
const GROUPS: { id: Group; label: string }[] = [
  { id: "login", label: "Logins" },
  { id: "key", label: "Keys & tokens" },
  { id: "twofactor", label: "Two-factor" },
];

/** Two-factor: a one-time code, or a login that holds only a code seed (and whose it is). */
export function groupOf(s: SecretInfo): Group {
  if (s.kind === "token") return /^ONE_TIME_CODE(_\d+)?$/.test(s.name) ? "twofactor" : "key";
  if (s.kind === "login") return s.fields.includes("totp") && s.fields.every((f) => ["totp", "username", "email", "url"].includes(f)) ? "twofactor" : "login";
  return "key";
}

/** Matches a search: its description, name, variables or sites. */
function matches(s: SecretInfo, q: string): boolean {
  if (!q) return true;
  const hay = [s.description, s.name, ...secretVars(s.name, s.kind, s.fields), ...s.hosts].join(" ").toLowerCase();
  return q
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((w) => hay.includes(w.replace(/^\$/, "")));
}

/** A search box shows once the list is longer than this. */
const SEARCH_FROM = 6;

const chatLabel = (id: string) => {
  const info = state.sessions[id]?.info;
  return info ? sessionLabel(info) : "a chat";
};

export function SecretsTab() {
  const [secrets, { refetch }] = createResource(() => api.listSecrets());
  // Older servers, or no machines: just this server.
  const [machines] = createResource(() => listMachines().catch(() => [] as MachineInfo[]), { initialValue: [] });
  const [security, { mutate: setSecurity }] = createResource(() => call<SecurityInfo>({ method: "get_security" }).catch(() => null));
  // Clearer titles for the ones that say little (older servers: none), again whenever the list changes.
  const [titles, { refetch: refetchTitles }] = createResource(secrets, () => call<SecretTitle[]>({ method: "suggest_secret_titles" }).catch(() => [] as SecretTitle[]));
  const [securityBusy, setSecurityBusy] = createSignal(false);
  const setSecurityPatch = async (patch: SecurityPatch) => {
    setSecurityBusy(true);
    try {
      setSecurity(await call<SecurityInfo>({ method: "set_security", params: patch }));
    } catch (e) {
      toastError(e, "Couldn't change it");
    } finally {
      setSecurityBusy(false);
    }
  };
  /** null = list; "new:<kind>" = adding; a name = editing that one. */
  const [editing, setEditing] = createSignal<string | null>(null);
  const [query, setQuery] = createSignal("");
  const all = createMemo(() => [...(secrets() ?? [])].sort((a, b) => (a.description || a.name).localeCompare(b.description || b.name)));
  const kept = createMemo(() => all().filter((s) => s.chat));
  const saved = createMemo(() => all().filter((s) => !s.chat));
  // Only while the search box shows (a list that got short again shows all).
  const shown = createMemo(() => saved().filter((s) => matches(s, saved().length > SEARCH_FROM ? query().trim() : "")));
  const adding = () => editing()?.startsWith("new:") ?? false;
  const done = (ok: boolean) => {
    setEditing(null);
    if (ok) void refetch();
  };
  const remove = async (s: SecretInfo) => {
    const ok = await confirmDialog({
      title: `Delete ${s.description || s.name}?`,
      body: s.chat
        ? `Only “${chatLabel(s.chat)}” keeps it now. Deleted, commands there that use ${secretVars(s.name, s.kind, s.fields)
            .map((v) => "$" + v)
            .join(", ")} stop working.`
        : `It's gone for good: commands that use ${secretVars(s.name, s.kind, s.fields)
            .map((v) => "$" + v)
            .join(", ")} stop working until you add it again.`,
      confirmLabel: "Delete",
      danger: true,
    });
    if (!ok) return;
    try {
      if (s.chat) await api.decideChatSecret(s.chat, s.name, "forget");
      else await api.deleteSecret(s.name);
      if (editing() === s.name) setEditing(null);
      void refetch();
    } catch (e) {
      toastError(e);
    }
  };
  const keep = async (s: SecretInfo) => {
    try {
      await api.decideChatSecret(s.chat!, s.name, "save");
      toast({ level: "success", message: `${s.description || s.name} saved — any chat can use it now`, timeout: 2600 });
      void refetch();
    } catch (e) {
      toastError(e, "Couldn't save it");
    }
  };
  const addMenu = (anchor: HTMLElement) =>
    openMenu(
      anchor,
      [
        { label: "Login", icon: "user", description: "Username, password and one-time codes for a site", onSelect: () => setEditing("new:login") },
        { label: "Key or token", icon: "key", description: "One value, like an API key", onSelect: () => setEditing("new:token") },
        { label: "Several values", icon: "variable", description: "Your own set of named fields", onSelect: () => setEditing("new:custom") },
      ],
      "bottom-end",
      { sheet: false, class: "cred-add-menu" },
    );
  const editor = (s?: SecretInfo) => (
    <SecretEditor
      existing={s}
      kind={(s?.kind ?? editing()!.slice(4) ?? "token") as SecretKind}
      taken={(secrets() ?? []).map((x) => x.name)}
      machines={machines() ?? []}
      security={security()}
      onDone={done}
      onDelete={s ? () => void remove(s) : undefined}
    />
  );

  const Row = (p: { s: SecretInfo }) => {
    const s = () => p.s;
    return (
      <Show
        when={editing() === s().name}
        fallback={
          <li class="secret-item" data-name={s().name}>
            <span class="secret-kind" data-tip={KIND_LABEL[s().kind]}>
              <Icon name={KIND_ICON[s().kind]} size={15} />
            </span>
            <div class="secret-title">{s().description || s().name}</div>
            <div class="secret-actions">
              <button type="button" class="btn btn-sm" aria-label={`Edit ${s().description || s().name}`} onClick={() => setEditing(s().name)}>
                <Icon name="pencil" size={13} /> Edit
              </button>
              <button type="button" class="icon-btn sm danger" aria-label={`Delete ${s().description || s().name}`} data-tip="Delete" onClick={() => void remove(s())}>
                <Icon name="trash" size={14} />
              </button>
            </div>
            <div class="var-chips">
              <For each={secretVars(s().name, s().kind, s().fields)}>{(v) => <code class="var-chip">${v}</code>}</For>
            </div>
            <div class="cred-scope">
              <span class="cred-who" classList={{ only: s().bots != null, nobody: accessLabel(s().bots) === "Nobody yet" }}>
                <Icon name={s().bots != null ? "bot" : "users"} size={13} />
                {accessLabel(s().bots, ACCESS_WORDS.any)}
                <Show when={s().bots == null}>{s().ask ? " · asks first" : " · doesn't ask"}</Show>
              </span>
              <span class="cred-where">
                <Icon name={s().on_server === false ? "laptop" : "server"} size={13} />
                {whereLabel(s(), security(), machines() ?? [])}
              </span>
              <span class="secret-meta">
                {s().last_used ? `Used ${relTime(s().last_used!)}` : "Never used"}
                <Show when={s().hosts.length}> · {s().hosts.join(", ")}</Show>
              </span>
            </div>
          </li>
        }
      >
        <li class="secret-item editing" data-name={s().name}>
          <div class="secret-edit">
            <div class="secret-card-head">
              <Icon name={KIND_ICON[s().kind]} size={16} />
              <span>Edit {s().description || s().name}</span>
            </div>
            {editor(s())}
          </div>
        </li>
      </Show>
    );
  };

  return (
    <div class="stack creds">
      <Show when={security()}>
        {(i) => (
          <>
            <SavingCard info={i()} busy={securityBusy()} onSet={(p) => void setSecurityPatch(p)} />
            <MachinesCard info={i()} machines={machines() ?? []} busy={securityBusy()} onSet={(p) => void setSecurityPatch(p)} />
          </>
        )}
      </Show>
      <TitlesCard
        list={titles() ?? []}
        onDone={() => {
          void refetch();
          void refetchTitles();
        }}
      />

      <Show when={kept().length}>
        <section class="cred-group cred-kept" aria-label="Kept for one chat">
          <div class="section-head">
            <h4>
              Kept for one chat <span class="cred-count">{kept().length}</span>
            </h4>
          </div>
          <p class="setting-help cred-lede">Typed in a chat and not saved here: only that chat can use them, and they go when it's deleted.</p>
          <ul class="secret-list">
            <For each={kept()}>
              {(s) => (
                <li class="secret-item kept" data-name={s.name}>
                  <span class="secret-kind" data-tip={KIND_LABEL[s.kind]}>
                    <Icon name={KIND_ICON[s.kind]} size={15} />
                  </span>
                  <div class="secret-title">{s.description || s.name}</div>
                  <div class="secret-actions">
                    <button type="button" class="btn btn-sm cred-keep-save" onClick={() => void keep(s)}>
                      <Icon name="lock" size={13} /> Save
                    </button>
                    <button type="button" class="icon-btn sm danger" aria-label={`Delete ${s.description || s.name}`} data-tip="Delete" onClick={() => void remove(s)}>
                      <Icon name="trash" size={14} />
                    </button>
                  </div>
                  <div class="var-chips">
                    <For each={secretVars(s.name, s.kind, s.fields)}>{(v) => <code class="var-chip">${v}</code>}</For>
                  </div>
                  <div class="cred-scope">
                    <span class="cred-who only">
                      <Icon name="chat" size={13} />
                      <button
                        type="button"
                        class="link-btn cred-chat"
                        onClick={() => {
                          setSettingsTab(null);
                          openSession(s.chat!);
                        }}
                      >
                        Only in “{chatLabel(s.chat!)}”
                      </button>
                    </span>
                    <span class="secret-meta">{s.last_used ? `Used ${relTime(s.last_used)}` : `Typed ${relTime(s.updated_at)}`}</span>
                  </div>
                </li>
              )}
            </For>
          </ul>
        </section>
      </Show>

      <div class="cred-bar">
        <h4 class="cred-bar-title">
          Saved <Show when={saved().length}><span class="cred-count">{saved().length}</span></Show>
        </h4>
        <Show when={saved().length > SEARCH_FROM}>
          <label class="cred-search">
            <Icon name="search" size={14} />
            <input
              class="input"
              type="search"
              value={query()}
              placeholder="Search"
              aria-label="Search passwords and keys"
              autocomplete="off"
              spellcheck={false}
              onInput={(e) => setQuery(e.currentTarget.value)}
            />
          </label>
        </Show>
        <button type="button" class="btn btn-sm btn-primary cred-add" aria-haspopup="menu" disabled={adding()} onClick={(e) => addMenu(e.currentTarget)}>
          <Icon name="plus" size={14} /> Add
        </button>
      </div>

      <Show when={adding()}>
        <div class="card form-card secret-card">
          <div class="secret-card-head">
            <Icon name={KIND_ICON[(editing()!.slice(4) || "token") as SecretKind]} size={16} />
            <span>{editing() === "new:login" ? "New login" : editing() === "new:token" ? "New key" : "New set of values"}</span>
          </div>
          {editor()}
        </div>
      </Show>

      <Show when={secrets()} fallback={<Show when={secrets.error} fallback={<div class="empty-state"><Spinner size={20} /></div>}><div class="form-error">{errorMessage(secrets.error)}</div></Show>}>
        <Show
          when={saved().length}
          fallback={
            <Show when={!adding()}>
              <div class="empty-state cred-empty">
                <span class="empty-icon">
                  <Icon name="key" size={20} />
                </span>
                <p class="cred-empty-title">Nothing saved yet</p>
                <p>Add the logins and API keys your agents need with Add. Or just type one in a chat: it's taken out before the AI sees it.</p>
              </div>
            </Show>
          }
        >
          <Show when={shown().length} fallback={<p class="cred-none">Nothing matches “{query().trim()}”.</p>}>
            <For each={GROUPS}>
              {(g) => {
                const items = () => shown().filter((s) => groupOf(s) === g.id);
                return (
                  <Show when={items().length}>
                    <section class="cred-group" data-group={g.id} aria-label={g.label}>
                      <h5 class="cred-group-title">
                        {g.label} <span class="cred-count">{items().length}</span>
                      </h5>
                      <ul class="secret-list">
                        <For each={items()}>{(s) => <Row s={s} />}</For>
                      </ul>
                    </section>
                  </Show>
                );
              }}
            </For>
          </Show>
        </Show>
      </Show>

      <p class="sec-doc cred-foot">
        <Icon name="lock" size={13} />
        <span>Once saved, a value can be replaced but never shown again. Each one says who can use it, and where.</span>
      </p>
    </div>
  );
}
