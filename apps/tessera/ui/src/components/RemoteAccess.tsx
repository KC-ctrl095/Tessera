// Reach Tessera from anywhere: automatic Cloudflare setup (a tunnel, a DNS
// name, Access in front of it and a service token for your devices), or your
// own tunnel / Tailscale. Used by the setup wizard and Settings → Connection.
import { For, Index, Match, Show, Switch, createEffect, createResource, createSignal, onCleanup } from "solid-js";
import "../styles/settings.css";
import "../styles/setup.css";
import type { CloudflareZone } from "../proto/CloudflareZone";
import type { RemoteStatus } from "../proto/RemoteStatus";
import type { SetupStatus } from "../proto/SetupStatus";
import type { TunnelStatus } from "../proto/TunnelStatus";
import { api, resetProgress, state, type ProgressStep } from "../lib/store";
import { localServerDetect, localServerRestart } from "../lib/native";
import { copyText, openExternal } from "../lib/util";
import { confirmDialog, errorMessage, toast } from "../lib/ui";
import { GuideLink } from "./Guide";
import { Icon, Spinner, Tessellate } from "./icons";
import { SettleTiles } from "./Mosaic";
import { RemoteAccessEditor } from "./settings/RemoteAccessEditor";
import { Toggle } from "./primitives";

const TOKENS_URL = "https://dash.cloudflare.com/profile/api-tokens";
const REGISTER_URL = "https://dash.cloudflare.com/?to=/:account/registrar/register";
const TAILSCALE_URL = "https://tailscale.com/download";
/** Cloudflare's Zero Trust dashboard: on a new account it starts the (free) sign-up there. */
const ZERO_TRUST_URL = "https://one.dash.cloudflare.com/";
const SERVICE_TOKENS_URL = "https://one.dash.cloudflare.com/?to=/:account/access/service-auth/service-tokens";

const DAY = 86_400;
/** From this many days before the devices' key runs out, Settings says so (the server notifies from then too). */
export const KEY_WARN_DAYS = 30;

const keyDate = (secs: number) => new Date(secs * 1000).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });

/**
 * Where the key the devices carry through Cloudflare Access stands: fine, running out within
 * KEY_WARN_DAYS, run out, or unknown (set up before Tessera kept its date).
 */
export function keyState(t: TunnelStatus | undefined | null, now = Date.now() / 1000): "ok" | "soon" | "expired" | "unknown" | null {
  // Servers from before renewing say nothing about the key.
  if (!t?.renewable) return null;
  if (t.key_expires_at == null) return "unknown";
  if (t.key_expires_at <= now) return "expired";
  return t.key_expires_at - now <= KEY_WARN_DAYS * DAY ? "soon" : "ok";
}

/** A line under the tunnel's when the devices' key needs renewing (or its date is unknown), with Renew. */
function KeyLine(props: { tunnel: TunnelStatus; onRenew: () => void }) {
  const st = () => keyState(props.tunnel);
  const at = () => props.tunnel.key_expires_at ?? 0;
  const days = () => Math.max(1, Math.ceil((at() - Date.now() / 1000) / DAY));
  return (
    <Show when={st() && st() !== "ok"}>
      <div class="key-line" data-state={st()} role="status">
        <Icon name={st() === "unknown" ? "key" : "alert"} size={13} />
        <span class="grow">
          {st() === "expired"
            ? `Your devices' key for remote access ran out on ${keyDate(at())}, so away from home they can't connect. Renew it, and they work again.`
            : st() === "soon"
              ? `Your devices' key for remote access runs out ${days() <= 1 ? "within a day" : `in ${days()} days`} (${keyDate(at())}). Renew it so they keep working away from home.`
              : "Your devices' key for remote access lasts a year from when it was set up. Renew it once, and Tessera reminds you before it runs out."}
        </span>
        <button type="button" class="btn btn-xs" onClick={() => props.onRenew()}>
          <Icon name="refresh" size={12} /> Renew
        </button>
      </div>
    </Show>
  );
}

/** The one Cloudflare permission renewing the devices' key needs. */
const RENEW_PERMISSION: [string, string, string] = ["Account", "Access: Service Tokens", "Edit"];

/**
 * Renew the devices' key (Cloudflare's service token, a year at a time): with an API token that
 * has one permission (used once, never kept), or by hand in Cloudflare and then "I renewed it".
 * The devices keep the same key, so nothing changes on them.
 */
function RenewKey(props: { tunnel: TunnelStatus; onDone: (s: RemoteStatus) => void; onCancel: () => void }) {
  const [tokenInput, setTokenInput] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [byHand, setByHand] = createSignal(false);
  const renew = async (token?: string) => {
    setError(null);
    setBusy(true);
    try {
      const s = await api.cloudflareRenew(token);
      const until = s.tunnel?.key_expires_at;
      toast({ level: "success", message: until ? `Renewed: your devices' key is good until ${keyDate(until)}` : "Renewed", timeout: 3200 });
      props.onDone(s);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
      setTokenInput("");
    }
  };
  const submit = (e: Event) => {
    e.preventDefault();
    const t = tokenInput().trim();
    if (!t) return setError("Paste the token you created in Cloudflare.");
    void renew(t);
  };
  return (
    <form class="card cf-card cf-renew stack" onSubmit={submit}>
      <p class="field-help">
        Renewing gives your devices' key another year. They keep the same key, so there's nothing to do on them. Cloudflare needs an API token with this
        permission (used once, never stored):
      </p>
      <ul class="cf-perms">
        <li>
          <span class="cf-scope">{RENEW_PERMISSION[0]}</span>
          <span class="cf-what">{RENEW_PERMISSION[1]}</span>
          <span class="cf-level">{RENEW_PERMISSION[2]}</span>
        </li>
      </ul>
      <button type="button" class="link-btn" onClick={() => void openExternal(TOKENS_URL)}>
        Cloudflare → My Profile → API Tokens <Icon name="external" size={12} />
      </button>
      <TokenField value={tokenInput()} onInput={setTokenInput} disabled={busy()} />
      <Show when={error()}>
        <div class="form-error">{error()}</div>
      </Show>
      <div class="form-actions">
        <button type="button" class="btn btn-ghost" onClick={() => props.onCancel()}>
          Cancel
        </button>
        <button type="submit" class="btn btn-primary" disabled={busy()}>
          <Show when={busy()}>
            <Spinner size={14} />
          </Show>
          Renew
        </button>
      </div>
      <button type="button" class="disclosure" aria-expanded={byHand()} onClick={() => setByHand(!byHand())}>
        <Icon name="chevronRight" size={14} class="disc-chev" />
        Or renew it in Cloudflare yourself
      </button>
      <Show when={byHand()}>
        <ol class="cf-steps cf-byhand">
          <li>
            Open{" "}
            <button type="button" class="link-btn" onClick={() => void openExternal(SERVICE_TOKENS_URL)}>
              Zero Trust → Service Tokens <Icon name="external" size={12} />
            </button>{" "}
            (Access controls → Service credentials).
          </li>
          <li>
            Find <b>Tessera apps ({props.tunnel.hostname})</b> and select <b>Refresh</b>. That adds a year.
          </li>
          <li>
            Then tell Tessera, so it reminds you again next year:{" "}
            <button type="button" class="btn btn-xs" disabled={busy()} onClick={() => void renew()}>
              I renewed it
            </button>
          </li>
        </ol>
      </Show>
    </form>
  );
}

/**
 * Cloudflare refuses Access on this account: almost always Zero Trust isn't turned on yet (it's
 * free, once per account). Says what to do, with a link, and a way to check again.
 */
function ZeroTrustNote(props: { said?: string; onCheck?: () => void; checking?: boolean }) {
  return (
    <div class="form-error warn zt-note" role="alert">
      <Icon name="alert" size={14} />
      <div class="zt-body">
        <div>
          <b>Turn on Zero Trust first.</b> Tessera puts Cloudflare Access in front of your address so only your devices get in, and Access is part of
          Cloudflare's Zero Trust, which this account doesn't have yet. It's free: open Zero Trust, choose any team name and the Free plan (Cloudflare asks
          for a card but doesn't charge it).
        </div>
        <div class="zt-small">Already on? Then the token is missing Access: Apps and Policies · Edit or Access: Service Tokens · Edit.</div>
        <Show when={props.said}>
          <div class="zt-small">Cloudflare said: {props.said}</div>
        </Show>
        <div class="zt-actions">
          <button type="button" class="link-btn" onClick={() => void openExternal(ZERO_TRUST_URL)}>
            Open Zero Trust <Icon name="external" size={12} />
          </button>
          <Show when={props.onCheck}>
            <button type="button" class="btn btn-xs" disabled={props.checking} onClick={() => props.onCheck!()}>
              <Show when={props.checking} fallback={<Icon name="refresh" size={12} />}>
                <Spinner size={11} />
              </Show>
              Check again
            </button>
          </Show>
        </div>
      </div>
    </div>
  );
}

/** The server's words when Cloudflare refused Access (see ZERO_TRUST_HELP in crates/core/src/remote.rs). */
const zeroTrustError = (msg: string | null) => !!msg && /Zero Trust isn't turned on/.test(msg);
const cloudflareSaid = (msg: string | null) => /\(Cloudflare said: (.*)\)\s*$/s.exec(msg ?? "")?.[1];

/**
 * The server's remote-access state, checked again every few seconds while a
 * Cloudflare tunnel is still connecting (or failing), so the UI follows it.
 */
export function useRemoteStatus() {
  const [status, ctl] = createResource(() => api.remoteStatus().catch(() => null));
  let timer: ReturnType<typeof setTimeout> | undefined;
  let tries = 0;
  createEffect(() => {
    const t = status()?.tunnel;
    clearTimeout(timer);
    if (status.loading) return;
    if (!t || t.connected) {
      tries = 0;
      return;
    }
    // Quick at first (it usually connects within seconds), then calmer.
    if (tries++ < 120) timer = setTimeout(() => void ctl.refetch(), tries < 20 ? 2500 : 10_000);
  });
  onCleanup(() => clearTimeout(timer));
  return [status, ctl] as const;
}

/** One line on the Cloudflare tunnel: connected, connecting, or why not (and a way to check again). */
export function TunnelLine(props: { tunnel: TunnelStatus; onCheck: () => void; checking?: boolean }) {
  const t = () => props.tunnel;
  return (
    <div class="tunnel-line" classList={{ ok: !!t().connected, bad: !t().connected && !!t().error }} role="status">
      <Show
        when={t().connected}
        fallback={
          <Show when={t().error} fallback={<Tessellate size={8} />}>
            <Icon name="alert" size={13} />
          </Show>
        }
      >
        <span class="sq sq-ok" />
      </Show>
      <span class="grow">
        {t().connected
          ? "Cloudflare tunnel connected"
          : t().error
            ? `The Cloudflare tunnel isn't connected: ${t().error}`
            : t().running
              ? "The Cloudflare tunnel is connecting…"
              : "The Cloudflare tunnel isn't running."}
      </span>
      <Show when={!t().connected}>
        <button type="button" class="btn btn-xs" disabled={props.checking} onClick={() => props.onCheck()}>
          <Show when={props.checking} fallback={<Icon name="refresh" size={12} />}>
            <Spinner size={11} />
          </Show>
          Check again
        </button>
      </Show>
    </div>
  );
}

/** No domain: buy one cheaply on Cloudflare, or use Tailscale; why quick tunnels aren't offered. */
function NoDomainGuide() {
  const [open, setOpen] = createSignal(false);
  return (
    <div class="nodomain">
      <button type="button" class="disclosure" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <Icon name="chevronRight" size={14} class="disc-chev" />
        No domain? Other ways to reach it from anywhere
      </button>
      <Show when={open()}>
        <div class="nodomain-body">
          <div class="nd-item">
            <span class="nd-icon">
              <Icon name="globe" size={15} />
            </span>
            <div>
              <div class="nd-title">Get a domain on Cloudflare</div>
              <p>
                Cloudflare sells domains at cost — a .com is about $10 a year. Buy one under Domain Registration in the same free account, then come back
                and choose <b>Set it up with Cloudflare</b>.
              </p>
              <button type="button" class="link-btn" onClick={() => void openExternal(REGISTER_URL)}>
                Register a domain <Icon name="external" size={12} />
              </button>
            </div>
          </div>
          <div class="nd-item">
            <span class="nd-icon">
              <Icon name="link" size={15} />
            </span>
            <div>
              <div class="nd-title">Tailscale — free, no domain</div>
              <p>
                Install Tailscale on this server and on your phone and sign in to both with the same account. Turn on{" "}
                <b>Let other devices on my network connect</b>, then choose <b>I use Tailscale or my own tunnel</b> and enter the server's Tailscale name
                (like <code>http://my-server.tail1234.ts.net:7878</code>). It works from anywhere, for your devices only.
              </p>
              <button type="button" class="link-btn" onClick={() => void openExternal(TAILSCALE_URL)}>
                Get Tailscale <Icon name="external" size={12} />
              </button>
            </div>
          </div>
          <p class="nd-note">
            Cloudflare's free “quick tunnels” need no domain, but their address changes every time the server restarts (your phone would lose it) and
            nothing guards the door, so Tessera doesn't use them.{" "}
            <GuideLink name="remote-access">The full guide</GuideLink>
          </p>
        </div>
      </Show>
    </div>
  );
}

/** Exactly the permissions the setup needs, nothing more. */
export const CF_PERMISSIONS: [string, string, string][] = [
  ["Account", "Cloudflare Tunnel", "Edit"],
  ["Account", "Access: Apps and Policies", "Edit"],
  ["Account", "Access: Service Tokens", "Edit"],
  ["Zone", "DNS", "Edit"],
  ["Zone", "Zone", "Read"],
];

const REASSURE = "The token is only used to set things up and is never stored. You can delete it afterwards. Cloudflare only ever sees encrypted traffic.";

/** The token recipe: where to click and what to tick. */
function TokenRecipe() {
  const copy = async () => {
    const text = CF_PERMISSIONS.map(([a, b, c]) => `${a} · ${b} · ${c}`).join("\n");
    if (await copyText(text)) toast({ level: "success", message: "Permissions copied", timeout: 1600 });
  };
  return (
    <div class="cf-recipe">
      <ol class="cf-steps">
        <li>
          Turn on <b>Zero Trust</b> in your Cloudflare account, once (free):{" "}
          <button type="button" class="link-btn" onClick={() => void openExternal(ZERO_TRUST_URL)}>
            open Zero Trust <Icon name="external" size={12} />
          </button>
          , choose a team name and the Free plan. Skip this if it's on already.
        </li>
        <li>
          Open{" "}
          <button type="button" class="link-btn" onClick={() => void openExternal(TOKENS_URL)}>
            Cloudflare → My Profile → API Tokens <Icon name="external" size={12} />
          </button>{" "}
          and choose <b>Create Custom Token</b>.
        </li>
        <li>
          Add these permissions:
          <ul class="cf-perms">
            <For each={CF_PERMISSIONS}>
              {([scope, what, level]) => (
                <li>
                  <span class="cf-scope">{scope}</span>
                  <span class="cf-what">{what}</span>
                  <span class="cf-level">{level}</span>
                </li>
              )}
            </For>
          </ul>
          <button type="button" class="btn btn-xs" onClick={() => void copy()}>
            <Icon name="copy" size={12} /> Copy the list
          </button>
        </li>
        <li>
          Under <b>Account Resources</b> pick your account, and under <b>Zone Resources</b> only the domain you'll use.
        </li>
        <li>Set a short expiry (a day is plenty), create the token and copy it.</li>
      </ol>
      <p class="cf-reassure">
        <Icon name="shieldCheck" size={14} />
        <span>{REASSURE}</span>
      </p>
    </div>
  );
}

/** Live steps from `setup_progress` (task "cloudflare"), as tesserae filling in. */
/**
 * The server may mark each step done, or only announce the next one (then a
 * step counts as done once a later one starts). A final "failed" step carries
 * the error.
 */
function progressRows(list: ProgressStep[]): ProgressStep[] {
  const rows = list.map((s) => (s.step === "failed" ? { ...s, step: "Setup stopped", done: false } : s));
  return rows.map((s, i) => (s.error ? s : { ...s, done: s.done || i < rows.length - 1 }));
}

function ProgressList() {
  const steps = () => progressRows(state.progress.cloudflare ?? []);
  // Index keeps each row (and its settle animation) stable as steps update.
  return (
    <ol class="cf-progress" aria-live="polite">
      <Index each={steps()}>
        {(s) => (
          <li classList={{ done: s().done, error: !!s().error }}>
            <span class="cf-progress-st">
              <Show when={s().error} fallback={<Show when={s().done} fallback={<Tessellate size={9} />}><span class="sq sq-ok set" /></Show>}>
                <span class="sq sq-error" />
              </Show>
            </span>
            <span class="cf-progress-step">{s().step}</span>
            <Show when={s().error}>
              <span class="cf-progress-err">{s().error}</span>
            </Show>
          </li>
        )}
      </Index>
    </ol>
  );
}

/** A write-only token field: never echoed, cleared once it has been used. */
function TokenField(props: { value: string; onInput: (v: string) => void; disabled?: boolean }) {
  return (
    <label class="field">
      <span class="field-label">Cloudflare API token</span>
      <input
        class="input mono"
        type="password"
        value={props.value}
        placeholder="Paste the token"
        autocomplete="new-password"
        autocapitalize="off"
        autocorrect="off"
        spellcheck={false}
        data-1p-ignore
        data-lpignore="true"
        disabled={props.disabled}
        onInput={(e) => props.onInput(e.currentTarget.value)}
      />
    </label>
  );
}

type Phase = "choose" | "cf-token" | "cf-zone" | "cf-running" | "cf-done" | "own";

/**
 * The remote-access setup. `onPair` shows the phone pairing QR (the wizard's
 * next step); `onChanged` fires after setup or removal.
 */
export function RemoteAccessPanel(props: { onPair?: () => void; onChanged?: (s: RemoteStatus) => void }) {
  const [status, { mutate, refetch }] = useRemoteStatus();
  const [checking, setChecking] = createSignal(false);
  const check = async () => {
    setChecking(true);
    await refetch();
    setChecking(false);
  };
  /** Set up, and the tunnel (if any) really connected. */
  const live = () => !!status()?.urls.length && (!status()?.tunnel || !!status()?.tunnel?.connected);
  const [phase, setPhase] = createSignal<Phase>("choose");
  const [tokenInput, setTokenInput] = createSignal("");
  // The token lives here only between "Check" and the end of setup, then it's dropped.
  let token = "";
  const [zones, setZones] = createSignal<CloudflareZone[]>([]);
  const [zone, setZone] = createSignal<CloudflareZone | null>(null);
  const [host, setHost] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [removing, setRemoving] = createSignal(false);
  const [renewing, setRenewing] = createSignal(false);
  /** Ask Cloudflare again with the token kept since "Check token" (after turning Zero Trust on). */
  const [rechecking, setRechecking] = createSignal(false);
  const recheck = async () => {
    if (!token) return setPhase("cf-token");
    setError(null);
    setRechecking(true);
    try {
      const list = await api.cloudflareZones(token);
      setZones(list);
      pickZone(list.find((z) => z.id === zone()?.id) ?? list[0], false);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setRechecking(false);
    }
  };

  const forget = () => {
    token = "";
    setTokenInput("");
  };

  const checkToken = async (e: Event) => {
    e.preventDefault();
    setError(null);
    const t = tokenInput().trim();
    if (!t) return setError("Paste the token you created in Cloudflare.");
    setBusy(true);
    try {
      const list = await api.cloudflareZones(t);
      token = t;
      setTokenInput("");
      setZones(list);
      if (!list.length) {
        forget();
        return setError("This token can't see any domain. In Zone Resources, include the domain you want to use.");
      }
      pickZone(list[0]);
      setPhase("cf-zone");
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };
  const pickZone = (z: CloudflareZone | undefined, name = true) => {
    if (!z) return;
    setZone(z);
    if (name || !host()) setHost(`tessera.${z.name}`);
  };
  const runSetup = async (e: Event) => {
    e.preventDefault();
    setError(null);
    const z = zone();
    const h = host().trim().toLowerCase();
    if (!z) return;
    if (!(h === z.name || h.endsWith("." + z.name)) || !/^[a-z0-9.-]+$/.test(h)) return setError(`Use a name under ${z.name}, like tessera.${z.name}.`);
    resetProgress("cloudflare");
    setPhase("cf-running");
    try {
      const s = await api.cloudflareSetup(token, z.id, h);
      mutate(s);
      setPhase("cf-done");
      props.onChanged?.(s);
    } catch (err) {
      setError(errorMessage(err));
      setPhase("cf-zone");
    } finally {
      // Used once: gone from memory whatever happened.
      forget();
    }
  };
  const remove = async (e: Event) => {
    e.preventDefault();
    setError(null);
    const t = tokenInput().trim();
    if (!t) return setError("Removing needs a Cloudflare token with the same permissions (you can make a new one).");
    setBusy(true);
    resetProgress("cloudflare");
    try {
      const s = await api.cloudflareRemove(t);
      mutate(s);
      setRemoving(false);
      toast({ level: "success", message: "Remote access removed", timeout: 2200 });
      props.onChanged?.(s);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      forget();
      setBusy(false);
    }
  };

  return (
    <div class="remote-panel stack">
      {/* Current state, always visible. */}
      <Show when={status()}>
        {(s) => (
          <div class="remote-status" classList={{ on: live() }}>
            <Icon name={s().urls.length ? "globe" : "home"} size={16} />
            <div class="grow">
              <Show
                when={s().urls.length}
                fallback={
                  <>
                    <div class="strong">Only at home for now</div>
                    <div class="muted small">{s().lan_urls.length ? `On your network at ${s().lan_urls.join(", ")}` : "This server can be reached from this computer only."}</div>
                  </>
                }
              >
                <div class="strong">{live() ? "Reachable from anywhere" : "Remote access is set up"}</div>
                <div class="muted small mono">{s().urls.join(", ")}</div>
              </Show>
            </div>
            <Show when={s().tunnel && !removing() && phase() !== "cf-done"}>
              <button type="button" class="btn btn-sm btn-ghost danger-text" onClick={() => setRemoving(true)}>
                Remove remote access
              </button>
            </Show>
            <Show when={s().urls.length && phase() !== "cf-done" ? s().tunnel : undefined}>{(t) => <TunnelLine tunnel={t()} onCheck={() => void check()} checking={checking()} />}</Show>
            <Show when={!renewing() && !removing() && phase() !== "cf-done" ? s().tunnel : undefined}>{(t) => <KeyLine tunnel={t()} onRenew={() => setRenewing(true)} />}</Show>
          </div>
        )}
      </Show>

      <Show when={renewing() ? status()?.tunnel : undefined}>
        {(t) => (
          <RenewKey
            tunnel={t()}
            onDone={(s) => {
              mutate(s);
              setRenewing(false);
            }}
            onCancel={() => setRenewing(false)}
          />
        )}
      </Show>

      <Show when={removing()}>
        <form class="card cf-card stack" onSubmit={remove}>
          <p class="field-help">
            Removing stops the tunnel and deletes its DNS record, Access app and service token. Cloudflare needs a token for that too — the same
            permissions as before.
          </p>
          <TokenField value={tokenInput()} onInput={setTokenInput} disabled={busy()} />
          <Show when={busy()}>
            <ProgressList />
          </Show>
          <Show when={error()}>
            <div class="form-error">{error()}</div>
          </Show>
          <div class="form-actions">
            <button type="button" class="btn btn-ghost" onClick={() => { setRemoving(false); forget(); setError(null); }}>
              Cancel
            </button>
            <button type="submit" class="btn btn-danger" disabled={busy()}>
              <Show when={busy()}>
                <Spinner size={14} />
              </Show>
              Remove
            </button>
          </div>
        </form>
      </Show>

      <Show when={!status()?.tunnel || phase() === "cf-done"}>
        <Switch>
          <Match when={phase() === "choose"}>
            <div class="search-choice">
              <button type="button" class="choice-card" onClick={() => setPhase("cf-token")}>
                <span class="choice-top">
                  <Icon name="cloud" size={15} />
                  <span class="choice-label">Set it up with Cloudflare</span>
                </span>
                <span class="choice-desc">Automatic: a private tunnel to a name on your domain, protected by Cloudflare Access. Needs a free Cloudflare account and a domain on it.</span>
              </button>
              <button type="button" class="choice-card" onClick={() => setPhase("own")}>
                <span class="choice-top">
                  <Icon name="link" size={15} />
                  <span class="choice-label">I use Tailscale or my own tunnel</span>
                </span>
                <span class="choice-desc">Tell Tessera the address your devices should use away from home.</span>
              </button>
            </div>
            <NoDomainGuide />
            <p class="cf-reassure">
              <Icon name="lock" size={14} />
              <span>Either way, your devices talk to Tessera end-to-end encrypted: the tunnel only ever carries traffic it can't read.</span>
            </p>
          </Match>
          <Match when={phase() === "cf-token"}>
            <form class="card cf-card stack" onSubmit={checkToken}>
              <p class="field-help">
                Tessera will create a tunnel from this server to a name on your domain, put Cloudflare Access in front of it and give your devices a key for it.
                Nothing listens on your router; no ports to open.
              </p>
              <TokenRecipe />
              <TokenField value={tokenInput()} onInput={setTokenInput} disabled={busy()} />
              <Show when={error()}>
                <div class="form-error">{error()}</div>
              </Show>
              <div class="form-actions">
                <button type="button" class="btn btn-ghost" onClick={() => { forget(); setError(null); setPhase("choose"); }}>
                  Back
                </button>
                <button type="submit" class="btn btn-primary" disabled={busy()}>
                  <Show when={busy()}>
                    <Spinner size={14} />
                  </Show>
                  Check token
                </button>
              </div>
            </form>
          </Match>
          <Match when={phase() === "cf-zone"}>
            <form class="card cf-card stack" onSubmit={runSetup}>
              <div class="field">
                <span class="field-label">Domain</span>
                <div class="cf-zones" role="radiogroup" aria-label="Domain">
                  <For each={zones()}>
                    {(z) => (
                      <button type="button" role="radio" class="cf-zone" aria-checked={zone()?.id === z.id} classList={{ on: zone()?.id === z.id }} onClick={() => pickZone(z)}>
                        <Icon name="globe" size={14} />
                        <span class="mono">{z.name}</span>
                      </button>
                    )}
                  </For>
                </div>
              </div>
              <label class="field">
                <span class="field-label">Address for Tessera</span>
                <input class="input mono" value={host()} autocomplete="off" spellcheck={false} onInput={(e) => setHost(e.currentTarget.value)} />
                <span class="field-help">Your devices will reach Tessera at https://{host() || "…"}</span>
              </label>
              <Show
                when={zone()?.zero_trust === false || zeroTrustError(error())}
                fallback={
                  <Show when={error()}>
                    <div class="form-error">{error()}</div>
                  </Show>
                }
              >
                <ZeroTrustNote said={cloudflareSaid(error())} onCheck={() => void recheck()} checking={rechecking()} />
              </Show>
              <div class="form-actions">
                <button type="button" class="btn btn-ghost" onClick={() => { forget(); setError(null); setPhase("cf-token"); }}>
                  Back
                </button>
                <button type="submit" class="btn btn-primary">
                  <Icon name="cloud" size={14} /> Set up remote access
                </button>
              </div>
            </form>
          </Match>
          <Match when={phase() === "cf-running"}>
            <div class="card cf-card stack">
              <div class="setting-title">Setting up {host()}…</div>
              <ProgressList />
            </div>
          </Match>
          <Match when={phase() === "cf-done"}>
            <div class="card cf-card cf-done stack">
              <div class="cf-done-head">
                <SettleTiles size={8} />
                <div>
                  <div class="setting-title">{status()?.tunnel?.connected === false ? "Remote access is set up" : "Remote access is ready"}</div>
                  <div class="muted small mono">https://{status()?.tunnel?.hostname ?? host()}</div>
                </div>
              </div>
              <Show when={status()?.tunnel}>{(t) => <TunnelLine tunnel={t()} onCheck={() => void check()} checking={checking()} />}</Show>
              <p class="field-help">
                You can delete the token in Cloudflare now — Tessera doesn't keep it.{" "}
                <button type="button" class="link-btn" onClick={() => void openExternal(TOKENS_URL)}>
                  Delete it in Cloudflare <Icon name="external" size={12} />
                </button>
              </p>
              <p class="field-help cf-devices-follow">
                Phones and computers you paired before get the new address by themselves the next time they connect: open Tessera on each one once
                while it's at home. No need to pair them again.
              </p>
              <Show when={props.onPair}>
                <div>
                  <button type="button" class="btn btn-primary" onClick={() => props.onPair!()}>
                    <Icon name="phone" size={15} /> Pair your phone
                  </button>
                </div>
              </Show>
            </div>
          </Match>
          <Match when={phase() === "own"}>
            <div class="card cf-card stack">
              <p class="field-help">
                With Tailscale, use this computer's Tailscale name (like <code>http://my-server.tail1234.ts.net:7878</code>). Behind your own reverse proxy,
                use its https address and any headers it needs.
              </p>
              <RemoteAccessEditor />
              <div>
                <button type="button" class="btn btn-ghost" onClick={() => { setPhase("choose"); void refetch(); }}>
                  Back
                </button>
              </div>
            </div>
          </Match>
        </Switch>
      </Show>
    </div>
  );
}

/**
 * "Let other devices on my network connect": where the server listens from
 * its next start. Off (this computer only) is the default.
 */
export function LanToggle(props: { status?: SetupStatus | null; onChanged?: (s: SetupStatus) => void }) {
  const [local, setLocal] = createSignal<SetupStatus | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // Without a status from the caller (Settings), ask the server.
  const [own] = createResource(
    () => props.status === undefined,
    () => api.setupStatus().catch(() => null),
  );
  const st = () => local() ?? props.status ?? own() ?? null;
  // The desktop app can restart a server running on this same computer.
  const [here] = createResource(() => localServerDetect().catch(() => null));
  const canRestart = () => !!here()?.running && !!here()?.saved;
  /**
   * The server runs on Windows (its sandbox description says so: there is none there yet; see
   * crates/core/src/sandbox.rs): Windows' firewall asks about it once it listens on the network.
   */
  const windowsServer = () => /on Windows/.test(st()?.sandbox ?? "");
  const restart = async () => {
    setError(null);
    setBusy(true);
    try {
      await localServerRestart();
      const s = await api.setupStatus();
      setLocal(s);
      props.onChanged?.(s);
      toast({ level: "success", message: "Tessera restarted" });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const port = () => st()?.bind.split(":").pop() || "7878";
  const on = () => !!st()?.bind && !/^(127\.0\.0\.1|\[::1\]|localhost):/.test(st()!.bind);
  const set = async (v: boolean) => {
    setError(null);
    setBusy(true);
    try {
      const s = await api.setServerBind(`${v ? "0.0.0.0" : "127.0.0.1"}:${port()}`);
      setLocal(s);
      props.onChanged?.(s);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Show when={st()?.bind}>
      <div class="lan-toggle">
        <div class="setting-row">
          <div class="setting-text">
            <div class="setting-title">Let other devices on my network connect</div>
            <div class="setting-help">
              {on()
                ? "Phones and computers at home can reach it. They still need to pair first, and everything stays end-to-end encrypted."
                : "Off: only this computer can reach Tessera (plus remote access, if you set it up)."}
            </div>
          </div>
          <Toggle checked={on()} disabled={busy()} onChange={(v) => void set(v)} label="Let other devices on my network connect" />
        </div>
        <Show when={on() && windowsServer()}>
          <p class="field-help lan-firewall">
            On the PC running Tessera, Windows may ask whether it may use your network: tick <b>Private networks</b> and choose <b>Allow</b>. If your
            phone still can't connect at home, make that PC's network private: Windows Settings → Network &amp; internet → your Wi-Fi or Ethernet →{" "}
            <b>Private network</b>.
          </p>
        </Show>
        <Show when={st()?.restart_needed}>
          <div class="form-error warn">
            <Icon name="refresh" size={14} />
            <span>Takes effect after the server restarts.</span>
            <Show when={canRestart()}>
              <button type="button" class="btn btn-sm" disabled={busy()} onClick={() => void restart()}>
                Restart now
              </button>
            </Show>
          </div>
        </Show>
        <Show when={error()}>
          <div class="form-error">{error()}</div>
        </Show>
      </div>
    </Show>
  );
}

/** The one Cloudflare permission letting browsers in needs. */
const WEB_DOOR_PERMISSION: [string, string, string] = ["Account", "Access: Apps and Policies", "Edit"];

/**
 * "Use Tessera in a web browser" (Settings → Connection): off by default.
 * One switch, what it changes in plain words, and once on: where to open it
 * and how a browser gets in (a pairing code from one of your devices).
 */
export function WebAccessPanel() {
  const [status, { mutate }] = useRemoteStatus();
  const web = () => status()?.web;
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  /** Turning on needs Cloudflare to let browsers through first: the token form. */
  const [asking, setAsking] = createSignal(false);
  const [tokenInput, setTokenInput] = createSignal("");

  const apply = async (on: boolean, token?: string) => {
    setError(null);
    setBusy(true);
    try {
      const s = await api.setWebAccess(on, token);
      mutate(s);
      setAsking(false);
      toast({ level: "success", message: on ? "Tessera can be used in a web browser now" : "Web browser access is off", timeout: 2400 });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
      setTokenInput("");
    }
  };
  const toggle = async (on: boolean) => {
    setError(null);
    if (!on) return apply(false);
    const ok = await confirmDialog({
      title: "Use Tessera in a web browser?",
      body: "Anyone who finds your address will see Tessera's sign-in page. Only browsers you pair with a code from one of your devices get in, and everything stays end-to-end encrypted. You can turn it off at any time.",
      confirmLabel: "Turn on",
    });
    if (!ok) return;
    if (web()?.needs_token) return setAsking(true);
    return apply(true);
  };
  const submitToken = (e: Event) => {
    e.preventDefault();
    const t = tokenInput().trim();
    if (!t) return setError("Paste the token you created in Cloudflare.");
    void apply(true, t);
  };
  const copy = async (url: string) => {
    if (await copyText(url)) toast({ level: "success", message: "Address copied", timeout: 1600 });
  };

  return (
    <Show when={web()} fallback={<Show when={status() && !status()?.web}><p class="muted small">Update the server to use Tessera in a web browser.</p></Show>}>
      {(w) => (
        <div class="web-access stack" data-on={w().on ? "" : undefined}>
          <div class="setting-row">
            <div class="setting-text">
              <div class="setting-title">Use Tessera in a web browser</div>
              <div class="setting-help">
                {w().on
                  ? "On: any web browser you pair can use Tessera at your address."
                  : "Off: only the Tessera apps can connect. Turn it on to use Tessera in any web browser, like a work computer's."}
              </div>
            </div>
            <Toggle checked={w().on} disabled={busy() || (!w().on && !w().available)} onChange={(v) => void toggle(v)} label="Use Tessera in a web browser" />
          </div>

          <ul class="web-facts">
            <li>
              <Icon name="lock" size={13} />
              <span>Only browsers you pair with a code from one of your devices get in, and everything stays end-to-end encrypted.</span>
            </li>
            <li>
              <Icon name="globe" size={13} />
              <span>While it's on, anyone who finds your address sees Tessera's sign-in page.</span>
            </li>
            <li>
              <Icon name="alert" size={13} />
              <span>A browser is a little less protected than the apps: it loads Tessera from your server each time, so whoever runs the address in between (like Cloudflare) could change the page. Use it on computers you trust.</span>
            </li>
          </ul>

          <Show when={!w().available}>
            <div class="form-error warn">
              <Icon name="alert" size={14} />
              <span>This server doesn't include the web app. Install Tessera again on the server with Node.js there, and the installer adds it.</span>
            </div>
          </Show>

          <Show when={asking()}>
            <form class="card cf-card stack" onSubmit={submitToken}>
              <p class="field-help">
                Cloudflare lets only the Tessera apps through to your address. To let browsers reach Tessera at <span class="mono">/web</span>, Tessera asks Cloudflare
                once. That needs an API token with this permission (the token is used once and never stored):
              </p>
              <ul class="cf-perms">
                <li>
                  <span class="cf-scope">{WEB_DOOR_PERMISSION[0]}</span>
                  <span class="cf-what">{WEB_DOOR_PERMISSION[1]}</span>
                  <span class="cf-level">{WEB_DOOR_PERMISSION[2]}</span>
                </li>
              </ul>
              <button type="button" class="link-btn" onClick={() => void openExternal(TOKENS_URL)}>
                Cloudflare → My Profile → API Tokens <Icon name="external" size={12} />
              </button>
              <TokenField value={tokenInput()} onInput={setTokenInput} disabled={busy()} />
              <div class="form-actions">
                <button type="button" class="btn btn-ghost" onClick={() => { setAsking(false); setTokenInput(""); setError(null); }}>
                  Cancel
                </button>
                <button type="submit" class="btn btn-primary" disabled={busy()}>
                  <Show when={busy()}>
                    <Spinner size={14} />
                  </Show>
                  Turn on
                </button>
              </div>
            </form>
          </Show>

          <Show when={w().on}>
            <Show
              when={w().url}
              fallback={
                <div class="form-error warn">
                  <Icon name="alert" size={14} />
                  <span>Your server has no https address yet. Set up remote access above; until then it works only in a browser on the server itself.</span>
                </div>
              }
            >
              {(url) => (
                <div class="web-how stack">
                  <div class="web-url">
                    <span class="mono grow ellipsis">{url()}</span>
                    <button type="button" class="btn btn-sm" onClick={() => void copy(url())}>
                      <Icon name="copy" size={13} /> Copy
                    </button>
                  </div>
                  <ol class="web-steps">
                    <li>Open this address in the web browser.</li>
                    <li>
                      Make a pairing code here in <b>Settings → Devices</b>.
                    </li>
                    <li>Type the code in the browser. Done: it shows up in Devices, where you can sign it out.</li>
                  </ol>
                </div>
              )}
            </Show>
          </Show>
          <Show when={w().browsers > 0}>
            <p class="muted small">
              {w().browsers === 1 ? "1 browser is paired" : `${w().browsers} browsers are paired`}
              {w().on ? "." : " (they can't connect while this is off)."} Sign them out in Devices.
            </p>
          </Show>
          <Show when={error()}>
            <div class="form-error">{error()}</div>
          </Show>
        </div>
      )}
    </Show>
  );
}
