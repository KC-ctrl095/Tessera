import { Show, createEffect, createMemo, createSignal, onCleanup } from "solid-js";
import { fmtDuration } from "../../lib/util";
import { Icon, Tessellate } from "../icons";
import { Markdown } from "./Markdown";

/** A ticking clock while `on()` is true. */
export function useTicker(on: () => boolean, ms = 1000) {
  const [now, setNow] = createSignal(Date.now());
  createEffect(() => {
    if (!on()) return;
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), ms);
    onCleanup(() => clearInterval(t));
  });
  return now;
}

/** The last line of reasoning, as plain text (for the one-line live preview). */
function lastLine(s: string): string {
  const t = s
    .replace(/```[\s\S]*?(```|$)/g, " ")
    .replace(/\*\*|__|`|^#{1,6}\s+|^>\s?/gm, "")
    .replace(/\[(.*?)\]\(.*?\)/g, "$1")
    .trimEnd();
  const lines = t.split(/\n+/);
  for (let i = lines.length - 1; i >= 0; i--) {
    const l = lines[i].trim();
    if (l) return l.length > 240 ? "…" + l.slice(-240) : l;
  }
  return "";
}

/**
 * Reasoning, as one quiet row: "Thinking · 8s" with the working indicator
 * and a one-line live preview while it streams, "Thought for 12s" after.
 * Click to read all of it. Never animates its height.
 */
export function ThinkingBlock(props: { text: string; live?: boolean; ms?: number; startedAt?: number }) {
  const [open, setOpen] = createSignal(false);
  const now = useTicker(() => !!props.live);
  const elapsed = () => (props.startedAt ? Math.max(0, now() - props.startedAt) : 0);
  const label = () => {
    if (props.live) return elapsed() >= 1000 ? `Thinking · ${fmtDuration(elapsed())}` : "Thinking";
    if (props.ms && props.ms > 500) return `Thought for ${fmtDuration(props.ms)}`;
    return "Thought process";
  };
  const preview = createMemo(() => (props.live && !open() ? lastLine(props.text.slice(-2400)) : ""));
  return (
    <div class="thinking" classList={{ open: open(), live: !!props.live }}>
      <button type="button" class="thinking-head" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <span class="thinking-icon">
          <Show when={props.live} fallback={<Icon name="brain" size={14} />}>
            <Tessellate size={9} />
          </Show>
        </span>
        <span class="thinking-label">{label()}</span>
        <Show when={preview()}>
          <span class="thinking-preview">{preview()}</span>
        </Show>
        <Icon name="chevronDown" size={13} class="chev" />
      </button>
      <Show when={open()}>
        <div class="thinking-body">
          <Markdown text={props.text} streaming={props.live} />
        </div>
      </Show>
    </div>
  );
}
