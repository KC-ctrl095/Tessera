// What changed in a note between two versions: added words marked, removed
// ones struck through, long unchanged stretches folded away (a tap unfolds).
import { For, Show, createMemo, createSignal } from "solid-js";
import { diffStat, diffText, type Run } from "../../lib/textDiff";

/** Lines of unchanged text kept around a change. */
const CONTEXT = 2;

type Piece = { op: 0 | -1 | 1; text: string } | { fold: number; text: string };

function pieces(runs: Run[]): Piece[] {
  const out: Piece[] = [];
  runs.forEach((r, i) => {
    if (r.op !== 0) return void out.push(r);
    const ls = r.text.split(/(?<=\n)/);
    const head = i === 0 ? 0 : CONTEXT + 1;
    const tail = i === runs.length - 1 ? 0 : CONTEXT;
    if (ls.length <= head + tail + 2) return void out.push(r);
    out.push({ op: 0, text: ls.slice(0, head).join("") });
    out.push({ fold: ls.length - head - tail, text: ls.slice(head, ls.length - tail).join("") });
    out.push({ op: 0, text: ls.slice(ls.length - tail).join("") });
  });
  return out;
}

export function NoteDiff(props: { before: string; after: string }) {
  const runs = createMemo(() => diffText(props.before, props.after));
  const stat = createMemo(() => diffStat(runs()));
  return (
    <div class="note-diff">
      <Show when={runs().some((r) => r.op !== 0)} fallback={<p class="nd-same">No differences in the text.</p>}>
        <p class="nd-stat">
          <span class="nd-add">+{stat().added}</span> <span class="nd-del">−{stat().removed}</span> {stat().added + stat().removed === 1 ? "line" : "lines"}
        </p>
        <div class="nd-text">
          <For each={pieces(runs())}>
            {(p) =>
              "fold" in p ? (
                <Fold piece={p} />
              ) : p.op === 1 ? (
                <ins>{p.text}</ins>
              ) : p.op === -1 ? (
                <del>{p.text}</del>
              ) : (
                <span>{p.text}</span>
              )
            }
          </For>
        </div>
      </Show>
    </div>
  );
}

function Fold(props: { piece: { fold: number; text: string } }) {
  const [open, setOpen] = createSignal(false);
  return (
    <Show
      when={open()}
      fallback={
        <button type="button" class="nd-fold" onClick={() => setOpen(true)}>
          ⋯ {props.piece.fold} unchanged {props.piece.fold === 1 ? "line" : "lines"}
        </button>
      }
    >
      <span>{props.piece.text}</span>
    </Show>
  );
}
