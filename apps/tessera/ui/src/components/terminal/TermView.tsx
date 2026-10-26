// The terminal itself (xterm.js), loaded only when a terminal tile opens.
// It opens (or shows again) the terminal on the server, draws its output in
// order and says what it drew (`terminal_ack`: the server reads no further
// than a window ahead), and sends keystrokes, pastes and its size. A chunk
// that starts anywhere but where drawing stopped is the kept output coming
// again after a reconnect: the screen is drawn afresh from it.
//
// It behaves like a native terminal: a bundled monospace face, loaded before
// measuring cells, at its natural line height so box and block characters
// join up, Unicode 11 widths (emoji, CJK), 256 and true colour, 10,000 lines
// to scroll back, find, text size. Selecting copies; right-click, Ctrl+Shift+V or Shift+Insert
// pastes; Ctrl+click opens a web link. Every key goes to the program, except
// the few the app uses to move between tiles (`leftToApp`).
import "@xterm/xterm/css/xterm.css";
import "../../styles/terminal-font.css";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Terminal } from "@xterm/xterm";
import { createEffect, onCleanup, onMount } from "solid-js";
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
  TERMINAL_THEMES,
  termAck,
  termInput,
  termResize,
  watchTerminal,
  type TerminalInfo,
} from "../../lib/terminal";
import { confirmDialog, promptDialog, toastError } from "../../lib/ui";
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
  theme: "dark" | "light";
  onTitle: (title: string) => void;
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

/** The CSS is loaded with this chunk. Measure only after both real weights are ready. */
let fontsReady: Promise<boolean> | undefined;
function loadFonts(): Promise<boolean> {
  return fontsReady ??= Promise.all([
    document.fonts.load('400 14px "Tessera Terminal"'),
    document.fonts.load('700 14px "Tessera Terminal"'),
  ]).then((faces) => {
    const loaded = faces.every((f) => f.length > 0);
    if (!loaded) fontsReady = undefined;
    return loaded;
  }, () => {
    // Offline or a damaged asset: the terminal still works in its fallback face.
    fontsReady = undefined;
    return false;
  });
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
  let bellTimer: ReturnType<typeof setTimeout> | undefined;
  let selTimer: ReturnType<typeof setTimeout> | undefined;
  let ro: ResizeObserver | undefined;
  let opened = false;

  const term = new Terminal({
    cursorBlink: true,
    cursorStyle: "block",
    cursorInactiveStyle: "outline",
    fontWeight: "400",
    fontWeightBold: "700",
    drawBoldTextInBrightColors: false,
    theme: TERMINAL_THEMES[props.theme],
    fontFamily: TERM_FONT,
    fontSize: savedFontSize() ?? defaultFontSize(props.touch),
    lineHeight: 1,
    scrollback: 10_000,
    // For Unicode 11 character widths (the `unicode` interface is still marked experimental).
    allowProposedApi: true,
    // Links open only through openLink below (http and https, with Ctrl or a tap),
    // also the program's own (OSC 8) links, set once openLink exists.
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
  // OSC 8 links would otherwise go through xterm's own window.open.
  term.options.linkHandler = { activate: (e, uri) => void openLink(e, uri), allowNonHttpProtocols: false };
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
    if (!opened || !host.offsetWidth || !host.offsetHeight) return;
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
    if (disposed || ended) return;
    let text: string | null = null;
    try {
      text = await navigator.clipboard.readText();
    } catch {
      if (disposed || ended) return;
      // Where clipboard reads are denied, use a normal field's system Paste.
      text = await promptDialog({
        title: "Paste into the terminal",
        body: props.touch ? "Touch and hold in the field, choose Paste, then Paste here." : "Paste into the field with Ctrl+V, then choose Paste.",
        label: "Text",
        confirmLabel: "Paste",
      });
    }
    if (disposed || ended) return;
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

  const onContext = (e: MouseEvent) => {
    if (props.touch) return;
    e.preventDefault();
    void pasteClipboard();
  };

  onMount(() => {
    // Keep this task separate from the component lifetime. Closing a tile while
    // the fonts load must never open a late shell or focus a disposed terminal.
    void loadFonts().then((loaded) => {
      if (disposed) return;
      host.dataset.font = loaded ? "loaded" : "fallback";
      if (!loaded) term.options.fontFamily = '"DejaVu Sans Mono", Consolas, monospace';
      host.setAttribute("aria-busy", "false");
      term.open(host);
      opened = true;
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
      term.onSelectionChange(() => {
        clearTimeout(selTimer);
        if (props.touch) return;
        selTimer = setTimeout(() => {
          const sel = term.getSelection();
          if (sel) void navigator.clipboard?.writeText(sel).catch(() => {});
        }, 120);
      });
      term.onTitleChange((title) => props.onTitle(title.replace(/[\x00-\x1f\x7f-\x9f\u061c\u200b-\u200f\u2028-\u202e\u2066-\u2069\ufeff]/g, "").trim().slice(0, 200)));
      term.onBell(() => {
        host.classList.add("term-bell");
        clearTimeout(bellTimer);
        bellTimer = setTimeout(() => host.classList.remove("term-bell"), 160);
      });
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
        // Browser paste shortcuts vary across WebKit, WebView2 and Chromium.
        // Read on this user gesture instead of leaving Ctrl+Shift+V to the browser.
        if ((e.ctrlKey && e.shiftKey && key === "v") || (e.metaKey && !e.shiftKey && key === "v") || (e.shiftKey && !mod && e.key === "Insert")) {
          e.preventDefault();
          void pasteClipboard();
          return false;
        }
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
      // Right-click pastes. Linux PRIMARY selection is left to xterm/the OS,
      // rather than pretending the ordinary clipboard is the middle-click buffer.
      host.addEventListener("contextmenu", onContext);
      ro = new ResizeObserver(refit);
      ro.observe(host);
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
  });

  createEffect(() => {
    term.options.theme = TERMINAL_THEMES[props.theme];
  });

  onCleanup(() => {
    disposed = true;
    clearTimeout(ackTimer);
    clearTimeout(resizeTimer);
    clearTimeout(bellTimer);
    clearTimeout(selTimer);
    ro?.disconnect();
    host?.removeEventListener("contextmenu", onContext);
    unwatch?.();
    // The view went (Start again, another place, or the tile closed, which closes
    // the terminal too): it waits for this device a short while. Tiles in other
    // mosaics and phone pages stay mounted, so they stay attached.
    if (id && !ended) void detachTerminal(id);
    term.dispose();
  });

  return <div class="term-screen" ref={host} aria-label="Terminal" aria-busy="true" data-term-id={props.termId ?? ""} />;
}
