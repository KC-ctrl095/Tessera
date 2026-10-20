// Work done only after the reader asks for older history.
import { TranscriptDeriver, turnText } from "./transcript";
import { produce, unwrap } from "solid-js/store";
import { connectionGeneration } from "./connectionGeneration";
import { api, ensureLoaded, historyPaging, loadOlderHistory, loadSession, state, setState, type ToolResultEntry } from "./store";

// Readers capture their viewport at the commit boundary, not before the fetch.
export const beforePrepend = new Map<string, Set<() => Promise<void>>>();

export async function prependHistory(id: string): Promise<void> {
  const page = historyPaging.pages.get(id), session = state.sessions[id];
  if (!session?.loaded || page?.before == null) return;
  const generation = historyPaging.generation(), connection = connectionGeneration(), revision = session.historyRevision;
  const valid = () => generation === historyPaging.generation() && connection === connectionGeneration()
    && historyPaging.pages.get(id) === page && state.sessions[id]?.historyRevision === revision;
  const snap = await api.getSession(id, undefined, { before_seq: page.before });
  if (!valid()) return;
  if ((snap.history_revision ?? 0) !== revision) return loadSession(id, false);
  // A cursor must advance even through hidden protocol rows. Refuse a stalled
  // continuation instead of spinning if an older server returns a bad page.
  if (snap.history_before != null && snap.history_before >= page.before) throw new Error("Older history did not advance. Try again.");
  for (const prepare of beforePrepend.get(id) ?? []) await prepare();
  if (!valid()) return;
  setState("sessions", id, produce((d) => {
    const first = d.entries[0]?.seq ?? Number.MAX_SAFE_INTEGER;
    const entries = snap.entries.filter((entry) => entry.seq < first);
    if (entries.length) {
      d.entries = [...entries, ...unwrap(d.entries)];
      d.results = { ...Object.fromEntries(entries.filter((e): e is ToolResultEntry => e.kind === "tool_result").map((e) => [e.call_id, e])), ...unwrap(d.results) };
      d.entriesVersion++;
    }
    d.hasOlderHistory = snap.history_before != null;
  }));
  historyPaging.pages.set(id, { before: snap.history_before ?? null });
}

/** Load just enough to cover a sequence, or all history for an explicit full reader. */
export async function historyTo(id: string, seq: number, valid: () => boolean = () => true): Promise<void> {
  await ensureLoaded(id);
  const session = state.sessions[id], generation = historyPaging.generation(), connection = connectionGeneration();
  if (!session) return;
  while (valid() && session === state.sessions[id] && generation === historyPaging.generation() && connection === connectionGeneration()
    && session.hasOlderHistory && (session.entries[0]?.seq ?? Infinity) > seq) {
    const before = historyPaging.pages.get(id);
    await loadOlderHistory(id);
    if (historyPaging.pages.get(id) === before) break;
  }
}

/** A very long answer may put its user's message before the newest page. */
export async function historyUntilUser(id: string, before = Infinity): Promise<void> {
  await ensureLoaded(id);
  const generation = historyPaging.generation(), connection = connectionGeneration();
  while (generation === historyPaging.generation() && connection === connectionGeneration()) {
    const s = state.sessions[id];
    if (!s?.hasOlderHistory || s.entries.some((e) => e.seq < before && e.kind === "user" && !e.author)) return;
    const page = historyPaging.pages.get(id);
    await loadOlderHistory(id);
    if (historyPaging.pages.get(id) === page) return;
  }
}

/** Copy the complete response, not just the portion that happened to open first. */
export async function responseText(id: string, to: number): Promise<string> {
  await historyUntilUser(id, to);
  const s = state.sessions[id];
  if (!s) return "";
  const footer = new TranscriptDeriver().derive(unwrap(s.entries), false, {
    speakers: s.info.kind === "channel" || !!s.info.bot_id,
    defaultAuthor: s.info.kind === "channel" ? undefined : s.info.bot_id,
  }).find((it) => it.k === "turn" && it.entry.seq === to);
  return footer?.k === "turn" ? turnText(s.entries, footer.first, to) : "";
}
