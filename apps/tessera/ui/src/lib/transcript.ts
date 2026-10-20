// Turns persisted entries into render items: tool calls are paired with their
// results, and finished runs of consecutive tool calls collapse into a
// "N steps" group. Items keep a stable identity across recomputation so the
// list never re-renders rows that did not change.
import { createSignal, type Accessor, type Setter } from "solid-js";
import type { AssistantPart } from "../proto/AssistantPart";
import type { Entry } from "../proto/Entry";
import type { AssistantEntry, ToolResultEntry } from "./store";
import { isBoardTool } from "./boards";
import { makesNote, toolPresentation } from "./tools";
import { argsObject } from "./tools";
import { isCheckAck, isProtocolEntry } from "./completion";
export { isCheckAck, isProtocolEntry } from "./completion";

export type UserEntry = Extract<Entry, { kind: "user" }>;
export type NoticeEntry = Extract<Entry, { kind: "notice" }>;
export type CompactionEntry = Extract<Entry, { kind: "compaction" }>;
export type ToolCallPart = Extract<AssistantPart, { type: "tool_call" }>;

export type StepItem =
  | { k: "thinking"; key: string; text: string; entry: AssistantEntry }
  | { k: "tool"; key: string; call: ToolCallPart; entry: AssistantEntry }
  /** Narration inside a message that goes on to call tools ("Let me check…"). */
  | { k: "narr"; key: string; text: string; entry: AssistantEntry };

export type Item =
  | StepItem
  | { k: "user"; key: string; entry: UserEntry }
  /** Who speaks next (bot chats and channels): a bot id. */
  | { k: "speaker"; key: string; author: string; ts: number; seq: number }
  /** A message someone else wrote on the "user" side: a bot (DMs), another chat's agent, a job. */
  | { k: "authored"; key: string; entry: UserEntry; author: string }
  /** A background subagent's report, delivered into the parent chat. */
  | { k: "report"; key: string; entry: UserEntry; childId: string }
  /** "[The user set the goal…]" notes the server adds to the history. */
  | { k: "goal"; key: string; entry: UserEntry }
  /** What the harness itself told the agent ("[Completion check] …", "[Harness restarted] …"). */
  | { k: "harness"; key: string; entry: UserEntry }
  /** Several subagent reports in a row, as one group. */
  | { k: "reports"; key: string; items: { entry: UserEntry; childId: string }[] }
  | { k: "text"; key: string; text: string; entry: AssistantEntry }
  | { k: "steps"; key: string; items: StepItem[]; tools: string[] }
  | { k: "notice"; key: string; entry: NoticeEntry }
  | { k: "compaction"; key: string; entry: CompactionEntry }
  | { k: "stop"; key: string; entry: AssistantEntry }
  | { k: "orphan"; key: string; entry: ToolResultEntry }
  /** A whiteboard the agent showed (never folded into a steps group). */
  | { k: "board"; key: string; call: ToolCallPart; entry: AssistantEntry }
  /** A file the agent shared for download (`share_file`; never folded either). */
  | { k: "file"; key: string; call: ToolCallPart; entry: AssistantEntry }
  /** A note the agent wrote (`note` create; a card, never folded either). */
  | { k: "note"; key: string; call: ToolCallPart; entry: AssistantEntry }
  /** A turn's footer; `check`: a completion check that found nothing left, folded in here. */
  | { k: "turn"; key: string; entry: AssistantEntry; first: number; check?: UserEntry };

/**
 * An entry's identity for item keys. A rewind frees seqs that new entries
 * then reuse, so the seq alone could hand a new entry the old one's item.
 */
const at = (e: { seq: number; ts: number }) => `${e.seq}@${e.ts}`;

/** The first entry an item shows (what a rewind to before it removes). */
export function itemSeq(it: Item): number {
  switch (it.k) {
    case "speaker":
      return it.seq;
    case "steps":
      return itemSeq(it.items[0]);
    case "reports":
      return it.items[0].entry.seq;
    default:
      return it.entry.seq;
  }
}

/** Built from the same things (entries, parts, texts): the old item can stay. */
function sameItem(a: object, b: object): boolean {
  const ka = Object.keys(a);
  if (ka.length !== Object.keys(b).length) return false;
  return ka.every((k) => {
    const x = (a as Record<string, unknown>)[k];
    const y = (b as Record<string, unknown>)[k];
    if (x === y) return true;
    if (!Array.isArray(x) || !Array.isArray(y) || x.length !== y.length) return false;
    return x.every((v, i) => v === y[i] || (!!v && typeof v === "object" && !!y[i] && typeof y[i] === "object" && sameItem(v, y[i])));
  });
}

export class ItemCache {
  private map = new Map<string, Item>();
  private next = new Map<string, Item>();
  private fallback?: ItemCache;
  private sources = new WeakMap<Item, Entry>();
  /** Keep row identity when a finished turn moves from the live suffix into the cached prefix. */
  reuse(cache?: ItemCache) {
    this.fallback = cache;
  }
  begin() {
    this.next = new Map();
  }
  /** The item last made under `key`, unless what it was made from changed (an entry rewritten in place). */
  get<T extends Item>(key: string, make: () => T, source?: Entry): T {
    const old = (this.map.get(key) ?? this.fallback?.map.get(key)) as T | undefined;
    if (old && source && source === (this.sources.get(old) ?? this.fallback?.sources.get(old))) {
      this.next.set(key, old);
      this.sources.set(old, source);
      return old;
    }
    const fresh = make();
    const hit = old && sameItem(old, fresh) ? old : fresh;
    this.next.set(key, hit);
    if (source) this.sources.set(hit, source);
    return hit;
  }
  end() {
    this.map = this.next;
  }
}

export interface DeriveOpts {
  /** Show who is speaking (bot chats, channels). */
  speakers?: boolean;
  /** Speaker for assistant entries without an `author`. */
  defaultAuthor?: string;
  /** Collapse every finished run of tool calls, even a single one (channels). */
  collapseAll?: boolean;
  /**
   * Author of user entries without one, where the user can't write (bot DMs:
   * older servers left out who asked a bot when a plain chat's agent did).
   */
  userAuthor?: string;
}

/** Finished turns do not change on every new tool call in the current turn. */
export class TranscriptDeriver {
  private prefixCache = new ItemCache();
  private tailCache = new ItemCache();
  private entries: readonly Entry[] | null = null;
  private version = -1;
  private length = 0;
  private last: Entry | undefined;
  private options = "";
  private from = 0;
  private prefix: Item[] = [];
  private calls = new Set<string>();
  private prefixOrphans = new Set<string>();

  derive(entries: readonly Entry[], running: boolean, opts: DeriveOpts = {}, version = 0): Item[] {
    const options = JSON.stringify(opts);
    const reset = entries !== this.entries || version !== this.version || options !== this.options || entries.length < this.length || (this.length > 0 && entries[this.length - 1] !== this.last);
    if (reset) this.calls.clear();
    let prefixChanged = false;
    for (let i = reset ? 0 : this.length; i < entries.length; i++) {
      const e = entries[i];
      if (e.kind === "assistant") for (const p of e.content) if (p.type === "tool_call") {
        this.calls.add(p.id);
        if (this.prefixOrphans.has(p.id)) prefixChanged = true;
      }
    }
    let from = reset ? 0 : this.from;
    // Bot messages start turns too. Completion checks stay with the answer they
    // check, and adjacent helper reports stay together for their grouped row.
    for (let i = reset ? 0 : this.length; i < entries.length; i++) {
      const e = entries[i];
      if (e.kind === "user" && !e.author?.startsWith("subagent:") && e.author !== "harness" && e.author !== "goal" && !isCompletionCheck(e)) from = i;
    }
    if (reset || prefixChanged || from !== this.from) {
      this.prefixCache.reuse(this.tailCache);
      this.prefix = from ? deriveItems(entries.slice(0, from), this.prefixCache, false, opts, this.calls) : [];
      this.prefixOrphans = new Set(this.prefix.flatMap((it) => it.k === "orphan" ? [it.entry.call_id] : []));
      this.prefixCache.reuse();
      this.from = from;
    }
    this.entries = entries;
    this.version = version;
    this.options = options;
    this.length = entries.length;
    this.last = entries.at(-1);
    this.tailCache.reuse(this.prefixCache);
    const tail = deriveItems(from ? entries.slice(from) : entries, this.tailCache, running, opts, this.calls);
    return this.prefix.length ? this.prefix.concat(tail) : tail;
  }
}

/**
 * @param running whether the session is mid-turn (the trailing run of tool
 *   calls stays expanded while the agent is still working).
 */
export function deriveItems(entries: readonly Entry[], cache: ItemCache, running: boolean, opts: DeriveOpts = {}, knownCalls?: ReadonlySet<string>): Item[] {
  cache.begin();
  let lastSpeaker: string | null = null;
  const calls = knownCalls ?? new Set<string>();
  if (!knownCalls) for (const e of entries) {
    if (e.kind === "assistant") for (const p of e.content) if (p.type === "tool_call") (calls as Set<string>).add(p.id);
  }

  const raw: Item[] = [];
  let turnFirst = -1;
  let lastAssistant: AssistantEntry | null = null;
  const closeTurn = (check?: UserEntry) => {
    if (lastAssistant) {
      const a = lastAssistant;
      const first = turnFirst;
      const key = `e:${at(a)}:${first}`;
      raw.push(cache.get(key, () => (check ? { k: "turn", key, entry: a, first, check } : { k: "turn", key, entry: a, first })));
    }
    lastAssistant = null;
    turnFirst = -1;
  };

  for (let i = 0; i < entries.length; i++) {
    const e = entries[i];
    switch (e.kind) {
      case "user": {
        // Old native histories may include ledger-only check/reply runs.
        const quiet = isCompletionCheck(e) ? quietCheckEnd(entries, i) : -1;
        if (quiet >= 0) {
          closeTurn();
          i = quiet - 1;
          break;
        }
        if (isProtocolEntry(e)) break;
        closeTurn();
        if (e.author?.startsWith("subagent:")) {
          const childId = e.author.slice("subagent:".length);
          raw.push(cache.get(`ur:${at(e)}`, () => ({ k: "report", key: `ur:${at(e)}`, entry: e, childId })));
        } else if (e.author === "goal") {
          raw.push(cache.get(`ug:${at(e)}`, () => ({ k: "goal", key: `ug:${at(e)}`, entry: e })));
        } else if (e.author === "harness") {
          raw.push(cache.get(`uh:${at(e)}`, () => ({ k: "harness", key: `uh:${at(e)}`, entry: e })));
        } else if (e.author || opts.userAuthor) {
          const author = (e.author || opts.userAuthor)!;
          lastSpeaker = author;
          raw.push(cache.get(`ua:${at(e)}`, () => ({ k: "authored", key: `ua:${at(e)}`, entry: e, author })));
        } else {
          lastSpeaker = null;
          raw.push(cache.get(`u:${at(e)}`, () => ({ k: "user", key: `u:${at(e)}`, entry: e })));
        }
        break;
      }
      case "assistant": {
        if (isProtocolEntry(e)) break;
        const author = e.author ?? opts.defaultAuthor;
        if (opts.speakers && author && author !== lastSpeaker) {
          // A new voice: close the previous speaker's turn and introduce this one.
          if (lastSpeaker) closeTurn();
          lastSpeaker = author;
          raw.push(cache.get(`sp:${at(e)}`, () => ({ k: "speaker", key: `sp:${at(e)}`, author, ts: e.ts, seq: e.seq })));
        }
        if (turnFirst < 0) turnFirst = e.seq;
        const callsTools = e.content.some((p) => p.type === "tool_call");
        e.content.forEach((p, i) => {
          if (p.type === "thinking") {
            // A run of reasoning parts reads as one block, keyed by its first part (some providers send
            // a part per summary, or empty ones that only carry hidden reasoning).
            if (continuesThinking(e.content, i)) return;
            const text = thinkingRun(e.content, i);
            if (!text) return;
            raw.push(cache.get(`th:${at(e)}:${i}`, () => ({ k: "thinking", key: `th:${at(e)}:${i}`, text, entry: e })));
          } else if (p.type === "text") {
            if (!p.text.trim() || isCheckAck(p.text)) return;
            if (callsTools) {
              raw.push(cache.get(`r:${at(e)}:${i}`, () => ({ k: "narr", key: `r:${at(e)}:${i}`, text: p.text, entry: e })));
            } else {
              raw.push(cache.get(`t:${at(e)}:${i}`, () => ({ k: "text", key: `t:${at(e)}:${i}`, text: p.text, entry: e })));
            }
          } else if (isBoardTool(p.name) && showsBoard(p.arguments)) {
            raw.push(cache.get(`b:${p.id}`, () => ({ k: "board", key: `b:${p.id}`, call: p, entry: e })));
          } else if (p.name === "share_file") {
            raw.push(cache.get(`f:${p.id}`, () => ({ k: "file", key: `f:${p.id}`, call: p, entry: e })));
          } else if (makesNote(p.name, p.arguments)) {
            raw.push(cache.get(`nt:${p.id}`, () => ({ k: "note", key: `nt:${p.id}`, call: p, entry: e })));
          } else {
            raw.push(cache.get(`c:${p.id}`, () => ({ k: "tool", key: `c:${p.id}`, call: p, entry: e }), e));
          }
        });
        if (e.stop === "aborted" || e.stop === "length" || e.stop === "error" || e.error) {
          raw.push(cache.get(`x:${at(e)}`, () => ({ k: "stop", key: `x:${at(e)}`, entry: e })));
        }
        lastAssistant = e;
        break;
      }
      case "tool_result":
        if (!calls.has(e.call_id)) raw.push(cache.get(`o:${at(e)}`, () => ({ k: "orphan", key: `o:${at(e)}`, entry: e })));
        break;
      case "compaction":
        raw.push(cache.get(`m:${at(e)}`, () => ({ k: "compaction", key: `m:${at(e)}`, entry: e })));
        break;
      case "notice":
        raw.push(cache.get(`n:${at(e)}`, () => ({ k: "notice", key: `n:${at(e)}`, entry: e })));
        break;
    }
  }
  if (!running) closeTurn();

  // Collapse finished runs of steps. What the agent said between its steps
  // ("narr") is never folded away: it splits the run and stays in view, so
  // text the reader saw doesn't vanish when a report, notice or the turn's
  // end closes the run.
  const out: Item[] = [];
  let buf: StepItem[] = [];
  const group = (seg: StepItem[]) => {
    const tools = seg.filter((b): b is Extract<StepItem, { k: "tool" }> => b.k === "tool");
    if (seg.length && (tools.length >= (opts.collapseAll ? 1 : 2) || tools.some((t) => toolPresentation(t.call.name, t.call.arguments).housekeeping))) {
      const key = `g:${seg[0].key}:${seg.length}`;
      out.push(cache.get(key, () => ({ k: "steps", key, items: seg, tools: tools.map((t) => t.call.name) })));
    } else out.push(...seg);
  };
  const flush = (finished: boolean) => {
    if (!finished) out.push(...buf);
    else {
      let seg: StepItem[] = [];
      for (const it of buf) {
        if (it.k !== "narr") {
          seg.push(it);
          continue;
        }
        group(seg);
        seg = [];
        out.push(it);
      }
      group(seg);
    }
    buf = [];
  };
  for (const it of raw) {
    // Secure entry must stay reachable, not hidden in a collapsed steps strip.
    if (it.k === "tool" && it.call.name === "save_secret" && argsObject(it.call.arguments)?.action !== "list") {
      flush(true);
      out.push(it);
      continue;
    }
    if (it.k === "thinking" || it.k === "tool" || it.k === "narr") {
      buf.push(it);
      continue;
    }
    // A turn footer does not break a run visually, but ends it.
    flush(true);
    out.push(it);
  }
  flush(!running);
  const grouped = groupReports(out, cache);
  cache.end();
  return grouped;
}

type Parts = readonly (AssistantPart | null | undefined)[];
/**
 * Nothing to show between reasoning parts: an empty text part (a model's
 * output item with no text, kept so later parts keep their numbers), or a
 * part a stream hasn't filled in. Reasoning on both sides is one run.
 */
const blank = (p: AssistantPart | null | undefined) => !p || (p.type === "text" && !p.text.trim());

/** Whether part `i` carries on a run of reasoning (the nearest part before it with anything in it is reasoning). */
export function continuesThinking(content: Parts, i: number): boolean {
  for (let j = i - 1; j >= 0; j--) if (!blank(content[j])) return content[j]!.type === "thinking";
  return false;
}

/** The text of the run of reasoning parts starting at `from`, empty parts left out. */
export function thinkingRun(content: Parts, from: number): string {
  const texts: string[] = [];
  for (let j = from; j < content.length; j++) {
    const p = content[j];
    if (blank(p)) continue;
    if (p!.type !== "thinking") break;
    const t = (p as Extract<AssistantPart, { type: "thinking" }>).text.trim();
    if (t) texts.push(t);
  }
  return texts.join("\n\n");
}

/** `whiteboard` calls that put a board up (not list/delete). */
function showsBoard(args: unknown): boolean {
  const a = argsObject(args);
  const action = typeof a?.action === "string" ? a.action : "show";
  return action === "show" || action === "ask";
}

/** Three or more reports in a row (a swarm finishing) collapse into one group. */
const REPORT_GROUP = 3;
function groupReports(items: Item[], cache: ItemCache): Item[] {
  const out: Item[] = [];
  let run: Extract<Item, { k: "report" }>[] = [];
  const flush = () => {
    if (run.length >= REPORT_GROUP) {
      const key = `rg:${run[0].key}:${run.length}`;
      const group = run.map((r) => ({ entry: r.entry, childId: r.childId }));
      out.push(cache.get(key, () => ({ k: "reports", key, items: group })));
    } else out.push(...run);
    run = [];
  };
  for (const it of items) {
    if (it.k === "report") {
      run.push(it);
      continue;
    }
    flush();
    out.push(it);
  }
  flush();
  return out;
}

/** All assistant text of a turn (for "copy response"). */
export function turnText(entries: readonly Entry[], fromSeq: number, toSeq: number): string {
  const out: string[] = [];
  for (const e of entries) {
    if (e.seq < fromSeq || e.seq > toSeq || e.kind !== "assistant" || isProtocolEntry(e)) continue;
    for (const p of e.content) if (p.type === "text" && p.text.trim() && !isCheckAck(p.text)) out.push(p.text.trim());
  }
  return out.join("\n\n");
}

export interface Report {
  name: string;
  steps: number | null;
  took: string | null;
  sessionId: string | null;
  body: string;
}

/**
 * A subagent report: `[Report from subagent "name" · 7 steps · 2 min ·
 * session ses_…]` followed by the report itself.
 */
export function parseReport(text: string): Report {
  const m = /^\[Report from subagent "([^"]*)"([^\]]*)\]\s*/.exec(text);
  if (!m) return { name: "Subagent", steps: null, took: null, sessionId: null, body: text };
  const meta = m[2].split("·").map((x) => x.trim()).filter(Boolean);
  let steps: number | null = null;
  let took: string | null = null;
  let sessionId: string | null = null;
  for (const part of meta) {
    const st = /^(\d+) steps?$/.exec(part);
    if (st) steps = Number(st[1]);
    else if (part.startsWith("session ")) sessionId = part.slice(8).trim();
    else if (/^\d+\s*(s|min|h)$/.test(part)) took = part;
  }
  return { name: m[1] || "Subagent", steps, took, sessionId, body: text.slice(m[0].length) };
}

export interface BotReply {
  handle: string;
  took: string | null;
  /** The bot's conversation, where it did the work. */
  sessionId: string | null;
  body: string;
}

/**
 * "[Reply from @orion · 4 min · session s1]\n\n…": a bot's reply to work this chat handed it in the
 * background (engine/handoffs.rs `reply_header`), delivered as a message from that bot. Null for anything else.
 */
export function parseBotReply(text: string): BotReply | null {
  const m = /^\[Reply from @([^\s·\]]+)([^\]]*)\]\s*/.exec(text);
  if (!m) return null;
  let took: string | null = null;
  let sessionId: string | null = null;
  for (const part of m[2].split("·").map((x) => x.trim()).filter(Boolean)) {
    if (part.startsWith("session ")) sessionId = part.slice(8).trim();
    else if (/^\d+\s*(s|min|h)\b/.test(part)) took = part;
  }
  return { handle: m[1], took, sessionId, body: text.slice(m[0].length) };
}

/** A delivered bot reply that says there is no answer (stopped, cut off, failed). */
export const NO_BOT_ANSWER = /^@\S+ (was stopped before it replied|was cut off by a server restart|did not reply|was removed before it replied)/;

/** "[Completion check] …": the harness's question before a turn ends (tools/meta.rs `completion_note`). */
export function isCompletionCheck(e: Entry): e is UserEntry {
  if (e.kind !== "user" || e.author !== "harness") return false;
  const first = e.content.find((p) => p.type === "text");
  return !!first && first.type === "text" && /^\s*\[Completion check\]/.test(first.text);
}



/** The reply being streamed is (so far) only the quiet answer to a completion check: nothing to show live. */
export function quietCheckOpen(entries: readonly Entry[]): boolean {
  for (let i = entries.length - 1; i >= 0; i--) {
    const e = entries[i];
    if (e.kind === "user") return isCompletionCheck(e) && quietCheckEnd(entries, i) === entries.length;
  }
  return false;
}

/**
 * Where the reply to the completion check at `i` ends (the next message), when it is only
 * ledger updates and a bare ✓ (or nothing yet); -1 when the agent did more or said more.
 */
export function quietCheckEnd(entries: readonly Entry[], i: number): number {
  let j = i + 1;
  for (; j < entries.length; j++) {
    const e = entries[j];
    if (e.kind === "tool_result") continue;
    if (e.kind !== "assistant") break;
    if (e.error || e.stop === "aborted" || e.stop === "length" || e.stop === "error") return -1;
    for (const p of e.content) {
      if (p.type === "tool_call" && p.name !== "task") return -1;
      if (p.type === "text" && p.text.trim() && !isCheckAck(p.text)) return -1;
    }
  }
  return j;
}

/** A harness note: `[Label] text` → its label and the rest. */
export function harnessNote(text: string): { label: string; body: string } {
  const m = /^\s*\[([^\]\n]{1,60})\]\s*([\s\S]*)$/.exec(text);
  return m ? { label: m[1].trim(), body: m[2].trim() } : { label: "Note from Tessera", body: text.trim() };
}

/** The goal text of a "[The user set the goal for this session]" note (null when it was cleared). */
export function goalNote(text: string): string | null {
  const m = /^\[The user set the goal for this session\]\s*([\s\S]*)$/.exec(text);
  return m ? m[1].trim() : null;
}

// ── the reply being streamed, as rows of the transcript ──────────────────

/**
 * A part of the reply being streamed (or the bot about to speak), shown as a
 * row of the transcript like any item. When the reply is committed the row
 * stays where it is and shows the committed item instead: the same DOM, so
 * nothing is rendered again, laid out again or moved (and a selection in it,
 * an open reasoning block or tool card stay as they are).
 */
export interface LiveRow {
  k: "live";
  key: string;
  part: "speaker" | "thinking" | "text" | "tool";
  /** The part's index in the streamed message (the same in the committed entry's content). */
  index: number;
  /** A tool call's id, once known. */
  callId?: string;
  /** The committed item it shows now, once there is one. */
  item: Accessor<Item | null>;
  setItem: Setter<Item | null>;
}

export type Row = Item | LiveRow;

/** What is streaming now: its identity, who speaks (when shown) and its parts. */
export interface LiveParts {
  stream: object | null;
  speaker: string | null;
  parts: { index: number; type: AssistantPart["type"]; id?: string }[];
}

let liveSeq = 0;

/** The transcript's rows: committed items, then the parts being streamed (see `LiveRow`). */
export class LiveRows {
  private stream: object | null = null;
  /** The current stream's rows, by part and index. */
  private live = new Map<string, LiveRow>();
  /** Rows of a stream that just ended, waiting for their committed items (one pass only). */
  private waiting: LiveRow[] = [];
  /** Committed item key → the row that showed it live (and still shows it). */
  private shown = new Map<string, LiveRow>();
  /** Item keys present last time: only new items can take over a row. */
  private seen = new Set<string>();

  merge(items: readonly Item[], now: LiveParts): Row[] {
    if (now.stream !== this.stream) {
      this.waiting = [...this.live.values()];
      this.live = new Map();
      this.stream = now.stream;
    }
    const out: Row[] = [];
    const keys = new Set<string>();
    for (const it of items) {
      keys.add(it.key);
      let row = this.shown.get(it.key);
      if (!row && this.waiting.length && !this.seen.has(it.key)) {
        const i = this.waiting.findIndex((r) => takes(r, it));
        if (i >= 0) {
          row = this.waiting.splice(i, 1)[0];
          this.shown.set(it.key, row);
        }
      }
      if (row) {
        if (row.item() !== it) row.setItem(it);
        out.push(row);
      } else out.push(it);
    }
    for (const k of this.shown.keys()) if (!keys.has(k)) this.shown.delete(k);
    this.seen = keys;
    this.waiting = [];
    if (!now.stream) return out;
    const next = new Map<string, LiveRow>();
    const add = (part: LiveRow["part"], index: number, callId?: string) => {
      const id = `${part}:${index}`;
      let row = this.live.get(id);
      if (!row) {
        const [item, setItem] = createSignal<Item | null>(null);
        row = { k: "live", key: `live:${++liveSeq}`, part, index, item, setItem };
      }
      if (callId) row.callId = callId;
      next.set(id, row);
      out.push(row);
    };
    if (now.speaker) add("speaker", -1);
    for (const p of now.parts) add(p.type === "tool_call" ? "tool" : p.type, p.index, p.id || undefined);
    this.live = next;
    return out;
  }
}

/** Whether committed item `it` is what live row `r` was showing. */
function takes(r: LiveRow, it: Item): boolean {
  const index = () => Number(it.key.slice(it.key.lastIndexOf(":") + 1));
  switch (it.k) {
    case "speaker":
      return r.part === "speaker";
    case "thinking":
      return r.part === "thinking" && r.index === index();
    case "text":
    case "narr":
      return r.part === "text" && r.index === index();
    case "tool":
      return r.part === "tool" && !!r.callId && r.callId === it.call.id;
    default:
      return false;
  }
}
