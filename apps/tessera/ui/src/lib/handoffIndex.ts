// One append-only call/reply index per parent, shared by all handoff cards.
// Completed cards read the same reply object; they never rescan old entries.
import { untrack } from "solid-js";
import type { Entry } from "../proto/Entry";
import { plainText, type SessionState } from "./store";
import { parseBotReply, type BotReply, type UserEntry } from "./transcript";
type Reply = { entry: UserEntry; reply: BotReply };
class HandoffIndex {
  private entries: readonly Entry[] | null = null;
  private version = -1;
  private length = 0;
  private last?: Entry;
  private calls = new Map<string, number>();
  private replies = new Map<string, Reply[]>();
  private key(handle: string, dmId: string) { return JSON.stringify([handle, dmId]); }
  update(session: SessionState) {
    const entries = session.entries, version = session.entriesVersion, length = entries.length;
    untrack(() => {
      if (entries !== this.entries || version !== this.version || length < this.length || (this.length && entries[this.length - 1] !== this.last)) {
        this.calls.clear(); this.replies.clear(); this.length = 0;
      }
      for (let i = this.length; i < length; i++) {
        const e = entries[i];
        if (e.kind === "assistant") {
          for (const p of e.content) if (p.type === "tool_call") this.calls.set(p.id, e.seq);
        } else if (e.kind === "user" && e.author) {
          const reply = parseBotReply(plainText(e.content));
          if (!reply?.sessionId) continue;
          const key = this.key(`@${reply.handle}`, reply.sessionId);
          const list = this.replies.get(key) ?? [];
          list.push({entry:e, reply}); this.replies.set(key, list);
        }
      }
      this.entries = entries; this.version = version; this.length = length; this.last = entries.at(-1);
    });
  }
  reply(callId: string, handle: string, dmId: string): Reply | null {
    const seq = this.calls.get(callId);
    if (seq === undefined) return null;
    const list = this.replies.get(this.key(handle, dmId)) ?? [];
    let lo = 0, hi = list.length;
    while (lo < hi) { const mid = (lo + hi) >>> 1; if (list[mid].entry.seq <= seq) lo = mid + 1; else hi = mid; }
    return list[lo] ?? null;
  }
}
const indexes = new WeakMap<SessionState, HandoffIndex>();
export function handoffReply(session: SessionState, callId: string, handle: string, dmId: string): Reply | null {
  let index = indexes.get(session);
  if (!index) indexes.set(session, index = new HandoffIndex());
  index.update(session);
  return index.reply(callId, handle, dmId);
}
