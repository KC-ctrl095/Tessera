// Coach marks after setup: five one-liners, each pointing at the thing it
// explains. Each is dismissed for good (prefs.hints["tour.<id>"]); "Skip tips"
// dismisses the rest. Settings → Help & tour (Take the tour, Show hints again) replays them.
import { Show, createEffect, createSignal, on, onCleanup } from "solid-js";
import { produce } from "solid-js/store";
import "../styles/tour.css";
import { dismissHint, hintSeen, setPrefs } from "../lib/prefs";
import { setTourStarted, setupOpen } from "../lib/ui";
import { touchUi } from "../lib/util";
import { workspace } from "../lib/workspace";
import { Icon } from "./icons";

interface Mark {
  id: string;
  /** Candidate targets, first visible wins. */
  at: string[];
  text: () => string;
}

const ALT = /Mac|iPhone|iPad/.test(navigator.platform) ? "⌥" : "Alt";
const phone = () => workspace.mode() !== "tile" || touchUi();

const MARKS: Mark[] = [
  {
    id: "new",
    at: [".topbar .tb-new", ".pane.focused .head-new", ".head-new"],
    text: () =>
      "Everything new starts here: a chat, a private chat that is never saved, a note or a web view. More holds bots, scheduled tasks, a terminal and new mosaics (screens of tiles).",
  },
  {
    id: "mosaic",
    at: [".ws-strip .ws-pill", ".pane.focused .head-overview"],
    text: () =>
      phone()
        ? "Every chat is a tile, and tiles make a mosaic. Tap here to see all of them."
        : `Every chat is a tile, and tiles make a mosaic: a screen of tiles. Switch between them here (${ALT} 1–9).`,
  },
  {
    id: "options",
    at: [".pane.focused .opts-btn", ".opts-btn", ".pane.focused .title-menu"],
    text: () => "A chat's options: how much freedom it has, its folder, and more. Its model is beside it.",
  },
  {
    id: "slash",
    at: [".pane.focused .composer textarea", ".composer textarea"],
    text: () => "Type / for commands and skills, and @ to bring a bot into the chat.",
  },
  {
    id: "move",
    at: [".pane.focused .pane-head", ".pane-head"],
    text: () =>
      phone()
        ? "Swipe sideways to move between tiles. In the overview, press and hold a tile to move it."
        : "Drag a tile by its title bar to move it: drop on an edge to split, in the middle to swap.",
  },
];

const [current, setCurrent] = createSignal<number | null>(null);

const nextUnseen = (from: number) => {
  for (let i = from; i < MARKS.length; i++) if (!hintSeen(`tour.${MARKS[i].id}`)) return i;
  return null;
};

/**
 * Start (or replay) the coach marks from the first one not dismissed yet. Only Setup and
 * Settings start them, so this file loads with those; the app shows <Tour /> from then on.
 */
export function startTour(replay = false) {
  if (replay)
    setPrefs(
      "hints",
      produce((h) => {
        for (const m of MARKS) delete h[`tour.${m.id}`];
      }),
    );
  setCurrent(nextUnseen(0));
  setTourStarted(true);
}

function visible(el: Element | null): el is HTMLElement {
  if (!(el instanceof HTMLElement)) return false;
  const r = el.getBoundingClientRect();
  return r.width > 0 && r.height > 0 && r.bottom > 0 && r.top < innerHeight && r.right > 0 && r.left < innerWidth;
}

function target(m: Mark): HTMLElement | null {
  for (const sel of m.at) {
    const found = [...document.querySelectorAll(sel)].find(visible);
    if (found) return found as HTMLElement;
  }
  return null;
}

interface Place {
  ring: { x: number; y: number; w: number; h: number } | null;
  x: number;
  y: number;
  below: boolean;
  arrow: number;
}

const W = 300;

function place(el: HTMLElement | null): Place {
  const vw = innerWidth;
  const vh = innerHeight;
  const width = Math.min(W, vw - 32);
  if (!el) return { ring: null, x: (vw - width) / 2, y: vh - 180, below: true, arrow: -1 };
  const r = el.getBoundingClientRect();
  const pad = 4;
  const ring = { x: r.left - pad, y: r.top - pad, w: r.width + pad * 2, h: r.height + pad * 2 };
  const cx = r.left + r.width / 2;
  const x = Math.max(16, Math.min(vw - width - 16, cx - width / 2));
  const below = r.bottom + 150 < vh || r.top < 150;
  const y = below ? Math.min(vh - 140, r.bottom + 12) : Math.max(12, r.top - 12);
  return { ring, x, y, below, arrow: Math.max(14, Math.min(width - 14, cx - x)) };
}

export function Tour() {
  const [pos, setPos] = createSignal<Place | null>(null);
  const mark = () => {
    const i = current();
    return i === null ? null : MARKS[i];
  };
  const measure = () => {
    const m = mark();
    setPos(m ? place(target(m)) : null);
  };
  createEffect(
    on(current, (i) => {
      if (i === null) return;
      measure();
      // Layout settles (tiles glide, the composer mounts): re-measure briefly and on resize.
      const t = setInterval(measure, 400);
      addEventListener("resize", measure);
      onCleanup(() => {
        clearInterval(t);
        removeEventListener("resize", measure);
      });
    }),
  );
  const done = () => {
    const i = current();
    if (i === null) return;
    dismissHint(`tour.${MARKS[i].id}`);
    setCurrent(nextUnseen(i + 1));
  };
  const skip = () => {
    for (const m of MARKS) dismissHint(`tour.${m.id}`);
    setCurrent(null);
  };
  const onKey = (e: KeyboardEvent) => {
    if (current() !== null && e.key === "Escape") skip();
  };
  addEventListener("keydown", onKey);
  onCleanup(() => removeEventListener("keydown", onKey));

  return (
    <Show when={!setupOpen() && mark() && pos()}>
      {(p) => (
        <div class="tour" role="status" aria-live="polite">
          <Show when={p().ring}>{(r) => <div class="tour-ring" style={{ transform: `translate(${r().x}px, ${r().y}px)`, width: `${r().w}px`, height: `${r().h}px` }} />}</Show>
          <div
            class="tour-card"
            classList={{ above: !p().below }}
            style={{ transform: `translate(${p().x}px, ${p().y}px)${p().below ? "" : " translateY(-100%)"}`, width: `${Math.min(W, innerWidth - 32)}px`, "--arrow": `${p().arrow}px` }}
          >
            <Show when={p().arrow >= 0}>
              <span class="tour-arrow" />
            </Show>
            <p>{mark()!.text()}</p>
            <div class="tour-actions">
              <span class="tour-count">
                {current()! + 1} / {MARKS.length}
              </span>
              <span class="spacer" />
              <button type="button" class="btn btn-sm btn-ghost" onClick={skip}>
                Skip tips
              </button>
              <button type="button" class="btn btn-sm btn-primary" onClick={done}>
                {nextUnseen(current()! + 1) === null ? "Done" : "Next"}
                <Show when={nextUnseen(current()! + 1) !== null}>
                  <Icon name="arrowRight" size={13} />
                </Show>
              </button>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
}
