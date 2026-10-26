// Terminals (engine/terminals.rs on the server): a terminal belongs to the
// device that opened it, and its output comes only to this connection
// (`terminal_output`, base64 bytes with their offset). A tile acknowledges
// what it drew (`terminal_ack`); the server reads no further than a window
// ahead, so a noisy program waits for the device instead of filling memory.
// After a reconnect every shown terminal attaches again and its kept output
// comes once more: a chunk that starts before what was drawn means "draw it
// all again" (see `TermView`).
//
// This module is the only place that knows the wire shapes.
import { createSignal } from "solid-js";
import type { ITheme } from "@xterm/xterm";
import type { Event } from "../proto/Event";
import type { TerminalInfo } from "../proto/TerminalInfo";
import type { TerminalSettings } from "../proto/TerminalSettings";
import type { TerminalStatus } from "../proto/TerminalStatus";
import { backend } from "./backend";
import { call, state } from "./store";

export type { TerminalInfo, TerminalStatus, TerminalSettings };
export type TermOutput = Extract<Event, { type: "terminal_output" }>;
export type TermExit = Extract<Event, { type: "terminal_exit" }>;

export interface TermHandlers {
  output: (seq: number, data: Uint8Array) => void;
  exit: (code: number | null, reason: string) => void;
  /** Back after a dropped connection: attach again. */
  reconnected: () => void;
}

const views = new Map<string, TermHandlers>();
/**
 * Output for a terminal no tile watches yet: a new terminal's first prompt
 * can come before the answer naming it. Kept briefly, then dropped.
 */
const early: { id: string; at: number; ev: TermOutput | TermExit }[] = [];

export const terminalStatus = () => call<TerminalStatus>({ method: "terminal_status" });
export const setTerminalSettings = (settings: TerminalSettings) => call<TerminalStatus>({ method: "set_terminal_settings", params: { settings } });

export const openTerminal = (machine: string | null, cols: number, rows: number) =>
  call<TerminalInfo>({ method: "terminal_open", params: { ...(machine ? { machine } : {}), cols, rows } });
export const attachTerminal = (id: string, cols: number, rows: number) => call<TerminalInfo>({ method: "terminal_attach", params: { id, cols, rows } });
export const detachTerminal = (id: string) => call<null>({ method: "terminal_detach", params: { id } }).catch(() => null);
export const closeTerminal = (id: string) => call<null>({ method: "terminal_close", params: { id } }).catch(() => null);
export const termInput = (id: string, data: string) => call<null>({ method: "terminal_input", params: { id, data } });
export const termResize = (id: string, cols: number, rows: number) => call<null>({ method: "terminal_resize", params: { id, cols, rows } }).catch(() => null);
export const termAck = (id: string, upto: number) => void call<null>({ method: "terminal_ack", params: { id, upto } }).catch(() => {});

/** A tile shows terminal `id`: its output and end come to `h`. */
export function watchTerminal(id: string, h: TermHandlers): () => void {
  views.set(id, h);
  for (let i = 0; i < early.length; ) {
    const e = early[i];
    if (e.id !== id) {
      i++;
      continue;
    }
    early.splice(i, 1);
    if (e.ev.type === "terminal_output") h.output(e.ev.seq, bytes(e.ev.data));
    else h.exit(e.ev.code ?? null, e.ev.reason);
  }
  return () => {
    if (views.get(id) === h) views.delete(id);
  };
}

function bytes(b64: string): Uint8Array {
  const s = atob(b64);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

function keepEarly(id: string, ev: TermOutput | TermExit) {
  const now = Date.now();
  while (early.length && (early.length >= 64 || now - early[0].at > 5000)) early.shift();
  early.push({ id, at: now, ev });
}

backend.onEvent((e) => {
  switch (e.type) {
    case "terminal_output": {
      const v = views.get(e.id);
      if (v) v.output(e.seq, bytes(e.data));
      else keepEarly(e.id, e);
      return;
    }
    case "terminal_exit": {
      const v = views.get(e.id);
      if (v) v.exit(e.code ?? null, e.reason);
      else keepEarly(e.id, e);
      return;
    }
  }
});

// Loaded late (with the first terminal tile): start from where things stand.
let wasConnected = state.conn.state === "connected";
backend.onStatus((s) => {
  const now = s.state === "connected";
  if (now && !wasConnected) for (const h of views.values()) h.reconnected();
  wasConnected = now;
});

/** What "full access" means before it is turned on (Settings → What commands can reach and the tile ask the same). */
export const fullAccessWarning = (sandboxOk: boolean) =>
  sandboxOk
    ? "A terminal could then read and change everything your account on this server can, including Tessera's own keys and saved passwords. Sandboxed keeps those out of reach, and works for almost everything else."
    : "The command sandbox isn't available on this server, so a terminal has everything your account here can reach, including Tessera's own keys and saved passwords. Turn it on only if you need it.";

/**
 * Typed or pasted text in pieces the server takes at once (64 KB each at
 * most; 16,384 UTF-16 units are at most 48 KB of UTF-8), never splitting a
 * character in two.
 */
export function inputPieces(data: string, max = 16384): string[] {
  if (data.length <= max) return [data];
  const out: string[] = [];
  for (let i = 0; i < data.length; ) {
    let end = Math.min(i + max, data.length);
    const last = data.charCodeAt(end - 1);
    if (end < data.length && last >= 0xd800 && last <= 0xdbff) end--;
    out.push(data.slice(i, end));
    i = end;
  }
  return out;
}

/** Send in order; a rejected piece stops the rest of that paste and warns once. */
export class TerminalInputQueue {
  private pending: string[] = [];
  private sending = false;
  private warned = false;
  constructor(private send: (data: string) => Promise<unknown>, private failed: (error: unknown) => void, private alive: () => boolean) {}
  input(data: string) {
    if (!this.alive()) return;
    this.pending.push(...inputPieces(data));
    void this.flush();
  }
  private async flush() {
    if (this.sending) return;
    this.sending = true;
    try {
      while (this.pending.length && this.alive()) {
        await this.send(this.pending.shift()!);
        this.warned = false;
      }
    } catch (e) {
      this.pending.length = 0;
      if (this.alive() && !this.warned) { this.warned = true; this.failed(e); }
    } finally {
      if (!this.alive()) this.pending.length = 0;
      this.sending = false;
    }
  }
}

/** The keys of the extra row on touch screens, as the terminal reads them. */
export const KEYS = {
  esc: "\x1b",
  tab: "\t",
  up: "\x1b[A",
  down: "\x1b[B",
  right: "\x1b[C",
  left: "\x1b[D",
} as const;

/** Ctrl with a key typed on a phone keyboard: the control character it makes. */
export function ctrlOf(ch: string): string | null {
  if (ch.length !== 1) return null;
  const c = ch.toUpperCase().charCodeAt(0);
  if (c >= 64 && c <= 95) return String.fromCharCode(c - 64);
  if (ch === " ") return "\x00";
  if (ch === "?") return "\x7f";
  return null;
}

/**
 * Keys a focused terminal leaves to the app: moving between tiles and
 * mosaics (Alt with an arrow or a digit, with or without Shift or Ctrl),
 * the shortcut sheet (Alt+/), a new chat (Alt+Enter) and the layout
 * (Alt+Space). Everything else goes to the program, as in a native
 * terminal: Ctrl+C, Ctrl+D, Ctrl+K, Ctrl+N, Alt+F, Alt+B…
 */
export function leftToApp(e: KeyboardEvent): boolean {
  if (!e.altKey || e.metaKey) return false;
  if (/^(Digit[1-9]|Numpad[1-9]|Arrow(Left|Right|Up|Down))$/.test(e.code)) return true;
  if (e.ctrlKey || e.shiftKey) return false;
  return e.code === "Slash" || e.key === "Enter" || e.code === "Space";
}

/** Links in a terminal open only to the web (never files, apps or scripts). */
export const openableLink = (uri: string) => /^https?:\/\/[^\s]+$/i.test(uri);

const FONT_KEY = "tessera.terminal.fontSize";
export const FONT_MIN = 8;
export const FONT_MAX = 28;

/** The text size terminals start at on this device: smaller on narrow phone screens so more fits. */
export function defaultFontSize(touch: boolean, width = window.innerWidth): number {
  if (!touch) return 14;
  return width < 420 ? 13 : 14;
}

/** The text size chosen on this device (Ctrl+= / Ctrl+- / Ctrl+0, or the ⋯ menu), if any. */
export function savedFontSize(): number | null {
  try {
    const v = Number(localStorage.getItem(FONT_KEY));
    return v >= FONT_MIN && v <= FONT_MAX ? v : null;
  } catch {
    return null;
  }
}

export function saveFontSize(size: number | null) {
  try {
    if (size === null) localStorage.removeItem(FONT_KEY);
    else localStorage.setItem(FONT_KEY, String(size));
  } catch {
    /* private mode: for this view only */
  }
}

/** The bundled terminal face, loaded by TermView only. Other scripts can still fall back to the system. */
export const TERM_FONT = '"Tessera Terminal", "DejaVu Sans Mono", Consolas, monospace';

export type TerminalTheme = "dark" | "light" | "match";
const THEME_KEY = "tessera.terminal.theme";
function savedTerminalTheme(): TerminalTheme {
  try {
    const v = localStorage.getItem(THEME_KEY);
    if (v === "light" || v === "match") return v;
  } catch { /* private mode */ }
  return "dark";
}
const [terminalTheme, updateTerminalTheme] = createSignal<TerminalTheme>(savedTerminalTheme());
export { terminalTheme };
export function setTerminalTheme(theme: TerminalTheme) {
  updateTerminalTheme(theme);
  try { localStorage.setItem(THEME_KEY, theme); } catch { /* this view only */ }
}

/** GNOME Terminal's Tango ANSI palette. Light keeps its hues with stronger contrast on white. */
export const TERMINAL_THEMES: Record<"dark" | "light", ITheme> = {
  dark: {
    background: "#0c0c0c", foreground: "#d3d7cf", cursor: "#d3d7cf", cursorAccent: "#0c0c0c",
    selectionBackground: "#3b5068", selectionForeground: "#ffffff", selectionInactiveBackground: "#343e4a",
    black: "#2e3436", red: "#cc0000", green: "#4e9a06", yellow: "#c4a000",
    blue: "#3465a4", magenta: "#75507b", cyan: "#06989a", white: "#d3d7cf",
    brightBlack: "#555753", brightRed: "#ef2929", brightGreen: "#8ae234", brightYellow: "#fce94f",
    brightBlue: "#729fcf", brightMagenta: "#ad7fa8", brightCyan: "#34e2e2", brightWhite: "#eeeeec",
  },
  light: {
    background: "#ffffff", foreground: "#2e3436", cursor: "#2e3436", cursorAccent: "#ffffff",
    selectionBackground: "#bdd4ef", selectionForeground: "#17293d", selectionInactiveBackground: "#dce3eb",
    black: "#2e3436", red: "#a40000", green: "#386b05", yellow: "#806600",
    blue: "#204a87", magenta: "#5c3566", cyan: "#006b70", white: "#555753",
    brightBlack: "#676b65", brightRed: "#c01c28", brightGreen: "#447d10", brightYellow: "#8a7000",
    brightBlue: "#3465a4", brightMagenta: "#864a86", brightCyan: "#087c80", brightWhite: "#777b75",
  },
};
