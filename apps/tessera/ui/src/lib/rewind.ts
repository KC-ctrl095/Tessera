// Eager rewind eligibility and dialog opening. Confirmation stays with the lazy dialog.
import { createMemo, createRoot } from "solid-js";
import type { SessionInfo } from "../proto/SessionInfo";
import { isBotDm, isBusy, state } from "./store";
import { setRewindDialog, toast } from "./ui";

/** Private chats keep nothing to go back to; a subagent is rewound through the chat that started it. */
export const canRewind = (info: SessionInfo | undefined): info is SessionInfo =>
  !!info && !info.private && info.kind !== "subagent" && !isBotDm(info);

/** Shared across every turn footer. A long chat must not scan the entire
 * reactive catalog once per rendered user message on each snapshot. */
const busyParents = createRoot(() => createMemo(() => new Set(
  Object.values(state.sessions).filter((s) => s.info.parent_id && s.info.kind === "subagent" && !s.info.internal && isBusy(s.info)).map((s) => s.info.parent_id),
)));

/** The agent or one of its subagents is working: the server refuses until they stop. */
export function rewindBlocked(info: SessionInfo | undefined): boolean {
  if (!info || isBusy(info)) return true;
  // The harness's own helpers (the memory review) don't count: the user never started them.
  return busyParents().has(info.id);
}

export const REWIND_BLOCKED = "Stop the agent first";

/** Why a chat can't be rewound, for commands typed where there's no button to hide. */
function refusal(info: SessionInfo | undefined): string | null {
  if (!info) return "Send a message first — this chat has no history yet.";
  if (info.private) return "Private chats can't be rewound: nothing of them is kept.";
  if (info.kind === "subagent") return "A helper is rewound with the chat that started it.";
  if (isBotDm(info)) return "Conversations between bots can't be rewound.";
  if (rewindBlocked(info)) return `${REWIND_BLOCKED}, then rewind.`;
  return null;
}

/** Open the rewind dialog for the user message at `seq` (its text returns to `paneId`'s composer). */
export function openRewind(sessionId: string, seq: number, paneId: string | null) {
  const why = refusal(state.sessions[sessionId]?.info);
  if (why) {
    toast({ level: "info", message: why, timeout: 2800 });
    return;
  }
  setRewindDialog({ sessionId, seq, paneId });
}

/** The latest message the user typed in a chat (what `/rewind` goes back to before). */
export function lastUserSeq(sessionId: string): number | null {
  const entries = state.sessions[sessionId]?.entries ?? [];
  for (let i = entries.length - 1; i >= 0; i--) {
    const e = entries[i];
    if (e.kind === "user" && !e.author) return e.seq;
  }
  return null;
}

/** `/rewind` and the palette: back to before your last message. */
export async function rewindLast(sessionId: string | null | undefined, paneId: string | null) {
  const info = sessionId ? state.sessions[sessionId]?.info : undefined;
  const why = refusal(info);
  if (sessionId && !why && (!state.sessions[sessionId]?.loaded || (lastUserSeq(sessionId) === null && state.sessions[sessionId]?.hasOlderHistory))) {
    try { await import("./history").then((m) => m.historyUntilUser(sessionId)); }
    catch { toast({ level: "error", message: "Could not load earlier messages. Try again." }); return; }
  }
  const seq = sessionId && !why ? lastUserSeq(sessionId) : null;
  if (why || seq === null) {
    toast({ level: "info", message: why ?? "None of your messages to rewind to yet.", timeout: 2800 });
    return;
  }
  openRewind(sessionId!, seq, paneId);
}
