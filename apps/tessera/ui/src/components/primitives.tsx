// Small UI primitives: Popover, Menu, Modal, Toggle, Segmented, Toasts, Dialogs.
import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  on,
  onCleanup,
  onMount,
  type JSX,
} from "solid-js";
import { Portal } from "solid-js/web";
import { Icon, type IconName } from "./icons";
import { dialog, dismissToast, holdToasts, setDialog, toastSlot, toasts, type Toast } from "../lib/ui";
import { viewer } from "../lib/viewer";
import { isDesktopApp } from "../lib/native";
import { copyText, touchUi } from "../lib/util";

export type Anchor = HTMLElement | { x: number; y: number };
export type Placement = "bottom-start" | "bottom-end" | "top-start" | "top-end" | "right-start";

function anchorRect(a: Anchor): DOMRect {
  if (a instanceof HTMLElement) return a.getBoundingClientRect();
  return new DOMRect(a.x, a.y, 0, 0);
}

/** Phones get bottom sheets instead of anchored popovers. */
export const sheetMode = () => typeof window !== "undefined" && window.innerWidth < 640;

/**
 * Taps and clicks inside a board's frame never reach the app (it's another
 * document), so they could never close a menu. While any popover or menu is
 * open, frames let pointer events through to the app (`layer-open` on the
 * body): a tap on a board lands here and closes it like a tap anywhere else.
 * Counted, so nested layers release frames only when the last one closes.
 */
let layers = 0;
function holdLayer(): () => void {
  layers++;
  document.body.classList.add("layer-open");
  let done = false;
  return () => {
    if (done) return;
    done = true;
    layers = Math.max(0, layers - 1);
    if (!layers) document.body.classList.remove("layer-open");
  };
}

/** A floating layer anchored to an element or a point; closes on outside click / Escape. On phones it is a bottom sheet. */
export function Popover(props: {
  anchor: Anchor;
  onClose: () => void;
  placement?: Placement;
  class?: string;
  children: JSX.Element;
  /** Match the anchor's width. */
  matchWidth?: boolean;
  label?: string;
  /** Allow the phone bottom-sheet presentation (default true). */
  sheet?: boolean;
}) {
  let el!: HTMLDivElement;
  const asSheet = props.sheet !== false && sheetMode();
  // A dialog opened over it (Settings by its shortcut, a link in the menu) closes it: the
  // dialog covers what it belongs to, and a menu left floating above a dialog would sit on top.
  const depthAtOpen = modalDepth;
  createEffect(on(modals, (n) => n > depthAtOpen && props.onClose(), { defer: true }));
  // The system back gesture closes a sheet, and a small menu on a touch device, instead of leaving the app.
  if (asSheet || props.sheet === false) onCleanup(useBackToClose(() => props.onClose()));
  const [pos, setPos] = createSignal<{ left: number; top: number; minWidth?: number; maxHeight: number }>({
    left: -9999,
    top: -9999,
    maxHeight: 400,
  });

  const place = () => {
    if (asSheet) {
      setPos({ left: 0, top: 0, maxHeight: window.innerHeight });
      return;
    }
    const r = anchorRect(props.anchor);
    const vw = window.innerWidth;
    const vh = window.visualViewport?.height ?? window.innerHeight;
    const pw = el.offsetWidth;
    const ph = el.scrollHeight;
    const m = 8;
    const pl = props.placement ?? "bottom-start";
    let left: number;
    let top: number;
    if (pl === "right-start") {
      left = r.right + 6;
      top = r.top;
    } else {
      left = pl.endsWith("end") ? r.right - pw : r.left;
      const below = vh - r.bottom - m;
      const above = r.top - m;
      const wantTop = pl.startsWith("top");
      // Prefer the requested side; flip when the other side fits the content better.
      const useTop = wantTop ? above >= ph || above > below : below < ph && above > below;
      top = useTop ? r.top - 6 - Math.min(ph, above) : r.bottom + 6;
    }
    left = Math.max(m, Math.min(left, vw - pw - m));
    const maxHeight = Math.max(160, vh - Math.max(m, top) - m);
    top = Math.max(m, Math.min(top, vh - Math.min(ph, maxHeight) - m));
    setPos({ left, top, minWidth: props.matchWidth ? r.width : undefined, maxHeight });
  };

  /**
   * The on-screen keyboard opening or closing moves what the layer hangs from (the composer
   * drops to the bottom) without resizing the window, and over several frames: follow the
   * anchor until it has stood still for a moment.
   */
  let follow = 0;
  const followAnchor = () => {
    cancelAnimationFrame(follow);
    const a = props.anchor;
    if (asSheet || !(a instanceof HTMLElement)) return;
    let last = "";
    let still = 0;
    let frames = 0;
    const step = () => {
      if (!a.isConnected) return;
      const r = a.getBoundingClientRect();
      const at = `${r.left},${r.top},${r.width},${r.height}`;
      if (at !== last) {
        last = at;
        still = 0;
        place();
      } else still++;
      if (still < 12 && ++frames < 90) follow = requestAnimationFrame(step);
    };
    follow = requestAnimationFrame(step);
  };

  onMount(() => {
    onCleanup(holdLayer());
    place();
    requestAnimationFrame(place);
    followAnchor();
    const vv = window.visualViewport;
    vv?.addEventListener("resize", followAnchor);
    onCleanup(() => {
      cancelAnimationFrame(follow);
      vv?.removeEventListener("resize", followAnchor);
    });
    // Focus going into a frame or another window closes it, like a native menu.
    const onBlur = () => props.onClose();
    window.addEventListener("blur", onBlur);
    onCleanup(() => window.removeEventListener("blur", onBlur));
    const onDown = (e: PointerEvent) => {
      const t = e.target as Node;
      if (el.contains(t)) return;
      if (props.anchor instanceof HTMLElement && props.anchor.contains(t)) return;
      props.onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        e.preventDefault();
        props.onClose();
        if (props.anchor instanceof HTMLElement) props.anchor.focus?.();
      }
    };
    const onResize = () => place();
    // Defer so the click that opened us doesn't immediately close us.
    const t = setTimeout(() => document.addEventListener("pointerdown", onDown, true));
    document.addEventListener("keydown", onKey, true);
    window.addEventListener("resize", onResize);
    const ro = new ResizeObserver(() => place());
    ro.observe(el);
    onCleanup(() => {
      clearTimeout(t);
      document.removeEventListener("pointerdown", onDown, true);
      document.removeEventListener("keydown", onKey, true);
      window.removeEventListener("resize", onResize);
      ro.disconnect();
    });
  });

  return (
    <Portal>
      <Show when={asSheet}>
        <div class="sheet-backdrop" onPointerDown={() => props.onClose()} />
      </Show>
      <div
        ref={el}
        class={"popover" + (props.class ? " " + props.class : "") + (asSheet ? " as-sheet" : "")}
        role="dialog"
        aria-label={props.label}
        style={{
          left: `${pos().left}px`,
          top: `${pos().top}px`,
          "min-width": pos().minWidth ? `${pos().minWidth}px` : undefined,
          "max-height": `${pos().maxHeight}px`,
        }}
      >
        {props.children}
      </div>
    </Portal>
  );
}

// ── Tooltips ─────────────────────────────────────────────────────────────
//
// Any element with `data-tip="Label"` (and optionally `data-kbd="Ctrl N"`)
// gets a quiet tooltip after a short hover. One global listener; nothing on
// touch devices.

export function TooltipHost() {
  const [tip, setTip] = createSignal<{ text: string; kbd?: string; x: number; y: number; below: boolean } | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  let current: HTMLElement | null = null;
  let warm = 0;
  onMount(() => {
    if (matchMedia("(hover: none)").matches) return;
    const show = (el: HTMLElement) => {
      const text = el.dataset.tip;
      if (!text || !el.isConnected) return;
      const r = el.getBoundingClientRect();
      const below = r.top < 56;
      setTip({ text, kbd: touchUi() ? undefined : el.dataset.kbd, x: r.left + r.width / 2, y: below ? r.bottom + 8 : r.top - 8, below });
      warm = Date.now();
    };
    const over = (e: PointerEvent) => {
      if (e.pointerType === "touch") return;
      const el = (e.target as HTMLElement | null)?.closest?.<HTMLElement>("[data-tip]") ?? null;
      if (el === current) return;
      current = el;
      clearTimeout(timer);
      setTip(null);
      if (!el) return;
      timer = setTimeout(() => show(el), Date.now() - warm < 600 ? 60 : 480);
    };
    const hide = () => {
      clearTimeout(timer);
      current = null;
      if (tip()) warm = Date.now();
      setTip(null);
    };
    document.addEventListener("pointerover", over, true);
    document.addEventListener("pointerdown", hide, true);
    document.addEventListener("keydown", hide, true);
    window.addEventListener("blur", hide);
    document.addEventListener("scroll", hide, true);
    onCleanup(() => {
      document.removeEventListener("pointerover", over, true);
      document.removeEventListener("pointerdown", hide, true);
      document.removeEventListener("keydown", hide, true);
      window.removeEventListener("blur", hide);
      document.removeEventListener("scroll", hide, true);
    });
  });
  let el!: HTMLDivElement;
  const [shift, setShift] = createSignal(0);
  createEffect(() => {
    const t = tip();
    if (!t || !el) return;
    requestAnimationFrame(() => {
      const w = el.offsetWidth;
      const left = t.x - w / 2;
      setShift(left < 8 ? 8 - left : left + w > window.innerWidth - 8 ? window.innerWidth - 8 - (left + w) : 0);
    });
  });
  return (
    <Show when={tip()}>
      {(t) => (
        <Portal>
          <div
            ref={el}
            class="tooltip"
            role="tooltip"
            style={{
              left: `${t().x + shift()}px`,
              top: `${t().y}px`,
              transform: `translate(-50%, ${t().below ? "0" : "-100%"})`,
            }}
          >
            <span>{t().text}</span>
            <Show when={t().kbd}>
              <For each={t().kbd!.split(" ")}>{(k) => <kbd>{k}</kbd>}</For>
            </Show>
          </div>
        </Portal>
      )}
    </Show>
  );
}

export type MenuItem =
  | {
      label: string;
      icon?: IconName;
      onSelect: () => void;
      danger?: boolean;
      /** Amber "careful" styling (YOLO). */
      warn?: boolean;
      disabled?: boolean;
      hint?: string;
      checked?: boolean;
      description?: string;
    }
  | { separator: true }
  | { heading: string }
  /** A row that opens more rows in its place, in the same menu (no second menu or sheet). */
  | { label: string; icon?: IconName; description?: string; more: MenuItem[] };

/** Keyboard-navigable menu list (use inside a Popover). */
export function Menu(props: { items: MenuItem[]; onClose: () => void; autofocus?: boolean }) {
  let el!: HTMLDivElement;
  /** "More" rows opened so far: each is replaced by its own rows. */
  const [opened, setOpened] = createSignal<MenuItem[]>([]);
  const rows = createMemo(() => props.items.flatMap((it) => ("more" in it && opened().includes(it) ? [{ separator: true } as MenuItem, ...it.more] : [it])));
  const openMore = (it: MenuItem) => {
    const before = buttons().length;
    setOpened((o) => [...o, it]);
    // The first of the new rows takes the focus (the keyboard goes on from there).
    requestAnimationFrame(() => buttons()[Math.max(0, before - 1)]?.focus());
  };
  const buttons = () => Array.from(el.querySelectorAll<HTMLButtonElement>("button.menu-item:not(:disabled)"));
  onMount(() => {
    if (props.autofocus !== false) requestAnimationFrame(() => (el.querySelector<HTMLButtonElement>("button.menu-item[aria-checked=true]") ?? buttons()[0])?.focus());
  });
  const onKey = (e: KeyboardEvent) => {
    const list = buttons();
    const i = list.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "ArrowDown") {
      e.preventDefault();
      list[(i + 1) % list.length]?.focus();
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      list[(i - 1 + list.length) % list.length]?.focus();
    } else if (e.key === "Home") {
      e.preventDefault();
      list[0]?.focus();
    } else if (e.key === "End") {
      e.preventDefault();
      list[list.length - 1]?.focus();
    } else if (e.key === "Tab") {
      props.onClose();
    }
  };
  return (
    <div class="menu" role="menu" ref={el} onKeyDown={onKey}>
      <For each={rows()}>
        {(it) =>
          "separator" in it ? (
            <div class="menu-sep" role="separator" />
          ) : "heading" in it ? (
            <div class="menu-heading">{it.heading}</div>
          ) : "more" in it ? (
            <button type="button" class="menu-item menu-more" role="menuitem" aria-expanded="false" onClick={() => openMore(it)}>
              <span class="menu-icon">
                <Icon name={it.icon ?? "chevronRight"} size={16} />
              </span>
              <span class="menu-text">
                <span class="menu-label">{it.label}</span>
                <Show when={it.description}>
                  <span class="menu-desc">{it.description}</span>
                </Show>
              </span>
            </button>
          ) : (
            <button
              type="button"
              class="menu-item"
              classList={{ danger: !!it.danger, warn: !!it.warn }}
              role={it.checked !== undefined ? "menuitemradio" : "menuitem"}
              aria-checked={it.checked !== undefined ? it.checked : undefined}
              disabled={it.disabled}
              onClick={() => {
                props.onClose();
                it.onSelect();
              }}
            >
              <span class="menu-icon">{it.icon ? <Icon name={it.icon} size={16} /> : null}</span>
              <span class="menu-text">
                <span class="menu-label">{it.label}</span>
                <Show when={it.description}>
                  <span class="menu-desc">{it.description}</span>
                </Show>
              </span>
              <Show when={it.hint}>
                <kbd class="menu-hint">{it.hint}</kbd>
              </Show>
              <Show when={it.checked}>
                <Icon name="check" size={16} class="menu-check" />
              </Show>
            </button>
          )
        }
      </For>
    </div>
  );
}

// ── global context-menu host ─────────────────────────────────────────────

interface MenuOpts {
  /** `false`: a small popover next to the anchor even on a phone, for a short menu of two or three rows. */
  sheet?: boolean;
  /** Extra class on the popover. */
  class?: string;
}

const [menuState, setMenuState] = createSignal<{ anchor: Anchor; items: MenuItem[]; placement?: Placement; opts?: MenuOpts } | null>(null);

export function openMenu(anchor: Anchor, items: MenuItem[], placement?: Placement, opts?: MenuOpts) {
  setMenuState({ anchor, items, placement, opts });
}

export function MenuHost() {
  return (
    <Show when={menuState()} keyed>
      {(m) => (
        <Popover anchor={m.anchor} placement={m.placement} onClose={() => setMenuState(null)} class={m.opts?.class ? "menu-pop " + m.opts.class : "menu-pop"} sheet={m.opts?.sheet}>
          <Menu items={m.items} onClose={() => setMenuState(null)} />
        </Popover>
      )}
    </Show>
  );
}

/** Long-press (touch) → context menu. Returns handlers to spread on an element. */
export function longPress(cb: (x: number, y: number) => void) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let sx = 0;
  let sy = 0;
  return {
    onPointerDown: (e: PointerEvent) => {
      if (e.pointerType !== "touch") return;
      sx = e.clientX;
      sy = e.clientY;
      timer = setTimeout(() => {
        navigator.vibrate?.(8);
        cb(sx, sy);
      }, 480);
    },
    onPointerMove: (e: PointerEvent) => {
      if (timer && Math.hypot(e.clientX - sx, e.clientY - sy) > 10) clearTimeout(timer);
    },
    onPointerUp: () => clearTimeout(timer),
    onPointerCancel: () => clearTimeout(timer),
  };
}

// ── Modal ────────────────────────────────────────────────────────────────

let modalDepth = 0;
/** How many modals are open (reactive: the toasts move out of their way). */
const [modals, setModals] = createSignal(0);
/**
 * WebKitGTK (the Linux desktop app) paints native scrollbars over whatever
 * is stacked above them, so a chat's scrollbar showed through Settings:
 * while a modal is open, the app under it hides its scrollbars (`modal-open`).
 */
const coverScrollbars = isDesktopApp && /Linux/.test(navigator.userAgent);
/** Open modals, innermost last: Escape closes the top one even when focus has wandered off it. */
// ── Back button closes the top overlay (Android back, browser back on phones) ──
const backStack: { close: () => void; token: number }[] = [];
let backToken = 0;
let ignorePop = 0;
/**
 * Overlays opened while our own `history.back()` is still on its way (one
 * overlay handing over to the next: the tile overview to all mosaics, the
 * drawer to a page). Their entries are pushed once it has landed; pushed
 * earlier, the back would take theirs instead and the next back gesture
 * would leave the app.
 */
const deferred: number[] = [];
const pushEntry = (token: number) => history.pushState({ tesseraOverlay: token }, "");
/**
 * Two overlays closing at once (a menu in the drawer, and the drawer): the
 * second one's entry isn't on top while the first one's back is on its way,
 * so it stays behind. Once that back has landed, take such an entry out too;
 * left there, the next back gesture would do nothing, and the one after
 * would close whatever opened since (a web view's page went back to empty).
 */
const dropStale = () => {
  const t = (history.state as { tesseraOverlay?: number } | null)?.tesseraOverlay;
  if (t === undefined || backStack.some((b) => b.token === t)) return false;
  ignorePop++;
  history.back();
  return true;
};
if (typeof window !== "undefined") {
  window.addEventListener("popstate", () => {
    if (ignorePop > 0) {
      ignorePop--;
      if (!ignorePop && !dropStale()) for (const t of deferred.splice(0)) if (backStack.some((b) => b.token === t)) pushEntry(t);
      return;
    }
    const top = backStack.pop();
    top?.close();
  });
}
/**
 * While an overlay (sheet, dialog, drawer, overview) is open on a touch
 * device, the system back gesture closes it instead of leaving the app.
 * Returns a function to call when the overlay closes by other means.
 */
export function useBackToClose(close: () => void): () => void {
  if (!touchUi() || typeof history === "undefined") return () => {};
  const token = ++backToken;
  backStack.push({ close, token });
  if (ignorePop > 0) deferred.push(token);
  else pushEntry(token);
  return () => {
    const i = backStack.findIndex((b) => b.token === token);
    if (i < 0) return; // already closed by back
    backStack.splice(i, 1);
    if ((history.state as { tesseraOverlay?: number } | null)?.tesseraOverlay === token) {
      ignorePop++;
      history.back();
    }
  };
}

const modalStack: (() => void)[] = [];
if (typeof window !== "undefined") {
  // On `window`, so it always runs after the page's own (document-delegated) key
  // handlers: a chart or field that used Escape has marked it handled by then. On
  // `document` the order followed which module loaded first, and lazy loading
  // changed that (Escape on the Usage chart closed Settings).
  window.addEventListener("keydown", (e) => {
    if (e.key !== "Escape" || e.defaultPrevented || !modalStack.length) return;
    e.preventDefault();
    modalStack[modalStack.length - 1]();
  });
}

export function Modal(props: {
  onClose: () => void;
  title?: string;
  class?: string;
  children: JSX.Element;
  size?: "sm" | "md" | "lg" | "full";
  hideHeader?: boolean;
  initialFocus?: string;
  /** The content handles the back gesture itself (Settings on a phone: back goes up a level first). */
  ownBack?: boolean;
}) {
  let panel!: HTMLDivElement;
  const prevFocus = document.activeElement as HTMLElement | null;
  const close = () => props.onClose();
  onMount(() => {
    modalDepth++;
    setModals(modalDepth);
    if (coverScrollbars) document.documentElement.classList.add("modal-open");
    modalStack.push(close);
    if (!props.ownBack) onCleanup(useBackToClose(close));
    const target =
      (props.initialFocus && panel.querySelector<HTMLElement>(props.initialFocus)) ||
      panel.querySelector<HTMLElement>("[autofocus], input:not([type=hidden]), textarea, select") ||
      panel.querySelector<HTMLElement>("button:not(.modal-close)") ||
      panel;
    requestAnimationFrame(() => target.focus());
    onCleanup(() => {
      modalDepth--;
      setModals(modalDepth);
      if (!modalDepth) document.documentElement.classList.remove("modal-open");
      const i = modalStack.lastIndexOf(close);
      if (i >= 0) modalStack.splice(i, 1);
      prevFocus?.focus?.();
    });
  });
  const onKey = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      // Already handled (the document-wide Escape closed a dialog above this one, or a field
      // inside used it): the delegated event still reaches this panel through that dialog's
      // portal, and must not close this one too.
      if (e.defaultPrevented) return;
      // Not the top dialog (the keyboard is still under the one just opened above): the
      // document-wide Escape closes that one instead.
      if (modalStack[modalStack.length - 1] !== close) return;
      // Handled: the document-wide Escape (modalStack) must not close the
      // modal under this one too.
      e.preventDefault();
      e.stopPropagation();
      props.onClose();
    } else if (e.key === "Tab") {
      const f = Array.from(
        panel.querySelectorAll<HTMLElement>(
          'button:not(:disabled), [href], input:not(:disabled), select, textarea, [tabindex]:not([tabindex="-1"])',
        ),
      ).filter((x) => x.offsetParent !== null);
      if (!f.length) return;
      const first = f[0];
      const last = f[f.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  };
  return (
    <Portal>
      <div
        class="modal-backdrop"
        onPointerDown={(e) => {
          if (e.target === e.currentTarget) props.onClose();
        }}
      >
        <div
          ref={panel}
          class={`modal modal-${props.size ?? "md"}` + (props.class ? " " + props.class : "")}
          role="dialog"
          aria-modal="true"
          aria-label={props.title}
          tabIndex={-1}
          onKeyDown={onKey}
        >
          <Show when={!props.hideHeader && props.title}>
            <header class="modal-head">
              <h2>{props.title}</h2>
              <button type="button" class="icon-btn modal-close" aria-label="Close" data-tip="Close" data-kbd="Esc" onClick={props.onClose}>
                <Icon name="x" />
              </button>
            </header>
          </Show>
          {props.children}
        </div>
      </div>
    </Portal>
  );
}

export const anyModalOpen = () => modalDepth > 0;

// ── form bits ────────────────────────────────────────────────────────────

export function Toggle(props: { checked: boolean; onChange: (v: boolean) => void; label: string; disabled?: boolean; busy?: boolean }) {
  return (
    <button
      type="button"
      role="switch"
      class="toggle"
      aria-checked={props.checked}
      aria-label={props.label}
      disabled={props.disabled}
      aria-disabled={props.disabled || props.busy}
      aria-busy={props.busy}
      onClick={() => !props.busy && props.onChange(!props.checked)}
    >
      <span class="toggle-knob" />
    </button>
  );
}

export function Segmented<T extends string>(props: {
  value: T;
  options: { value: T; label: string; icon?: IconName; warn?: boolean }[];
  onChange: (v: T) => void;
  label: string;
  class?: string;
}) {
  return (
    <div class={"segmented" + (props.class ? " " + props.class : "")} role="radiogroup" aria-label={props.label}>
      {/* Choice values stay the same when a settings reply brings fresh option objects. */}
      <For each={props.options.map((o) => o.value)}>
        {(value) => {
          const o = () => props.options.find((o) => o.value === value)!;
          return (
            <button
              type="button"
              role="radio"
              aria-checked={props.value === value}
              classList={{ active: props.value === value, warn: !!o().warn }}
              onClick={() => props.onChange(value)}
            >
              <Show when={o().icon}>
                <Icon name={o().icon!} size={15} />
              </Show>
              <span>{o().label}</span>
            </button>
          );
        }}
      </For>
    </div>
  );
}

export function CopyButton(props: { text: () => string | Promise<string>; label?: string; class?: string; size?: number }) {
  const [done, setDone] = createSignal(false);
  let t: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(t));
  return (
    <button
      type="button"
      class={"icon-btn" + (props.class ? " " + props.class : "")}
      aria-label={props.label ?? "Copy"}
      data-tip={done() ? "Copied" : props.label ?? "Copy"}
      onClick={async (e) => {
        e.stopPropagation();
        if (await copyText(await props.text())) {
          setDone(true);
          clearTimeout(t);
          t = setTimeout(() => setDone(false), 1400);
        }
      }}
    >
      <Icon name={done() ? "check" : "copy"} size={props.size ?? 15} />
    </button>
  );
}

// ── Toasts ───────────────────────────────────────────────────────────────

const toastIcon = (t: Toast): IconName =>
  t.level === "error" ? "alertCircle" : t.level === "warning" ? "alert" : t.level === "success" ? "circleCheck" : t.from ? "bell" : "info";

/**
 * A toast under a finger: swiped sideways (or up, where toasts come from the
 * top) it goes; while a finger or the pointer is on it, no toast times out.
 * Returns whether the last touch was a swipe (its click is no tap).
 */
function toastGestures(el: HTMLElement, opts: { up: () => boolean; dismiss: () => void }): () => boolean {
  let start: { x: number; y: number; id: number; axis: "x" | "y" | null } | null = null;
  let swiped = false;
  let hovering = false;
  const settle = (gone: string | null) => {
    el.style.transition = "transform 160ms var(--ease-out), opacity 160ms var(--ease-out)";
    el.style.transform = gone ?? "";
    el.style.opacity = gone ? "0" : "";
    if (gone) setTimeout(opts.dismiss, 150);
  };
  el.addEventListener("pointerenter", () => {
    if (!hovering) holdToasts(true);
    hovering = true;
  });
  el.addEventListener("pointerleave", () => {
    if (hovering) holdToasts(false);
    hovering = false;
  });
  onCleanup(() => hovering && holdToasts(false));
  el.addEventListener("pointerdown", (e) => {
    swiped = false;
    // Swipes are for fingers; a mouse selects text and clicks the ✕.
    if (e.pointerType !== "touch" || (e.target as Element).closest("button")) return;
    start = { x: e.clientX, y: e.clientY, id: e.pointerId, axis: null };
  });
  el.addEventListener("pointermove", (e) => {
    if (!start || e.pointerId !== start.id) return;
    const dx = e.clientX - start.x;
    const dy = e.clientY - start.y;
    if (!start.axis) {
      if (Math.hypot(dx, dy) < 8) return;
      start.axis = Math.abs(dx) > Math.abs(dy) ? "x" : "y";
      el.setPointerCapture(e.pointerId);
      el.style.transition = "none";
    }
    if (start.axis === "x") {
      el.style.transform = `translateX(${dx}px)`;
      el.style.opacity = String(Math.max(0.15, 1 - Math.abs(dx) / 260));
    } else if (opts.up()) {
      el.style.transform = `translateY(${Math.min(0, dy)}px)`;
      el.style.opacity = String(Math.max(0.15, 1 + Math.min(0, dy) / 90));
    }
  });
  const end = (e: PointerEvent, cancel: boolean) => {
    if (!start || e.pointerId !== start.id) return;
    const { axis, x, y } = start;
    start = null;
    if (!axis) return;
    swiped = true;
    const dx = e.clientX - x;
    const dy = e.clientY - y;
    if (!cancel && axis === "x" && Math.abs(dx) > 60) settle(`translateX(${dx > 0 ? 110 : -110}%)`);
    else if (!cancel && axis === "y" && opts.up() && dy < -24) settle("translateY(-120%)");
    else settle(null);
  };
  el.addEventListener("pointerup", (e) => end(e, false));
  el.addEventListener("pointercancel", (e) => end(e, true));
  return () => swiped;
}

/** A tap on a toast (not on its buttons): an agent's message opens its chat. */
function tapToast(t: Toast, e: MouseEvent, swiped: () => boolean): boolean {
  if (swiped() || (e.target as Element).closest("button") || !t.open) return false;
  t.open();
  dismissToast(t.id);
  return true;
}

function ToastActions(props: { t: Toast; more?: JSX.Element }) {
  return (
    <>
      <Show when={props.t.action}>
        {(a) => (
          <button
            type="button"
            class="btn btn-sm"
            onClick={() => {
              a().run();
              dismissToast(props.t.id);
            }}
          >
            {a().label}
          </button>
        )}
      </Show>
      {props.more}
      <button type="button" class="icon-btn toast-x" aria-label="Dismiss" onClick={() => dismissToast(props.t.id)}>
        <Icon name="x" size={15} />
      </button>
    </>
  );
}

/**
 * A toast in full: who it is from, its title and message, its action. One
 * that opens something (an agent's message) or an error takes a phone's
 * taps and swipes whole (`toast-grab`); any other (a notice, an Undo) lets
 * taps through to what's under it but its own buttons, as before: it sits
 * over the tile header, which must keep working while it shows.
 */
function ToastCard(props: { t: Toast; up: () => boolean; more?: JSX.Element }) {
  const t = props.t;
  let swiped = () => false;
  return (
    <div
      ref={(el) => (swiped = toastGestures(el, { up: props.up, dismiss: () => dismissToast(t.id) }))}
      class={`toast toast-${t.level}`}
      classList={{ "toast-opens": !!t.open, "toast-grab": !!t.open || t.level === "error" }}
      role={t.level === "error" ? "alert" : "status"}
      data-toast={t.id}
      onClick={(e) => tapToast(t, e, swiped)}
    >
      <Icon name={toastIcon(t)} size={17} class="toast-icon" />
      <div class="toast-body">
        <Show when={t.from}>
          <div class="toast-from">{t.from}</div>
        </Show>
        <Show when={t.title}>
          <div class="toast-title">{t.title}</div>
        </Show>
        <div class="toast-msg">{t.message}</div>
      </div>
      <ToastActions t={t} more={props.more} />
    </div>
  );
}

/**
 * A toast as one line in the top bar; only the newest shows. A tap opens
 * what it is about, or shows it whole with the others under the bar.
 */
function ToastPill(props: { t: Toast; newest: boolean; count: number; open: boolean; onToggle: () => void }) {
  const t = props.t;
  let line!: HTMLSpanElement;
  let swiped = () => false;
  return (
    <div
      ref={(el) => (swiped = toastGestures(el, { up: () => true, dismiss: () => dismissToast(t.id) }))}
      class={`toast toast-pill toast-${t.level}`}
      classList={{ "toast-opens": !!t.open, newest: props.newest }}
      role={t.level === "error" ? "alert" : "status"}
      data-toast={t.id}
      onClick={(e) => {
        if (tapToast(t, e, swiped) || swiped() || (e.target as Element).closest("button")) return;
        // Cut short, or more behind it: show them all.
        if (props.open || props.count > 1 || line.scrollWidth > line.clientWidth + 1) props.onToggle();
      }}
    >
      <Icon name={toastIcon(t)} size={15} class="toast-icon" />
      <span class="toast-body toast-line" ref={line}>
        <Show when={t.from}>
          <span class="toast-from">{t.from}</span>
        </Show>
        <Show when={t.title}>
          <span class="toast-title">{t.title}</span>
        </Show>
        <span class="toast-msg">{t.message}</span>
      </span>
      <ToastActions
        t={t}
        more={
          <Show when={props.newest && props.count > 1}>
            <button
              type="button"
              class="toast-more"
              aria-expanded={props.open}
              aria-label={props.open ? "Hide notifications" : `${props.count - 1} more notification${props.count > 2 ? "s" : ""}`}
              onClick={props.onToggle}
            >
              <Show when={!props.open} fallback={<Icon name="chevronUp" size={14} />}>
                +{props.count - 1}
              </Show>
            </button>
          </Show>
        }
      />
    </div>
  );
}

/**
 * Where toasts show. With the top bar (an unfolded phone, a computer) the
 * newest sits in the bar's free middle as one line, never over a tile, with
 * a count for the others that drops them down as a short list. Without it
 * (a folded phone, sign-in) and over a dialog or the viewer they float: at
 * the top on a phone, the newest only, with the same count; in the corner
 * on a computer.
 */
export function Toasts() {
  let host!: HTMLDivElement;
  const [slot, setSlot] = createSignal<{ x: number; y: number; w: number; h: number } | null>(null);
  createEffect(() => {
    const el = toastSlot();
    if (!el) return setSlot(null);
    const measure = () => {
      const r = el.getBoundingClientRect();
      setSlot(r.width > 0 && r.height > 0 ? { x: r.left, y: r.top, w: r.width, h: r.height } : null);
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    window.addEventListener("resize", measure);
    onCleanup(() => {
      ro.disconnect();
      window.removeEventListener("resize", measure);
    });
  });
  const bar = () => (modals() === 0 && !viewer() ? slot() : null);
  const [open, setOpen] = createSignal(false);
  createEffect(() => {
    if (!toasts().length) setOpen(false);
  });
  // Open: a tap elsewhere (or Escape, in the bar) puts the list away.
  createEffect(() => {
    if (!open()) return;
    const away = (e: PointerEvent) => {
      if (!host.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || !bar() || e.defaultPrevented) return;
      // Only the list closes (not a chat's run, not the tile).
      e.preventDefault();
      e.stopPropagation();
      setOpen(false);
    };
    document.addEventListener("pointerdown", away, true);
    document.addEventListener("keydown", esc, true);
    onCleanup(() => {
      document.removeEventListener("pointerdown", away, true);
      document.removeEventListener("keydown", esc, true);
    });
  });
  const toggle = () => setOpen((o) => !o);
  // The list under the bar: centred on it, inside the window.
  const list = () => {
    const b = bar()!;
    const w = Math.min(420, window.innerWidth - 16);
    const x = Math.min(Math.max(8, b.x + b.w / 2 - w / 2), window.innerWidth - 8 - w);
    return { "--list-x": `${x - b.x}px`, "--list-w": `${w}px` };
  };
  // Floating at the top (a phone, over a dialog): toasts come from there, and go back up.
  const fromTop = () => window.innerWidth <= 700 ? !document.documentElement.classList.contains("has-viewer") : modals() > 0;
  return (
    <div
      ref={host}
      class="toasts"
      classList={{ "over-modal": modals() > 0, "in-bar": !!bar(), open: open() }}
      style={
        bar()
          ? { "--bar-x": `${bar()!.x}px`, "--bar-y": `${bar()!.y}px`, "--bar-w": `${bar()!.w}px`, "--bar-h": `${bar()!.h}px` }
          : undefined
      }
      role="region"
      aria-live="polite"
      aria-label="Notifications"
    >
      <Show
        when={bar()}
        fallback={
          <For each={toasts()}>
            {(t, i) => (
              <ToastCard
                t={t}
                up={fromTop}
                more={
                  // A phone shows the newest only: the count shows (and opens) the others.
                  <Show when={i() === toasts().length - 1 && toasts().length > 1}>
                    <button
                      type="button"
                      class="toast-more"
                      aria-expanded={open()}
                      aria-label={open() ? "Show the newest only" : `${toasts().length - 1} more notification${toasts().length > 2 ? "s" : ""}`}
                      onClick={toggle}
                    >
                      <Show when={!open()} fallback={<Icon name="chevronUp" size={14} />}>
                        +{toasts().length - 1}
                      </Show>
                    </button>
                  </Show>
                }
              />
            )}
          </For>
        }
      >
        <For each={toasts()}>
          {(t, i) => <ToastPill t={t} newest={i() === toasts().length - 1} count={toasts().length} open={open()} onToggle={toggle} />}
        </For>
        <Show when={open()}>
          <div class="toast-list" style={list()}>
            <For each={[...toasts()].reverse()}>{(t) => <ToastCard t={t} up={() => false} />}</For>
          </div>
        </Show>
      </Show>
    </div>
  );
}

// ── confirm / prompt ─────────────────────────────────────────────────────

export function DialogHost() {
  return (
    <Show when={dialog()} keyed>
      {(d) => {
        const [value, setValue] = createSignal(d.kind === "prompt" ? d.value ?? "" : "");
        const close = (ok: boolean) => {
          setDialog(null);
          if (d.kind === "confirm") d.resolve(ok);
          else d.resolve(ok ? value() : null);
        };
        let input: HTMLInputElement | undefined;
        createEffect(() => {
          if (input) requestAnimationFrame(() => input!.select());
        });
        return (
          <Modal onClose={() => close(false)} title={d.title} size="sm" initialFocus={d.kind === "confirm" ? ".btn-primary, .btn-danger" : "input"}>
            <form
              class="modal-body dialog-body"
              onSubmit={(e) => {
                e.preventDefault();
                close(true);
              }}
            >
              <Show when={d.body}>
                <p class="dialog-text">{d.body}</p>
              </Show>
              <Show when={d.kind === "prompt"}>
                <label class="field">
                  <Show when={(d as { label?: string }).label}>
                    <span class="field-label">{(d as { label?: string }).label}</span>
                  </Show>
                  <input
                    ref={input}
                    class="input"
                    type={(d as { secret?: boolean }).secret ? "password" : "text"}
                    autocomplete={(d as { secret?: boolean }).secret ? "new-password" : undefined}
                    value={value()}
                    placeholder={(d as { placeholder?: string }).placeholder}
                    onInput={(e) => setValue(e.currentTarget.value)}
                  />
                </label>
              </Show>
              <div class="dialog-actions">
                <button type="button" class="btn btn-ghost" onClick={() => close(false)}>
                  {(d.kind === "confirm" && d.cancelLabel) || "Cancel"}
                </button>
                <button type="submit" class={d.kind === "confirm" && d.danger ? "btn btn-danger" : "btn btn-primary"}>
                  {d.confirmLabel ?? (d.kind === "confirm" ? "Confirm" : "Save")}
                </button>
              </div>
            </form>
          </Modal>
        );
      }}
    </Show>
  );
}
