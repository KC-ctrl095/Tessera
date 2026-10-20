// Only the rows in (or near) view exist in the DOM: a sidebar with thousands
// of chats costs about as much as one with thirty. Rows have known heights
// (headers and rows each one height); the list scrolls with its container,
// so it can sit under other content in the same scroller.
import { For, createMemo, createSignal, onCleanup, onMount, type JSX } from "solid-js";

export function WindowedRows<T>(props: {
  items: T[];
  /** The element that scrolls (the list sits somewhere inside it). */
  scroller: () => HTMLElement | undefined;
  /** Height of one item, in px. */
  height: (item: T) => number;
  /** Extra rows above and below the view, in px. */
  overscan?: number;
  class?: string;
  children: (item: T) => JSX.Element;
}) {
  let host!: HTMLUListElement;
  const [view, setView] = createSignal({ top: 0, height: 800, offset: 0 });
  const measure = () => {
    const sc = props.scroller();
    if (!sc || !host) return;
    // Where the list starts inside the scroller's content.
    const offset = host.getBoundingClientRect().top - sc.getBoundingClientRect().top + sc.scrollTop;
    const next = { top: sc.scrollTop, height: sc.clientHeight, offset };
    const cur = view();
    if (next.top !== cur.top || next.height !== cur.height || Math.abs(next.offset - cur.offset) > 0.5) setView(next);
  };
  onMount(() => {
    const sc = props.scroller();
    if (!sc) return;
    let raf = 0;
    const onScroll = () => {
      if (raf) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        measure();
      });
    };
    sc.addEventListener("scroll", onScroll, { passive: true });
    const ro = new ResizeObserver(onScroll);
    ro.observe(sc);
    measure();
    onCleanup(() => {
      sc.removeEventListener("scroll", onScroll);
      ro.disconnect();
      cancelAnimationFrame(raf);
    });
  });

  const offsets = createMemo(() => {
    const out = new Array<number>(props.items.length + 1);
    out[0] = 0;
    for (let i = 0; i < props.items.length; i++) out[i + 1] = out[i] + props.height(props.items[i]);
    return out;
  });
  const range = createMemo(() => {
    const o = offsets();
    const n = props.items.length;
    const v = view();
    const pad = props.overscan ?? 320;
    const from = v.top - v.offset - pad;
    const to = v.top - v.offset + v.height + pad;
    // Binary search for the first row that ends below `from`.
    let lo = 0;
    let hi = n;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (o[mid + 1] <= from) lo = mid + 1;
      else hi = mid;
    }
    let last = lo;
    while (last < n && o[last] < to) last++;
    return { first: lo, last };
  });
  const visible = createMemo(() => props.items.slice(range().first, range().last));

  return (
    <ul
      ref={host}
      class={"srows vrows" + (props.class ? " " + props.class : "")}
      style={{ height: `${offsets()[props.items.length]}px`, "padding-top": `${offsets()[range().first]}px` }}
    >
      <For each={visible()}>{(item) => props.children(item)}</For>
    </ul>
  );
}
