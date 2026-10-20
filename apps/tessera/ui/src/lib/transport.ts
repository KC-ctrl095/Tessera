// Transport layer: how the UI reaches a Tessera server.
//
// * TauriTransport — inside the desktop/Android app the Rust side
//   (`tessera-client`) owns connections, endpoint fail-over, custom headers and
//   reconnects. We only `invoke` commands and `listen` to events
//   (see "Tauri bridge" in docs/PROTOCOL.md).
// * WsTransport — plain browsers (`tessera serve`): a direct WebSocket to
//   `/api/ws?token=…`, request/response correlation by id, auto-reconnect with
//   backoff, pairing via `POST /api/pair`, token in localStorage.
// * WebTransport — a browser at `/web/` ("Use Tessera in a web browser"):
//   the same, end-to-end encrypted like the apps (see e2e.ts), through
//   `/web/api/pair/*` and `/web/api/secure`.

import type { Request } from "../proto/Request";
import type { Event } from "../proto/Event";
import type { ErrorCode } from "../proto/ErrorCode";
import type { PairResponse } from "../proto/PairResponse";
import type { PairRequest } from "../proto/PairRequest";
import type { ServerMessage } from "../proto/ServerMessage";
import { lsGet, lsSet, uid } from "./util";

export type ConnectionState = "disconnected" | "connecting" | "connected" | "offline";

export interface ConnectionStatus {
  state: ConnectionState;
  serverId: string | null;
  /** Which endpoint is in use. */
  endpoint: string | null;
  error: string | null;
  /** Client-side extension (WsTransport only): when the next retry happens. */
  retryAt?: number | null;
  /** Tauri: the session is end-to-end encrypted (Noise) on top of the transport. */
  encrypted?: boolean;
}

export interface SavedServer {
  id: string;
  name: string;
  /** Saved preference order; Rust races addresses and prefers the last successful login. */
  endpoints: string[];
  /** Proxy headers for remote HTTPS addresses, never plain HTTP or LAN hosts. */
  headers: Record<string, string>;
  /** Device token from pairing (never shown in the UI). */
  token: string;
  deviceName: string;
  /** Tauri: digest of the server key pinned at pairing, for display ("3f9a 1c2b …"). */
  fingerprint?: string;
  /** Tauri: always empty in the UI (kept on the Rust side); round-tripped as is. */
  serverKey?: string;
  /**
   * Tauri: how many times the server changed `endpoints`/`headers` (it says how devices reach it:
   * `get_server_route`). Round-tripped as is: a save carrying an older count doesn't undo that.
   */
  routeRev?: number;
}

export interface ClientConfig {
  servers: SavedServer[];
  active: string | null;
  /** Native saved-server snapshot revision. Preserve it on every save; a stale save is rejected, never silently retried. */
  revision?: number;
}

export interface PairParams {
  endpoints: string[];
  headers: Record<string, string>;
  code: string;
  deviceName: string;
  name: string;
}

export class RpcError extends Error {
  code: ErrorCode | "transport";
  constructor(code: ErrorCode | "transport", message: string) {
    super(message);
    this.name = "RpcError";
    this.code = code;
  }
  static from(e: unknown): RpcError {
    if (e instanceof RpcError) return e;
    if (e && typeof e === "object" && "code" in e && "message" in e) {
      const o = e as { code: ErrorCode; message: string };
      return new RpcError(o.code, String(o.message));
    }
    if (typeof e === "string") return new RpcError("transport", e);
    if (e instanceof Error) return new RpcError("transport", e.message);
    return new RpcError("transport", "Unknown error");
  }
}

export interface Transport {
  connect(): Promise<void>;
  call<T>(req: Request): Promise<T>;
  onEvent(cb: (e: Event) => void): () => void;
  onStatus(cb: (s: ConnectionStatus) => void): () => void;
}

/** Saved-server management. Implemented by both transports. */
export interface ServerRegistry {
  readonly kind: "tauri" | "browser";
  /** Browser served by `tessera serve`: one implicit server (this origin); only a pairing code is needed. */
  readonly sameOrigin: boolean;
  getConfig(): Promise<ClientConfig>;
  saveConfig(config: ClientConfig): Promise<void>;
  /** Pair with a server; the result is saved and becomes the active server. */
  pair(p: PairParams): Promise<SavedServer>;
  /**
   * Make `id` the active server and connect to it. `keep`: a connection the
   * shell already has to it (after pairing, or starting a local server) stays.
   */
  switchTo(id: string, opts?: { keep?: boolean }): Promise<void>;
  disconnect(): Promise<void>;
  /**
   * The saved servers changed outside the page (Tauri: the shell updated a server's addresses and
   * proxy headers as its server said, e.g. after remote access was set up). Read them again.
   */
  onConfigChanged?(cb: () => void): () => void;
  /** Optional reachability check (browser only; Tauri probes on the Rust side). */
  probe?(endpoint: string, headers: Record<string, string>): Promise<ProbeResult>;
  /**
   * Pair from a scanned or pasted `tessera://pair?…` link (Tauri only). The link holds
   * the addresses, proxy headers, code and key fingerprint; the Rust side
   * refuses to save anything if the server's key doesn't match it.
   */
  pairLink?(link: string, deviceName: string): Promise<SavedServer>;
}

export interface ProbeResult {
  ok: boolean;
  version?: string;
  error?: string;
}

/** What a `tessera://pair?…` link holds (crates/proto `PairLink`). */
export interface PairLinkInfo {
  name: string;
  urls: string[];
  headers: Record<string, string>;
  code: string;
  /** The server key's fingerprint, grouped in fours ("3f9a 1c2b …"). */
  fingerprint?: string;
}

/** Read a pairing link the way the Rust side does; a message when it isn't a usable one. */
export function parsePairLink(link: string): PairLinkInfo | string {
  const m = /^tessera:\/\/pair\?(.*)$/s.exec(link.trim());
  if (!m) return "That isn't a Tessera pairing link. It starts with tessera://pair?";
  const out: PairLinkInfo = { name: "", urls: [], headers: {}, code: "" };
  for (const [k, v] of new URLSearchParams(m[1])) {
    if (k === "v" && v !== "1") return "This pairing link needs a newer Tessera app.";
    if (k === "n") out.name = v;
    else if (k === "u") out.urls.push(v);
    else if (k === "h") {
      const i = v.indexOf(":");
      if (i < 1) return "The pairing link is incomplete (a header is cut off). Copy it again.";
      out.headers[v.slice(0, i).trim()] = v.slice(i + 1).trim();
    } else if (k === "c") out.code = v;
    else if (k === "f") out.fingerprint = (v.match(/[0-9a-f]/gi) ?? []).join("").replace(/(.{4})(?=.)/g, "$1 ");
  }
  if (!out.urls.length || !out.urls.every((u) => /^https?:\/\//.test(u))) return "The pairing link has no server address. Copy it again.";
  if (out.code.replace(/\D/g, "").length !== 8) return "The pairing link has no pairing code. Copy it again — all of it.";
  return out;
}

export type Backend = Transport & ServerRegistry;

class Emitter<T> {
  private subs = new Set<(v: T) => void>();
  on(cb: (v: T) => void): () => void {
    this.subs.add(cb);
    return () => this.subs.delete(cb);
  }
  emit(v: T) {
    for (const cb of [...this.subs]) {
      try {
        cb(v);
      } catch (err) {
        console.error(err);
      }
    }
  }
}

const joinUrl = (base: string, path: string) => base.replace(/\/+$/, "") + path;

// ─────────────────────────────────────────────────────────────── Tauri ────

type TauriInvoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

export class TauriTransport implements Backend {
  readonly kind = "tauri" as const;
  readonly sameOrigin = false;
  private events = new Emitter<Event>();
  private statuses = new Emitter<ConnectionStatus>();
  private configs = new Emitter<void>();
  private ready?: Promise<TauriInvoke>;

  private init(): Promise<TauriInvoke> {
    this.ready ??= (async () => {
      const [core, ev] = await Promise.all([import("@tauri-apps/api/core"), import("@tauri-apps/api/event")]);
      await ev.listen<Event>("tessera://event", (e) => this.events.emit(e.payload));
      await ev.listen<ConnectionStatus>("tessera://status", (e) => this.statuses.emit(e.payload));
      await ev.listen("tessera://servers", () => this.configs.emit());
      return core.invoke as TauriInvoke;
    })();
    return this.ready;
  }

  onConfigChanged(cb: () => void) {
    void this.init();
    return this.configs.on(cb);
  }

  private async invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    const invoke = await this.init();
    try {
      return await invoke<T>(cmd, args);
    } catch (e) {
      throw RpcError.from(e);
    }
  }

  async connect(): Promise<void> {
    const cfg = await this.getConfig();
    if (!cfg.active) {
      this.statuses.emit({ state: "disconnected", serverId: null, endpoint: null, error: null });
      return;
    }
    // The shell starts connecting before the WebView loads. Adopt that link
    // rather than cutting its first sync (or an already healthy link) short.
    await this.invoke("server_connect", { id: cfg.active, keep: true });
  }

  call<T>(request: Request): Promise<T> {
    return this.invoke<T>("rpc", { request });
  }

  onEvent(cb: (e: Event) => void) {
    void this.init();
    return this.events.on(cb);
  }

  onStatus(cb: (s: ConnectionStatus) => void) {
    void this.init();
    return this.statuses.on(cb);
  }

  getConfig(): Promise<ClientConfig> {
    return this.invoke<ClientConfig>("servers_get");
  }

  saveConfig(config: ClientConfig): Promise<void> {
    return this.invoke("servers_save", { config });
  }

  async pair(p: PairParams): Promise<SavedServer> {
    return this.invoke<SavedServer>("server_pair", {
      endpoints: p.endpoints,
      headers: p.headers,
      code: p.code,
      deviceName: p.deviceName,
      name: p.name,
    });
  }

  async pairLink(link: string, deviceName: string): Promise<SavedServer> {
    // `server_pair_link` saves the server, makes it active and connects.
    return this.invoke<SavedServer>("server_pair_link", { link, deviceName });
  }

  async switchTo(id: string, opts: { keep?: boolean } = {}): Promise<void> {
    // Kept, the shell reports the live connection's status itself.
    if (!opts.keep) this.statuses.emit({ state: "connecting", serverId: id, endpoint: null, error: null });
    await this.invoke("server_connect", { id, keep: !!opts.keep });
  }

  async disconnect(): Promise<void> {
    await this.invoke("server_disconnect");
  }
}

// ─────────────────────────────────────────────────────────── WebSocket ────

const LS_CONFIG = "tessera.clientConfig";
const LS_ORIGIN_TOKEN = "tessera.token";
const LS_ORIGIN_DEVICE = "tessera.deviceName";
export const ORIGIN_SERVER_ID = "origin";

interface Pending {
  resolve: (v: unknown) => void;
  reject: (e: RpcError) => void;
  timer: ReturnType<typeof setTimeout>;
}

export class WsTransport implements Backend {
  readonly kind = "browser" as const;
  readonly sameOrigin: boolean;
  private events = new Emitter<Event>();
  private statuses = new Emitter<ConnectionStatus>();
  protected status: ConnectionStatus = { state: "disconnected", serverId: null, endpoint: null, error: null };
  protected ws: WebSocket | null = null;
  private nextId = 1;
  private pending = new Map<number, Pending>();
  protected openWaiters = new Set<() => void>();
  protected attempt = 0;
  protected generation = 0;
  private retryTimer: ReturnType<typeof setTimeout> | undefined;
  private diagnosticAbort: AbortController | null = null;

  constructor(sameOrigin: boolean) {
    this.sameOrigin = sameOrigin;
    // Retry immediately when the network comes back or the app is foregrounded.
    window.addEventListener("online", () => this.kick());
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") this.kick();
    });
  }

  private kick() {
    if (this.status.state === "offline") void this.connect();
  }

  protected setStatus(s: ConnectionStatus) {
    this.status = s;
    this.statuses.emit(s);
  }

  onEvent(cb: (e: Event) => void) {
    return this.events.on(cb);
  }

  onStatus(cb: (s: ConnectionStatus) => void) {
    cb(this.status);
    return this.statuses.on(cb);
  }

  async getConfig(): Promise<ClientConfig> {
    if (this.sameOrigin) {
      const token = lsGet<string | null>(LS_ORIGIN_TOKEN, null);
      if (!token) return { servers: [], active: null };
      return {
        servers: [
          {
            id: ORIGIN_SERVER_ID,
            name: location.host || "This server",
            endpoints: [location.origin],
            headers: {},
            token,
            deviceName: lsGet(LS_ORIGIN_DEVICE, "Browser"),
          },
        ],
        active: ORIGIN_SERVER_ID,
      };
    }
    return lsGet<ClientConfig>(LS_CONFIG, { servers: [], active: null });
  }

  async saveConfig(config: ClientConfig): Promise<void> {
    if (this.sameOrigin) {
      const s = config.servers.find((x) => x.id === ORIGIN_SERVER_ID);
      lsSet(LS_ORIGIN_TOKEN, s?.token ?? undefined);
      if (s) lsSet(LS_ORIGIN_DEVICE, s.deviceName);
      return;
    }
    lsSet(LS_CONFIG, config);
  }

  async probe(endpoint: string, headers: Record<string, string>): Promise<ProbeResult> {
    const ctl = new AbortController();
    const t = setTimeout(() => ctl.abort(), 3500);
    try {
      const res = await fetch(joinUrl(endpoint, "/api/health"), {
        headers: Object.keys(headers).length ? headers : undefined,
        signal: ctl.signal,
        cache: "no-store",
      });
      if (!res.ok) return { ok: false, error: `HTTP ${res.status}` };
      const body = (await res.json()) as { name?: string; version?: string; protocol?: number };
      if (body.name !== "tessera") return { ok: false, error: "Not a Tessera server" };
      return { ok: true, version: body.version };
    } catch (e) {
      // fetch() only says "Failed to fetch" for DNS/refused/CORS/mixed-content.
      return { ok: false, error: ctl.signal.aborted ? "Timed out" : e instanceof TypeError ? "Unreachable" : e instanceof Error ? e.message : "Unreachable" };
    } finally {
      clearTimeout(t);
    }
  }

  async pair(p: PairParams): Promise<SavedServer> {
    const endpoints = this.sameOrigin ? [location.origin] : p.endpoints;
    let lastError = "No endpoints given";
    for (const ep of endpoints) {
      let res: Response;
      try {
        const body: PairRequest = { code: p.code.trim(), device_name: p.deviceName };
        res = await fetch(joinUrl(ep, "/api/pair"), {
          method: "POST",
          headers: { "content-type": "application/json", ...p.headers },
          body: JSON.stringify(body),
        });
      } catch (e) {
        lastError = `${ep}: ${e instanceof Error ? e.message : "unreachable"}`;
        continue;
      }
      if (!res.ok) {
        let message = `HTTP ${res.status}`;
        try {
          const err = (await res.json()) as { message?: string; error?: { message?: string } | string };
          message = (typeof err.error === "object" ? err.error?.message : typeof err.error === "string" ? err.error : undefined) ?? err.message ?? message;
        } catch {
          /* not JSON */
        }
        if (res.status === 401 || res.status === 403 || res.status === 400 || res.status === 429) {
          throw new RpcError(res.status === 429 ? "busy" : "unauthorized", message);
        }
        lastError = `${ep}: ${message}`;
        continue;
      }
      const data = (await res.json()) as PairResponse;
      const saved: SavedServer = {
        id: this.sameOrigin ? ORIGIN_SERVER_ID : uid("srv_"),
        name: p.name.trim() || data.server.name || new URL(ep).host,
        endpoints,
        headers: p.headers,
        token: data.token,
        deviceName: p.deviceName,
      };
      const cfg = await this.getConfig();
      await this.saveConfig({ ...cfg, servers: [...cfg.servers.filter((s) => s.id !== saved.id), saved], active: saved.id });
      return saved;
    }
    throw new RpcError("transport", `Could not reach the server (${lastError})`);
  }

  async switchTo(id: string): Promise<void> {
    const cfg = await this.getConfig();
    if (cfg.active !== id) await this.saveConfig({ ...cfg, active: id });
    await this.connect();
  }

  async disconnect(): Promise<void> {
    this.generation++;
    clearTimeout(this.retryTimer);
    this.closeSocket("Disconnected");
    this.setStatus({ state: "disconnected", serverId: null, endpoint: null, error: null });
  }

  /** (Re)connect to the active server now. Safe to call repeatedly (acts as "retry now"). */
  async connect(): Promise<void> {
    const gen = ++this.generation;
    clearTimeout(this.retryTimer);
    this.closeSocket("Reconnecting");
    const cfg = await this.getConfig();
    const server = cfg.servers.find((s) => s.id === cfg.active);
    if (!server) {
      this.setStatus({ state: "disconnected", serverId: null, endpoint: null, error: null });
      return;
    }
    void this.loop(gen, server);
  }

  protected closeSocket(reason: string) {
    this.diagnosticAbort?.abort();
    const ws = this.ws;
    this.ws = null;
    if (ws) {
      ws.onclose = null;
      ws.onmessage = null;
      try {
        ws.close(1000, reason);
      } catch {
        /* ignore */
      }
    }
    this.failPending("Connection closed");
  }

  protected async loop(gen: number, server: SavedServer) {
    this.diagnosticAbort?.abort();
    this.setStatus({
      state: "connecting",
      serverId: server.id,
      endpoint: null,
      error: this.attempt > 0 ? this.status.error : null,
    });
    let lastError = "Unreachable";
    let diagnosticEndpoint: string | null = null;
    for (const ep of server.endpoints) {
      if (gen !== this.generation) return;
      if (!this.sameOrigin) {
        const p = await this.probe(ep, server.headers);
        if (gen !== this.generation) return;
        if (!p.ok) {
          lastError = `${ep} — ${p.error ?? "unreachable"}`;
          diagnosticEndpoint = null;
          continue;
        }
      }
      if (await this.openSocket(gen, server, ep)) return;
      if (gen !== this.generation) return;
      lastError = `${ep} — WebSocket failed (token revoked or proxy blocking upgrades?)`;
      diagnosticEndpoint = ep;
    }
    if (gen !== this.generation) return;
    // Retry scheduling is never held up by optional diagnostics, including
    // an HTTP reply whose JSON body never finishes.
    this.scheduleRetry(gen, server, lastError);
    const attempt = this.attempt;
    if (diagnosticEndpoint) void this.diagnoseWsFailure(diagnosticEndpoint, server.headers).then((error) => {
      if (error && gen === this.generation && attempt === this.attempt && this.status.state === "offline") {
        this.setStatus({ ...this.status, error });
      }
    });
  }

  protected async diagnoseWsFailure(ep: string, headers: Record<string, string>): Promise<string | null> {
    this.diagnosticAbort?.abort();
    const ctl = new AbortController();
    this.diagnosticAbort = ctl;
    const timeout = setTimeout(() => ctl.abort(), 2000);
    const aborted = new Promise<null>((resolve) => ctl.signal.addEventListener("abort", () => resolve(null), { once: true }));
    try {
      return await Promise.race([this.explainWsFailure(ep, headers, ctl.signal).catch(() => null), aborted]);
    } finally {
      clearTimeout(timeout);
      if (this.diagnosticAbort === ctl) this.diagnosticAbort = null;
    }
  }

  protected openSocket(gen: number, server: SavedServer, ep: string): Promise<boolean> {
    return new Promise((resolve) => {
      const url = joinUrl(ep, "/api/ws").replace(/^http/i, "ws") + `?token=${encodeURIComponent(server.token)}`;
      let ws: WebSocket;
      try {
        ws = new WebSocket(url);
      } catch {
        resolve(false);
        return;
      }
      let opened = false;
      const timeout = setTimeout(() => {
        if (!opened) {
          ws.onclose = null;
          ws.close();
          resolve(false);
        }
      }, 8000);
      ws.onopen = () => {
        clearTimeout(timeout);
        if (gen !== this.generation) {
          ws.close();
          resolve(false);
          return;
        }
        opened = true;
        this.ws = ws;
        this.attempt = 0;
        this.setStatus({ state: "connected", serverId: server.id, endpoint: ep, error: null, retryAt: null });
        for (const w of [...this.openWaiters]) w();
        this.openWaiters.clear();
        resolve(true);
      };
      ws.onmessage = (e) => this.onMessage(e.data);
      ws.onclose = (ev) => {
        clearTimeout(timeout);
        if (!opened) {
          resolve(false);
          return;
        }
        if (this.ws === ws) {
          this.ws = null;
          this.failPending("Connection lost");
          if (gen === this.generation) this.scheduleRetry(gen, server, ev.reason || "Connection lost", true);
        }
      };
    });
  }

  /** A failed upgrade has no readable body; ask over plain HTTP why (e.g. a 403 with an explanation). */
  protected async explainWsFailure(ep: string, headers: Record<string, string>, signal: AbortSignal): Promise<string | null> {
    try {
      const res = await fetch(joinUrl(ep, "/api/ws"), { headers: Object.keys(headers).length ? headers : undefined, cache: "no-store", signal });
      if (res.status !== 401 && res.status !== 403) return null;
      const body = (await res.json().catch(() => null)) as { message?: string; error?: { message?: string } } | null;
      return body?.error?.message ?? body?.message ?? null;
    } catch {
      return null;
    }
  }

  protected scheduleRetry(gen: number, server: SavedServer, error: string, immediate = false) {
    this.attempt++;
    const base = immediate && this.attempt === 1 ? 300 : Math.min(15_000, 700 * 2 ** (this.attempt - 1));
    const delay = Math.round(base * (0.85 + Math.random() * 0.3));
    this.setStatus({ state: "offline", serverId: server.id, endpoint: null, error, retryAt: Date.now() + delay });
    clearTimeout(this.retryTimer);
    this.retryTimer = setTimeout(() => {
      if (gen === this.generation) void this.loop(gen, server);
    }, delay);
  }

  protected failPending(message: string) {
    for (const [, p] of this.pending) {
      clearTimeout(p.timer);
      p.reject(new RpcError("transport", message));
    }
    this.pending.clear();
  }

  protected onMessage(data: unknown) {
    if (typeof data !== "string") return;
    let msg: ServerMessage;
    try {
      msg = JSON.parse(data) as ServerMessage;
    } catch {
      return;
    }
    if ("event" in msg) {
      this.events.emit(msg.event);
      return;
    }
    const p = this.pending.get(msg.id);
    if (!p) return;
    this.pending.delete(msg.id);
    clearTimeout(p.timer);
    if ("error" in msg) p.reject(new RpcError(msg.error.code, msg.error.message));
    else p.resolve(msg.result);
  }

  private waitOpen(ms: number): Promise<void> {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) return Promise.resolve();
    return new Promise((resolve, reject) => {
      const done = () => {
        clearTimeout(t);
        resolve();
      };
      const t = setTimeout(() => {
        this.openWaiters.delete(done);
        reject(new RpcError("transport", "Not connected"));
      }, ms);
      this.openWaiters.add(done);
    });
  }

  async call<T>(request: Request): Promise<T> {
    await this.waitOpen(20_000);
    const ws = this.ws!;
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new RpcError("transport", "Request timed out"));
      }, 120_000);
      this.pending.set(id, { resolve: resolve as (v: unknown) => void, reject, timer });
      this.send(ws, JSON.stringify({ id, ...request }));
    });
  }

  /** One message to the server on `ws`. */
  protected send(ws: WebSocket, text: string) {
    ws.send(text);
  }
}

// ──────────────────────────────────────────────── Web browser (`/web/`) ────

const LS_WEB = "tessera.web";
/** Where "Use Tessera in a web browser" serves the app and its API. */
const WEB = "/web";

/** What a browser keeps of its pairing (its key is in IndexedDB, unreadable; see e2e.ts). */
interface WebPairing {
  token: string;
  deviceId: string;
  deviceName: string;
  /** The server's long-term key (base64), pinned at pairing. */
  serverKey: string;
  name: string;
}

type E2e = typeof import("./e2e");
let e2eModule: Promise<E2e> | undefined;
const e2e = () => (e2eModule ??= import("./e2e"));

async function postJson<T>(path: string, body: unknown): Promise<T> {
  let res: Response;
  try {
    res = await fetch(location.origin + WEB + path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body), cache: "no-store" });
  } catch {
    throw new RpcError("transport", "Couldn't reach the server. Check your connection and try again.");
  }
  if (res.ok) return (await res.json()) as T;
  if (res.status === 404) throw new RpcError("unauthorized", `${WEB_OFF} Turn it on in Settings → Connection on one of your devices.`);
  const err = (await res.json().catch(() => null)) as { error?: { message?: string } } | null;
  const message = err?.error?.message ?? `HTTP ${res.status}`;
  throw new RpcError(res.status === 429 ? "busy" : res.status === 400 ? "bad_request" : "unauthorized", message);
}

const WEB_OFF = "Web browser access is turned off on this server.";

/**
 * A browser at `/web/` ("Use Tessera in a web browser"): pairs with a code
 * like the apps, keeps a key no script can read out, and talks to the
 * server end-to-end encrypted. Requests, reconnects and statuses work as in
 * {@link WsTransport}; only the socket underneath is sealed.
 */
export class WebTransport extends WsTransport {
  private channel: import("./e2e").Channel | null = null;
  /** Why the last connection attempt failed, when the server or the handshake said (shown while offline). */
  private failure: string | null = null;

  constructor() {
    super(true);
  }

  private saved(): WebPairing | null {
    return lsGet<WebPairing | null>(LS_WEB, null);
  }

  override async getConfig(): Promise<ClientConfig> {
    const w = this.saved();
    if (!w) return { servers: [], active: null };
    const fp = (await e2e()).fingerprint((await e2e()).unb64(w.serverKey));
    return {
      servers: [{ id: ORIGIN_SERVER_ID, name: w.name || location.host, endpoints: [location.origin], headers: {}, token: "", deviceName: w.deviceName, fingerprint: fp }],
      active: ORIGIN_SERVER_ID,
    };
  }

  /** Only forgetting is saved here: "Unpair this browser" signs it out on the server too and drops its key. */
  override async saveConfig(config: ClientConfig): Promise<void> {
    const w = this.saved();
    if (!w) return;
    const kept = config.servers.find((s) => s.id === ORIGIN_SERVER_ID);
    if (kept) {
      lsSet(LS_WEB, { ...w, deviceName: kept.deviceName || w.deviceName, name: kept.name || w.name });
      return;
    }
    if (this.ws) await this.call({ method: "revoke_device", params: { id: w.deviceId } } as Request).catch(() => undefined);
    lsSet(LS_WEB, undefined);
    await (await e2e()).dropDeviceKey();
  }

  override async probe(): Promise<ProbeResult> {
    return { ok: true };
  }

  override async pair(p: PairParams): Promise<SavedServer> {
    const lib = await e2e();
    const key = await lib.newDeviceKey();
    const pake = lib.pairStart(p.code);
    const started = await postJson<{ pairing: string; pake: string }>("/api/pair/start", { pake: lib.b64(pake.message) });
    const keys = pake.finish(lib.unb64(started.pake));
    const hello = { device_name: p.deviceName, device_key: lib.b64(key.publicKey) };
    const finished = await postJson<{ sealed: string }>("/api/pair/finish", {
      pairing: started.pairing,
      sealed: lib.b64(keys.sealToServer(new TextEncoder().encode(JSON.stringify(hello)))),
    });
    const grant = JSON.parse(new TextDecoder().decode(keys.openFromServer(lib.unb64(finished.sealed)))) as {
      token: string;
      device_id: string;
      server_key: string;
      server: { name: string };
    };
    await key.keep();
    lsSet(LS_WEB, { token: grant.token, deviceId: grant.device_id, deviceName: p.deviceName, serverKey: grant.server_key, name: grant.server.name } satisfies WebPairing);
    const cfg = await this.getConfig();
    return cfg.servers[0];
  }

  protected override send(ws: WebSocket, text: string) {
    const ch = this.channel;
    if (!ch) throw new RpcError("transport", "Not connected");
    for (const f of ch.sealer.seal(new TextEncoder().encode(text))) ws.send(f);
  }

  protected override closeSocket(reason: string) {
    this.channel = null;
    super.closeSocket(reason);
  }

  protected override async explainWsFailure(_ep: string, _headers: Record<string, string>, signal: AbortSignal): Promise<string | null> {
    const failure = this.failure;
    this.failure = null;
    try {
      const res = await fetch(location.origin + WEB + "/api/health", { cache: "no-store", signal });
      if (res.status === 404) return WEB_OFF;
    } catch {
      /* offline */
    }
    return failure;
  }

  /** The handshake, then this browser's login with its proof; connected once the server says yes. */
  protected override openSocket(gen: number, server: SavedServer, ep: string): Promise<boolean> {
    return new Promise((resolve) => {
      const w = this.saved();
      if (!w) return resolve(false);
      let ws: WebSocket;
      try {
        ws = new WebSocket(ep.replace(/^http/i, "ws") + WEB + "/api/secure");
      } catch {
        return resolve(false);
      }
      ws.binaryType = "arraybuffer";
      let done = false;
      const finish = (ok: boolean) => {
        if (done) return;
        done = true;
        clearTimeout(timeout);
        if (!ok) {
          ws.onclose = null;
          ws.onmessage = null;
          try {
            ws.close();
          } catch {
            /* closed */
          }
        }
        resolve(ok);
      };
      const timeout = setTimeout(() => finish(false), 15_000);
      let step: "handshake" | "login" = "handshake";
      let hs: ReturnType<E2e["handshake"]> | null = null;
      let channel: import("./e2e").Channel | null = null;
      ws.onopen = async () => {
        try {
          hs = (await e2e()).handshake((await e2e()).unb64(w.serverKey));
          ws.send(hs.message);
        } catch {
          finish(false);
        }
      };
      ws.onmessage = async (ev) => {
        if (!(ev.data instanceof ArrayBuffer)) return finish(false);
        const frame = new Uint8Array(ev.data);
        try {
          if (step === "handshake" && hs) {
            step = "login";
            channel = hs.finish(frame);
            const proof = await (await e2e()).prove(channel.handshakeHash);
            for (const f of channel.sealer.seal(new TextEncoder().encode(JSON.stringify({ token: w.token, proof })))) ws.send(f);
            return;
          }
          if (!channel) return finish(false);
          const reply = channel.opener.open(frame);
          if (!reply) return;
          const answer = JSON.parse(new TextDecoder().decode(reply)) as { ok: boolean; error?: { message?: string } };
          if (!answer.ok) {
            // Switched off on the server meanwhile: still paired, tried again later.
            if (/turned off/i.test(answer.error?.message ?? "")) {
              this.failure = WEB_OFF;
              return finish(false);
            }
            // Signed out (in Devices, or its pairing is gone on the server):
            // forget it and start again at pairing.
            this.generation++;
            finish(false);
            lsSet(LS_WEB, undefined);
            await (await e2e()).dropDeviceKey();
            location.reload();
            return;
          }
          if (gen !== this.generation) return finish(false);
          const ch = channel;
          this.channel = ch;
          this.ws = ws;
          this.attempt = 0;
          ws.onmessage = (e) => {
            if (!(e.data instanceof ArrayBuffer)) return;
            try {
              const m = ch.opener.open(new Uint8Array(e.data));
              if (m) this.onMessage(new TextDecoder().decode(m));
            } catch {
              // Tampered with or out of order: start again.
              ws.close();
            }
          };
          ws.onclose = (ev) => {
            if (this.ws === ws) {
              this.ws = null;
              this.channel = null;
              this.failPending("Connection lost");
              if (gen === this.generation) this.scheduleRetry(gen, server, ev.reason || "Connection lost", true);
            }
          };
          this.setStatus({ state: "connected", serverId: server.id, endpoint: ep, error: null, retryAt: null, encrypted: true });
          for (const wake of [...this.openWaiters]) wake();
          this.openWaiters.clear();
          finish(true);
        } catch (err) {
          // The handshake failed (e.g. the server's key changed): said as it is.
          if (step === "login" && !channel && err instanceof Error) this.failure = err.message;
          finish(false);
        }
      };
      ws.onclose = () => finish(false);
    });
  }
}
