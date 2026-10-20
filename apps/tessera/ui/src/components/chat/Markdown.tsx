import { createEffect, onCleanup } from "solid-js";
import { StreamRenderer, adopt, park } from "../../lib/mdstream";
import { copyText, openExternal } from "../../lib/util";

/** How many Markdown blocks are streaming right now (across all tiles). */
let live = 0;

/**
 * Sanitized markdown.
 *
 * While `streaming`, it renders at most once per frame (or ~12 times a
 * second when several tiles stream at once), and only what changed: blocks
 * that are done are rendered once and kept, only the one being written is
 * redone (lib/mdstream.ts), so a long reply costs the same per frame as a
 * short one. A tail that is expensive to redo anyway (one huge paragraph)
 * is redone less often rather than taking every frame. When the reply is
 * committed, the message that replaces this copy takes its DOM as it is.
 */
export function Markdown(props: { text: string; streaming?: boolean; class?: string }) {
  let el!: HTMLDivElement;
  let r: StreamRenderer | undefined;
  let raf = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let counted = false;
  const count = (on: boolean) => {
    if (on === counted) return;
    counted = on;
    live += on ? 1 : -1;
  };
  /** The text last seen (what the committed message will show). */
  let seen = "";
  /** When the last streaming render ended, and what it cost. */
  let lastAt = 0;
  let cost = 0;

  const render = () => {
    raf = 0;
    timer = undefined;
    const text = props.text;
    const streaming = !!props.streaming;
    r ??= adopt(text, el) ?? new StreamRenderer(el);
    const t0 = performance.now();
    r.update(text, !streaming);
    if (!streaming) return;
    lastAt = performance.now();
    cost = lastAt - t0;
    if (import.meta.env.DEV) (window as { __mdTrace?: [number, number][] }).__mdTrace?.push([text.length, cost]);
  };
  const cancel = () => {
    if (raf) cancelAnimationFrame(raf);
    clearTimeout(timer);
    raf = 0;
    timer = undefined;
  };
  const schedule = () => {
    if (raf || timer) return;
    // Several replies at once, or a tail that is slow to redo: fewer renders, never a busy main thread.
    const gap = Math.max(live > 2 ? 80 : 0, cost > 8 ? cost * 3 : 0) - (performance.now() - lastAt);
    if (gap > 4) timer = setTimeout(() => ((timer = undefined), (raf = requestAnimationFrame(render))), gap);
    else raf = requestAnimationFrame(render);
  };

  createEffect(() => {
    seen = props.text;
    count(!!props.streaming);
    if (props.streaming) schedule();
    else {
      cancel();
      render();
    }
  });
  onCleanup(() => {
    cancel();
    count(false);
    // A streamed reply being committed: the message that replaces it takes this DOM.
    if (r?.streamed) park(r, seen);
  });

  const onClick = (e: MouseEvent) => {
    const t = e.target as HTMLElement;
    const copy = t.closest<HTMLButtonElement>(".code-copy");
    if (copy) {
      const code = copy.closest(".code-block")?.querySelector("pre")?.textContent ?? "";
      void copyText(code).then((ok) => {
        if (!ok) return;
        copy.classList.add("done");
        const label = copy.querySelector("span");
        if (label) label.textContent = "Copied";
        setTimeout(() => {
          copy.classList.remove("done");
          if (label) label.textContent = "Copy";
        }, 1400);
      });
      return;
    }
    const a = t.closest<HTMLAnchorElement>("a[href]");
    if (a) {
      const href = a.getAttribute("href") ?? "";
      if (href.startsWith("#")) return;
      e.preventDefault();
      void openExternal(a.href);
    }
  };

  // Middle-click must not open links inside the app either.
  const onAuxClick = (e: MouseEvent) => {
    if (e.button !== 1) return;
    const a = (e.target as HTMLElement).closest<HTMLAnchorElement>("a[href]");
    if (!a) return;
    e.preventDefault();
    const href = a.getAttribute("href") ?? "";
    if (!href.startsWith("#")) void openExternal(a.href);
  };
  const onMouseDown = (e: MouseEvent) => {
    // Stop the browser's middle-click autoscroll / new-tab handling on links.
    if (e.button === 1 && (e.target as HTMLElement).closest("a[href]")) e.preventDefault();
  };

  return (
    <div
      ref={el}
      class={"md" + (props.class ? " " + props.class : "")}
      classList={{ streaming: !!props.streaming }}
      onClick={onClick}
      onAuxClick={onAuxClick}
      onMouseDown={onMouseDown}
    />
  );
}
