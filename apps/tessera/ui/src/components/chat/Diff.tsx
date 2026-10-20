import { For, createMemo } from "solid-js";

interface DiffLine {
  kind: "add" | "del" | "ctx" | "hunk" | "meta";
  text: string;
  old?: number;
  new?: number;
}

export function parseDiff(diff: string): DiffLine[] {
  const out: DiffLine[] = [];
  let o = 0;
  let n = 0;
  for (const raw of diff.replace(/\n$/, "").split("\n")) {
    if (raw.startsWith("@@")) {
      const m = raw.match(/@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
      if (m) {
        o = +m[1];
        n = +m[2];
      }
      out.push({ kind: "hunk", text: raw });
    } else if (raw.startsWith("+++") || raw.startsWith("---") || raw.startsWith("diff ") || raw.startsWith("index ")) {
      out.push({ kind: "meta", text: raw });
    } else if (raw.startsWith("+")) {
      out.push({ kind: "add", text: raw.slice(1), new: n++ });
    } else if (raw.startsWith("-")) {
      out.push({ kind: "del", text: raw.slice(1), old: o++ });
    } else if (raw.startsWith("\\")) {
      out.push({ kind: "meta", text: raw });
    } else {
      out.push({ kind: "ctx", text: raw.startsWith(" ") ? raw.slice(1) : raw, old: o++, new: n++ });
    }
  }
  return out;
}

export { diffStats } from "../../lib/tools";

/** Colored unified diff with old/new line numbers. */
export function DiffView(props: { diff: string }) {
  const lines = createMemo(() => parseDiff(props.diff));
  return (
    <div class="diff" role="table" aria-label="Diff">
      <For each={lines()}>
        {(l) => (
          <div class={`diff-line diff-${l.kind}`} role="row">
            <span class="diff-ln" aria-hidden="true">{l.kind === "hunk" || l.kind === "meta" ? "" : l.old ?? ""}</span>
            <span class="diff-ln" aria-hidden="true">{l.kind === "hunk" || l.kind === "meta" ? "" : l.new ?? ""}</span>
            <span class="diff-sign" aria-hidden="true">{l.kind === "add" ? "+" : l.kind === "del" ? "−" : ""}</span>
            <span class="diff-text">{l.text || " "}</span>
          </div>
        )}
      </For>
    </div>
  );
}
