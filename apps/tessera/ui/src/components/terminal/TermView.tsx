// The terminal itself (xterm.js), loaded only when a terminal tile opens.
// It opens (or shows again) the terminal on the server, draws its output in
// order and says what it drew (`terminal_ack`: the server reads no further
// than a window ahead), and sends keystrokes, pastes and its size. A chunk
// that starts anywhere but where drawing stopped is the kept output coming
// again after a reconnect: the screen is drawn afresh from it.
//
// It behaves like a native terminal: the system's monospace font (the app's
// own has no box or block characters), at its natural line height so those
// join up, Unicode 11 widths (emoji, CJK), 256 and true colour, 10,000 lines
// to scroll back, find, text size. Selecting copies; right-click, Ctrl+Shift+V or Shift+Insert
// pastes; Ctrl+click opens a web link. Every key goes to the program, except
// the few the app uses to move between tiles (`leftToApp`).
import "@xterm/xterm/css/xterm.css";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Terminal, type ITheme } from "@xterm/xterm";
import { createEffect, onCleanup, onMount } from "solid-js";
import { resolvedTheme } from "../../lib/prefs";
import {
  attachTerminal,
  closeTerminal,
  defaultFontSize,
  detachTerminal,
  FONT_MAX,
  FONT_MIN,
  TerminalInputQueue,
  leftToApp,
  openableLink,
  openTerminal,
  saveFontSize,
  savedFontSize,
  TERM_FONT,
  termAck,
  termInput,
  termResize,
  watchTerminal,
  type TerminalInfo,
} from "../../lib/terminal";
import { confirmDialog, promptDialog, toast, toastError } from "../../lib/ui";
import { copyText, openExternal } from "../../lib/util";

export interface TermViewHandle {
  copy: () => string;
  paste: (text: string) => void;
  /** Paste what is on the clipboard (asks the system for it; says how otherwise). */
  pasteClipboard: () => Promise<void>;
  selectAll: () => void;
  clear: () => void;
  focus: () => void;
  /** Keys from the extra row (already as the terminal reads them). */
  send: (data: string) => void;
  hasSelection: () => boolean;
  /** Find `text` in the scrollback, forwards (newer) or backwards (older); whether it was found. */
  find: (text: string, back: boolean) => boolean;
  findDone: () => void;
  /** Text size: bigger (+1), smaller (-1), or this device's default (0). */
  zoom: (step: -1 | 0 | 1) => void;
}

export interface TermViewProps {
  /** Show this one again; without it a new one opens on `machine`. */
  termId?: string;
  machine: string | null;
  touch: boolean;
  /** Turn the next key typed into Ctrl or Alt with it (the extra row's sticky keys); returns what to send. */
  modify?: (data: string) => string;
  onOpened: (info: TerminalInfo) => void;
  onExit: (code: number | null, reason: string) => void;
  onError: (message: string) => void;
  handle: (h: TermViewHandle) => void;
  /** Ctrl+Shift+F: open the tile's find bar. */
  onFind?: () => void;
}

/** Acknowledge after this much was drawn, or this long after the last chunk. */
const ACK_BYTES = 32 << 10;
const ACK_MS = 60;

const DARK: ITheme = {
  black: "#2b2724",
  red: "#e4826a",
  green: "#8fc48a",
  yellow: "#e2c172",
  blue: "#86a8e0",
  magenta: "#c79ad2",
  cyan: "#7cc5c0",
  white: "#d9d2c6",
  brightBlack: "#7a7268",
  brightRed: "#f19a83",
  brightGreen: "#a8d8a2",
  brightYellow: "#f0d38f",
  brightBlue: "#a2bff0",
  brightMagenta: "#dcb2e5",
  brightCyan: "#98dad5",
  brightWhite: "#f4efe7",
};

const LIGHT: ITheme = {
  black: "#2b2724",
  red: "#b23a24",
  green: "#2f7a32",
  yellow: "#8a6410",
  blue: "#2f56a8",
  magenta: "#86407e",
  cyan: "#1d7a74",
  white: "#6f685f",
  brightBlack: "#5b544b",
  brightRed: "#c94a30",
  brightGreen: "#3a8f3d",
  brightYellow: "#9c7424",
  brightBlue: "#3d5ca8",
  brightMagenta: "#9a4c90",
  brightCyan: "#25877f",
  brightWhite: "#2b2724",
};

function themeOf(el: HTMLElement, light: boolean): ITheme {
  const css = getComputedStyle(el);
  const v = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback;
  return {
    ...(light ? LIGHT : DARK),
    background: v("--pane-bg", light ? "#f8f5ef" : "#1d1b19"),
    foreground: v("--text", light ? "#211e1a" : "#ece6dc"),
    cursor: v("--accent", "#d4af5f"),
    cursorAccent: v("--pane-bg", "#1d1b19"),
    selectionBackground: light ? "rgba(156, 116, 36, 0.28)" : "rgba(212, 175, 95, 0.32)",
  };
}

export default function TermView(props: TermViewProps) {
  let host!: HTMLDivElement;
  let id: string | undefined = props.termId;
  let ended = false;
  let disposed = false;
  /** Where drawing stopped (the offset of the next byte). */
  let next = 0;
  let drawn = 0;
  let acked = 0;
  let ackTimer: ReturnType<typeof setTimeout> | undefined;
  let resizeTimer: ReturnType<typeof setTimeout> | undefined;
  let unwatch: (() => void) | undefined;

  const term = new Terminal({
    cursorBlink: true,
    fontFamily: TERM_FONT,
    fontSize: savedFontSize() ?? defaultFontSize(props.touch),
    lineHeight: 1,
    scrollback: 10_000,
    // For Unicode 11 character widths (the `unicode` interface is still marked experimental).
    allowProposedApi: true,
    // Links open only through the web links handler below (http and https, with Ctrl or a tap).
    linkHandler: null,
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  const search = new SearchAddon();
  term.loadAddon(search);
  const unicode = new Unicode11Addon();
  term.loadAddon(unicode);
  term.unicode.activeVersion = "11";

  /** A web link from the program's output: Ctrl/⌘+click opens it on a computer; on a phone a tap asks first. */
  const openLink = async (e: MouseEvent, uri: string) => {
    if (!openableLink(uri)) return;
    if (props.touch) {
      const ok = await confirmDialog({ title: "Open this link?", body: uri, confirmLabel: "Open" });
      if (ok) void openExternal(uri);
      return;
    }
    if (!(e.ctrlKey || e.metaKey)) return;
    e.preventDefault();
    void openExternal(uri);
  };
  term.loadAddon(
    new WebLinksAddon((e, uri) => void openLink(e, uri), {
      hover: (_e, uri) => {
        if (!props.touch && openableLink(uri)) host.title = `${navigator.platform.startsWith("Mac") ? "⌘" : "Ctrl"}+click to open ${uri}`;
      },
      leave: () => host.removeAttribute("title"),
    }),
  );

  const ack = (force = false) => {
    if (!id || drawn <= acked) return;
    if (!force && drawn - acked < ACK_BYTES) {
      clearTimeout(ackTimer);
      ackTimer = setTimeout(() => ack(true), ACK_MS);
      return;
    }
    clearTimeout(ackTimer);
    acked = drawn;
    termAck(id, drawn);
  };

  const draw = (seq: number, data: Uint8Array) => {
    if (seq !== next) {
      // The kept output again (after a reconnect), or a jump past what was
      // dropped while nobody showed it: draw the screen afresh.
      term.reset();
      drawn = acked = seq;
    }
    next = seq + data.length;
    const end = next;
    term.write(data, () => {
      if (end > drawn) drawn = end;
      ack();
    });
  };

  const handlers = {
    output: (seq: number, data: Uint8Array) => draw(seq, data),
    exit: (code: number | null, reason: string) => {
      ended = true;
      // What it showed stays readable; the cursor goes and nothing more is typed.
      term.options.disableStdin = true;
      term.options.cursorBlink = false;
      term.write("\x1b[?25l");
      props.onExit(code, reason);
    },
    reconnected: () => void attach(),
  };

  /** Keystrokes and pastes, in order; a long paste goes in pieces the server takes. */
  const input = new TerminalInputQueue(
    (piece) => termInput(id!, piece),
    (error) => toastError(error, "Terminal input wasn't sent; the remaining paste was stopped"),
    () => !!id && !ended && !disposed,
  );
  const typeIn = (data: string) => input.input(data);

  const size = () => ({ cols: Math.max(2, term.cols), rows: Math.max(1, term.rows) });

  /** The size the terminal last heard of. */
  let told = { cols: 0, rows: 0 };
  /** Its size may have changed while the answer was on its way (the first fit, a rotation). */
  const tellSize = () => {
    const s = size();
    if (id && !ended && (s.cols !== told.cols || s.rows !== told.rows)) {
      told = s;
      void termResize(id, s.cols, s.rows);
    }
  };

  const refit = () => {
    if (!host.offsetWidth || !host.offsetHeight) return;
    try {
      fit.fit();
    } catch {
      /* hidden */
    }
  };

  async function attach() {
    if (!id || ended || disposed) return;
    try {
      told = size();
      const info = await attachTerminal(id, told.cols, told.rows);
      if (disposed) {
        void detachTerminal(info.id);
        return;
      }
      tellSize();
      props.onOpened(info);
    } catch (e) {
      if (disposed) return;
      ended = true;
      props.onExit(null, String((e as { message?: string })?.message ?? e).replace(/^that terminal ended: /, ""));
    }
  }

  async function open() {
    try {
      told = size();
      const info = await openTerminal(props.machine, told.cols, told.rows);
      if (disposed) {
        void closeTerminal(info.id);
        return;
      }
      id = info.id;
      tellSize();
      props.onOpened(info);
      // Output that came before the answer is handed over here.
      unwatch = watchTerminal(id, handlers);
    } catch (e) {
      if (disposed) return;
      ended = true;
      props.onError(String((e as { message?: string })?.message ?? e));
    }
  }

  const pasteClipboard = async () => {
    let text: string | null = null;
    try {
      text = await navigator.clipboard.readText();
    } catch {
      // Phones' web views rarely let a page read the clipboard: a field takes the system's own Paste instead.
      if (props.touch) {
        text = await promptDialog({
          title: "Paste into the terminal",
          body: "Touch and hold in the field, choose Paste, then Paste here.",
          label: "Text",
          confirmLabel: "Paste",
        });
      } else {
        toast({ level: "info", message: "Press Ctrl+Shift+V to paste" });
      }
    }
    if (text) term.paste(text);
    term.focus();
  };

  const zoom = (step: -1 | 0 | 1) => {
    const now = term.options.fontSize ?? 13;
    const size = step === 0 ? defaultFontSize(props.touch) : Math.min(FONT_MAX, Math.max(FONT_MIN, Math.round(now) + step));
    saveFontSize(step === 0 ? null : size);
    term.options.fontSize = size;
    refit();
  };

  onMount(() => {
    term.open(host);
    refit();
    term.onData((data) => {
      if (!id || ended) return;
      typeIn(props.modify ? props.modify(data) : data);
    });
    term.onResize(() => {
      clearTimeout(resizeTimer);
      resizeTimer = setTimeout(tellSize, 80);
    });
    // Selecting copies, as in a native terminal, once the selection settles. Only the
    // system clipboard is used here: the fallback would take the keyboard from the terminal.
    let selTimer: ReturnType<typeof setTimeout> | undefined;
    term.onSelectionChange(() => {
      clearTimeout(selTimer);
      if (props.touch) return;
      selTimer = setTimeout(() => {
        const sel = term.getSelection();
        if (sel) void navigator.clipboard?.writeText(sel).catch(() => {});
      }, 120);
    });
    onCleanup(() => clearTimeout(selTimer));
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== "keydown") return true;
      // The app's own keys for moving between tiles pass through.
      if (leftToApp(e)) return false;
      const key = e.key.toLowerCase();
      const mod = e.ctrlKey || e.metaKey;
      if ((e.ctrlKey && e.shiftKey && key === "c") || (e.metaKey && !e.shiftKey && key === "c")) {
        const sel = term.getSelection();
        if (sel) void copyText(sel);
        e.preventDefault();
        return false;
      }
      // The browser's own paste follows (xterm.js takes it from there).
      if ((e.ctrlKey && e.shiftKey && key === "v") || (e.metaKey && !e.shiftKey && key === "v") || (e.shiftKey && !mod && e.key === "Insert")) return false;
      if (mod && e.shiftKey && key === "f") {
        e.preventDefault();
        props.onFind?.();
        return false;
      }
      // Text size, as terminals do it: Ctrl+= / Ctrl++ bigger, Ctrl+- smaller, Ctrl+0 back to the default.
      if (mod && !e.altKey && (key === "=" || key === "+" || key === "-" || key === "0")) {
        e.preventDefault();
        zoom(key === "-" ? -1 : key === "0" ? 0 : 1);
        return false;
      }
      return true;
    });
    // Right-click pastes (what you selected is on the clipboard already).
    const onContext = (e: MouseEvent) => {
      if (props.touch) return;
      e.preventDefault();
      void pasteClipboard();
    };
    host.addEventListener("contextmenu", onContext);
    const ro = new ResizeObserver(refit);
    ro.observe(host);
    onCleanup(() => {
      ro.disconnect();
      host.removeEventListener("contextmenu", onContext);
    });
    props.handle({
      copy: () => term.getSelection(),
      paste: (text) => term.paste(text),
      pasteClipboard,
      selectAll: () => term.selectAll(),
      clear: () => term.clear(),
      focus: () => term.focus(),
      send: typeIn,
      hasSelection: () => term.hasSelection(),
      find: (text, back) => (back ? search.findPrevious(text) : search.findNext(text)),
      findDone: () => {
        search.clearDecorations();
        term.clearSelection();
        term.focus();
      },
      zoom,
    });
    if (id) {
      unwatch = watchTerminal(id, handlers);
      void attach();
    } else void open();
    term.focus();
  });

  createEffect(() => {
    const light = resolvedTheme() === "light";
    term.options.theme = themeOf(host ?? document.documentElement, light);
  });

  onCleanup(() => {
    disposed = true;
    clearTimeout(ackTimer);
    clearTimeout(resizeTimer);
    unwatch?.();
    // The view went (Start again, another place, or the tile closed, which closes
    // the terminal too): it waits for this device a short while. Tiles in other
    // mosaics and phone pages stay mounted, so they stay attached.
    if (id && !ended) void detachTerminal(id);
    term.dispose();
  });

  return <div class="term-screen" ref={host} data-term-id={props.termId ?? ""} />;
}
