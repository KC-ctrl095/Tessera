import { For, Show, createEffect, createSignal, onCleanup } from "solid-js";
import { activeServer, isLocalEndpoint, pairAgain, retryNow, switchServer } from "../lib/app";
import { backend } from "../lib/backend";
import { localServerRecovery, localServerResume, onLocalServerRecovery } from "../lib/native";
import { state } from "../lib/store";
import { openSettings, setAddServerOpen, setOfflineLate } from "../lib/ui";
import { workspace } from "../lib/workspace";
import { Icon, Logo, Spinner, Tessellate } from "./icons";
import { Popover } from "./primitives";

function useNow(ms = 1000) {
  const [now, setNow] = createSignal(Date.now());
  const t = setInterval(() => setNow(Date.now()), ms);
  onCleanup(() => clearInterval(t));
  return now;
}

const hostOf = (url: string | null | undefined) => {
  if (!url) return "";
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
};

export function statusText(): string {
  switch (state.conn.state) {
    case "connected":
      return "Connected";
    case "connecting":
      return "Connecting…";
    case "offline":
      return "Offline";
    default:
      return "Disconnected";
  }
}

/** The server told us this pairing is no longer valid. */
export const needsRepair = () => /pair again/i.test(state.conn.error ?? "");
/** "Use Tessera in a web browser" was turned off on the server (a browser at `/web/`): said as it is. */
const webOff = () => /turned off on this server/i.test(state.conn.error ?? "");

/**
 * How this connection is protected:
 *  - "e2e": the app's own end-to-end encryption to the pinned server key;
 *  - "plain-local" / "plain-remote": the app without E2EE (an old pairing);
 *  - "browser": a browser session (HTTPS at best).
 */
export function security(): "e2e" | "plain-local" | "plain-remote" | "browser" | null {
  if (state.conn.state !== "connected") return null;
  // A browser at /web/ is end-to-end encrypted like the apps.
  if (state.conn.encrypted) return "e2e";
  if (backend.kind === "browser") return "browser";
  return isLocalEndpoint(state.conn.endpoint) ? "plain-local" : "plain-remote";
}

/** Dot + server name; opens connection details. */
export function ConnectionBadge(props: { compact?: boolean }) {
  const [anchor, setAnchor] = createSignal<HTMLElement | null>(null);
  const server = activeServer;
  const sub = () => {
    if (state.conn.state !== "connected") return statusText();
    const sec = security();
    if (sec === "e2e") return "End-to-end encrypted";
    if (sec === "plain-remote") return "Not encrypted end-to-end";
    if (backend.sameOrigin) return state.server?.version ? `Connected · v${state.server.version}` : "Connected";
    return hostOf(state.conn.endpoint);
  };
  const name = () => (backend.sameOrigin ? state.server?.name : undefined) ?? server()?.name ?? state.server?.name ?? "Tessera";
  return (
    <>
      <button
        type="button"
        class="conn-badge"
        classList={{ compact: !!props.compact, warn: security() === "plain-remote" || needsRepair() }}
        data-state={state.conn.state}
        aria-label={`${statusText()}${server() ? ` to ${server()!.name}` : ""}`}
        data-tip={props.compact ? `${server()?.name ?? "Server"} · ${sub()}` : undefined}
        onClick={(e) => setAnchor(anchor() ? null : e.currentTarget)}
      >
        <span class="conn-dot" />
        <Show when={!props.compact}>
          <span class="conn-text">
            <span class="conn-name">{name()}</span>
            <span class="conn-sub">
              <Show when={security() === "e2e"}>
                <Icon name="lock" size={11} class="conn-lock" />
              </Show>
              <Show when={security() === "plain-remote"}>
                <Icon name="lockOpen" size={11} />
              </Show>
              {sub()}
            </span>
          </span>
        </Show>
      </button>
      <Show when={anchor()}>
        <Popover anchor={anchor()!} onClose={() => setAnchor(null)} placement="top-start" class="conn-pop" label="Connection">
          <ConnectionDetails onClose={() => setAnchor(null)} />
        </Popover>
      </Show>
    </>
  );
}

/** One line on how the connection is protected. */
export function SecurityNote(props: { onClose?: () => void; compact?: boolean }) {
  return (
    <Switch2
      value={security()}
      e2e={
        <div class="sec-note ok">
          <Icon name="lock" size={15} />
          <div>
            <div class="sec-title">End-to-end encrypted</div>
            <Show when={activeServer()?.fingerprint}>
              <div class="sec-sub">
                Server key <span class="mono">{activeServer()!.fingerprint}</span>
              </div>
            </Show>
          </div>
        </div>
      }
      plain={(remote) => (
        <div class="sec-note" classList={{ warn: remote }}>
          <Icon name={remote ? "shieldAlert" : "lockOpen"} size={15} />
          <div class="grow">
            <div class="sec-title">Not end-to-end encrypted</div>
            <div class="sec-sub">
              {remote
                ? "This connection crosses the internet without Tessera's own encryption. Pair again to turn it on."
                : "This device was paired before encryption existed. Pair again to turn it on."}
            </div>
          </div>
          <button
            type="button"
            class="btn btn-sm"
            onClick={() => {
              props.onClose?.();
              void pairAgain();
            }}
          >
            Pair again
          </button>
        </div>
      )}
      browser={
        <div class="sec-note subtle">
          <Icon name="globe" size={15} />
          <div>
            <div class="sec-title">Browser session</div>
            <div class="sec-sub">
              Not end-to-end encrypted{location.protocol === "https:" ? " — protected by HTTPS" : ""}. The desktop and Android apps encrypt end to end.
            </div>
          </div>
        </div>
      }
    />
  );
}

function Switch2(props: {
  value: ReturnType<typeof security>;
  e2e: import("solid-js").JSX.Element;
  plain: (remote: boolean) => import("solid-js").JSX.Element;
  browser: import("solid-js").JSX.Element;
}) {
  return (
    <>
      <Show when={props.value === "e2e"}>{props.e2e}</Show>
      <Show when={props.value === "plain-local" || props.value === "plain-remote"}>{props.plain(props.value === "plain-remote")}</Show>
      <Show when={props.value === "browser"}>{props.browser}</Show>
    </>
  );
}

function ConnectionDetails(props: { onClose: () => void }) {
  const others = () => (state.config?.servers ?? []).filter((s) => s.id !== activeServer()?.id);
  return (
    <div class="conn-details">
      <div class="conn-details-head">
        <span class="conn-dot lg" data-state={state.conn.state} />
        <div class="grow">
          <div class="strong ellipsis">{activeServer()?.name ?? "Tessera"}</div>
          <div class="muted small">
            {statusText()}
            <Show when={state.server?.version}> · Tessera {state.server!.version}</Show>
          </div>
        </div>
      </div>
      <SecurityNote onClose={props.onClose} />
      <Show when={activeServer() && !backend.sameOrigin}>
        <ul class="endpoint-list">
          <For each={activeServer()!.endpoints}>
            {(ep) => (
              <li classList={{ active: ep === state.conn.endpoint }}>
                <Icon name={isLocalEndpoint(ep) ? "home" : "cloud"} size={13} />
                <span class="grow ellipsis">{ep}</span>
                <Show when={ep === state.conn.endpoint} fallback={<span class="ep-kind">{isLocalEndpoint(ep) ? "Home" : "Remote"}</span>}>
                  <span class="badge badge-accent">in use</span>
                </Show>
              </li>
            )}
          </For>
        </ul>
      </Show>
      <Show when={state.conn.error && state.conn.state !== "connected"}>
        <p class="conn-error">{state.conn.error}</p>
      </Show>
      <div class="conn-actions">
        <Show when={needsRepair()}>
          <button
            type="button"
            class="btn btn-sm btn-primary"
            onClick={() => {
              props.onClose();
              void pairAgain();
            }}
          >
            <Icon name="key" size={14} /> Pair again
          </button>
        </Show>
        <Show when={state.conn.state !== "connected"}>
          <button type="button" class="btn btn-sm" onClick={() => void retryNow()}>
            <Icon name="refresh" size={14} /> Retry now
          </button>
        </Show>
        <button
          type="button"
          class="btn btn-sm btn-ghost"
          onClick={() => {
            props.onClose();
            openSettings("connection");
          }}
        >
          <Icon name="server" size={14} /> Manage
        </button>
      </div>
      <Show when={others().length}>
        <div class="menu-heading">Switch to</div>
        <For each={others()}>
          {(s) => (
            <button
              type="button"
              class="menu-item"
              onClick={() => {
                props.onClose();
                void switchServer(s.id);
              }}
            >
              <span class="menu-icon">
                <Icon name="server" size={15} />
              </span>
              <span class="menu-text">
                <span class="menu-label">{s.name}</span>
                <span class="menu-desc">{s.endpoints[0]}</span>
              </span>
            </button>
          )}
        </For>
      </Show>
    </div>
  );
}

/** Thin banner when a previously working connection drops. */
/**
 * A small "Reconnecting…" pill — only when getting back takes more than
 * ~1.5 s, so short blips (screen off, switching apps) never flash anything.
 */
export function OfflineBanner() {
  const now = useNow();
  const secs = () => (state.conn.retryAt ? Math.max(0, Math.ceil((state.conn.retryAt - now()) / 1000)) : 0);
  const down = () => state.everConnected && state.conn.state !== "connected" && state.conn.state !== "disconnected";
  const [late, setLate] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(() => {
    clearTimeout(timer);
    if (!down()) return setLate(false);
    // A pairing problem needs the user at once; anything else gets a grace period.
    if (needsRepair()) return setLate(true);
    timer = setTimeout(() => setLate(true), 1500);
  });
  // The composer and unsent messages say so too, on the same terms.
  createEffect(() => setOfflineLate(down() && late()));
  onCleanup(() => {
    clearTimeout(timer);
    setOfflineLate(false);
  });
  return (
    <Show when={down() && late()}>
      <div class="offline-banner" classList={{ below: workspace.mode() === "mobile" }} role="status">
        <Show when={state.conn.state === "connecting"} fallback={<Icon name="wifiOff" size={15} />}>
          <Spinner size={13} />
        </Show>
        <span class="ellipsis">
          <Show when={state.conn.state === "offline"} fallback={"Reconnecting…"}>
            <Show when={needsRepair() || webOff()} fallback={<>Offline{secs() > 0 ? ` · retrying in ${secs()}s` : " · retrying"}</>}>
              {state.conn.error}
            </Show>
          </Show>
        </span>
        <Show when={needsRepair()}>
          <button type="button" class="link-btn" onClick={() => void pairAgain()}>
            Pair again
          </button>
        </Show>
        <Show when={state.conn.state === "offline" && !needsRepair()}>
          <button type="button" class="link-btn" onClick={() => void retryNow()}>
            Retry now
          </button>
        </Show>
      </div>
    </Show>
  );
}

/** Full-screen splash until the first successful connection. */
export function ConnectScreen() {
  const now = useNow();
  const secs = () => (state.conn.retryAt ? Math.max(0, Math.ceil((state.conn.retryAt - now()) / 1000)) : 0);
  const offline = () => state.conn.state === "offline";
  // The server on this computer didn't start again (desktop app): why, where its log is, and a way to try again.
  const [recovery, setRecovery] = createSignal<string | null>(null);
  const [resuming, setResuming] = createSignal(false);
  let stop: (() => void) | undefined;
  let gone = false;
  void onLocalServerRecovery(setRecovery).then((f) => (gone ? f() : (stop = f)));
  void localServerRecovery()
    .then((why) => setRecovery(why ?? null))
    .catch(() => {});
  onCleanup(() => {
    gone = true;
    stop?.();
  });
  const resume = async () => {
    setResuming(true);
    try {
      await localServerResume();
      setRecovery(null);
    } catch (e) {
      setRecovery(String((e as { message?: string })?.message ?? e));
    } finally {
      setResuming(false);
    }
  };
  return (
    <div class="connect-screen">
      <div class="connect-card">
        <div class="connect-mark">
          <Logo size={56} />
        </div>
        <h1 class="serif">{offline() ? "Can't reach your server" : `Connecting to ${activeServer()?.name ?? "Tessera"}`}</h1>
        <Show when={!offline()}>
          <p class="muted connect-status">
            <Tessellate size={9} />
            Looking for it at home first, then remotely.
          </p>
        </Show>
        <Show when={activeServer() && !backend.sameOrigin}>
          <ul class="endpoint-list">
            <For each={activeServer()!.endpoints}>
              {(ep) => (
                <li>
                  <Icon name={isLocalEndpoint(ep) ? "home" : "cloud"} size={13} />
                  <span class="grow ellipsis">{ep}</span>
                  <span class="ep-kind">{isLocalEndpoint(ep) ? "Home" : "Remote"}</span>
                </li>
              )}
            </For>
          </ul>
        </Show>
        <Show when={recovery()}>
          <div class="connect-recovery" role="alert">
            <p class="conn-error">{recovery()}</p>
            <button type="button" class="btn btn-primary" disabled={resuming()} onClick={() => void resume()}>
              <Show when={resuming()} fallback={<Icon name="refresh" size={16} />}>
                <Spinner size={14} />
              </Show>
              Start it again
            </button>
          </div>
        </Show>
        <Show when={offline()}>
          <p class="conn-error">{state.conn.error ?? "The server did not respond."}</p>
          <Show when={!needsRepair()}>
            <p class="muted small">{secs() > 0 ? `Trying again in ${secs()}s.` : "Trying again…"}</p>
          </Show>
        </Show>
        <div class="connect-actions">
          <Show when={offline()} fallback={<Spinner size={18} />}>
            <Show when={needsRepair()}>
              <button type="button" class="btn btn-primary" onClick={() => void pairAgain()}>
                <Icon name="key" size={16} /> Pair again
              </button>
            </Show>
            <button type="button" class={needsRepair() ? "btn" : "btn btn-primary"} onClick={() => void retryNow()}>
              <Icon name="refresh" size={16} /> Retry now
            </button>
            <button type="button" class="btn" onClick={() => openSettings("connection")}>
              <Icon name="server" size={16} /> Servers
            </button>
            <Show when={!backend.sameOrigin}>
              <button type="button" class="btn btn-ghost" onClick={() => setAddServerOpen({})}>
                <Icon name="plus" size={16} /> Add server
              </button>
            </Show>
          </Show>
        </div>
      </div>
    </div>
  );
}
