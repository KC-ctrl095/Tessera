import { createSignal, onCleanup, onMount } from "solid-js";
import type { SessionState } from "../../lib/store";
import { historyPaging, loadOlderHistory } from "../../lib/store";
import { isProtocolEntry } from "../../lib/transcript";
import { setJumpTarget, toastError } from "../../lib/ui";
import { Icon } from "../icons";

/** Find requests older pages only until it can navigate to the next matching entry. */
export default function ChatFind(props: { session: SessionState; paneId: string; close: () => void }) {
  const [query, setQuery] = createSignal("");
  const [status, setStatus] = createSignal("");
  let field!: HTMLInputElement, generation = 0, last = Infinity, prior = "";
  let alive = true;
  onCleanup(() => { alive = false; generation++; });
  onMount(() => field.focus());
  const find = async () => {
    const needle = query().trim().toLocaleLowerCase(), g = ++generation;
    if (!needle) return;
    if (needle !== prior) last = Infinity;
    prior = needle;
    const session = props.session, revision = session.historyRevision, epoch = historyPaging.generation();
    const valid = () => alive && g === generation && revision === session.historyRevision && epoch === historyPaging.generation();
    setStatus("Finding…");
    try {
      while (valid()) {
        const hit = [...session.entries].reverse().find((e) => e.seq < last && !isProtocolEntry(e)
          && (e.kind === "user" || e.kind === "assistant" || e.kind === "tool_result")
          && e.content.some((p) => p.type === "text" && p.text.toLocaleLowerCase().includes(needle)));
        if (hit) {
          last = hit.seq;
          setJumpTarget({ sessionId: session.info.id, paneId: props.paneId, seq: hit.seq });
          setStatus("Found");
          return;
        }
        if (!session.hasOlderHistory) { last = Infinity; setStatus("No earlier matches"); return; }
        const page = historyPaging.pages.get(session.info.id);
        await loadOlderHistory(session.info.id);
        if (historyPaging.pages.get(session.info.id) === page) break;
      }
      if (valid()) setStatus("Try again");
    } catch (e) { if (valid()) { setStatus("Try again"); toastError(e, "Could not search earlier messages"); } }
  };
  return <form class="chat-find" role="search" onSubmit={(e) => { e.preventDefault(); void find(); }}>
    <input ref={field} class="input" type="search" aria-label="Find in chat" placeholder="Find in chat" value={query()} onInput={(e) => { generation++; setQuery(e.currentTarget.value); setStatus(""); }} onKeyDown={(e) => { if (e.key === "Escape") { e.stopPropagation(); props.close(); } }} />
    <span role="status">{status()}</span>
    <button class="icon-btn" type="submit" aria-label="Earlier match"><Icon name="arrowUp" size={15} /></button>
    <button class="icon-btn" type="button" aria-label="Close find" onClick={props.close}><Icon name="x" size={15} /></button>
  </form>;
}
