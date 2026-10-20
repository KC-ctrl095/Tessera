// The `recall` tool's card: what the agent looked up in past chats, as a
// compact list of sources that open the chat at that message. Searches show
// their hits; reading one item (`seq`) or a stored output (`artifact`) shows
// where it came from. Registered by ToolCard (APP-WEB):
//   if (isRecallTool(view.name)) return <RecallCard {...props} />
import { For, Show, createMemo, createSignal } from "solid-js";
import { state, type SessionState } from "../../lib/store";
import { argsObject } from "../../lib/tools";
import { setJumpTarget } from "../../lib/ui";
import { openSession, paneShowing } from "../../lib/workspace";
import { Icon, Tessellate } from "../icons";
import { Snippet } from "../Snippet";
import type { ToolView } from "./ToolCard";
import "../../styles/recall.css";

export const isRecallTool = (name: string) => name === "recall";

export interface RecallHit {
  session_id: string;
  session_title: string;
  seq: number;
  /** Unix ms, when known. */
  ts?: number;
  day?: string;
  snippet: string;
}

/** Hits from the result: structured `details.recall.hits`, else the text lines. */
export function recallHits(view: ToolView, here?: string): RecallHit[] {
  const det = (view.result?.details as { recall?: { hits?: unknown } } | undefined)?.recall?.hits;
  if (Array.isArray(det)) {
    return det
      .filter((h): h is Record<string, unknown> => !!h && typeof h === "object")
      .map((h) => ({
        session_id: String(h.session_id ?? here ?? ""),
        session_title: String(h.session_title ?? h.title ?? ""),
        seq: Number(h.seq ?? 0),
        ts: typeof h.ts === "number" ? h.ts : undefined,
        snippet: String(h.snippet ?? ""),
      }))
      .filter((h) => h.session_id);
  }
  const text = view.result?.content.map((c) => (c.type === "text" ? c.text : "")).join("\n") ?? "";
  const out: RecallHit[] = [];
  for (const line of text.split("\n")) {
    // `seq 12 [Title · 2026-09-01 · ses_id]: snippet` (all chats) or `seq 12: snippet` (this chat).
    const m = /^seq (\d+)(?: \[(.*) · (\d{4}-\d{2}-\d{2}) · ([A-Za-z0-9_-]+)\])?: (.*)$/.exec(line.trim());
    if (!m) continue;
    const session_id = m[4] ?? here;
    if (!session_id) continue;
    out.push({ session_id, session_title: m[2] ?? "", seq: Number(m[1]), day: m[3], snippet: m[5] });
  }
  return out;
}

const fmtDay = (h: RecallHit) => {
  const t = h.ts ?? (h.day ? new Date(`${h.day}T12:00:00`).getTime() : NaN);
  if (!Number.isFinite(t)) return "";
  const d = new Date(t);
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric", year: d.getFullYear() === new Date().getFullYear() ? undefined : "numeric" });
};

/** Open the chat at that message (beside, unless it is this chat). */
export function openRecallHit(h: RecallHit, here?: string) {
  const same = h.session_id === here;
  const pane = (same ? paneShowing(h.session_id) : null) ?? openSession(h.session_id, { newPane: !same });
  if (pane) setJumpTarget({ paneId: pane, sessionId: h.session_id, seq: h.seq });
}

export function RecallCard(props: { view: ToolView; defaultOpen?: boolean; flat?: boolean; session?: SessionState; ts?: number; open?: boolean; onOpenChange?: (open: boolean) => void }) {
  const v = () => props.view;
  const here = () => props.session?.info.id;
  const args = createMemo(() => argsObject(v().args) ?? {});
  const query = () => (typeof args().query === "string" ? (args().query as string) : "");
  const mode = () => (query() ? "search" : typeof args().artifact === "string" ? "artifact" : args().seq != null ? "item" : "search");
  const hits = createMemo(() => (v().result && !v().result!.is_error ? recallHits(v(), here()) : []));
  const [localOpen, setLocalOpen] = createSignal(!!props.defaultOpen);
  const open = () => props.open ?? localOpen();
  const setOpen = (value: boolean) => props.onOpenChange ? props.onOpenChange(value) : setLocalOpen(value);
  const running = () => v().status === "running" || v().status === "preparing" || v().status === "pending";
  const failed = () => v().status === "error";
  const titleOf = (h: RecallHit) => h.session_title || state.sessions[h.session_id]?.info.title || (h.session_id === here() ? "This chat" : "A chat");
  /** Distinct chats among the hits, for the collapsed line. */
  const sources = createMemo(() => {
    const seen = new Map<string, RecallHit>();
    for (const h of hits()) if (!seen.has(h.session_id)) seen.set(h.session_id, h);
    return [...seen.values()];
  });
  const where = () => {
    const a = args();
    if (mode() === "artifact") return `stored output ${String(a.artifact)}`;
    if (mode() === "item") {
      const sid = typeof a.session_id === "string" ? a.session_id : here();
      const title = sid ? state.sessions[sid]?.info.title : undefined;
      return `message ${String(a.seq)}${title && sid !== here() ? ` in “${title}”` : sid === here() ? " of this chat" : ""}`;
    }
    return query() ? `“${query()}”` : "";
  };
  const scopeLabel = () => (args().scope === "all" ? "all chats" : mode() === "search" ? "this chat" : "");

  return (
    <div class="tool recall" data-status={v().status} classList={{ open: open(), flat: !!props.flat }}>
      <button type="button" class="tool-head" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <span class="tool-st">
          <Show when={running()} fallback={<span class={`sq sq-${v().status}`} role="img" aria-label={failed() ? "Failed" : "Done"} />}>
            <Tessellate size={9} label="Looking back" />
          </Show>
        </span>
        <span class="tool-name">recall</span>
        <span class="tool-sum" title={where()}>
          {running() ? `Looking back${where() ? ` for ${where()}` : ""}…` : where()}
          <Show when={scopeLabel() && !running()}>
            <span class="recall-scope"> · {scopeLabel()}</span>
          </Show>
        </span>
        <span class="tool-meta">
          <Show when={mode() === "search" && v().status === "ok"}>
            <span class="recall-count">{hits().length ? `${hits().length} ${hits().length === 1 ? "match" : "matches"}` : "nothing found"}</span>
          </Show>
        </span>
        <Icon name="chevronDown" size={13} class="chev" />
      </button>

      {/* Where it came from, always in view: one link per chat. */}
      <Show when={!open() && sources().length}>
        <div class="recall-sources">
          <For each={sources().slice(0, 3)}>
            {(h) => (
              <button type="button" class="recall-src" onClick={() => openRecallHit(h, here())} title={`Open “${titleOf(h)}” at that message`}>
                <Icon name={h.session_id === here() ? "cornerDownRight" : "chat"} size={12} />
                <span class="recall-src-title">{titleOf(h)}</span>
                <Show when={fmtDay(h)}>
                  <span class="recall-src-day">{fmtDay(h)}</span>
                </Show>
              </button>
            )}
          </For>
          <Show when={sources().length > 3}>
            <button type="button" class="recall-more" onClick={() => setOpen(true)}>
              +{sources().length - 3} more
            </button>
          </Show>
        </div>
      </Show>

      <Show when={open()}>
        <div class="tool-body">
          <Show
            when={hits().length}
            fallback={
              <pre class="tool-pre" classList={{ err: failed() }}>
                {v().result ? v().result!.content.map((c) => (c.type === "text" ? c.text : "")).join("\n") || "(no output)" : v().live?.output || "…"}
              </pre>
            }
          >
            <ul class="recall-hits">
              <For each={hits()}>
                {(h) => (
                  <li>
                    <button type="button" class="recall-hit" onClick={() => openRecallHit(h, here())}>
                      <span class="recall-hit-head">
                        <Icon name={h.session_id === here() ? "cornerDownRight" : "chat"} size={12} />
                        <span class="recall-hit-title">{titleOf(h)}</span>
                        <span class="recall-hit-day">{fmtDay(h)}</span>
                        <Icon name="arrowRight" size={12} class="recall-go" />
                      </span>
                      <span class="recall-hit-snippet">
                        <Snippet text={h.snippet} />
                      </span>
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </div>
      </Show>
    </div>
  );
}
