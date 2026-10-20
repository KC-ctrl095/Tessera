// Follow the end until a reader's gesture leaves it. Scroll events alone are not intent:
// engines send them after layout clamping, anchoring and programmatic moves, sometimes delayed
// or coalesced. Only wheel, touch, scrollbar/selection drag or paging keys arm a reader scroll.
// During a gesture (including its momentum), never move the scroller. A gesture ending within
// `repin` of its painted end resumes following, even if output arrived before the scroll event.

export interface FollowOptions {
  /** Within this distance of the end, a finished scroll gesture re-attaches (px). */
  repin?: number;
  /** Following started or stopped. */
  onChange?: (following: boolean) => void;
  /** Right before following stops (the reader's view is still where it was). */
  onDetach?: () => void;
  /** After the detached view moved (at most once a frame), including an explicit search jump. */
  onReaderScroll?: () => void;
}

/** How long after the last scroll event a gesture counts as over (ms). */
const SETTLE_MS = 140;

export class Follow {
  following = true;
  /** A finger is on the scroller / the scrollbar is held. */
  touching = false;
  dragging = false;
  private repin: number;
  /** Our last write remains recognizable through duplicate/delayed scroll events. */
  private own = Number.NaN;
  private lastTop = 0;
  private reader = false;
  private gestureEnd = 0;
  private pressY: number | null = null;
  /** Gap at a genuine reader scroll, not at a later layout clamp. */
  private gapAtScroll = Number.POSITIVE_INFINITY;
  private end = 0;
  private beforeResize: { top: number; gap: number } | null = null;
  /** Cumulative touch travel, so a slow drag in either direction counts. */
  private startY = 0;
  private lastY = 0;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private raf = 0;
  private off: (() => void)[] = [];
  /** Where the touches now on the scroller started (their events go there). */
  private watched = new Map<EventTarget, () => void>();

  constructor(
    readonly el: HTMLElement,
    private o: FollowOptions = {},
  ) {
    this.repin = o.repin ?? 24;
    this.lastTop = el.scrollTop;
    this.end = Math.max(0, el.scrollHeight - el.clientHeight);
    const on = <K extends keyof HTMLElementEventMap>(t: EventTarget, type: K | string, fn: (e: never) => void, opts?: AddEventListenerOptions) => {
      t.addEventListener(type, fn as EventListener, opts);
      this.off.push(() => t.removeEventListener(type, fn as EventListener, opts));
    };
    on(el, "wheel", (e: WheelEvent) => {
      if (e.ctrlKey || !e.deltaY || Math.abs(e.deltaY) < Math.abs(e.deltaX)) return;
      if (this.consumed(e.target, e.deltaY)) return;
      this.gesture();
    }, { passive: true });
    // Pointer moves too: some engines send no touch moves once the browser scrolls by itself.
    const fingerAt = (y: number, target: EventTarget | null) => {
      const dy = this.lastY - y;
      this.lastY = y;
      if (Math.abs(y - this.startY) > 3 && !this.consumed(target, dy)) this.gesture();
    };
    const touchMove = (e: TouchEvent) => {
      if (e.touches.length === 1) fingerAt(e.touches[0].clientY, e.target);
    };
    const touchEnd = (e: TouchEvent) => {
      if (e.touches.length) return;
      this.touching = false;
      this.unwatchTouch();
      if (this.reader) this.settle();
      else this.stick();
    };
    on(el, "touchstart", (e: TouchEvent) => {
      if (!this.touching) this.startY = this.lastY = e.touches[0]?.clientY ?? 0;
      this.touching = true;
      // A touch's events keep going to where it started, even once that is gone from the page (the
      // block being written is redrawn under the finger) and they no longer reach the scroller:
      // listen there.
      const t = e.target;
      if (t && !this.watched.has(t)) {
        const opts = { passive: true };
        t.addEventListener("touchmove", touchMove as EventListener, opts);
        t.addEventListener("touchend", touchEnd as EventListener, opts);
        t.addEventListener("touchcancel", touchEnd as EventListener, opts);
        this.watched.set(t, () => {
          t.removeEventListener("touchmove", touchMove as EventListener);
          t.removeEventListener("touchend", touchEnd as EventListener);
          t.removeEventListener("touchcancel", touchEnd as EventListener);
        });
      }
    }, { passive: true });
    on(el, "pointermove", (e: PointerEvent) => {
      if (this.touching && e.pointerType !== "mouse" && e.isPrimary) fingerAt(e.clientY, e.target);
    }, { passive: true });
    on(el, "keydown", (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (t.closest("textarea, input, [contenteditable], select")) return;
      if (e.key === "PageUp" || e.key === "ArrowUp" || e.key === "Home" || (e.key === " " && e.shiftKey)) this.gesture();
      // End: "take me to the latest" (a smooth scroll there would land short of a growing end).
      else if (e.key === "End") {
        e.preventDefault();
        this.attach();
      }
      else if (e.key === "PageDown" || e.key === "ArrowDown" || e.key === " ") this.gesture();
    });
    on(el, "pointerdown", (e: PointerEvent) => {
      if (e.button !== 0 || e.pointerType !== "mouse") return;
      this.pressY = e.clientY;
      // Native/overlay scrollbar; an ordinary click on a card does not pause following.
      if (e.target === el && e.clientX >= el.getBoundingClientRect().right - 16) {
        this.dragging = true;
        this.gesture();
      }
    });
    on(window, "pointermove", (e: PointerEvent) => {
      // A selection drag, including outside the transcript's edge.
      if (this.pressY !== null && e.pointerType === "mouse" && e.buttons === 1 && Math.abs(e.clientY - this.pressY) > 3) {
        this.dragging = true;
        this.gesture();
      }
    }, { passive: true });
    const up = () => {
      this.pressY = null;
      if (!this.dragging) return;
      this.dragging = false;
      this.settle();
    };
    on(window, "pointerup", up);
    on(window, "pointercancel", up);
    on(el, "scroll", () => this.onScroll(), { passive: true });
  }

  /** Wait for touch momentum and wheel/key paging to finish before installing older rows. */
  get gesturing() { return this.touching || this.dragging || this.reader; }
  /** Distance from the end (px). */
  gap() {
    return this.el.scrollHeight - this.el.scrollTop - this.el.clientHeight;
  }

  /** Moves the scroller ourselves (not the reader). */
  scrollTo(top: number) {
    this.el.scrollTop = top;
    this.own = this.el.scrollTop;
    this.lastTop = this.own;
  }

  /**
   * To the end, when following. Never while a finger is on the scroller: the browser may be
   * scrolling for it before any touch event says so, and a move of ours would undo the reader's.
   */
  stick() {
    this.observeEnd();
    if (!this.following || this.touching || this.dragging || this.reader) return;
    const max = this.end;
    if (max - this.el.scrollTop > 0.5) this.scrollTo(max);
  }

  /** Follow the end again (the reader asked: "jump to latest", sending a message). */
  attach() {
    clearTimeout(this.timer);
    this.reader = false;
    this.set(true);
    this.stick();
    // Cancel an older smooth-scroll animation even if it has not moved off the end yet.
    if (!this.touching && !this.dragging) this.scrollTo(this.end);
  }

  /** The reader is reading: stop following (nothing to scroll: stay). */
  detach() {
    if (this.el.scrollHeight <= this.el.clientHeight + 1) return;
    clearTimeout(this.timer);
    if (this.following) this.o.onDetach?.();
    this.set(false);
  }

  /** Sets the state without moving anything. */
  set(v: boolean) {
    if (v === this.following) return;
    this.following = v;
    // A new state: only the reader's scrolls from now on count for coming back.
    this.gapAtScroll = Number.POSITIVE_INFINITY;
    this.end = Math.max(0, this.el.scrollHeight - this.el.clientHeight);
    this.beforeResize = null;
    this.o.onChange?.(v);
  }

  dispose() {
    clearTimeout(this.timer);
    cancelAnimationFrame(this.raf);
    for (const f of this.off) f();
    this.off = [];
    this.unwatchTouch();
  }

  private unwatchTouch() {
    for (const off of this.watched.values()) off();
    this.watched.clear();
  }

  /** Ignore a gesture consumed inside a code block, tool output or another nested scroller. */
  private consumed(target: EventTarget | null, dy: number) {
    for (let n = target instanceof Element ? target : null; n && n !== this.el; n = n.parentElement) {
      if (n instanceof HTMLElement && n.scrollHeight > n.clientHeight + 1 && /auto|scroll/.test(getComputedStyle(n).overflowY)) {
        if (dy < 0 ? n.scrollTop > 0 : n.scrollTop + n.clientHeight < n.scrollHeight - 1) return true;
      }
    }
    return false;
  }

  private gesture() {
    if (!this.reader) {
      this.gapAtScroll = Number.POSITIVE_INFINITY;
      this.beforeResize = null;
      this.end = Math.max(0, this.el.scrollHeight - this.el.clientHeight);
      this.gestureEnd = this.end;
    }
    this.reader = true;
    this.settle();
  }

  /** One timer per gesture, not a polling loop; scroll events extend it through momentum. */
  private settle() {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      this.timer = undefined;
      if (this.touching || this.dragging) return;
      // Some engines deliver the gesture's scroll event after this timer. Account for its
      // actual position first, while intent is still armed, rather than snapping it back.
      if (this.reader && Math.abs(this.el.scrollTop - this.lastTop) > 0.5) {
        this.onScroll();
        if (this.timer !== undefined) return;
      }
      this.reader = false;
      // Layout alone never reattaches a reader. Use where their last scroll landed.
      if (!this.following && this.gapAtScroll <= this.repin) this.attach();
      else this.stick();
    }, SETTLE_MS);
  }

  /** Remember a return to the painted end before a same-frame content burst hides it. */
  private observeEnd() {
    const top = this.el.scrollTop;
    const end = Math.max(0, this.el.scrollHeight - this.el.clientHeight);
    if (this.reader && end >= this.end && Math.abs(top - this.lastTop) > 0.5) {
      const gap = Math.max(0, this.end - top);
      this.beforeResize = { top, gap: Math.min(gap, this.beforeResize?.top === top ? this.beforeResize.gap : Infinity) };
    }
    this.end = end;
  }

  private onScroll() {
    const top = this.el.scrollTop;
    const end = Math.max(0, this.el.scrollHeight - this.el.clientHeight);
    const mine = Math.abs(top - this.own) < 1 && Math.abs(top - this.lastTop) < 1;
    const up = top < this.lastTop - 0.5;
    const down = top > this.lastTop + 0.5;
    const gap = Math.max(0, end - top);
    const readerGap = this.following ? Math.max(0, this.gestureEnd - top)
      : (up || down) && end >= this.end
        ? Math.min(gap, Math.max(0, this.end - top), this.beforeResize?.top === top ? this.beforeResize.gap : Infinity)
        : gap;
    const clamped = up && gap < 1;
    this.beforeResize = null;
    this.end = end;
    this.lastTop = top;
    if (mine || clamped) return;
    if (!this.reader) {
      // An unowned scroll is still not user intent. Correct it without changing follow state.
      this.stick();
      if (!this.following && (up || down)) this.readingMoved();
      return;
    }
    if (!up && !down) return;
    if (this.following && readerGap > this.repin) this.detach();
    this.gapAtScroll = readerGap;
    this.settle();
    if (!this.following) this.readingMoved();
  }

  private readingMoved() {
    if (this.o.onReaderScroll && !this.raf) {
      // Capture the new view before ResizeObserver handles deferred row layout.
      // Waiting another frame can restore an anchor from before scrollIntoView.
      this.o.onReaderScroll();
      this.raf = requestAnimationFrame(() => { this.raf = 0; });
    }
  }
}
