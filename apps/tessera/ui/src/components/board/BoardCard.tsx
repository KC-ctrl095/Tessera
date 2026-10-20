// Whiteboards in the chat transcript: a compact card for the board a
// `whiteboard` tool call made (one tap opens its tile; an agent's board only
// gets a tile by itself when the user asked to see it), an image shown right
// in the chat, and the board's question mirrored so it can be answered even
// with the tile closed.
import { For, Show, createSignal } from "solid-js";
import type { Whiteboard } from "../../proto/Whiteboard";
import { BOARD_KIND, askOutcome, boardOfCall, type BoardKind } from "../../lib/boards";
import { pendingAsks, plainText, state, type SessionState } from "../../lib/store";
import { argsObject } from "../../lib/tools";
import { openViewer } from "../../lib/viewer";
import { openBoard, paneShowingBoard, workspace } from "../../lib/workspace";
import { Icon, Tessellate } from "../icons";
import { AnsweredLine } from "./AnsweredLine";
import { onDemand } from "../../lib/onDemand";

/** The question itself (and its signature pad) loads when a board asks one. */
const askPanel = onDemand(() => import("./AskPanel"));

/** Actions of the `whiteboard` tool that put a board on screen. */
export function showsBoard(args: unknown): boolean {
  const a = argsObject(args);
  const action = typeof a?.action === "string" ? a.action : "show";
  return action === "show" || action === "ask";
}

export function BoardCallCard(props: { session: SessionState; callId: string; args: unknown; preparing?: boolean }) {
  const result = () => props.session.results[props.callId];
  const ref = () => boardOfCall(props.session.info.id, props.args, result()?.details);
  const board = (): Whiteboard | undefined => (ref() ? state.boards[ref()!.id] : undefined);
  const kind = (): BoardKind | undefined => board()?.content.type ?? (ref()?.kind as BoardKind | undefined);
  const title = () => board()?.title ?? ref()?.title ?? (argsObject(props.args)?.title as string | undefined) ?? "Whiteboard";
  const failed = () => !!result()?.is_error;
  const pending = () => !result() && !failed();
  const isAsk = () => argsObject(props.args)?.action === "ask";
  /** An ask's outcome, once its result is in. */
  const outcome = () => (isAsk() && result() && !result()!.is_error ? askOutcome(plainText(result()!.content)) : null);
  const shown = () => (board() ? !!paneShowingBoard(board()!.id) : false);
  const onScreen = () => {
    const id = board() ? paneShowingBoard(board()!.id) : null;
    return !!id && workspace.paneWorkspace(id) === workspace.active();
  };
  const thumb = () => {
    const c = board()?.content;
    return c?.type === "image" ? `data:${c.mime};base64,${c.data}` : null;
  };
  const open = () => {
    const b = board();
    if (b) openBoard(b.id);
  };

  return (
    <Show
      when={!failed()}
      fallback={
        <div class="board-card gone" role="note">
          <span class="board-card-art">
            <Icon name="board" size={20} />
          </span>
          <span class="board-card-text">
            <span class="board-card-title">{title()}</span>
            <span class="board-card-meta">The whiteboard couldn't be shown</span>
          </span>
        </div>
      }
    >
      <button
        type="button"
        class="board-card"
        classList={{ gone: !!ref() && !board() && !pending() }}
        disabled={!board()}
        onClick={open}
        aria-label={board() ? `Open whiteboard “${title()}”` : title()}
      >
        {/* An image shows in full right below, so its card has the plain icon. */}
        <span class="board-card-art" aria-hidden="true">
          <Icon name={kind() ? BOARD_KIND[kind()!].icon : "board"} size={20} />
        </span>
        <span class="board-card-text">
          <span class="board-card-title">{title()}</span>
          <span class="board-card-meta">
            <Show when={pending() && !board()} fallback={<span>{kind() ? BOARD_KIND[kind()!].label : "Whiteboard"}</span>}>
              <Tessellate size={7} />
              <span>{props.preparing ? "Drawing the whiteboard…" : "Putting it up…"}</span>
            </Show>
            <Show when={board()?.ask && !outcome()}>
              <span class="needs-you">
                <i aria-hidden="true" />
                Needs you
              </span>
            </Show>
            <Show when={outcome()}>
              <span class="dot-sep">·</span>
              <span class="board-card-outcome">
                <Icon name="check" size={12} stroke={2.4} />
                {outcome()}
              </span>
            </Show>
            <Show when={!board()?.ask && board() && !outcome() && shown()}>
              <span class="dot-sep">·</span>
              <span>{onScreen() ? "On screen" : "In another mosaic"}</span>
            </Show>
            <Show when={ref() && !board() && !pending()}>
              <span class="dot-sep">·</span>
              <span>Deleted</span>
            </Show>
          </span>
        </span>
        <Show when={board()}>
          <span class="btn btn-sm board-card-open" aria-hidden="true">
            {onScreen() ? "Show" : "Open"}
          </span>
        </Show>
      </button>
      {/* A picture the agent shared is seen right here; a tap views it full size. */}
      <Show when={thumb()}>
        <button type="button" class="board-inline-image" aria-label={`View “${title()}”`} onClick={() => openViewer([{ src: thumb()!, alt: title() }])}>
          <img src={thumb()!} alt={title()} />
        </button>
      </Show>
    </Show>
  );
}

/**
 * Questions waiting on this chat's boards, and ones just answered here. While
 * the board's tile is on screen (its own panel asks there) the copy here is
 * one line; with the tile closed or elsewhere it's the full question.
 */
export function ChatAsks(props: { session: SessionState }) {
  const asks = () => pendingAsks(props.session.info.id);
  const answered = () =>
    Object.entries(state.answered)
      .filter(([, a]) => a.session === props.session.info.id)
      .map(([id, a]) => ({ id, ...a }));
  return (
    <>
      <For each={answered()}>
        {(a) => (
          <div class="chat-ask">
            <AnsweredLine label={a.label} prompt={a.prompt} />
          </div>
        )}
      </For>
      <For each={asks()}>
        {(b) => {
          void askPanel.load();
          const [expanded, setExpanded] = createSignal(false);
          const onScreen = () => {
            const id = paneShowingBoard(b.id);
            return !!id && workspace.paneWorkspace(id) === workspace.active() && workspace.mode() !== "mobile";
          };
          return (
            <div class="chat-ask">
              <Show
                when={!onScreen() || expanded()}
                fallback={
                  <div class="ask-pointer" role="status">
                    <span class="needs-you">
                      <i aria-hidden="true" />
                      Needs you
                    </span>
                    <span class="ask-pointer-text">{b.ask!.prompt}</span>
                    <button type="button" class="link-btn" onClick={() => openBoard(b.id)}>
                      {b.content.type === "url" || b.content.type === "local" ? "In the web view" : "On the whiteboard"}
                    </button>
                    <button type="button" class="link-btn" onClick={() => setExpanded(true)}>
                      Answer here
                    </button>
                  </div>
                }
              >
                <Show when={askPanel.get()}>
                  {(m) => {
                    const Ask = m().AskPanel;
                    return <Ask board={b} where="chat" onOpenBoard={() => openBoard(b.id)} />;
                  }}
                </Show>
              </Show>
            </div>
          );
        }}
      </For>
    </>
  );
}
