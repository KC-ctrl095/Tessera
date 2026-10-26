// Settings → Models: connecting model services, which model does what, each service's models.
import { For, Index, Show, createEffect, createResource, createSignal, on } from "solid-js";
import type { HelperSettings } from "../../proto/HelperSettings";
import type { ModelRef } from "../../proto/ModelRef";
import type { ModelRole } from "../../proto/ModelRole";
import type { OAuthStart } from "../../proto/OAuthStart";
import type { ProviderInfo } from "../../proto/ProviderInfo";
import type { ProviderKind } from "../../proto/ProviderKind";
import type { SmallMode } from "../../proto/SmallMode";
import { accountShort, api, modelLabel, providersTick, refreshProviders, setState, state } from "../../lib/store";
import { shortLabel } from "../../lib/thinking";
import { confirmDialog, errorMessage, promptDialog, toast, toastError } from "../../lib/ui";
import { fmtTokens, openExternal, plural } from "../../lib/util";
import { Icon, Spinner } from "../icons";
import { CopyButton, Modal, Popover, Segmented, Toggle } from "../primitives";
import { ModelMenu } from "../chat/Pickers";
import { LimitField, parseLimit, rememberDefaults } from "../chat/HelperLimits";
import { ClaudeCodeSetup } from "./ClaudeCodeSetup";
import { Advanced, Card, Row, SwitchRow } from "./kit";

const KIND_LABEL: Record<ProviderKind, string> = {
  openai_chat: "Custom",
  openai_responses: "OpenAI",
  openai_codex: "ChatGPT sign-in",
  anthropic: "Anthropic",
  claude_code: "Your Claude Code",
};

/** "https://api.anthropic.com/v1" → "api.anthropic.com" (the address as a person says it). */
const hostOf = (url: string) => {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
};

const PRESETS: { name: string; id: string; url: string; kind: ProviderKind; hint: string }[] = [
  { name: "llama.cpp", id: "llamacpp", url: "http://127.0.0.1:8080/v1", kind: "openai_chat", hint: "On your server" },
  { name: "Ollama", id: "ollama", url: "http://127.0.0.1:11434/v1", kind: "openai_chat", hint: "On your server" },
  { name: "LM Studio", id: "lmstudio", url: "http://127.0.0.1:1234/v1", kind: "openai_chat", hint: "On your server" },
  { name: "vLLM", id: "vllm", url: "http://127.0.0.1:8000/v1", kind: "openai_chat", hint: "On your server" },
  { name: "OpenRouter", id: "openrouter", url: "https://openrouter.ai/api/v1", kind: "openai_chat", hint: "API key" },
  { name: "OpenAI", id: "openai", url: "https://api.openai.com/v1", kind: "openai_responses", hint: "API key" },
];

/**
 * Settings → Models: the one control for the model new chats use, the services
 * connected (one line each; their details hold the models, simpler instructions
 * for small models, sign out and remove), connecting another, and under More
 * the models for titles, summaries and memory. Usage has a section of its own.
 */
export function ModelsTab() {
  const [oauth, setOauth] = createSignal<{ provider: string; start: OAuthStart } | null>(null);
  const [form, setForm] = createSignal<"openai" | "anthropic" | "claude" | "more-chatgpt" | "more-claude" | null>(null);
  /** A Claude account just added: its sign-in steps stay open here until it is ready. */
  const [newClaude, setNewClaude] = createSignal<string | null>(null);
  const [refreshing, setRefreshing] = createSignal(false);
  const [starting, setStarting] = createSignal<string | null>(null);
  /** Connecting another service: open by itself while nothing is connected. */
  const [connecting, setConnecting] = createSignal(false);
  const showConnect = () => connecting() || (state.providersLoaded && !state.providers.length);

  const startOauth = async (provider: string) => {
    setStarting(provider);
    try {
      const start = await api.startOauth(provider);
      setOauth({ provider, start });
      if (!start.user_code && /^https:\/\//i.test(start.url)) void openExternal(start.url);
    } catch (e) {
      toastError(e, "Could not start sign-in");
    } finally {
      setStarting(null);
    }
  };

  const codex = () => state.providers.find((p) => p.kind === "openai_codex");
  /** The user's own Claude Code, once it is installed and signed in on the server. */
  const claudeReady = () => state.providers.some((p) => p.kind === "claude_code" && p.authenticated);
  const done = () => {
    setForm(null);
    setNewClaude(null);
    setConnecting(false);
  };
  /** Another account was added: ChatGPT signs in at once; Claude shows what to run on the server. */
  const accountAdded = (p: ProviderInfo) => {
    if (p.kind === "openai_codex") {
      done();
      void startOauth(p.id);
    } else {
      setForm(null);
      setNewClaude(p.id);
    }
  };

  return (
    <div class="stack">
      <DefaultModelCard />

      <Card class="models-connected">
        <div class="section-head">
          <h4>Connected</h4>
          <button type="button" class="btn btn-sm" aria-expanded={showConnect()} onClick={() => setConnecting(!connecting())}>
            <Icon name={showConnect() ? "x" : "plus"} size={14} /> {showConnect() ? "Close" : "Connect"}
          </button>
        </div>
        <Show when={showConnect()}>
          <div class="connect-grid">
            <Show when={!codex()?.authenticated}>
              <button type="button" class="connect-card-btn" disabled={!!starting()} onClick={() => startOauth(codex()?.id ?? "openai-codex")}>
                <span class="cc-icon">
                  <Show when={starting()} fallback={<Icon name="user" size={18} />}>
                    <Spinner size={16} />
                  </Show>
                </span>
                <span class="cc-text">
                  <span class="cc-title">Sign in with ChatGPT</span>
                  <span class="cc-sub">Use your ChatGPT plan's models</span>
                </span>
              </button>
            </Show>
            <Show when={codex()?.authenticated}>
              <button type="button" class="connect-card-btn" classList={{ on: form() === "more-chatgpt" }} onClick={() => setForm(form() === "more-chatgpt" ? null : "more-chatgpt")}>
                <span class="cc-icon">
                  <Icon name="user" size={18} />
                </span>
                <span class="cc-text">
                  <span class="cc-title">Another ChatGPT account</span>
                  <span class="cc-sub">A second ChatGPT plan, such as one for work</span>
                </span>
              </button>
            </Show>
            <Show
              when={!claudeReady()}
              fallback={
                <button type="button" class="connect-card-btn" classList={{ on: form() === "more-claude" }} onClick={() => setForm(form() === "more-claude" ? null : "more-claude")}>
                  <span class="cc-icon">
                    <Icon name="sparkles" size={18} />
                  </span>
                  <span class="cc-text">
                    <span class="cc-title">Another Claude account</span>
                    <span class="cc-sub">A second Claude plan, signed in through Claude Code</span>
                  </span>
                </button>
              }
            >
              <button type="button" class="connect-card-btn" classList={{ on: form() === "claude" }} onClick={() => setForm(form() === "claude" ? null : "claude")}>
                <span class="cc-icon">
                  <Icon name="sparkles" size={18} />
                </span>
                <span class="cc-text">
                  <span class="cc-title">Your Claude plan</span>
                  <span class="cc-sub">Claude Pro or Max, through your own Claude Code</span>
                </span>
              </button>
            </Show>
            <button type="button" class="connect-card-btn" classList={{ on: form() === "anthropic" }} onClick={() => setForm(form() === "anthropic" ? null : "anthropic")}>
              <span class="cc-icon">
                <Icon name="key" size={18} />
              </span>
              <span class="cc-text">
                <span class="cc-title">Anthropic API key</span>
                <span class="cc-sub">Claude models</span>
              </span>
            </button>
            <button type="button" class="connect-card-btn" classList={{ on: form() === "openai" }} onClick={() => setForm(form() === "openai" ? null : "openai")}>
              <span class="cc-icon">
                <Icon name="cpu" size={18} />
              </span>
              <span class="cc-text">
                <span class="cc-title">Another service</span>
                <span class="cc-sub">A model on your own computer, OpenRouter, or another service you have an address and key for</span>
              </span>
            </button>
          </div>
          <Show when={form() === "openai"}>
            <OpenAIForm onDone={done} />
          </Show>
          <Show when={form() === "anthropic"}>
            <AnthropicForm onDone={done} />
          </Show>
          <Show when={form() === "claude"}>
            <ClaudeCodeSetup />
          </Show>
          <Show when={form() === "more-chatgpt" || form() === "more-claude"}>
            <AccountForm kind={form() === "more-chatgpt" ? "openai_codex" : "claude_code"} onAdded={accountAdded} onCancel={() => setForm(null)} />
          </Show>
          <Show when={newClaude()} keyed>
            {(id) => <ClaudeCodeSetup provider={id} />}
          </Show>
        </Show>
        <Show when={!state.providers.length && state.providersLoaded}>
          <p class="muted small models-none">Nothing connected yet: sign in with ChatGPT, or add a key above.</p>
        </Show>
        <For each={state.providers}>{(p) => <ProviderCard p={p} onSignIn={() => startOauth(p.id)} />}</For>
        <AccountsRow />
      </Card>

      <HelpersCard />

      <Advanced label="More: models for titles, summaries and memory; look for new models">
        <RolesSection others />
        <div class="models-refresh">
          <button
            type="button"
            class="btn btn-sm"
            disabled={refreshing()}
            onClick={async () => {
              setRefreshing(true);
              await refreshProviders(true);
              setRefreshing(false);
            }}
          >
            <Show when={refreshing()} fallback={<Icon name="refresh" size={14} />}>
              <Spinner size={13} />
            </Show>
            Look for new models
          </button>
          <span class="muted small">Asks each service again which models it has.</span>
        </div>
      </Advanced>

      <Show when={oauth()} keyed>
        {(o) => <OAuthDialog provider={o.provider} start={o.start} onClose={() => setOauth(null)} />}
      </Show>
    </div>
  );
}

// ── Helpers (subagents): the limits of chats that set none ─────────────────

/** Plain number fields; a chat changes its own in Chat options (hidden on a server too old to say). */
function HelpersCard() {
  const [helpers, { mutate }] = createResource(() => api.getHelpers().catch(() => null));
  type Key = "at_once" | "steps" | "minutes";
  const [text, setText] = createSignal({ at_once: "", steps: "", minutes: "" });
  const [none, setNone] = createSignal({ at_once: false, steps: false, minutes: false });
  const [busy, setBusy] = createSignal(false);
  const [enabled, setEnabled] = createSignal(true);
  const keys: Key[] = ["at_once", "steps", "minutes"];
  const fill = (h: HelperSettings) => {
    rememberDefaults(h);
    setEnabled(h.enabled ?? h.at_once !== 0);
    setText({ at_once: h.at_once ? String(h.at_once) : "", steps: h.steps ? String(h.steps) : "", minutes: h.minutes ? String(h.minutes) : "" });
    setNone({ at_once: h.at_once === 0 && h.enabled === true, steps: h.steps === 0, minutes: h.minutes === 0 });
  };
  createEffect(on(helpers, (h) => h && fill(h)));
  const dirty = () => {
    const h = helpers();
    return !!h && (enabled() !== (h.enabled ?? h.at_once !== 0) || keys.some((k) => none()[k] !== (k === "at_once" ? h.at_once === 0 && h.enabled === true : h[k] === 0) || (!none()[k] && text()[k] !== String(h[k] || ""))));
  };
  const save = async () => {
    const h = helpers();
    if (!h || busy() || !dirty()) return;
    const values = keys.map((k) => (k === "at_once" && !enabled()) || none()[k] ? 0 : parseLimit(text()[k], h.max));
    if (values.some((v, i) => v === undefined || (v === null && h[keys[i]] != null))) {
      toast({ level: "error", message: `Each needs a whole number from 1 to ${h.max}, or No limit.` });
      return;
    }
    setBusy(true);
    try {
      const saved = await api.setHelpers(values[0]!, values[1] ?? undefined, values[2] ?? undefined, enabled());
      mutate(saved);
      fill(saved);
    } catch (e) {
      toastError(e, "Could not change it");
    } finally {
      setBusy(false);
    }
  };
  const field = (key: Key, label: string, unit: string) => (
    <LimitField label={label} unit={unit} max={helpers()?.max} value={text()[key]} none={none()[key]}
      onNone={helpers()?.capacity != null ? (on) => setNone({ ...none(), [key]: on }) : undefined}
      onInput={(v) => setText({ ...text(), [key]: v })} onEnter={() => void save()} />
  );
  return (
    <Show when={helpers()}>
      {(h) => (
        <Card class="helpers-card">
          <Row col title="Helpers"
            help={`A chat can split a big job between helpers. These are the limits for chats that don't set their own (in a chat's options). More at once finishes big jobs sooner, but uses more of your server and your plan's limits. Usual for this server: ${h().default} at once.`}>
            <div class="hl-fields">
              <label class="hl-none"><input type="checkbox" checked={enabled()} onChange={(e) => setEnabled(e.currentTarget.checked)} />Allow helpers</label>
              <Show when={enabled()}>{field("at_once", "At once in a chat", "helpers")}</Show>
              <Show when={!enabled()}><p class="hl-note">Helpers are disabled on this server.</p></Show>
              <Show when={h().steps != null}>{field("steps", "Steps for each", "steps")}</Show>
              <Show when={h().minutes != null}>{field("minutes", "Time for each", "min")}</Show>
              <Show when={keys.some((k) => none()[k])}>
                <p class="hl-warn">Without a limit, helpers keep going until they're done. That can use a lot of your plan or credits.</p>
              </Show>
              <Show when={h().capacity}>
                <p class="hl-note">Your server runs up to {h().capacity} helpers at once across all chats; more wait their turn. Claude Code's own helpers follow Claude's rules.</p>
              </Show>
              <div class="hl-acts">
                <button type="button" class="btn btn-sm btn-primary" disabled={busy() || !dirty()} onClick={() => void save()}>Save</button>
              </div>
            </div>
          </Row>
        </Card>
      )}
    </Show>
  );
}

// ── Model roles (default, utility, memory…) ──────────────────────────────

/** The server's roles; `false` when it has none (an older server). */
const [roles, setRoles] = createSignal<ModelRole[] | null | false>(null);
async function loadRoles() {
  try {
    setRoles((await api.getModelRoles()).roles);
  } catch {
    setRoles(false);
  }
}

const refLabel = (r: ModelRef | undefined) => (r ? modelLabel(r) : "");
const parseRef = (v: string): ModelRef | null => {
  const i = v.indexOf("/");
  return i > 0 ? { provider: v.slice(0, i), model: v.slice(i + 1) } : null;
};

/** What an unset role falls back to, in words ("Same as Utility", "Same as each chat"). */
function fallbackOf(role: ModelRole, all: ModelRole[]): string {
  if (role.name === "default") return "Automatic";
  if (role.name === "memory") {
    const util = all.find((r) => r.name === "utility");
    if (util?.resolved) return `Same as ${PLAIN.utility.title.toLowerCase()}`;
  }
  return "Same as each chat";
}

/** How a role's current setting reads: a model, another role, or its fallback. */
function roleValue(role: ModelRole, all: ModelRole[]): { main: string; sub?: string; unset: boolean } {
  const v = role.value?.trim();
  if (!v) {
    const main = fallbackOf(role, all);
    return { main, sub: role.resolved ? refLabel(role.resolved) : undefined, unset: true };
  }
  if (v.startsWith("@")) {
    const other = all.find((r) => r.name === v.slice(1) || (v === "@fast" && r.name === "utility"));
    return { main: `Same as ${other ? (PLAIN[other.name]?.title.toLowerCase() ?? other.label) : v.slice(1)}`, sub: role.resolved ? refLabel(role.resolved) : undefined, unset: false };
  }
  const ref = parseRef(v);
  return { main: ref ? modelLabel(ref) : v, sub: ref ? state.providers.find((p) => p.id === ref.provider)?.name ?? ref.provider : undefined, unset: false };
}

/** Tessera's own roles, in plain words (the server's names are for the config file). */
const PLAIN: Record<string, { title: string; help: string }> = {
  default: { title: "Model for new chats", help: "Each chat can switch from its model button. Bots have their own." },
  utility: { title: "Titles and summaries", help: "Chat titles, summaries and page extracts. Unset: each chat's own model." },
  memory: { title: "Memory", help: "The look back after a chat that saves what's worth remembering. Unset: the one for titles, then each chat's own." },
};

/** The roles besides the default (titles and summaries, memory, the user's own), under Models › More. */
function RolesSection(props: { others?: boolean }) {
  if (roles() === null) void loadRoles();
  const list = () => {
    const r = roles();
    return r ? r.filter((x) => !props.others || x.name !== "default") : [];
  };
  return (
    <Show when={list().length && state.providers.some((p) => p.authenticated)} fallback={<p class="muted small">Connect a model service first.</p>}>
      <div class="roles">
        {/* By position: a reload of the roles (any providers_changed) keeps an open picker open. */}
        <Index each={list()}>{(r) => <RoleRow role={r()} all={roles() || []} />}</Index>
      </div>
    </Show>
  );
}

/**
 * The model new chats use: the one control for it. Through the default role
 * where the server has roles, else (an older server) set directly.
 */
function DefaultModelCard() {
  if (roles() === null) void loadRoles();
  // Changed elsewhere (another device, the terminal): the server says the providers changed.
  createEffect(on(providersTick, () => void loadRoles(), { defer: true }));
  const role = () => (roles() || []).find((r) => r.name === "default");
  return (
    <Card class="default-model">
      <Show when={role()} fallback={<LegacyDefaultRow />}>
        {(r) => <RoleRow role={r()} all={roles() || []} />}
      </Show>
    </Card>
  );
}

/** An older server without roles: the default model, picked from the model menu. */
function LegacyDefaultRow() {
  const [anchor, setAnchor] = createSignal<HTMLElement | null>(null);
  const current = () => state.server?.default_model ?? null;
  const set = async (m: ModelRef) => {
    setAnchor(null);
    try {
      await api.setDefaultModel(m);
      if (state.server) setState("server", "default_model", m);
      toast({ level: "success", message: "New chats use it now", timeout: 1800 });
    } catch (e) {
      toastError(e);
    }
  };
  return (
    <div class="setting-row role-row" data-role="default">
      <div class="setting-text">
        <div class="setting-title">{PLAIN.default.title}</div>
        <div class="setting-help">{PLAIN.default.help}</div>
      </div>
      <button type="button" class="role-pick" aria-haspopup="menu" aria-expanded={!!anchor()} aria-label={`${PLAIN.default.title}: ${current() ? modelLabel(current()!) : "none"}`} onClick={(e) => setAnchor(anchor() ? null : e.currentTarget)}>
        <span class="role-pick-text">
          <span class="role-pick-main">{current() ? modelLabel(current()!) : "Pick one"}</span>
        </span>
        <Icon name="chevronDown" size={13} class="chip-chev" />
      </button>
      <Show when={anchor()}>
        <Popover anchor={anchor()!} placement="bottom-end" onClose={() => setAnchor(null)} class="pop-model" label={PLAIN.default.title}>
          <ModelMenu current={current()} manage={false} allowOffline onSelect={(m) => void set(m)} onClose={() => setAnchor(null)} />
        </Popover>
      </Show>
    </div>
  );
}

function RoleRow(props: { role: ModelRole; all: ModelRole[] }) {
  const [anchor, setAnchor] = createSignal<HTMLElement | null>(null);
  const [busy, setBusy] = createSignal(false);
  const shown = () => roleValue(props.role, props.all);
  const current = (): ModelRef | null => {
    const v = props.role.value?.trim();
    return v && !v.startsWith("@") ? parseRef(v) : null;
  };
  const set = async (value: string | null) => {
    setAnchor(null);
    setBusy(true);
    try {
      const next = await api.setModelRole(props.role.name, value);
      setRoles(next.roles);
      // New chats pick the default up from the server info.
      const def = next.roles.find((r) => r.name === "default")?.resolved;
      if (props.role.name === "default" && def && state.server) setState("server", "default_model", def);
    } catch (e) {
      toastError(e, "Could not change the model");
    } finally {
      setBusy(false);
    }
  };
  const canClear = () => props.role.name !== "default" && !!props.role.value;
  return (
    <div class="setting-row role-row" data-role={props.role.name}>
      <div class="setting-text">
        <div class="setting-title">{PLAIN[props.role.name]?.title ?? props.role.label}</div>
        <div class="setting-help">{PLAIN[props.role.name]?.help ?? props.role.description}</div>
      </div>
      <button
        type="button"
        class="role-pick"
        classList={{ unset: shown().unset }}
        aria-haspopup="menu"
        aria-expanded={!!anchor()}
        aria-label={`${PLAIN[props.role.name]?.title ?? props.role.label}: ${shown().main}`}
        disabled={busy()}
        onClick={(e) => setAnchor(anchor() ? null : e.currentTarget)}
      >
        <span class="role-pick-text">
          <span class="role-pick-main">{shown().main}</span>
          <Show when={shown().sub}>
            <span class="role-pick-sub">{shown().sub}</span>
          </Show>
        </span>
        <Show when={busy()} fallback={<Icon name="chevronDown" size={13} class="chip-chev" />}>
          <Spinner size={12} />
        </Show>
      </button>
      <Show when={anchor()}>
        <Popover anchor={anchor()!} placement="bottom-end" onClose={() => setAnchor(null)} class="pop-model" label={props.role.label}>
          <ModelMenu
            current={current()}
            manage={false}
            allowOffline
            onSelect={(m) => void set(`${m.provider}/${m.model}`)}
            onClose={() => setAnchor(null)}
            before={
              <Show when={props.role.name !== "default"}>
                <div class="model-group">
                  <button
                    type="button"
                    class="menu-item model-item"
                    classList={{ selected: !props.role.value }}
                    role="menuitemradio"
                    aria-checked={!props.role.value}
                    onClick={() => (canClear() ? void set(null) : setAnchor(null))}
                  >
                    <span class="menu-text">
                      <span class="menu-label">{fallbackOf({ ...props.role, value: undefined }, props.all)}</span>
                      <span class="menu-desc">{props.role.name === "memory" ? "The model for titles if it has one, else each chat's own" : "Each chat's own model"}</span>
                    </span>
                    <Show when={!props.role.value}>
                      <Icon name="check" size={15} class="menu-check" />
                    </Show>
                  </button>
                </div>
              </Show>
            }
          />
        </Popover>
      </Show>
    </div>
  );
}

function ProviderCard(props: { p: ProviderInfo; onSignIn: () => void }) {
  const [open, setOpen] = createSignal(false);
  const p = () => props.p;
  /** The user's own Claude Code: it signs in by itself (`claude`), so the card says what to do instead. */
  const cc = () => p().kind === "claude_code";
  const [ccInfo, { mutate: setCcInfo }] = createResource(
    () => (cc() ? p().id : null),
    (id) => api.claudeCodeStatus(false, id).catch(() => null),
  );
  const [checking, setChecking] = createSignal(false);
  const checkAgain = async () => {
    setChecking(true);
    try {
      setCcInfo(await api.claudeCodeStatus(true, p().id));
      await refreshProviders(true);
    } catch (e) {
      toastError(e);
    } finally {
      setChecking(false);
    }
  };
  const isDefault = (m: ModelRef) => state.server?.default_model?.provider === m.provider && state.server?.default_model?.model === m.model;
  /** The model for new chats, when its server runs it with a window too small for Tessera: said on the card itself. */
  const flagged = () => p().models.find((m) => m.context_warning && isDefault({ provider: m.provider || p().id, model: m.id }));
  const authLabel = () => {
    const x = p();
    if (!x.authenticated) return x.auth === "oauth" ? "Not signed in" : "Needs an API key";
    if (x.auth === "oauth") return x.account ? `Signed in as ${x.account}` : "Signed in";
    if (x.auth === "api_key") return "Key saved";
    return "Ready";
  };
  /** ChatGPT and Claude subscriptions: the user names each account. */
  const subscription = () => p().kind === "openai_codex" || cc();
  const rename = async () => {
    const name = (await promptDialog({ title: "Rename this account", label: "Name", value: accountShort(p()), placeholder: "Work", confirmLabel: "Rename" }))?.trim();
    if (!name) return;
    try {
      await api.renameProvider(p().id, name);
      await refreshProviders();
    } catch (e) {
      toastError(e, "Couldn't rename it");
    }
  };
  const remove = async () => {
    // Another Claude account's sign-in stays in its own Claude Code folder: Tessera never touches it.
    const body =
      cc() && ccInfo()?.sign_in
        ? "Chats using its models will need another model. Claude Code stays signed in to it in its own folder until you sign out there."
        : "Chats using its models will need another model.";
    if (!(await confirmDialog({ title: `Remove ${p().name}?`, body, confirmLabel: "Remove", danger: true }))) return;
    try {
      await api.removeProvider(p().id);
      void refreshProviders();
    } catch (e) {
      toastError(e);
    }
  };
  const logout = async () => {
    try {
      await api.logout(p().id);
      void refreshProviders();
    } catch (e) {
      toastError(e);
    }
  };
  /** The list shows the server's answer (what Auto means for the model comes from it). */
  const setSmall = async (m: ModelRef, mode: SmallMode, el: HTMLSelectElement, was: SmallMode) => {
    try {
      await api.setModelSmall(m.provider, m.model, mode);
      await refreshProviders();
    } catch (e) {
      el.value = was;
      toastError(e);
    }
  };
  return (
    <Card class="provider">
      <div class="provider-head">
        <span class="provider-icon">
          <Icon name={p().kind === "openai_codex" ? "user" : p().kind === "anthropic" || cc() ? "sparkles" : "cpu"} size={17} />
        </span>
        <div class="provider-info">
          <div class="provider-name">
            {p().name}
            <Show when={KIND_LABEL[p().kind] !== p().name}>
              <span class="badge">{KIND_LABEL[p().kind]}</span>
            </Show>
          </div>
          {/* Where it lives: the host only; the whole address is under Details. */}
          <div class="provider-sub" classList={{ mono: cc() }}>{p().base_url ? hostOf(p().base_url!) : cc() ? ccInfo()?.command ?? "claude" : ""}</div>
        </div>
        <span class={`auth-pill ${p().authenticated ? "ok" : "no"}`}>
          <span class="dot" />
          {authLabel()}
        </span>
      </div>
      <Show when={cc() && ccInfo() && (!p().authenticated || ccInfo()!.api_key)}>
        <p class="setting-help provider-hint">{ccInfo()!.hint}</p>
        <Show when={!p().authenticated && ccInfo()!.sign_in}>
          <div class="claude-cmd provider-cmd">
            <code>{ccInfo()!.sign_in}</code>
            <CopyButton text={() => ccInfo()!.sign_in!} label="Copy" />
          </div>
        </Show>
      </Show>
      <Show when={cc() && p().authenticated && !ccInfo()?.api_key}>
        <p class="provider-built-in">Tessera's notes, whiteboards, memory, notifications and other tools are built into its chats.</p>
      </Show>
      <Show when={!open() && flagged()}>
        {(m) => (
          <p class="setting-help provider-hint ctx-warn" role="note">
            <strong>{m().name}:</strong> {m().context_warning}
          </p>
        )}
      </Show>
      <div class="provider-actions">
        <button type="button" class="btn btn-sm btn-ghost" aria-expanded={open()} onClick={() => setOpen(!open())}>
          <Icon name="chevronRight" size={14} class="disc-chev" />
          Details · {plural(p().models.length, "model", "models")}
        </button>
        <span class="spacer" />
        <Show when={cc() && !p().authenticated}>
          <button type="button" class="btn btn-sm btn-ghost" disabled={checking()} onClick={() => void checkAgain()}>
            <Show when={checking()} fallback={<Icon name="refresh" size={14} />}>
              <Spinner size={13} />
            </Show>
            Check again
          </button>
        </Show>
        <Show when={p().auth === "oauth" && !p().authenticated && !cc()}>
          <button type="button" class="btn btn-sm btn-primary" onClick={props.onSignIn}>
            Sign in
          </button>
        </Show>
      </div>
      <Show when={open()}>
        <Show when={p().base_url}>
          <p class="setting-help provider-address">
            Address <span class="mono">{p().base_url}</span>
          </p>
        </Show>
        <Show when={!cc() && p().models.length}>
          <p class="setting-help model-small-help">Simpler instructions: fewer tools and shorter instructions, for small models running on your own computer.</p>
        </Show>
        <ul class="model-list">
          <For each={p().models} fallback={<li class="muted small">No models found.</li>}>
            {(m) => {
              const ref = { provider: m.provider || p().id, model: m.id };
              const small = () => m.small_mode ?? "auto";
              return (
                <li classList={{ "has-ctx-warn": !!m.context_warning }}>
                  <div class="model-li-main">
                    <span class="strong">{m.name}</span>
                    <span class="muted small">
                      <span class="mono">{m.id}</span>
                      <Show when={(m.thinking ?? []).length}> · thinking: {(m.thinking ?? []).map((t) => shortLabel(t.label)).join(", ")}</Show>
                    </span>
                  </div>
                  <span class="model-caps">
                    {/* What the account's own catalog lists (OpenAI sign-ins and keys only). */}
                    <Show when={m.cyber_access_programs?.includes("daybreak_blue")}>
                      <span class="cap cap-daybreak" data-tip="Your account can turn Daybreak Blue on for this model, in a chat's options">
                        <Icon name="shield" size={13} />
                        <span>Daybreak</span>
                      </span>
                    </Show>
                    <Show when={m.vision}>
                      <span class="cap" data-tip="Understands images">
                        <Icon name="image" size={13} />
                      </span>
                    </Show>
                    <span class="ctx-badge" classList={{ warn: !!m.context_warning }} data-tip="Context window">
                      {fmtTokens(m.context_window)}
                    </span>
                  </span>
                  <Show when={isDefault(ref)}>
                    <span class="badge badge-accent" data-tip="The model for new chats (change it above)">New chats</span>
                  </Show>
                  <Show when={!cc()}>
                    <label class="model-small">
                      <span class="model-small-name">Simpler instructions</span>
                      <select
                        class="input"
                        aria-label={`Simpler instructions for ${m.name}`}
                        value={small()}
                        onChange={(e) => void setSmall(ref, e.currentTarget.value as SmallMode, e.currentTarget, small())}
                      >
                        {/* What Auto means for this model is known only while it's the choice. */}
                        <option value="auto">{small() === "auto" ? `Auto (${m.small ? "on" : "off"})` : "Auto"}</option>
                        <option value="on">On</option>
                        <option value="off">Off</option>
                      </select>
                    </label>
                  </Show>
                  <Show when={m.context_warning}>
                    <p class="model-ctx-warn" role="note">
                      {m.context_warning}
                    </p>
                  </Show>
                </li>
              );
            }}
          </For>
        </ul>
      </Show>
      <Show when={open()}>
        <div class="provider-more">
          <Show when={cc() && p().authenticated}>
            <button type="button" class="btn btn-sm btn-ghost" disabled={checking()} onClick={() => void checkAgain()}>
              <Show when={checking()} fallback={<Icon name="refresh" size={14} />}>
                <Spinner size={13} />
              </Show>
              Check again
            </button>
          </Show>
          <Show when={subscription()}>
            <button type="button" class="btn btn-sm btn-ghost" onClick={() => void rename()}>
              <Icon name="pencil" size={14} /> Rename
            </button>
          </Show>
          <Show when={p().auth === "oauth" && p().authenticated && !cc()}>
            <button type="button" class="btn btn-sm btn-ghost" onClick={logout}>
              <Icon name="logOut" size={14} /> Sign out
            </button>
          </Show>
          <button type="button" class="btn btn-sm btn-ghost danger-text" onClick={remove}>
            <Icon name="trash" size={14} /> Remove
          </button>
        </div>
      </Show>
    </Card>
  );
}

// ── More than one subscription of a kind ─────────────────────────────────

/** Another ChatGPT or Claude account: just a name ("Work"); signing in comes next. */
function AccountForm(props: { kind: ProviderKind; onAdded: (p: ProviderInfo) => void; onCancel: () => void }) {
  const [name, setName] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const service = () => (props.kind === "openai_codex" ? "ChatGPT" : "Claude");
  const submit = async (e: Event) => {
    e.preventDefault();
    if (!name().trim()) return;
    setBusy(true);
    setError(null);
    try {
      const p = await api.addAccount(props.kind, name().trim());
      await refreshProviders();
      props.onAdded(p);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Card class="form-card account-form">
      <form class="stack" onSubmit={submit}>
        <label class="field">
          <span class="field-label">Name this {service()} account</span>
          <input class="input" value={name()} onInput={(e) => setName(e.currentTarget.value)} placeholder="Work" maxLength={40} autofocus />
          <span class="field-help">
            {props.kind === "openai_codex"
              ? "Next you sign in with that ChatGPT account. Each chat uses the account of the model you pick for it."
              : "Next you sign in to it once through Claude Code on your server. Each chat uses the account of the model you pick for it."}
          </span>
        </label>
        <Show when={error()}>
          <div class="form-error">{withCode(error()!)}</div>
        </Show>
        <div class="form-actions">
          <button type="button" class="btn btn-ghost" onClick={props.onCancel}>
            Cancel
          </button>
          <button type="submit" class="btn btn-primary" disabled={busy() || !name().trim()}>
            <Show when={busy()}>
              <Spinner size={14} />
            </Show>
            Add account
          </button>
        </div>
      </form>
    </Card>
  );
}

/** A server sentence with `commands` in backticks, shown as code. */
const withCode = (text: string) => text.split("`").map((part, i) => (i % 2 ? <code>{part}</code> : part));

/**
 * With two or more ChatGPT or Claude accounts: whether a chat whose account reached its usage limit goes on
 * with the same model on the next one (off unless turned on). Hidden on servers that can't.
 */
function AccountsRow() {
  const several = () => (["openai_codex", "claude_code"] as ProviderKind[]).some((k) => state.providers.filter((p) => p.kind === k).length > 1);
  const [settings, { mutate }] = createResource(
    () => several() || null,
    () => api.getAccounts().catch(() => null),
  );
  const [saving, setSaving] = createSignal(false);
  const set = async (on: boolean) => {
    setSaving(true);
    try {
      mutate(await api.setAccounts(on));
    } catch (e) {
      toastError(e, "Couldn't change it");
    } finally {
      setSaving(false);
    }
  };
  return (
    <Show when={several() && settings()}>
      {(s) => (
        <div class="accounts-row">
          <SwitchRow
            title="When an account reaches its limit, go on with the next one"
            help="A chat keeps its model and moves to your next account of the same kind that's signed in and has room left."
            checked={s().continue_on_next}
            saving={saving()}
            onChange={(v) => void set(v)}
          />
        </div>
      )}
    </Show>
  );
}

export function OAuthDialog(props: { provider: string; start: OAuthStart; onClose: () => void }) {
  const [input, setInput] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [done, setDone] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const wasAuthed = state.providers.find((p) => p.id === props.provider)?.authenticated ?? false;
  /** Only ever show or open an https verification link. */
  const safeUrl = () => (/^https:\/\//i.test(props.start.url) ? props.start.url : null);

  // `providers_changed` refreshes the list; completion shows up here.
  createEffect(() => {
    const p = state.providers.find((x) => x.id === props.provider);
    if (p?.authenticated && !wasAuthed && !done()) {
      setDone(true);
      setTimeout(props.onClose, 1400);
    }
  });

  const complete = async (e: Event) => {
    e.preventDefault();
    if (!input().trim()) return;
    setBusy(true);
    setError(null);
    try {
      await api.completeOauth(props.provider, input().trim());
      void refreshProviders();
      setDone(true);
      setTimeout(props.onClose, 1200);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal onClose={props.onClose} title="Sign in with ChatGPT" size="sm">
      <div class="modal-body oauth">
        <Show
          when={!done()}
          fallback={
            <div class="oauth-done">
              <Icon name="circleCheck" size={40} />
              <p>Signed in. Your ChatGPT models are ready.</p>
            </div>
          }
        >
          <p class="muted">{props.start.instructions}</p>
          <Show when={props.start.user_code}>
            <div class="oauth-code">
              <span class="code-big mono">{props.start.user_code}</span>
              <CopyButton text={() => props.start.user_code!} label="Copy code" size={16} />
            </div>
          </Show>
          <Show
            when={safeUrl()}
            fallback={
              <div class="form-error">
                <Icon name="alertCircle" size={16} />
                <span>The server sent a sign-in link that isn't https, so it isn't shown.</span>
              </div>
            }
          >
            <div class="oauth-url">
              <a
                href={safeUrl()!}
                class="mono"
                onClick={(e) => {
                  e.preventDefault();
                  void openExternal(safeUrl()!);
                }}
                onAuxClick={(e) => {
                  e.preventDefault();
                  void openExternal(safeUrl()!);
                }}
              >
                {safeUrl()}
              </a>
              <CopyButton text={() => safeUrl()!} label="Copy link" />
            </div>
            <button type="button" class="btn btn-primary full" onClick={() => openExternal(safeUrl()!)}>
              <Icon name="external" size={16} /> Open sign-in page
            </button>
          </Show>
          <Show when={props.start.auto_complete}>
            <div class="oauth-wait">
              <Spinner size={15} />
              <span>Waiting for you to finish signing in…</span>
            </div>
          </Show>
          <form class="oauth-manual" onSubmit={complete}>
            <label class="field">
              <span class="field-label">{props.start.auto_complete ? "Stuck? Paste the redirect URL or code" : "Paste the redirect URL or code"}</span>
              <div class="row gap">
                <input class="input mono" value={input()} onInput={(e) => setInput(e.currentTarget.value)} placeholder="http://localhost:1455/auth/callback?code=…" />
                <button type="submit" class="btn" disabled={busy() || !input().trim()}>
                  <Show when={busy()} fallback="Submit">
                    <Spinner size={14} />
                  </Show>
                </button>
              </div>
            </label>
            <Show when={error()}>
              <div class="form-error">{error()}</div>
            </Show>
          </form>
        </Show>
      </div>
    </Modal>
  );
}

function slug(s: string) {
  return s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 32);
}

export function OpenAIForm(props: { onDone: () => void }) {
  const [name, setName] = createSignal("");
  const [id, setId] = createSignal("");
  const [idTouched, setIdTouched] = createSignal(false);
  const [url, setUrl] = createSignal("");
  const [key, setKey] = createSignal("");
  const [showKey, setShowKey] = createSignal(false);
  const [discover, setDiscover] = createSignal(true);
  const [kind, setKind] = createSignal<ProviderKind>("openai_chat");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const effId = () => (idTouched() ? id() : slug(name()));

  const submit = async (e: Event) => {
    e.preventDefault();
    setError(null);
    if (!name().trim() || !effId() || !/^https?:\/\//.test(url().trim())) {
      setError("A name and the server's address (starting with http:// or https://) are needed.");
      return;
    }
    setBusy(true);
    try {
      await api.saveProvider({
        id: effId(),
        name: name().trim(),
        kind: kind(),
        base_url: url().trim().replace(/\/+$/, ""),
        ...(key().trim() ? { api_key: key().trim() } : {}),
        discover: discover(),
        models: [],
      });
      await refreshProviders();
      toast({ level: "success", message: `${name().trim()} added`, timeout: 2000 });
      props.onDone();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card class="form-card">
      <form class="stack" onSubmit={submit}>
        <div class="field">
          <span class="field-label">Start from</span>
          <div class="preset-row">
            <For each={PRESETS}>
              {(p) => (
                <button
                  type="button"
                  class="chip"
                  classList={{ active: url() === p.url }}
                  onClick={() => {
                    setName(p.name);
                    setIdTouched(false);
                    setUrl(p.url);
                    setKind(p.kind);
                  }}
                >
                  {p.name}
                  <span class="chip-note">{p.hint}</span>
                </button>
              )}
            </For>
          </div>
        </div>
        <div class="grid2">
          <label class="field">
            <span class="field-label">Name</span>
            <input class="input" value={name()} onInput={(e) => setName(e.currentTarget.value)} placeholder="My desktop GPU" />
          </label>
          <label class="field">
            <span class="field-label">Address</span>
            <input class="input mono" value={url()} onInput={(e) => setUrl(e.currentTarget.value)} placeholder="http://192.168.1.20:8080/v1" inputmode="url" />
            <span class="field-help">The address your Tessera server uses to reach the model, not your phone's address.</span>
          </label>
        </div>
        <label class="field">
          <span class="field-label">
            API key<span class="opt">if the service needs one</span>
          </span>
          <div class="secret">
            <input class="input mono" type={showKey() ? "text" : "password"} value={key()} onInput={(e) => setKey(e.currentTarget.value)} autocomplete="off" />
            <button type="button" class="icon-btn" aria-label={showKey() ? "Hide key" : "Show key"} onClick={() => setShowKey(!showKey())}>
              <Icon name={showKey() ? "eyeOff" : "eye"} size={16} />
            </button>
          </div>
          <span class="field-help">Stored on your server only; never sent back to this device.</span>
        </label>
        <Advanced>
          <div class="grid2">
            <label class="field">
              <span class="field-label">ID</span>
              <input
                class="input mono"
                value={effId()}
                onInput={(e) => {
                  setIdTouched(true);
                  setId(slug(e.currentTarget.value));
                }}
                placeholder="local"
              />
              <span class="field-help">Used in model names like id/model.</span>
            </label>
            <div class="field">
              <span class="field-label">API style</span>
              <Segmented
                label="API"
                value={kind()}
                onChange={setKind}
                options={[
                  { value: "openai_chat", label: "Chat Completions" },
                  { value: "openai_responses", label: "Responses" },
                ]}
              />
            </div>
          </div>
          <label class="inline-toggle">
            <Toggle checked={discover()} onChange={setDiscover} label="Discover models" />
            <span>
              Find models automatically (<code>/models</code>)
            </span>
          </label>
        </Advanced>
        <Show when={error()}>
          <div class="form-error">{error()}</div>
        </Show>
        <div class="form-actions">
          <button type="button" class="btn btn-ghost" onClick={props.onDone}>
            Cancel
          </button>
          <button type="submit" class="btn btn-primary" disabled={busy()}>
            <Show when={busy()}>
              <Spinner size={14} />
            </Show>
            Add service
          </button>
        </div>
      </form>
    </Card>
  );
}

export function AnthropicForm(props: { onDone: () => void }) {
  const address = "https://api.anthropic.com";
  const existing = () => state.providers.find((p) => p.kind === "anthropic" && p.base_url.replace(/\/+$/, "") === address);
  const [key, setKey] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const submit = async (e: Event) => {
    e.preventDefault();
    if (!key().trim()) return;
    setBusy(true);
    setError(null);
    try {
      const ex = existing();
      let id = ex?.id ?? "anthropic";
      for (let n = 2; !ex && state.providers.some((p) => p.id === id); n++) id = `anthropic-${n}`;
      await api.saveProvider({
        id,
        name: ex?.name ?? "Anthropic",
        kind: "anthropic",
        base_url: address,
        api_key: key().trim(),
        discover: true,
        models: ex?.models ?? [],
      });
      await refreshProviders();
      toast({ level: "success", message: "Anthropic key saved", timeout: 2000 });
      props.onDone();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Card class="form-card">
      <form class="stack" onSubmit={submit}>
        <label class="field">
          <span class="field-label">Anthropic API key</span>
          <input class="input mono" type="password" value={key()} onInput={(e) => setKey(e.currentTarget.value)} placeholder="sk-ant-…" autocomplete="off" autofocus />
          <span class="field-help">Stored on your server only; never sent back to this device.</span>
        </label>
        <Show when={error()}>
          <div class="form-error">{error()}</div>
        </Show>
        <div class="form-actions">
          <button type="button" class="btn btn-ghost" onClick={props.onDone}>
            Cancel
          </button>
          <button type="submit" class="btn btn-primary" disabled={busy() || !key().trim()}>
            Save key
          </button>
        </div>
      </form>
    </Card>
  );
}
