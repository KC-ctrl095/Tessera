// A fixed-row-height virtual list: only the rows in view (plus a few) exist in
// the DOM, so hundreds of rows cost the same as ten.
import { For, createMemo, createSignal, type JSX } from "solid-js";

export function VirtualList<T>(props: {
  items: T[];
  rowHeight: number;
  maxHeight: number;
  overscan?: number;
  class?: string;
  label?: string;
  children: (item: T, index: () => number) => JSX.Element;
}) {
  const [top, setTop] = createSignal(0);
  const over = () => props.overscan ?? 4;
  const height = () => Math.min(props.maxHeight, props.items.length * props.rowHeight);
  const range = createMemo(() => {
    const first = Math.max(0, Math.floor(top() / props.rowHeight) - over());
    const last = Math.min(props.items.length, Math.ceil((top() + height()) / props.rowHeight) + over());
    return { first, last };
  });
  const visible = createMemo(() => props.items.slice(range().first, range().last));
  let raf = 0;
  return (
    <div
      class={"vlist" + (props.class ? " " + props.class : "")}
      role="list"
      aria-label={props.label}
      style={{ height: `${height()}px`, "overflow-y": "auto", position: "relative", "overscroll-behavior": "contain" }}
      onScroll={(e) => {
        const el = e.currentTarget;
        if (raf) return;
        raf = requestAnimationFrame(() => {
          raf = 0;
          setTop(el.scrollTop);
        });
      }}
    >
      <div style={{ height: `${props.items.length * props.rowHeight}px`, position: "relative" }}>
        <div style={{ transform: `translateY(${range().first * props.rowHeight}px)` }}>
          <For each={visible()}>{(item, i) => <div style={{ height: `${props.rowHeight}px` }}>{props.children(item, () => range().first + i())}</div>}</For>
        </div>
      </div>
    </div>
  );
}
