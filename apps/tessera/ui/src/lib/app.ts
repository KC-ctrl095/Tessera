// App lifecycle: server selection, connection status → bootstrap, onboarding.
import { createSignal } from "solid-js";
import { backend } from "./backend";
import { applyEvent, bootstrap, resetServerState, setState, state } from "./store";
import { appResumed } from "./native";
import { resetAlerts, watchAlerts } from "./alerts";
import { resetInbox } from "./inboxState";
import { watchFiles } from "./files";
import type { ClientConfig, ConnectionStatus, PairParams, SavedServer } from "./transport";
import { setAddServerOpen, toastError } from "./ui";
import { sayHello } from "./devices";
import { loadWorkspace } from "./workspace";

export type Phase = "boot" | "onboarding" | "app";
export const [phase, setPhase] = createSignal<Phase>("boot");

let currentServer: string | null = null;
let started = false;

function selectServerLocal(id: string) {
  if (currentServer === id) return;
  // Nothing of one server's notifications stays on screen for another.
  resetInbox();
  resetAlerts();
  if (currentServer) resetServerState();
  currentServer = id;
  loadWorkspace(id);
}

function onStatus(s: ConnectionStatus) {
  const prev = state.conn.state;
  setState("conn", s);
  if (s.serverId && s.serverId !== currentServer) selectServerLocal(s.serverId);
  if (s.state === "connected" && prev !== "connected") {
    bootstrap().catch((e) => toastError(e, "Could not sync with the server"));
    // What this device is (Settings → Devices), and its name as the server has it.
    void sayHello().then((d) => d && rememberDeviceName(d.name)).catch((e) => toastError(e, "Could not save this device's name"));
  }
}

/** Keep the name this device goes by on the server (renamed on any device) for Settings → Connection. */
export async function rememberDeviceName(name: string) {
  const s = activeServer();
  if (!s || !name || s.deviceName === name) return;
  const cfg = await reloadConfig();
  await backend.saveConfig({ ...cfg, servers: cfg.servers.map((x) => (x.id === s.id ? { ...x, deviceName: name } : x)) });
  await reloadConfig();
}


export async function reloadConfig(): Promise<ClientConfig> {
  const cfg = await backend.getConfig();
  setState("config", cfg);
  return cfg;
}

export async function startApp() {
  if (started) return;
  started = true;
  // Back in front (screen on, app switched to, network back): let the shell
  // check the connection at once instead of waiting for a timeout.
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") appResumed();
  });
  window.addEventListener("focus", appResumed);
  window.addEventListener("online", appResumed);
  requestAnimationFrame(() => setTimeout(() => {
    void import("./notificationPresence").then((m) => m.watchNotifications()).catch((e) => toastError(e, "Could not start notifications"));
  }, 0));
  watchAlerts();
  // The notification center follows events once loaded, a moment after start (not on the first screen).
  void import("./notificationInbox").then((m) => m.watchInbox());
  watchFiles();
  backend.onEvent(applyEvent);
  backend.onStatus(onStatus);
  // The shell kept a server's addresses up with what it said (remote access set up or changed).
  backend.onConfigChanged?.(() => void reloadConfig());
  const cfg = await reloadConfig();
  const active = cfg.servers.find((s) => s.id === cfg.active);
  if (!active) {
    setPhase("onboarding");
    return;
  }
  selectServerLocal(active.id);
  setPhase("app");
  try {
    await backend.connect();
  } catch (e) {
    toastError(e, "Could not connect");
  }
}

/**
 * Pair with a server and connect. `stay` keeps the onboarding screen up
 * (it shows a "you're in" step) while the app connects in the background.
 */
export async function pairAndConnect(p: PairParams, opts: { stay?: boolean } = {}): Promise<SavedServer> {
  const saved = await backend.pair(p);
  const cfg = await reloadConfig();
  setState("everConnected", false);
  selectServerLocal(saved.id);
  if (!opts.stay) setPhase("app");
  await adoptConnection(saved.id);
  // The Tauri side adds the key fingerprint; prefer the stored copy.
  return cfg.servers.find((s) => s.id === saved.id) ?? saved;
}

/**
 * The desktop shell started (or found) a server on this computer, saved it
 * and connected — no pairing code needed. Adopt it and enter the app.
 */
export async function adoptLocalServer(id: string) {
  await reloadConfig();
  setState("everConnected", false);
  selectServerLocal(id);
  setPhase("app");
  await adoptConnection(id);
}

/**
 * Follow a connection the shell has just made (pairing, a local server)
 * rather than starting it over, which would cut the first sync short. The
 * browser opens its socket here.
 */
async function adoptConnection(id: string) {
  await backend.switchTo(id, { keep: true });
  // Synced before this screen caught up: sync again so the app opens.
  if (state.conn.state === "connected" && state.conn.serverId === id && !state.everConnected)
    bootstrap().catch((e) => toastError(e, "Could not sync with the server"));
}

export async function switchServer(id: string) {
  if (id === currentServer && state.conn.state === "connected") return;
  setState("everConnected", false);
  selectServerLocal(id);
  await backend.switchTo(id);
  await reloadConfig();
}

export async function retryNow() {
  try {
    if (backend.kind === "tauri" && currentServer) await backend.switchTo(currentServer);
    else await backend.connect();
  } catch (e) {
    toastError(e, "Retry failed");
  }
}

export async function saveServer(server: SavedServer) {
  const cfg = await reloadConfig();
  const servers = cfg.servers.map((s) => (s.id === server.id ? server : s));
  await backend.saveConfig({ ...cfg, servers });
  await reloadConfig();
  if (server.id === currentServer) await retryNow();
}

/** Remove a saved server (or forget this browser's pairing in same-origin mode). */
export async function removeServer(id: string) {
  const cfg = await reloadConfig();
  const servers = cfg.servers.filter((s) => s.id !== id);
  const active = cfg.active === id ? servers[0]?.id ?? null : cfg.active;
  await backend.saveConfig({ ...cfg, servers, active });
  await reloadConfig();
  if (id === currentServer) {
    if (active) await switchServer(active);
    else {
      await backend.disconnect().catch(() => {});
      resetInbox();
      resetAlerts();
      resetServerState();
      currentServer = null;
      setState("everConnected", false);
      setPhase("onboarding");
    }
  }
}

export const activeServer = () => state.config?.servers.find((s) => s.id === (state.conn.serverId ?? state.config?.active));

/** Pair from a scanned or pasted pairing link (the apps). Like `pairAndConnect`, but everything comes from the link. */
export async function pairLinkAndConnect(link: string, deviceName: string, opts: { stay?: boolean } = {}): Promise<SavedServer> {
  if (!backend.pairLink) throw new Error("Scanning a pairing code needs the Tessera app.");
  const saved = await backend.pairLink(link, deviceName);
  const cfg = await reloadConfig();
  setState("everConnected", false);
  selectServerLocal(saved.id);
  if (!opts.stay) setPhase("app");
  // The Rust side already connected; this only syncs the UI's status.
  await adoptConnection(saved.id);
  return { ...(cfg.servers.find((s) => s.id === saved.id) ?? saved), fingerprint: saved.fingerprint ?? cfg.servers.find((s) => s.id === saved.id)?.fingerprint };
}

/** Pair this device with a server again (new code, new key); replaces the old entry on success. */
export async function pairAgain(id: string | undefined = activeServer()?.id) {
  if (!id) return;
  if (backend.sameOrigin) {
    await removeServer(id);
    return;
  }
  setAddServerOpen({ repair: id });
}

/** Drop a saved server entry without touching the current connection (after re-pairing). */
export async function forgetServerEntry(id: string) {
  const cfg = await reloadConfig();
  if (!cfg.servers.some((s) => s.id === id)) return;
  await backend.saveConfig({ ...cfg, servers: cfg.servers.filter((s) => s.id !== id), active: cfg.active === id ? null : cfg.active });
  await reloadConfig();
}

/** LAN / loopback addresses (vs. a remote URL such as a tunnel). */
export function isLocalEndpoint(url: string | null | undefined): boolean {
  if (!url) return false;
  try {
    const h = new URL(url).hostname.replace(/^\[|\]$/g, "");
    return (
      h === "localhost" ||
      h.endsWith(".local") ||
      h.endsWith(".lan") ||
      h.endsWith(".home.arpa") ||
      /^127\./.test(h) ||
      /^10\./.test(h) ||
      /^192\.168\./.test(h) ||
      /^172\.(1[6-9]|2\d|3[01])\./.test(h) ||
      /^100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\./.test(h) || // CGNAT / Tailscale
      h === "::1" ||
      /^f[cd][0-9a-f]{2}:/i.test(h) ||
      /^fe80:/i.test(h)
    );
  } catch {
    return false;
  }
}
