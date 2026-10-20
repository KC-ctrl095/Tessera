// Text diffs for notes: what changed between two versions (shown word by
// word), and a three-way merge of two edits made to the same version (line
// by line; changes to different lines combine, the same lines conflict).

/** A run of tokens: kept (0), removed (-1) or added (1). */
export type Run = { op: 0 | -1 | 1; text: string };

/**
 * Myers' diff of two token lists, as runs. Common ends are taken off first
 * (most edits touch one place), and very different texts give up on detail
 * past `maxD` steps: the rest is one removal and one addition.
 */
export function diffTokens(a: string[], b: string[], maxD = 2000): Run[] {
  let pre = 0;
  while (pre < a.length && pre < b.length && a[pre] === b[pre]) pre++;
  let suf = 0;
  while (suf < a.length - pre && suf < b.length - pre && a[a.length - 1 - suf] === b[b.length - 1 - suf]) suf++;
  const out: Run[] = [];
  const push = (op: 0 | -1 | 1, text: string) => {
    if (!text) return;
    const last = out[out.length - 1];
    if (last && last.op === op) last.text += text;
    else out.push({ op, text });
  };
  push(0, a.slice(0, pre).join(""));
  const A = a.slice(pre, a.length - suf);
  const B = b.slice(pre, b.length - suf);
  for (const r of middle(A, B, maxD)) push(r.op, r.text);
  push(0, a.slice(a.length - suf).join(""));
  return out;
}

function middle(a: string[], b: string[], maxD: number): Run[] {
  const n = a.length;
  const m = b.length;
  if (!n && !m) return [];
  if (!n) return [{ op: 1, text: b.join("") }];
  if (!m) return [{ op: -1, text: a.join("") }];
  const max = n + m;
  const off = max;
  const v = new Int32Array(2 * max + 2);
  const trace: Int32Array[] = [];
  let found = false;
  for (let d = 0; d <= Math.min(max, maxD); d++) {
    // Only diagonals -d..d matter at step d: keep just those (memory ~ d²).
    trace.push(v.slice(off - d, off + d + 1));
    for (let k = -d; k <= d; k += 2) {
      let x = k === -d || (k !== d && v[off + k - 1] < v[off + k + 1]) ? v[off + k + 1] : v[off + k - 1] + 1;
      let y = x - k;
      while (x < n && y < m && a[x] === b[y]) {
        x++;
        y++;
      }
      v[off + k] = x;
      if (x >= n && y >= m) {
        found = true;
        break;
      }
    }
    if (found) break;
  }
  if (!found) return [
    { op: -1, text: a.join("") },
    { op: 1, text: b.join("") },
  ];
  // Walk back through the saved frontiers.
  const steps: Run[] = [];
  let x = n;
  let y = m;
  for (let d = trace.length - 1; d > 0; d--) {
    const vd = trace[d];
    const at = (k: number) => vd[k + d];
    const k = x - y;
    const prevK = k === -d || (k !== d && at(k - 1) < at(k + 1)) ? k + 1 : k - 1;
    const prevX = at(prevK);
    const prevY = prevX - prevK;
    while (x > prevX && y > prevY) steps.push({ op: 0, text: a[--x] }), y--;
    if (x === prevX) steps.push({ op: 1, text: b[--y] });
    else steps.push({ op: -1, text: a[--x] });
  }
  while (x > 0 && y > 0) steps.push({ op: 0, text: a[--x] }), y--;
  steps.reverse();
  const out: Run[] = [];
  for (const s of steps) {
    const last = out[out.length - 1];
    if (last && last.op === s.op) last.text += s.text;
    else out.push({ ...s });
  }
  return out;
}

/** Lines, each with its newline. */
export const lines = (s: string): string[] => s.match(/[^\n]*\n|[^\n]+$/g) ?? [];
/** Words, spaces and single other characters. */
export const words = (s: string): string[] => s.match(/\s+|[\p{L}\p{N}_'’]+|[^\s\p{L}\p{N}_]/gu) ?? [];

/**
 * What changed from `a` to `b`, word by word: lines first (quick on long
 * texts), then the words of each changed stretch of lines.
 */
export function diffText(a: string, b: string): Run[] {
  const byLine = diffTokens(lines(a), lines(b));
  const out: Run[] = [];
  const push = (r: Run) => {
    if (!r.text) return;
    const last = out[out.length - 1];
    if (last && last.op === r.op) last.text += r.text;
    else out.push({ ...r });
  };
  for (let i = 0; i < byLine.length; i++) {
    const r = byLine[i];
    const next = byLine[i + 1];
    // A replaced stretch: show the words that changed in it.
    if (r.op === -1 && next?.op === 1 && r.text.length + next.text.length < 40_000) {
      for (const w of diffTokens(words(r.text), words(next.text), 600)) push(w);
      i++;
    } else push(r);
  }
  return out;
}

/** How much changed: the lines with added text (in the new version) and with removed text (in the old). */
export function diffStat(runs: Run[]): { added: number; removed: number } {
  const added = new Set<number>();
  const removed = new Set<number>();
  let a = 0; // line in the old text
  let b = 0; // line in the new text
  for (const r of runs) {
    const parts = r.text.split("\n");
    parts.forEach((part, i) => {
      if (part.trim()) {
        if (r.op === 1) added.add(b + i);
        if (r.op === -1) removed.add(a + i);
      }
    });
    const breaks = parts.length - 1;
    if (r.op !== 1) a += breaks;
    if (r.op !== -1) b += breaks;
  }
  return { added: added.size, removed: removed.size };
}

/** A change to the base: lines [from, to) become `with`. */
type Hunk = { from: number; to: number; with: string[] };

function hunks(base: string[], other: string[]): Hunk[] {
  const out: Hunk[] = [];
  let i = 0;
  let cur: Hunk | null = null;
  for (const r of diffTokens(base, other)) {
    const n = lines(r.text).length;
    if (r.op === 0) {
      if (cur) out.push(cur), (cur = null);
      i += n;
      continue;
    }
    cur ??= { from: i, to: i, with: [] };
    if (r.op === -1) {
      cur.to += n;
      i += n;
    } else cur.with.push(...lines(r.text));
  }
  if (cur) out.push(cur);
  return out;
}

/**
 * Combine two edits of `base`: `mine` and `theirs`. Changes to different
 * lines both apply; changes to the same lines (or additions at the same
 * place) are a conflict unless they are the same change.
 */
export function merge3(base: string, mine: string, theirs: string): { ok: true; text: string } | { ok: false } {
  if (mine === theirs) return { ok: true, text: mine };
  if (mine === base) return { ok: true, text: theirs };
  if (theirs === base) return { ok: true, text: mine };
  // Every line ends with a newline here, so text typed at the very end and
  // text added after it don't look like one changed last line.
  const nl = (s: string) => (s.endsWith("\n") ? s : s + "\n");
  const b = lines(nl(base));
  const ours = hunks(b, lines(nl(mine)));
  const their = hunks(b, lines(nl(theirs)));
  const all = [...ours.map((h) => ({ h, mine: true })), ...their.map((h) => ({ h, mine: false }))].sort(
    (x, y) => x.h.from - y.h.from || x.h.to - y.h.to || (x.mine ? -1 : 1),
  );
  const out: string[] = [];
  let at = 0;
  for (let k = 0; k < all.length; k++) {
    const { h } = all[k];
    const next = all[k + 1]?.h;
    if (next) {
      const overlap = next.from < h.to || (next.from === h.from && (next.from === next.to || h.from === h.to));
      if (overlap) {
        const same = next.from === h.from && next.to === h.to && next.with.join("") === h.with.join("");
        if (!same) return { ok: false };
        k++; // the same change on both sides: once
      }
    }
    out.push(...b.slice(at, h.from), ...h.with);
    at = h.to;
  }
  out.push(...b.slice(at));
  let text = out.join("");
  // Keep the ending the edits had.
  if (!mine.endsWith("\n") && !theirs.endsWith("\n") && text.endsWith("\n")) text = text.slice(0, -1);
  return { ok: true, text };
}

/**
 * Where offsets in the old text are in the new one (`runs`: old → new). An
 * offset at an insertion goes before it (`bias` -1) or after it (1); one in
 * removed text goes to where that text was.
 */
export function offsetMap(runs: Run[]): (o: number, bias?: -1 | 1) => number {
  return (o, bias = 1) => {
    let a = 0;
    let b = 0;
    for (const r of runs) {
      const n = r.text.length;
      if (r.op === 0) {
        if (o < a + n || (o === a + n && bias < 0)) return b + (o - a);
        a += n;
        b += n;
      } else if (r.op === -1) {
        if (o < a + n) return b;
        a += n;
      } else {
        if (o === a && bias < 0) return b;
        b += n;
      }
    }
    return b + (o - a);
  };
}
