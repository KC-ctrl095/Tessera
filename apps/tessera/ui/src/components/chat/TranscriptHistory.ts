// Reader-only paging and search jumps stay out of the initial chat bundle.
import type { Setter } from "solid-js";
import type { Follow } from "../../lib/follow";
import { historyPaging, loadOlderHistory, type SessionState } from "../../lib/store";
import { beforePrepend, historyTo } from "../../lib/history";
import { itemSeq, type Item } from "../../lib/transcript";
import { jumpTarget, setJumpTarget, toastError } from "../../lib/ui";

export interface HistoryHost {
  session: () => SessionState;
  items: () => Item[];
  limit: () => number;
  setLimit: Setter<number>;
  scroller: HTMLDivElement;
  content: HTMLDivElement;
  follow: Follow;
  pickReading: () => void;
  restoreReading?: () => void;
  releaseReading?: () => void;
  alive: () => boolean;
}
const pending = new WeakSet<HistoryHost>();
const frame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

export async function showEarlier(h: HistoryHost): Promise<void> {
  const s = h.session();
  if (!h.alive() || pending.has(h) || s.loadingOlderHistory) return;
  pending.add(h);
  h.follow.set(false);
  let anchors: { el: HTMLElement; top: number }[] = [];
  const end = h.content.querySelector<HTMLElement>(".transcript-end")!;
  // A short initial page has empty space below it. Preserve that space too,
  // otherwise the browser clamps the requested scroll before the anchor fits.
  let space = parseFloat(end.style.flexBasis) || 0;
  const release = () => {
    end.style.flex = "";
    if (h.restoreReading === restore) h.restoreReading = undefined;
    if (h.releaseReading === release) h.releaseReading = undefined;
  };
  const restore = () => {
    if (!h.alive() || h.follow.following) return release();
    if (h.follow.gesturing) { h.restoreReading = undefined; return; }
    const anchor = anchors.find((a) => a.el.isConnected);
    if (!anchor) return;
    // Residual viewport movement, not content growth: native anchoring, when
    // enabled, has already done some or all of this work.
    const delta = anchor.el.getBoundingClientRect().top - anchor.top;
    if (Math.abs(delta) > 0.5) {
      const top = h.scroller.scrollTop + delta;
      const missing = top - (h.scroller.scrollHeight - h.scroller.clientHeight);
      if (missing > 0.5) { space += missing; end.style.flex = `0 0 ${space}px`; }
      h.follow.scrollTo(top);
    }
    h.pickReading();
  };
  const prepare = async () => {
    // A scrollTop write cancels momentum in some WebViews. Keep the fetched
    // page pending until the gesture ends, then capture immediately before it.
    while (h.alive() && h.follow.gesturing) await new Promise((r) => setTimeout(r, 40));
    if (!h.alive()) return;
    h.pickReading();
    const view = h.scroller.getBoundingClientRect();
    anchors = [];
    for (const el of h.content.querySelectorAll<HTMLElement>(":scope > .item")) {
      const r = el.getBoundingClientRect();
      if (r.bottom < view.top) continue;
      anchors.push({ el, top: r.top });
      if (anchors.length >= 8 || r.top > view.bottom) break;
    }
    h.restoreReading = restore;
    h.releaseReading = release;
  };
  const listeners = beforePrepend.get(s.info.id) ?? new Set();
  beforePrepend.set(s.info.id, listeners);
  listeners.add(prepare);
  const before = h.items().length;
  try {
    if (before > h.limit()) {
      await prepare();
      if (h.alive()) h.setLimit((n) => n + 60);
    } else if (s.hasOlderHistory) {
      const page = historyPaging.pages.get(s.info.id);
      await loadOlderHistory(s.info.id);
      if (!h.alive()) return;
      h.setLimit((n) => n + Math.max(60, h.items().length - before));
      // Hidden-only pages still advance. Continue only this explicit top-read.
      if (h.items().length <= before && s.hasOlderHistory && historyPaging.pages.get(s.info.id) !== page) {
        await frame();
        if (h.alive() && !h.follow.following && h.scroller.scrollTop < 200) {
          pending.delete(h);
          return showEarlier(h);
        }
      }
    }
    if (h.alive()) {
      restore();
      await frame();
      if (h.alive() && h.restoreReading === restore) restore();
      // The transcript's ResizeObserver continues restoring late markdown,
      // images and content-visibility layout until the reader moves again.
    }
  } catch (e) { toastError(e, "Could not load earlier messages"); }
  finally {
    listeners.delete(prepare);
    if (!listeners.size) beforePrepend.delete(s.info.id);
    pending.delete(h);
  }
}

export async function jump(h: HistoryHost, target: NonNullable<ReturnType<typeof jumpTarget>>): Promise<void> {
  const valid = () => h.alive() && jumpTarget() === target;
  try {
    await historyTo(target.sessionId, target.seq, valid);
    if (!valid()) return;
    const items = h.items(), index = items.findIndex((it) => itemSeq(it) >= target.seq);
    if (index >= 0) h.setLimit(Math.max(h.limit(), items.length - index + 10));
    await frame();
    if (!valid()) return;
    let best: HTMLElement | undefined, seq = Infinity;
    for (const el of h.content.querySelectorAll<HTMLElement>("[data-seq]")) {
      const value = Number(el.dataset.seq);
      if (value >= target.seq && value < seq) { best = el; seq = value; }
    }
    if (!best) return;
    h.follow.set(false);
    best.scrollIntoView({ block: "center", behavior: "instant" });
    h.pickReading();
    best.classList.add("flash");
    setTimeout(() => best?.classList.remove("flash"), 1800);
    if (valid()) setJumpTarget(null);
  } catch (e) { if (valid()) toastError(e, "Could not find this message"); }
}

// Expansion animation is needed only after the reader opens a history card.
export function glideBelow(rows: { el: HTMLElement; top: number }[], at: number): void {
  // A cold import must not animate back to an old position after it was painted.
  if (performance.now() - at > 32) return;
  for (const r of rows) {
    if (!r.el.isConnected) continue;
    const dy = r.top - r.el.getBoundingClientRect().top;
    if (Math.abs(dy) < 1) continue;
    r.el.animate([{ transform: `translateY(${dy}px)` }, { transform: "none" }], { duration: 200, easing: "cubic-bezier(0.16, 0.84, 0.3, 1)" });
  }
}
