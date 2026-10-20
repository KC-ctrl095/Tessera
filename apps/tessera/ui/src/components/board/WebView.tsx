// A web view tile: the server's browser tab for a `url` (or `local`) board,
// drawn from its screencast, with input sent back (lib/browser.ts). It feels
// like a browser tab: back, forward, reload or stop, an address you can edit,
// zoom in a small menu (and pinch on touch screens), the phone's keyboard when
// a text field is focused, and the page's dialogs.
//
// A tile spawned from the New chat menu shows this with no board yet: typing
// an address makes the board in this very tile (lib/spawn.ts).
//
// Nothing of the page runs on this device: it only ever gets pictures. The
// browser is the server's, or (when the board has a `host`) the one of the
// machine the user made it from ("Web views here"): the tile says "on
// Desktop", and if that machine goes offline it offers to open the page on
// the server instead.
import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import "../../styles/webview.css";
import type { Whiteboard } from "../../proto/Whiteboard";
import type { WebViewControls } from "../../lib/webViewControls";
import {
  answerDialog,
  chooseFiles,
  attach,
  buttonOf,
  detach,
  displayAddress,
  frameBlob,
  input,
  modifiersOf,
  nav,
  newTab,
  resize,
  pageAction,
  tabs,
  toAddress,
  showOnScreen,
  toServer,
  type BrowserDialog,
  type BrowserFrame,
} from "../../lib/browserView";
import { askAboutBoard, boardChat, checkBoardUrl, confirmDeleteBoard, openBoardExternally, shownBy } from "../../lib/boards";
import { knownMachine } from "../../lib/machines";
import { call, state } from "../../lib/store";
import { fillDraft, urlContent } from "../../lib/spawn";
import { bookmarkPage, rememberTabGroup, webBookmarks, webPageScope, webTabGroup } from "../../lib/webViewPrefs";
import { confirmDialog, promptDialog, errorMessage, setDrawerOpen, toast } from "../../lib/ui";
import { coarsePointer, copyText, openExternal, touchUi } from "../../lib/util";
import { closePane, openBoard, openSession, workspace } from "../../lib/workspace";
import { Icon, Tessellate } from "../icons";
import { WebDownloads } from "../chat/FileCard";
import { usePane } from "../PaneContext";
import { RestoreButton } from "../RestoreButton";
import { openMenu, type MenuItem } from "../primitives";
import { ALT } from "../TopBar";
import { NewTileButton, newInHeader } from "../NewMenu";

const hostOf = (u: string) => {
  try {
    return new URL(u).host;
  } catch {
    return "";
  }
};
const ZOOMS = [0.5, 0.67, 0.75, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2, 2.5, 3];
/** Keys that are the page's, not text: sent as key presses. */
const NAMED = new Set([
  "Enter", "Backspace", "Tab", "Escape", "Delete", "Insert", "Home", "End", "PageUp", "PageDown",
  "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "ContextMenu",
  "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
]);

/**
 * A tap that closes an open menu or popover only closes it: it doesn't also
 * click the page. (Seen before the popover's own outside-tap handler runs.)
 */
let layerAtDown = false;
window.addEventListener("pointerdown", () => (layerAtDown = document.body.classList.contains("layer-open")), true);

/** The colour at the bottom (or top) left corner of a picture (a page's margin): fills where a scroll uncovers the page. */
function edgeColor(bmp: ImageBitmap | null, bottom: boolean): string {
  if (!bmp) return "#fff";
  try {
    const c = typeof OffscreenCanvas !== "undefined" ? new OffscreenCanvas(1, 1) : Object.assign(document.createElement("canvas"), { width: 1, height: 1 });
    const ctx = c.getContext("2d", { willReadFrequently: true }) as CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D | null;
    if (!ctx) return "#fff";
    ctx.drawImage(bmp, 1, bottom ? bmp.height - 3 : 1, 2, 2, 0, 0, 1, 1);
    const [r, g, b] = ctx.getImageData(0, 0, 1, 1).data;
    return `rgb(${r},${g},${b})`;
  } catch {
    return "#fff";
  }
}

export default function WebView(props: { paneId: string; board?: Whiteboard; controls?: (api: WebViewControls | null) => void }) {
  const pane = usePane();
  const boardId = () => props.board?.id ?? null;
  const tab = () => (boardId() ? tabs[boardId()!] : undefined);
  const boardUrl = () => {
    const c = props.board?.content;
    if (c?.type === "url") return c.url;
    if (c?.type === "local") return `http://127.0.0.1:${c.port}${c.path}`;
    return "";
  };
  const tabIds = () => {
    const ids = tab()?.tab_ids?.length ? tab()!.tab_ids : webTabGroup(boardId() ?? "");
    return ids?.length ? ids.filter((id) => state.boards[id] && webPageScope(id) === webPageScope(boardId() ?? "")) : boardId() ? [boardId()!] : [];
  };
  createEffect(() => { const ids = tab()?.tab_ids; if (ids?.length) rememberTabGroup(ids); });
  const tabSwitcher = (anchor: HTMLElement) => openMenu(anchor, tabIds().map((id) => ({ label: tabs[id]?.title || state.boards[id]?.title || savedUrl(id) || "Page", icon: "globe" as const, checked: id === boardId(), onSelect: () => switchTab(id) })), "bottom-start", { sheet: true });
  const savedUrl = (id: string) => state.boards[id]?.content.type === "url" ? (state.boards[id].content as { url: string }).url : "";
  const switchTab = (id: string) => openBoard(id, { pane: props.paneId });
  const openTab = async (url?: string) => {
    const id = boardId();
    if (!id) return;
    const raw = url ?? await promptDialog({ title: "New web tab", label: "Address or search", placeholder: "example.com" });
    if (!raw?.trim()) return;
    try { const b = await newTab(id, toAddress(raw)); switchTab(b.id); }
    catch (e) { toast({ level: "error", message: errorMessage(e) }); }
  };
  const [lastClosed, setLastClosed] = createSignal("");
  const closeTab = async (id: string) => {
    setLastClosed(tabs[id]?.url || savedUrl(id));
    const next = tabIds().find((s) => s !== id);
    if (id === boardId() && next) switchTab(next);
    try { await call({ method: "delete_board", params: { id } }); }
    catch (e) { toast({ level: "error", message: errorMessage(e) }); }
  };
  createEffect(on(() => tab()?.tab_ids ?? [], (ids, before) => {
    if (before && ids.length > Math.max(1, before.length) && ids.includes(boardId()!) && state.boards[ids.at(-1)!]) switchTab(ids.at(-1)!);
  }));
  const url = () => tab()?.url || boardUrl();
  /** A board whose address may not be shown (not the web, or Tessera itself): never opened in the browser. */
  const refused = createMemo(() => {
    if (props.board?.content.type !== "url") return null;
    const c = checkBoardUrl(props.board.content.url);
    return c.ok ? null : c.reason;
  });
  const loading = () => !!tab()?.loading;
  const zoom = () => tab()?.zoom ?? 1;
  const draft = () => !props.board;

  // ── where the page runs ──────────────────────────────────────────────
  /** The machine whose browser shows it (none: the server's). */
  const hostId = () => props.board?.host ?? null;
  const hostMachine = createMemo(() => (hostId() ? knownMachine(hostId()!) : undefined));
  const hostName = () => hostMachine()?.name ?? "another computer";
  const serverName = () => state.server?.name || "the server";
  /** Why the machine can't show it now (only once its state is known). */
  const hostDown = createMemo((): string | null => {
    const m = hostMachine();
    if (!hostId() || !m) return null;
    if (!m.online) return `${m.name} is offline`;
    if (m.paused) return `${m.name} is paused`;
    if (!m.scope.webviews) return `Web views are off on ${m.name}`;
    return null;
  });
  // ── "Show on <machine>'s screen": for what only a person can finish ──
  /** When the page went to the machine's screen (the tile waits meanwhile). */
  const [onScreen, setOnScreen] = createSignal<number | null>(null);
  const [screenBusy, setScreenBusy] = createSignal(false);
  const toScreen = async () => {
    const id = boardId();
    if (!id || screenBusy()) return;
    setScreenBusy(true);
    try {
      await showOnScreen(id, true);
      setOnScreen(Date.now());
    } catch (err) {
      toast({ level: "error", message: errorMessage(err) });
    } finally {
      setScreenBusy(false);
    }
  };
  const backFromScreen = async () => {
    const id = boardId();
    if (!id || screenBusy()) return;
    setScreenBusy(true);
    try {
      await showOnScreen(id, false);
      setOnScreen(null);
    } catch (err) {
      toast({ level: "error", message: errorMessage(err) });
    } finally {
      setScreenBusy(false);
    }
  };
  /** The page looks like a site's "prove you're a person" check (the server says so): offer the way out, once per page. */
  const [checkDismissed, setCheckDismissed] = createSignal(false);
  createEffect(on(url, () => setCheckDismissed(false), { defer: true }));
  const checkHint = () => !!tab()?.check && !checkDismissed() && onScreen() === null && !hostDown();
  /** A web view on the server: there is no screen to finish a check on; say what works instead. */
  const noScreen = async () => {
    const ok = await confirmDialog({
      title: "Finish a check yourself",
      body: `Some sites ask you to prove you're a person before you sign in. This web view runs on ${serverName()}, which has no screen, so you can't do that here. Open the page in your own browser instead (you sign in there, not in this web view), or use web views on one of your computers: turn on Web views here on it, and pages you open there can be shown on its screen.`,
      confirmLabel: "Open in your browser",
    });
    if (ok) openOutside();
  };

  const [moving, setMoving] = createSignal(false);
  const openOnServer = async () => {
    const id = boardId();
    if (!id || moving()) return;
    setMoving(true);
    try {
      await toServer(id);
      setFailure(null);
    } catch (err) {
      toast({ level: "error", message: errorMessage(err) });
    } finally {
      setMoving(false);
    }
  };

  // ── the address field ────────────────────────────────────────────────
  let field!: HTMLInputElement;
  const [editing, setEditing] = createSignal(false);
  const [addr, setAddr] = createSignal("");
  const [history, setHistory] = createSignal<string[]>([]);
  createEffect(on(() => url(), (value) => { if (/^https?:/.test(value)) setHistory((old) => [value, ...old.filter((s) => s !== value)].slice(0, 12)); }));
  createEffect(on(() => webPageScope(boardId() ?? ""), () => { setHistory([]); setLastClosed(""); }));
  const suggestions = () => [...new Set([...history(), ...webBookmarks(boardId() ?? "").map(savedUrl)])].filter((s) => s && s !== addr() && s.toLowerCase().includes(addr().toLowerCase())).slice(0, 5);
  const [addrError, setAddrError] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  const secure = () => /^https:/i.test(url());
  const go = async (e: Event) => {
    e.preventDefault();
    const raw = addr().trim();
    if (!raw || busy()) return;
    if (draft()) {
      const c = urlContent(raw);
      if (typeof c === "string") return setAddrError(c);
      setBusy(true);
      await fillDraft(props.paneId, c.title, c.content);
      setBusy(false);
      return;
    }
    const u = toAddress(raw);
    const ok = checkBoardUrl(u);
    if (!ok.ok) return setAddrError(ok.reason);
    setEditing(false);
    field.blur();
    focusPage();
    await nav(boardId()!, { type: "go", url: ok.url.toString() }).catch((err) => toast({ level: "error", message: errorMessage(err) }));
  };
  onMount(() => {
    // A new web view is ready to type into (not on touch screens: the keyboard would jump up).
    if (draft() && !coarsePointer()) requestAnimationFrame(() => field?.focus({ preventScroll: true }));
  });

  // ── the picture ──────────────────────────────────────────────────────
  let host!: HTMLDivElement;
  let canvas!: HTMLCanvasElement;
  let keys!: HTMLInputElement;
  const [box, setBox] = createSignal({ w: 0, h: 0 });
  const [shown, setShown] = createSignal(false);
  const [failure, setFailure] = createSignal<string | null>(null);
  const [inView, setInView] = createSignal(false);
  const [desktopSite, setDesktopSite] = createSignal(false);
  const dpr = () => Math.min(3, Math.max(1, window.devicePixelRatio || 1));
  onMount(() => {
    const ro = new ResizeObserver(() => setBox({ w: host.clientWidth, h: host.clientHeight }));
    ro.observe(host);
    const io = new IntersectionObserver(([entry]) => setInView(entry.isIntersecting));
    io.observe(host);
    onCleanup(() => io.disconnect());
    onCleanup(() => ro.disconnect());
  });
  // Only a visible, intersecting tile receives pictures.
  const [live, setLive] = createSignal(false);
  const [pageVisible, setPageVisible] = createSignal(document.visibilityState === "visible");
  const onVis = () => setPageVisible(document.visibilityState === "visible");
  document.addEventListener("visibilitychange", onVis);
  onCleanup(() => document.removeEventListener("visibilitychange", onVis));
  createEffect(() => {
    const on = pane.visible() && inView() && pageVisible() && !!boardId() && !refused();
    setLive(on);
  });

  let view: Awaited<ReturnType<typeof attach>>["view"] | null = null;
  let disposed = false;
  let attempt: AbortController | undefined;
  let sizeTimer: ReturnType<typeof setTimeout> | undefined;
  const sizeNow = () => {
    const desktop = touchUi() && desktopSite();
    const width = desktop ? Math.max(1024, box().w) : box().w;
    return { width, height: Math.round(box().h * width / Math.max(1, box().w)), dpr: desktop ? 1 : dpr(), mobile: touchUi() && !desktop };
  };
  createEffect(
    on([live, boardId, () => box().w > 0 && box().h > 0, hostId, hostDown], ([on, id, sized, host, down], prev) => {
      // Moved to another browser, or its machine went away: attach afresh.
      attempt?.abort();
      const moved = !!prev && (prev[3] !== host || prev[4] !== down);
      if (view && (!on || view.boardId !== id || moved)) {
        detach(view);
        view = null;
      }
      if (moved) setShown(false);
      if (!on || !id || !sized || view || down) return;
      setFailure(null);
      const controller = attempt = new AbortController();
      attach(id, sizeNow(), { frame: onFrame, focus: onFocus, dialog: onDialog }, controller.signal)
        .then((r) => {
          if (disposed || controller.signal.aborted || !live() || boardId() !== id) return detach(r.view);
          view = r.view;
        })
        .catch((err) => {
          if (!disposed && !controller.signal.aborted) setFailure(errorMessage(err));
        });
    }),
  );
  // A new size: the canvas follows at once, the tab shortly after (resizing is costly there).
  createEffect(
    on([box, desktopSite], ([b]) => {
      if (!canvas || !b.w || !b.h) return;
      const w = Math.round(b.w * dpr());
      const h = Math.round(b.h * dpr());
      if (canvas.width !== w || canvas.height !== h) {
        // Keep the last picture while the next one comes (a resized canvas is cleared).
        canvas.width = w;
        canvas.height = h;
        paint();
      }
      clearTimeout(sizeTimer);
      sizeTimer = setTimeout(() => view && void resize(view, sizeNow()).catch(() => {}), 140);
    }),
  );
  onCleanup(() => {
    disposed = true;
    attempt?.abort();
    clearTimeout(sizeTimer);
    if (view) detach(view);
    picture?.close();
  });

  /** Draw one frame; resolves once it is on screen or skipped (lib/browser then acknowledges it). */
  let drawing = false;
  let next: { f: BrowserFrame; done: () => void } | null = null;
  const onFrame = (f: BrowserFrame) =>
    new Promise<void>((done) => {
      if (disposed) return done();
      // The window there was closed and the page draws here again.
      const since = onScreen();
      if (since !== null && Date.now() - since > 2000) setOnScreen(null);
      if (drawing) {
        // Only the newest one matters (one skipped counts as done).
        next?.done();
        next = { f, done };
        return;
      }
      void draw(f).then(done);
    });
  /** The page viewport the picture on screen shows (CSS px), and where the page was scrolled to in it. */
  let viewport: { width: number; height: number; scrollY: number | null } | null = null;
  /** The picture on screen (kept to draw it again: shifted under a finger, or after a resize). */
  let picture: ImageBitmap | null = null;
  const draw = async (f: BrowserFrame) => {
    drawing = true;
    try {
      const bmp = await createImageBitmap(frameBlob(f.data, f.mime));
      picture?.close();
      picture = bmp;
      viewport = { width: f.width || bmp.width, height: f.height || bmp.height, scrollY: f.scroll_y ?? null };
      followed(viewport.scrollY);
      if (paint()) setShown(true);
    } catch {
      /* a broken frame: the next one will do */
    } finally {
      drawing = false;
      const n = next;
      next = null;
      if (n) void draw(n.f).then(n.done);
    }
  };
  /** Draw the picture: the whole viewport, moved by a scroll the page hasn't shown yet. Returns whether it drew. */
  const paint = () => {
    const bmp = picture;
    const ctx = canvas?.getContext("2d");
    if (!bmp || !ctx || !canvas.width || !canvas.height) return false;
    // While a resize settles, the picture is drawn to the width.
    const exact = Math.abs(bmp.width / bmp.height - canvas.width / canvas.height) < 0.02;
    const h = exact ? canvas.height : bmp.height * (canvas.width / bmp.width);
    const shift = Math.round(ahead() * (h / (viewport?.height || bmp.height)));
    if (!exact || shift) {
      ctx.fillStyle = scrollFill;
      ctx.fillRect(0, 0, canvas.width, canvas.height);
    }
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(bmp, 0, shift, canvas.width, h);
    return true;
  };

  // ── a finger scrolling the page ──────────────────────────────────────
  // Frames take a round trip to show a scroll, so while a finger drags the
  // page the picture moves with it at once and the page's frames catch up.
  // Only once a frame shows the page really scrolling that way (a map or a
  // slider inside it doesn't move the page); never back towards where the
  // finger started; after it lifts, until frames get there (or briefly).
  let drag: { y0: number; from: number; to: number; on: boolean; seen: boolean; fade: number; at: number; movedAt: number } | null = null;
  let scrollFill = "#fff";
  let dragTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(dragTimer));
  /** How far (page px) the page is still to scroll beyond the picture: negative when it scrolls down. */
  const ahead = () => {
    const sy = viewport?.scrollY;
    if (!drag?.seen || sy == null || !viewport) return 0;
    const dir = Math.sign(drag.to - drag.from);
    let d = sy - drag.to;
    if (Math.sign(-d) !== dir) {
      // The page got there (or went further on its own).
      if (!drag.on) drag = null;
      return 0;
    }
    d = Math.max(-viewport.height / 2, Math.min(viewport.height / 2, d));
    if (drag.fade) d *= Math.max(0, 1 - (performance.now() - drag.fade) / 160);
    return d;
  };
  /** A new frame: does it show the page scrolling with the finger (and when did it last move)? */
  const followed = (sy: number | null) => {
    if (!drag || sy == null) return;
    if (Math.abs(sy - drag.at) > 1) {
      drag.at = sy;
      drag.movedAt = performance.now();
    }
    if (drag.seen) return;
    const dir = Math.sign(drag.to - drag.from);
    if (dir && Math.sign(sy - drag.from) === dir && Math.abs(sy - drag.from) >= 2) {
      drag.seen = true;
      scrollFill = edgeColor(picture, dir > 0);
    }
  };
  let painting = 0;
  const repaint = () => {
    if (!painting)
      painting = requestAnimationFrame(() => {
        painting = 0;
        paint();
        if (drag?.fade) {
          if (performance.now() - drag.fade < 170) repaint();
          else {
            drag = null;
            paint();
          }
        }
      });
  };
  onCleanup(() => cancelAnimationFrame(painting));
  const dragStart = (clientY: number) => {
    clearTimeout(dragTimer);
    const sy = viewport?.scrollY;
    drag = sy == null ? null : { y0: clientY, from: sy, to: sy, on: true, seen: false, fade: 0, at: sy, movedAt: performance.now() };
  };
  const dragMove = (clientY: number) => {
    if (!drag?.on || !viewport) return;
    if (drag.seen && performance.now() - drag.movedAt > 500 && viewport.scrollY != null) {
      // The page stopped following (its end): start over from where it is.
      drag.from = drag.to = viewport.scrollY;
      drag.y0 = clientY;
      drag.movedAt = performance.now();
    }
    const perPx = viewport.height / Math.max(1, canvas.getBoundingClientRect().height);
    drag.to = Math.max(0, drag.from - (clientY - drag.y0) * perPx);
    if (drag.seen) repaint();
  };
  const dragEnd = (cancel = false) => {
    if (!drag) return;
    if (cancel || !drag.seen) {
      drag = null;
      repaint();
      return;
    }
    drag.on = false;
    // Frames that never get there (the end of the page): let go gently.
    dragTimer = setTimeout(() => {
      if (drag && !drag.on) {
        drag.fade = performance.now();
        repaint();
      }
    }, 700);
  };

  // ── input ────────────────────────────────────────────────────────────
  /** Where a point on the picture is in the page: CSS px of the viewport the last frame showed. */
  const at = (e: { clientX: number; clientY: number }) => {
    const r = canvas.getBoundingClientRect();
    const vw = viewport?.width ?? view?.size.width ?? r.width;
    const vh = viewport?.height ?? view?.size.height ?? r.height;
    // The frame is drawn to the canvas's width (and its full height when the shapes match).
    const sx = vw / Math.max(1, r.width);
    const sy = viewport && Math.abs(vw / vh - r.width / r.height) >= 0.02 ? sx : vh / Math.max(1, r.height);
    return { x: Math.round((e.clientX - r.left) * sx), y: Math.round((e.clientY - r.top) * sy - ahead()) };
  };
  const send = (ev: Parameters<typeof input>[1]) => {
    const id = boardId();
    if (id) input(id, ev);
  };
  /** The keyboard goes to the page (desktop: once clicked; phones: when a field there is focused). */
  const focusPage = () => keys?.focus({ preventScroll: true });
  /** When a finger last went down on the page here, and what had focus then. */
  let touchedAt = -Infinity;
  let activeAtTouch: Element | null = null;

  let last = { t: 0, x: 0, y: 0, b: -1, n: 0 };
  let held: string = "none";
  const touches = new Map<number, { x: number; y: number }>();
  let pinch: { d0: number; z0: number } | null = null;
  const [pinchScale, setPinchScale] = createSignal(1);
  const dist = () => {
    const [a, b] = [...touches.values()];
    return a && b ? Math.hypot(a.x - b.x, a.y - b.y) : 0;
  };
  const touchPoints = () => [...touches].map(([id, p]) => ({ id, ...at({ clientX: p.x, clientY: p.y }) }));
  let holdTimer: ReturnType<typeof setTimeout> | undefined;
  let edgeSwipe: { x: number; y: number; active: boolean } | null = null;
  const cancelHold = () => clearTimeout(holdTimer);
  onCleanup(cancelHold);
  const context = async (clientX: number, clientY: number, selectWord = false) => {
    const id = boardId();
    if (!id) return;
    try {
      const found = await pageAction(id, { type: "context", ...at({ clientX, clientY }), select_word: selectWord });
      if (id !== boardId() || !live()) return;
      const items: MenuItem[] = [];
      if (found.link) items.push(
        { label: "Open link in new tab", icon: "plus", onSelect: () => void openTab(found.link!) },
        { label: "Copy link", icon: "copy", onSelect: () => void copyText(found.link!) },
        { label: "Open link in your browser", icon: "external", onSelect: () => void openExternal(found.link!) },
      );
      if (found.text) items.push({ label: "Copy text", icon: "copy", onSelect: () => void copyText(found.text) });
      items.push({ label: "Paste", icon: "copy", onSelect: () => void pasteFromDevice() });
      openMenu({ x: clientX, y: clientY }, items, "bottom-start", { sheet: false });
    } catch (e) { toast({ level: "error", message: errorMessage(e) }); }
  };

  /** Pointers that only closed a menu: the page never hears of them. */
  const ignored = new Set<number>();
  const onDown = (e: PointerEvent) => {
    if (!boardId()) return;
    // A fresh first finger: a release lost while the app was in the background leaves no phantom touch behind.
    if (e.pointerType === "touch" && e.isPrimary) touches.clear();
    if (layerAtDown) {
      ignored.add(e.pointerId);
      e.preventDefault();
      return;
    }
    canvas.setPointerCapture(e.pointerId);
    if (e.pointerType === "touch") {
      e.preventDefault();
      touchedAt = performance.now();
      activeAtTouch = document.activeElement;
      touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (touches.size === 2) {
        cancelHold();
        // Two fingers: a pinch zooms the page (the first finger's touch is called off).
        send({ type: "touch", kind: "cancel", points: [], modifiers: 0 });
        pinch = { d0: dist(), z0: zoom() };
        tapAt = null;
        dragEnd(true);
        return;
      }
      if (touches.size === 1) {
        send({ type: "touch", kind: "start", points: touchPoints(), modifiers: modifiersOf(e) });
        dragStart(e.clientY);
        tapAt = { x: e.clientX, y: e.clientY, t: performance.now() };
        const r = canvas.getBoundingClientRect();
        edgeSwipe = tab()?.can_go_back && e.clientX - r.left < 24 ? { x: e.clientX, y: e.clientY, active: false } : null;
        cancelHold();
        holdTimer = setTimeout(() => {
          if (!tapAt || touches.size !== 1) return;
          send({ type: "touch", kind: "cancel", points: [], modifiers: 0 });
          ignored.add(e.pointerId);
          touches.clear();
          tapAt = null;
          edgeSwipe = null;
          dragEnd(true);
          void context(e.clientX, e.clientY, true);
        }, 550);
        // The caret may move: the keyboard's next word starts afresh.
        resetKeys();
      }
      return;
    }
    if (e.button === 1 || (e.button === 0 && (e.ctrlKey || e.metaKey))) {
      // Native browser-UI auxiliary tabs have no opener ID in CDP. Resolve
      // this device's link instead of guessing which unowned target is ours.
      e.preventDefault();
      ignored.add(e.pointerId);
      resetKeys();
      const id = boardId()!, p = at(e), button = buttonOf(e.button), modifiers = modifiersOf(e);
      void pageAction(id, { type: "context", ...p, select_word: false }).then((found) => {
        if (id !== boardId() || !live()) return;
        if (found.link) void openTab(found.link);
        else for (const kind of ["down", "up"] as const) send({ type: "mouse", kind, ...p, button, clicks: 1, dx: 0, dy: 0, modifiers });
      }).catch((error) => toast({ level: "error", message: errorMessage(error) }));
      return;
    }
    if (e.button === 2) { e.preventDefault(); return; }
    resetKeys();
    if (!touchUi()) focusPage();
    e.preventDefault();
    const p = at(e);
    const near = Date.now() - last.t < 450 && Math.abs(p.x - last.x) < 5 && Math.abs(p.y - last.y) < 5 && last.b === e.button;
    last = { t: Date.now(), x: p.x, y: p.y, b: e.button, n: near ? last.n + 1 : 1 };
    held = buttonOf(e.button);
    send({ type: "mouse", kind: "down", ...p, button: held as "left", clicks: last.n, dx: 0, dy: 0, modifiers: modifiersOf(e) });
  };
  const onMove = (e: PointerEvent) => {
    if (!boardId() || ignored.has(e.pointerId)) return;
    if (e.pointerType === "touch") {
      if (!touches.has(e.pointerId)) return;
      touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (pinch) {
        const d = dist();
        if (pinch.d0 > 0 && d > 0) setPinchScale(Math.max(0.3, Math.min(4, d / pinch.d0)));
        return;
      }
      if (tapAt && Math.hypot(e.clientX - tapAt.x, e.clientY - tapAt.y) > 10) cancelHold();
      if (edgeSwipe && e.clientX - edgeSwipe.x > 18 && Math.abs(e.clientY - edgeSwipe.y) < 40) {
        if (!edgeSwipe.active) send({ type: "touch", kind: "cancel", points: [], modifiers: 0 });
        edgeSwipe.active = true;
        dragEnd(true);
        return;
      }
      send({ type: "touch", kind: "move", points: touchPoints(), modifiers: modifiersOf(e) });
      if (touches.size === 1) dragMove(e.clientY);
      return;
    }
    send({ type: "mouse", kind: "move", ...at(e), button: held as "left", clicks: 0, dx: 0, dy: 0, modifiers: modifiersOf(e) });
  };
  const onUp = (e: PointerEvent) => {
    if (ignored.delete(e.pointerId) || !boardId()) return;
    cancelHold();
    if (e.pointerType !== "touch" && e.button === 2) return;
    if (e.pointerType === "touch") {
      if (!touches.delete(e.pointerId)) return;
      if (pinch) {
        if (touches.size === 0) {
          const z = Math.max(0.25, Math.min(5, pinch.z0 * pinchScale()));
          pinch = null;
          setPinchScale(1);
          if (Math.abs(z - zoom()) > 0.02) void nav(boardId()!, { type: "zoom", factor: Math.round(z * 100) / 100 });
        }
        return;
      }
      if (edgeSwipe?.active) {
        const goBack = e.type !== "pointercancel" && e.clientX - edgeSwipe.x > 80;
        edgeSwipe = null;
        tapAt = null;
        if (goBack) void nav(boardId()!, { type: "back" });
        return;
      }
      edgeSwipe = null;
      send({ type: "touch", kind: e.type === "pointercancel" ? "cancel" : "end", points: touchPoints(), modifiers: modifiersOf(e) });
      if (touches.size === 0) {
        dragEnd(e.type === "pointercancel");
        tapped(e);
      }
      return;
    }
    send({ type: "mouse", kind: "up", ...at(e), button: buttonOf(e.button) as "left", clicks: last.n, dx: 0, dy: 0, modifiers: modifiersOf(e) });
    held = "none";
  };
  onMount(() => {
    // Not passive: the page scrolls, not the app.
    const onWheel = (e: WheelEvent) => {
      if (!boardId()) return;
      e.preventDefault();
      if (e.ctrlKey && !touchUi()) {
        // Pinch on a touchpad (and Ctrl+wheel): zoom.
        const z = Math.max(0.25, Math.min(5, zoom() * Math.exp(-e.deltaY / 300)));
        void nav(boardId()!, { type: "zoom", factor: Math.round(z * 100) / 100 });
        return;
      }
      const k = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? host.clientHeight : 1;
      send({ type: "mouse", kind: "wheel", ...at(e), button: "none", clicks: 0, dx: e.deltaX * k, dy: e.deltaY * k, modifiers: modifiersOf(e) });
    };
    canvas.addEventListener("wheel", onWheel, { passive: false });
    onCleanup(() => canvas.removeEventListener("wheel", onWheel));
  });

  // Keys: a hidden text box takes them (and the phone's keyboard, IME, paste).
  const keyEvent = (e: KeyboardEvent, kind: "down" | "up") => {
    const named = NAMED.has(e.key);
    const printable = [...e.key].length === 1;
    const shortcut = e.ctrlKey || e.metaKey || (e.altKey && !e.getModifierState?.("AltGraph"));
    if (!named && !printable && !shortcut) return false;
    if (!named && !printable && shortcut && e.key.length > 1 && !/^[A-Z]/.test(e.key)) return false;
    const text = kind === "down" && !shortcut ? (e.key === "Enter" ? "\r" : printable ? e.key : undefined) : undefined;
    send({ type: "key", kind, key: e.key, code: e.code, ...(text ? { text } : {}), modifiers: modifiersOf(e) });
    return true;
  };
  const onKeyDown = (e: KeyboardEvent) => {
    if (e.isComposing || e.key === "Process" || e.key === "Unidentified" || e.key === "Dead") return;
    if ((e.ctrlKey || e.metaKey) && !e.altKey) {
      if (e.key.toLowerCase() === "t") { e.preventDefault(); if (e.shiftKey && lastClosed()) { const raw = lastClosed(); void openTab(raw); } else void openTab(); return; }
      if (e.key.toLowerCase() === "w") { e.preventDefault(); if (boardId() && tabIds().length > 1) void closeTab(boardId()!); else closePane(props.paneId); return; }
      if (e.key === "Tab") { e.preventDefault(); const ids=tabIds(), i=ids.indexOf(boardId()!); switchTab(ids[(i+(e.shiftKey?-1:1)+ids.length)%ids.length]); return; }
      if (e.key.toLowerCase() === "f") { e.preventDefault(); showFind(); return; }
      if (e.key.toLowerCase() === "c") { e.preventDefault(); void copySelection(); return; }
    }
    // Paste comes as text (this device's clipboard, not the server's).
    if ((e.ctrlKey || e.metaKey) && !e.altKey && (e.key === "v" || e.key === "V")) return;
    // The app's own tile shortcuts stay the app's.
    if (e.altKey && !e.ctrlKey && !e.metaKey && !e.getModifierState?.("AltGraph")) return;
    if (!keyEvent(e, "down")) return;
    e.preventDefault();
    setTyped(true);
    // The caret moved, or a line or form ended: what the keyboard composes next starts afresh.
    if (e.key === "Enter" || e.key.startsWith("Arrow") || e.key === "Tab") resetKeys();
  };
  const onKeyUp = (e: KeyboardEvent) => {
    if (e.isComposing || e.key === "Process" || e.key === "Unidentified" || e.key === "Dead") return;
    if (e.altKey && !e.ctrlKey && !e.metaKey) return;
    if ((e.ctrlKey || e.metaKey) && (e.key === "v" || e.key === "V")) return;
    keyEvent(e, "up");
  };
  /**
   * Phone keyboards type a word at a time: they compose it, correct it,
   * swipe it in whole, and resetting the box under them while they do
   * confuses them (letters doubled or lost). So the box keeps what was
   * typed, and whatever changed in it since the page was last told goes
   * there as backspaces and text. It starts with one space, so a backspace
   * always has something to delete.
   */
  const SENTINEL = " ";
  /** What the box held when the page was last told. */
  let told = SENTINEL;
  let composing = false;
  const resetKeys = () => {
    // Mid-word the keyboard owns the box: it starts afresh once the word is done.
    if (composing || !keys) return;
    keys.value = told = SENTINEL;
    keys.setSelectionRange(1, 1);
  };
  const press = (key: string, code = key) => {
    send({ type: "key", kind: "down", key, code, ...(key === "Enter" ? { text: "\r" } : {}), modifiers: 0 });
    send({ type: "key", kind: "up", key, code, modifiers: 0 });
  };
  const graphemes = typeof Intl !== "undefined" && "Segmenter" in Intl ? new Intl.Segmenter() : null;
  /** How many characters a backspace takes one at a time (an emoji is one, whatever it's made of). */
  const chars = (s: string) => (graphemes ? [...graphemes.segment(s)].length : [...s].length);
  /** The last place at or before `i` where `s` can be cut without splitting a character. */
  const cutAt = (s: string, i: number) => {
    if (!graphemes) return i > 0 && i < s.length && /[\udc00-\udfff]/.test(s[i]) ? i - 1 : i;
    let at = 0;
    for (const g of graphemes.segment(s)) {
      if (g.index + g.segment.length > i) break;
      at = g.index + g.segment.length;
    }
    return at;
  };
  /** Tell the page what changed in the box: backspaces for what went, then what came. */
  const sync = () => {
    const now = keys.value;
    if (now === told) return;
    let i = 0;
    while (i < now.length && i < told.length && now[i] === told[i]) i++;
    i = Math.min(cutAt(told, i), cutAt(now, i));
    const gone = chars(told.slice(i));
    const added = now.slice(i);
    told = now;
    for (let k = 0; k < gone; k++) press("Backspace");
    if (added) send({ type: "text", text: added });
    setTyped(true);
  };
  const onInput = () => {
    sync();
    // Out of words to delete, or a long run: a fresh box (never mid-word).
    if (!keys.value || told.length > 200) resetKeys();
  };
  const onCompositionStart = () => (composing = true);
  const onCompositionEnd = () => {
    composing = false;
    sync();
  };
  const onPaste = (e: ClipboardEvent) => {
    const t = e.clipboardData?.getData("text/plain");
    e.preventDefault();
    if (t) send({ type: "text", text: t });
  };

  // ── the phone's keyboard ─────────────────────────────────────────────
  // Phones raise their keyboard for a box focused soon after a tap: the
  // page says when one of its fields takes focus (a round trip after the
  // tap), and a tap while a field has it brings the keyboard back (the page
  // doesn't say anything new then). If it still isn't up, a Type button
  // offers it.
  const [fieldFocus, setFieldFocus] = createSignal<{ editable: boolean; type: string }>({ editable: false, type: "" });
  /** Typed into this field (a hardware keyboard needs no Type button). */
  const [typed, setTyped] = createSignal(false);
  /** The on-screen keyboard is up: what's visible is much shorter than at its tallest (at this width). */
  const [kbdUp, setKbdUp] = createSignal(false);
  onMount(() => {
    const vv = window.visualViewport;
    if (!vv) return;
    let width = 0;
    let tallest = 0;
    const look = () => {
      if (Math.abs(vv.width - width) > 1) {
        width = vv.width;
        tallest = 0;
      }
      tallest = Math.max(tallest, vv.height);
      const up = vv.height < tallest - 120;
      // Put away while the field still has focus: offer it again.
      if (kbdUp() && !up) setTyped(false);
      setKbdUp(up);
    };
    look();
    vv.addEventListener("resize", look);
    onCleanup(() => vv.removeEventListener("resize", look));
  });
  const [pasteOpen, setPasteOpen] = createSignal(false);
  const [pasteText, setPasteText] = createSignal("");
  const pasteFromDevice = async () => {
    try {
      // Capture the trusted button gesture before waiting on the browser.
      const id = boardId();
      const text = await navigator.clipboard.readText();
      if (id === boardId() && live() && text) send({ type: "text", text: text.slice(0, 65536) });
      resetKeys();
    } catch {
      setPasteText("");
      setPasteOpen(true);
    }
  };
  const copySelection = async () => {
    const id = boardId();
    if (!id) return;
    try {
      const result = await pageAction(id, { type: "selection" });
      if (result.text) await copyText(result.text);
      else toast({ level: "info", message: "Select some text on the page first." });
    } catch (e) { toast({ level: "error", message: errorMessage(e) }); }
  };
  let findField: HTMLInputElement | undefined;
  const [findOpen, setFindOpen] = createSignal(false);
  const [findText, setFindText] = createSignal("");
  const [findFound, setFindFound] = createSignal<boolean | null>(null);
  let findGeneration = 0;
  let findBoard = boardId();
  const clearFind = (id: string | null = boardId()) => {
    findGeneration++;
    setFindFound(null);
    if (id) void pageAction(id, { type: "clear_find" }).catch(() => {});
  };
  const closeFind = () => { clearFind(); setFindOpen(false); focusPage(); };
  createEffect(on(() => [boardId(), url(), loading()] as const, ([id], before) => { clearFind(before?.[0] ?? id); findBoard = id; }));
  onCleanup(() => clearFind(findBoard));
  const showFind = () => { setFindOpen(true); requestAnimationFrame(() => findField?.focus()); };
  const find = async (backwards = false) => {
    const id = boardId(), text = findText(), generation = ++findGeneration;
    const document = url();
    if (!id || !text) return;
    const current = () => generation === findGeneration && id === boardId() && document === url() && text === findText() && findOpen();
    try { const result = await pageAction(id, { type: "find", text, backwards }); if (current()) setFindFound(result.found ?? false); }
    catch (e) { if (current()) toast({ level: "error", message: errorMessage(e) }); }
  };
  onMount(() => props.controls?.({ keyboard: raiseKeyboard, paste: pasteFromDevice, desktopSite, setDesktopSite, find: showFind }));
  onCleanup(() => props.controls?.(null));
  const raiseKeyboard = () => {
    // Focusing the box again is what makes a phone show its keyboard again.
    if (document.activeElement === keys) keys.blur();
    focusPage();
  };
  const onFocus = (f: { editable: boolean; input_type: string }) => {
    setFieldFocus({ editable: f.editable, type: f.input_type });
    setTyped(false);
    resetKeys();
    if (!touchUi()) return;
    if (f.editable) {
      // A page can focus a field by itself at any time: the keyboard follows only
      // while it is here already, or right after a tap here, and never to leave a
      // field the user went to since (what they type into the chat stays there).
      const here = document.activeElement === keys;
      const tapped = performance.now() - touchedAt < 1500 && (!editableElsewhere() || document.activeElement === activeAtTouch);
      if (here || tapped) focusPage();
    } else if (document.activeElement === keys) keys.blur();
  };
  const editableElsewhere = () => {
    const a = document.activeElement as HTMLElement | null;
    return !!a && a !== keys && (a.isContentEditable || a instanceof HTMLTextAreaElement || (a instanceof HTMLInputElement && !["button", "checkbox", "radio", "range", "submit", "reset", "file", "color"].includes(a.type)));
  };
  /** A finger went down at this spot (a tap, if it comes up near it soon). */
  let tapAt: { x: number; y: number; t: number } | null = null;
  const tapped = (e: PointerEvent) => {
    const t = tapAt;
    tapAt = null;
    if (!t || e.type === "pointercancel" || performance.now() - t.t > 600 || Math.hypot(e.clientX - t.x, e.clientY - t.y) > 12) return;
    if (fieldFocus().editable && !(kbdUp() && document.activeElement === keys)) raiseKeyboard();
  };
  const inputMode = () => {
    const t = fieldFocus().type;
    return t === "email" ? "email" : t === "url" ? "url" : t === "number" ? "decimal" : t === "tel" ? "tel" : t === "search" ? "search" : "text";
  };
  /** The keyboard's action key: Search for a search box, Go for a one-line field, a new line in a text area. */
  const enterHint = () => {
    const t = fieldFocus().type;
    return t === "search" ? "search" : t === "textarea" || t === "contenteditable" ? "enter" : "go";
  };
  const [dialog, setDialog] = createSignal<BrowserDialog | null>(null);
  const [dialogText, setDialogText] = createSignal("");
  const onDialog = (d: BrowserDialog | null) => {
    setDialogText(d?.default ?? "");
    setDialog(d);
  };
  const answer = (accept: boolean) => {
    const d = dialog();
    if (!d) return;
    setDialog(null);
    void answerDialog(d.board_id, accept, d.kind === "prompt" && accept ? dialogText() : null).catch(() => {});
    focusPage();
  };
  const [uploadBusy, setUploadBusy] = createSignal(false);
  const [uploadProgress, setUploadProgress] = createSignal(0);
  let uploadStop: AbortController | undefined;
  onCleanup(() => uploadStop?.abort());
  const upload = async (files: File[], chooser: string) => {
    const id = boardId();
    if (!id || !chooser || uploadBusy()) return;
    setUploadBusy(true);
    setUploadProgress(0);
    uploadStop = new AbortController();
    try { await chooseFiles(id, chooser, files, (got, total) => setUploadProgress(Math.round(got / Math.max(1, total) * 100)), uploadStop.signal); }
    catch (e) { toast({ message: errorMessage(e), level: "error" }); }
    finally { setUploadBusy(false); }
  };
  // The Type button: a field in the page has focus, but no keyboard is up for it.
  const [kbdHint, setKbdHint] = createSignal(false);
  let hintTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(hintTimer));
  createEffect(() => {
    const want = touchUi() && live() && fieldFocus().editable && !kbdUp() && !typed() && !dialog();
    clearTimeout(hintTimer);
    if (!want) setKbdHint(false);
    else hintTimer = setTimeout(() => setKbdHint(true), 700);
  });

  // ── toolbar ──────────────────────────────────────────────────────────
  const zoomBy = (dir: 1 | -1 | 0) => {
    const id = boardId();
    if (!id) return;
    const z = zoom();
    const target = dir === 0 ? 1 : dir > 0 ? ZOOMS.find((x) => x > z + 0.001) ?? z : [...ZOOMS].reverse().find((x) => x < z - 0.001) ?? z;
    void nav(id, { type: "zoom", factor: target });
  };
  const menu = (anchor: HTMLElement) => {
    const b = props.board;
    const pct = `${Math.round(zoom() * 100)}%`;
    const items: MenuItem[] = [];
    if (b) {
      items.push(
        { label: "New tab", icon: "plus", onSelect: () => void openTab() },
        { label: webBookmarks(boardId() ?? "").includes(b.id) ? "Remove bookmark" : "Bookmark this page", icon: "bookmark", onSelect: () => bookmarkPage(b.id) },
        ...webBookmarks(boardId() ?? "").filter((id) => state.boards[id] && id !== b.id).map((id) => ({ label: state.boards[id].title || savedUrl(id) || "Saved page", icon: "bookmark" as const, onSelect: () => switchTab(id) })),
        { label: "Save page as PDF", icon: "file", onSelect: async () => { try { const r = await pageAction(b.id, { type: "print_pdf" }); toast({ level: "success", message: r.text }); } catch (e) { toast({ level: "error", message: errorMessage(e) }); } } },
        { label: "Find in page", icon: "search", hint: touchUi() ? undefined : "Ctrl+F", onSelect: showFind },
        { label: "Paste", icon: "copy", onSelect: () => void pasteFromDevice() },
        ...(touchUi() ? [{ label: desktopSite() ? "Mobile site" : "Desktop site", icon: "monitor" as const, onSelect: () => setDesktopSite(!desktopSite()) }] : []),
        { label: "Zoom in", icon: "plus", hint: pct, onSelect: () => zoomBy(1) },
        { label: "Zoom out", icon: "minus", onSelect: () => zoomBy(-1) },
        { label: "Actual size", icon: "maximize", disabled: Math.abs(zoom() - 1) < 0.01, onSelect: () => zoomBy(0) },
        { separator: true },
      );
      if (touchUi()) items.push({ label: "Keyboard", icon: "keyboard", description: "Type into the page", onSelect: raiseKeyboard });
      items.push({ label: "Ask about this", icon: "chat", description: "Mention it in the chat's message box", onSelect: () => void askAboutBoard(b, props.paneId) });
      items.push({ label: "Go to the chat", icon: "arrowRight", onSelect: () => openSession(boardChat(b), { newPane: true }) });
      if (url()) {
        items.push({ label: "Audio / video in your browser", icon: "external", description: "Web views send pictures, not sound. Your device browser plays the lecture.", onSelect: openOutside });
        items.push({ label: "Open in your browser", icon: "external", onSelect: openOutside });
        items.push(
          hostId()
            ? {
                label: `Show on ${hostName()}'s screen`,
                icon: "monitor",
                description: "Finish a check or a sign-in yourself, in a normal browser window there",
                disabled: !!hostDown() || onScreen() !== null,
                onSelect: () => void toScreen(),
              }
            : { label: "Finish a check yourself…", icon: "shieldCheck", description: "When a site asks you to prove you're a person", onSelect: () => void noScreen() },
        );
        items.push({ label: "Copy address", icon: "copy", onSelect: () => void copyText(url()) });
        items.push({ label: "Share link", icon: "external", onSelect: () => {
          if (navigator.share) void navigator.share({ url: url(), title: tab()?.title }).catch(() => {});
          else void copyText(url());
        } });
      }
    }
    items.push({ label: "Close tile", icon: "x", hint: workspace.mode() === "tile" ? `${ALT} W` : undefined, onSelect: () => closePane(props.paneId) });
    if (b) items.push({ separator: true }, { label: "Delete web view…", icon: "trash", danger: true, description: "Closes its tab everywhere", onSelect: () => void confirmDeleteBoard(b) });
    openMenu(anchor, items, "bottom-end");
  };
  /** This device's browser: the page, or a web app through this device's way to the server. */
  const openOutside = () => {
    const b = props.board;
    if (b?.content.type === "local" && tab()?.url === boardUrl()) void openBoardExternally(b);
    else void openExternal(url());
  };
  const onHeadDown = (e: PointerEvent) => {
    if ((e.target as HTMLElement).closest("button, input, a, form, [data-nodrag]")) return;
    pane.startDrag(e);
  };
  // The address fills most of the bar, so until it is being edited it is part
  // of what you grab: a press and move there moves the tile, a click (or a
  // tap) edits it. Its text box takes no pointer until then (see .wv-addr).
  const onAddrDown = (e: PointerEvent) => {
    if (editing()) return;
    pane.startDrag(e);
  };
  const onAddrClick = () => {
    if (!editing()) field.focus();
  };
  const by = createMemo(() => (props.board ? shownBy(props.board) : ""));

  return (
    <div class="board web-view" classList={{ draft: draft() }}>
      <header class="pane-head board-head wv-bar" onPointerDown={onHeadDown}>
        <Show when={pane.showMenuButton()}>
          <button type="button" class="icon-btn head-menu" data-keep-focus aria-label="Open sidebar" onClick={() => setDrawerOpen(true)}>
            <Icon name="menu" />
          </button>
        </Show>
        <button type="button" class="icon-btn wv-nav" aria-label="Back" data-tip="Back" disabled={!tab()?.can_go_back} onClick={() => void nav(boardId()!, { type: "back" })}>
          <Icon name="arrowLeft" size={16} />
        </button>
        <button type="button" class="icon-btn wv-nav wv-fwd" aria-label="Forward" data-tip="Forward" disabled={!tab()?.can_go_forward} onClick={() => void nav(boardId()!, { type: "forward" })}>
          <Icon name="arrowRight" size={16} />
        </button>
        <button
          type="button"
          class="icon-btn wv-nav"
          aria-label={loading() ? "Stop" : "Reload"}
          data-tip={loading() ? "Stop" : "Reload"}
          disabled={!boardId()}
          onClick={() => void nav(boardId()!, { type: loading() ? "stop" : "reload" })}
        >
          <Icon name={loading() ? "x" : "refresh"} size={15} />
        </button>
        <form
          class="wv-addr"
          classList={{ editing: editing(), insecure: !!url() && !secure() }}
          onSubmit={go}
          onPointerDown={onAddrDown}
          onClick={onAddrClick}
          noValidate
          data-nodrag
        >
          <span class="wv-lock" aria-hidden="true" data-tip={url() ? (secure() ? "Encrypted connection (https)" : "Not encrypted (http)") : undefined}>
            <Show when={secure() && !editing() && /^data:image\//.test(tab()?.favicon ?? "")} fallback={<Icon name={!url() ? "search" : secure() ? "lock" : "lockOpen"} size={12} />}>
              <img class="wv-favicon" src={tab()!.favicon} alt="" width="14" height="14" />
            </Show>
          </span>
          <input
            ref={field}
            class="wv-input"
            type="text"
            inputmode="url"
            enterkeyhint="go"
            autocomplete="off"
            autocapitalize="off"
            spellcheck={false}
            aria-label="Address"
            placeholder="Search or type an address"
            value={editing() ? addr() : displayAddress(url())}
            title={url()}
            onFocus={(e) => {
              setAddr(url());
              setEditing(true);
              setAddrError(null);
              const el = e.currentTarget;
              requestAnimationFrame(() => el.select());
            }}
            onBlur={() => {
              if (!busy()) setEditing(false);
            }}
            onInput={(e) => {
              setAddr(e.currentTarget.value);
              setAddrError(null);
            }}
            list={`web-history-${props.paneId}`}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                setEditing(false);
                setAddrError(null);
                e.currentTarget.blur();
              }
            }}
          />
          <Show when={busy()}>
            <Tessellate size={8} />
          </Show>
          <Show when={hostId() && !editing()}>
            <span
              class="wv-host"
              classList={{ down: !!hostDown() }}
              data-tip={`This page runs in the browser on ${hostName()}. Sites you sign in to there stay on that computer.`}
            >
              on {hostName()}
            </span>
          </Show>
          <datalist id={`web-history-${props.paneId}`}><For each={suggestions()}>{(s) => <option value={s} />}</For></datalist>
        </form>
        <div class="head-controls">
          <Show when={props.board && Math.abs(zoom() - 1) > 0.01}>
            <button type="button" class="wv-zoom" aria-label={`Zoom ${Math.round(zoom() * 100)}%, back to actual size`} data-tip="Actual size" onClick={() => zoomBy(0)}>
              {Math.round(zoom() * 100)}%
            </button>
          </Show>
          <Show when={props.board && touchUi()}>
            <button type="button" class="icon-btn wv-keyboard" aria-label="Show page keyboard" data-tip="Keyboard" onClick={raiseKeyboard}><Icon name="keyboard" size={16} /></button>
          </Show>
          <button type="button" class="icon-btn" aria-label="Web view options" aria-haspopup="menu" data-tip="Zoom and more" onClick={(e) => menu(e.currentTarget)}>
            <Icon name="more" size={17} />
          </button>
          <RestoreButton />
          {/* Phones: New at the top right; Close is in the ⋯ menu. */}
          <Show
            when={newInHeader() && pane.showMenuButton()}
            fallback={
              <button type="button" class="icon-btn head-close" aria-label="Close tile" data-tip="Close tile" data-kbd={`${ALT} W`} onClick={() => closePane(props.paneId)}>
                <Icon name="x" size={15} />
              </button>
            }
          >
            <NewTileButton />
          </Show>
        </div>
        <div class="wv-progress" classList={{ on: loading() || busy() }} aria-hidden="true" />
      </header>
      <Show when={addrError()}>
        <p class="form-error wv-error" role="alert">
          <Icon name="alertCircle" size={14} />
          <span>{addrError()}</span>
        </p>
      </Show>
      <Show when={props.board}>
        <div class="wv-tabs" role="tablist" aria-label="Web pages">
          <For each={tabIds()}>{(id) => <div class="wv-tab" classList={{ active: id === boardId() }}>
            <button type="button" class="wv-tab-title" role="tab" aria-selected={id === boardId()} title={tabs[id]?.url || savedUrl(id) || "Page"} onClick={() => switchTab(id)}>
              <Show when={tabs[id]?.favicon} fallback={<Icon name="globe" size={13} />}><img src={tabs[id]?.favicon!} alt="" /></Show>
              <span>{tabs[id]?.title || state.boards[id]?.title || "Page"}</span>
            </button>
            <Show when={tabIds().length > 1}><button type="button" class="icon-btn" aria-label={`Close ${tabs[id]?.title || "page"}`} onClick={() => void closeTab(id)}><Icon name="x" size={12} /></button></Show>
          </div>}</For>
          <button type="button" class="icon-btn wv-tab-count" aria-label={`Switch web tabs (${tabIds().length})`} onClick={(e) => tabSwitcher(e.currentTarget)}>{tabIds().length}</button>
          <button type="button" class="icon-btn wv-new-tab" aria-label="New web tab" onClick={() => void openTab()}><Icon name="plus" size={15} /></button>
        </div>
      </Show>
      <Show when={findOpen()}>
        <form class="wv-find" onSubmit={(e) => { e.preventDefault(); void find(); }}>
          <input ref={findField} class="input" aria-label="Find in page" maxlength={200} value={findText()} onInput={(e) => { findGeneration++; setFindText(e.currentTarget.value); setFindFound(null); }} onKeyDown={(e) => { if (e.key === "Escape") { e.preventDefault(); closeFind(); } }} />
          <span role="status">{findFound() === false ? "Not found" : findFound() === true ? "Found" : ""}</span>
          <button type="button" class="icon-btn" aria-label="Previous match" onClick={() => void find(true)}><Icon name="arrowLeft" size={15} /></button>
          <button type="submit" class="icon-btn" aria-label="Next match"><Icon name="arrowRight" size={15} /></button>
          <button type="button" class="icon-btn" aria-label="Close find" onClick={closeFind}><Icon name="x" size={15} /></button>
        </form>
      </Show>
      <Show when={boardId()}>{(id) => <WebDownloads boardId={id()} />}</Show>
      <div ref={host} class="board-body wv-body" data-noswipe={boardId() ? "" : undefined}>
        <canvas
          ref={canvas}
          class="wv-canvas"
          classList={{ shown: shown() }}
          style={{ transform: pinchScale() !== 1 ? `scale(${pinchScale()})` : undefined }}
          aria-label={tab()?.title || props.board?.title || "Web view"}
          role="img"
          onPointerDown={onDown}
          onPointerMove={onMove}
          onPointerUp={onUp}
          onPointerCancel={onUp}
          onContextMenu={(e) => { e.preventDefault(); void context(e.clientX, e.clientY); }}
        />
        {/* A one-line box: keyboards send their action key as Enter. For a password field it is one
            too, so the phone's keyboard neither suggests nor learns what is typed. */}
        <input
          ref={keys}
          class="wv-keys"
          type={fieldFocus().type === "password" ? "password" : "text"}
          aria-label="Type into the page"
          value={SENTINEL}
          inputmode={fieldFocus().editable ? inputMode() : "text"}
          enterkeyhint={enterHint()}
          autocomplete="off"
          autocapitalize={fieldFocus().type === "email" || fieldFocus().type === "url" || fieldFocus().type === "password" ? "off" : "sentences"}
          spellcheck={false}
          onFocus={resetKeys}
          onKeyDown={onKeyDown}
          onKeyUp={onKeyUp}
          onInput={onInput}
          onCompositionStart={onCompositionStart}
          onCompositionEnd={onCompositionEnd}
          onPaste={onPaste}
        />
        <Show when={draft()}>
          <div class="wv-blank">
            <Icon name="globe" size={26} />
            <p>Type an address or a search above. The agent can see and use the page with you.</p>
          </div>
        </Show>
        <Show when={hostDown()}>
          {(why) => (
            <div class="board-loading wv-host-down">
              <div class="board-note" role="note">
                <span class="board-note-icon">
                  <Icon name="globe" size={22} />
                </span>
                <h3>{why()}</h3>
                <p>
                  This web view runs in the browser on {hostName()}. It comes back when {hostName()} can show it again, or open it on {serverName()} now
                  (sites you signed in to on {hostName()} aren't signed in there).
                </p>
                <div class="board-note-actions">
                  <button type="button" class="btn btn-sm btn-primary" disabled={moving()} onClick={() => void openOnServer()}>
                    Open on {serverName()}
                  </button>
                </div>
              </div>
            </div>
          )}
        </Show>
        <Show when={!draft() && !shown() && !hostDown()}>
          <div class="board-loading">
            <Show
              when={!failure() && !refused()}
              fallback={
                <div class="board-note" role="note">
                  <span class="board-note-icon">
                    <Icon name="globe" size={22} />
                  </span>
                  <h3>{refused() ? "This address isn't shown here" : "The page can't be shown"}</h3>
                  <p>{refused() ?? failure()}</p>
                  <div class="board-note-actions">
                    <Show when={url() && !refused()}>
                      <button type="button" class="btn btn-sm" onClick={openOutside}>
                        <Icon name="external" size={14} /> Open in your browser
                      </button>
                    </Show>
                  </div>
                </div>
              }
            >
              <Tessellate size={10} />
              <span>{live() ? "Loading…" : "Paused while hidden"}</span>
            </Show>
          </div>
        </Show>
        <Show when={checkHint()}>
          <div class="wv-check" role="status">
            <Icon name="shieldAlert" size={14} />
            <span class="grow">This site wants to check you're a person, which a web view can't always pass.</span>
            <Show
              when={hostId()}
              fallback={
                <button type="button" class="btn btn-xs" onClick={() => void noScreen()}>
                  Finish it yourself
                </button>
              }
            >
              <button type="button" class="btn btn-xs btn-primary" disabled={screenBusy()} onClick={() => void toScreen()}>
                Show on {hostName()}'s screen
              </button>
            </Show>
            <button type="button" class="icon-btn" aria-label="Dismiss" onClick={() => setCheckDismissed(true)}>
              <Icon name="x" size={13} />
            </button>
          </div>
        </Show>
        <Show when={onScreen() !== null}>
          <div class="board-loading wv-on-screen">
            <div class="board-note" role="note">
              <span class="board-note-icon">
                <Icon name="monitor" size={22} />
              </span>
              <h3>Open on {hostName()}'s screen</h3>
              <p>
                Finish there: the check, the sign-in, whatever the site asks. Then close that window, or come back here. What you did there stays
                (the same browser, the same sign-ins).
              </p>
              <div class="board-note-actions">
                <button type="button" class="btn btn-sm btn-primary" disabled={screenBusy()} onClick={() => void backFromScreen()}>
                  <Icon name="arrowLeft" size={14} /> Back to the tile
                </button>
              </div>
            </div>
          </div>
        </Show>
        <Show when={kbdHint()}>
          <button
            type="button"
            class="wv-kbd"
            onPointerDown={(e) => e.preventDefault()}
            onClick={() => {
              // From a tap the phone raises its keyboard.
              raiseKeyboard();
              setKbdHint(false);
            }}
          >
            <Icon name="keyboard" size={15} /> Type
          </button>
        </Show>
        <Show when={pasteOpen()}>
          <div class="wv-dialog-wrap" role="presentation">
            <form class="wv-dialog" role="dialog" aria-label="Paste into the page" onSubmit={(e) => {
              e.preventDefault(); if (pasteText()) send({ type: "text", text: pasteText() }); setPasteText(""); setPasteOpen(false); resetKeys();
            }}>
              <p class="wv-dialog-msg">Long-press the box and choose Paste, then send it to the page.</p>
              <textarea class="input" aria-label="Text to paste" maxlength={65536} value={pasteText()} onInput={(e) => setPasteText(e.currentTarget.value)} ref={(el) => requestAnimationFrame(() => el.focus())} />
              <div class="wv-dialog-actions"><button type="button" class="btn btn-sm" onClick={() => { setPasteText(""); setPasteOpen(false); }}>Cancel</button><button class="btn btn-sm btn-primary" type="submit">Paste into page</button></div>
            </form>
          </div>
        </Show>
        <Show when={tab()?.file_chooser} keyed>
          {(chooser) => (
            <div class="wv-dialog-wrap wv-file-wrap" role="presentation">
              <div class="wv-dialog wv-file-dialog" role="dialog" aria-label="Choose files for this page">
                <p class="wv-dialog-from">{hostOf(url()) || "The page"} asks for a file</p>
                <p class="wv-dialog-msg">Only files you choose are sent to this page. Up to 8 files totaling 200 MB. Keep this view open while sending.</p>
                <Show when={uploadBusy()}><p role="status">Sending… {uploadProgress()}%</p><progress aria-label="Upload progress" max={100} value={uploadProgress()} /></Show>
                <input type="file" aria-label="Choose files" multiple={!!tab()?.file_multiple} disabled={uploadBusy()}
                  onChange={(e) => { void upload(Array.from(e.currentTarget.files ?? []), chooser); }} />
                <div class="wv-dialog-actions"><button type="button" class="btn btn-sm" onClick={() => { if (uploadBusy()) uploadStop?.abort(); else void upload([], chooser); }}>Cancel</button></div>
              </div>
            </div>
          )}
        </Show>
        <Show when={dialog()}>
          {(d) => (
            <div class="wv-dialog-wrap" role="presentation">
              <form
                class="wv-dialog"
                role="alertdialog"
                aria-label="The page asks"
                onSubmit={(e) => {
                  e.preventDefault();
                  answer(true);
                }}
              >
                <p class="wv-dialog-from">{hostOf(url()) || "The page"} says</p>
                <p class="wv-dialog-msg">{d().kind === "beforeunload" ? "Leave this page? Changes you made may not be saved." : d().message}</p>
                <Show when={d().kind === "prompt"}>
                  <input class="input" value={dialogText()} onInput={(e) => setDialogText(e.currentTarget.value)} ref={(el) => requestAnimationFrame(() => el.focus())} />
                </Show>
                <div class="wv-dialog-actions">
                  <Show when={d().kind !== "alert"}>
                    <button type="button" class="btn btn-sm" onClick={() => answer(false)}>
                      {d().kind === "beforeunload" ? "Stay" : "Cancel"}
                    </button>
                  </Show>
                  <button type="submit" class="btn btn-sm btn-primary" ref={(el) => d().kind !== "prompt" && requestAnimationFrame(() => el.focus())}>
                    {d().kind === "beforeunload" ? "Leave" : "OK"}
                  </button>
                </div>
              </form>
            </div>
          )}
        </Show>
      </div>
      <Show when={props.board && touchUi() && workspace.mode() !== "tile"}>
        {/* Touches on the page are the page's: the tiles swipe from here (and the header). */}
        <div class="board-grip" aria-hidden="true">
          <i />
        </div>
      </Show>
      <span class="sr-only" aria-live="polite">
        {by() ? `Shown by ${by()}` : ""}
      </span>
    </div>
  );
}
