// A question an agent asks on a whiteboard: a signature, a line of text, a
// choice or yes/no. Always the app's own UI (never inside the board's frame),
// docked at the bottom of the board tile and mirrored in the chat.
import { For, Match, Show, Switch, createSignal, onMount } from "solid-js";
import type { AskAnswer } from "../../proto/AskAnswer";
import type { Whiteboard } from "../../proto/Whiteboard";
import { answerAsk, shownBy } from "../../lib/boards";
import { coarsePointer } from "../../lib/util";
import { Icon, Spinner } from "../icons";
import { SignaturePad, type SignatureHandle } from "./SignaturePad";
import "../../styles/ask.css";

export function AskPanel(props: { board: Whiteboard; where: "tile" | "chat"; onOpenBoard?: () => void }) {
  const ask = () => props.board.ask!;
  const [busy, setBusy] = createSignal<AskAnswer["type"] | null>(null);
  const [text, setText] = createSignal("");
  const [inked, setInked] = createSignal(false);
  let sig: SignatureHandle | undefined;
  let field: HTMLInputElement | HTMLTextAreaElement | undefined;

  const send = async (a: AskAnswer) => {
    if (busy()) return;
    setBusy(a.type);
    try {
      await answerAsk(props.board, a);
    } finally {
      setBusy(null);
    }
  };
  const sign = () => {
    const png = sig?.png();
    if (png) void send({ type: "signature", png });
  };
  const sendText = () => {
    if (text().trim()) void send({ type: "text", text: text().trim() });
  };
  /** Several may be picked: what is ticked so far. */
  const [picked, setPicked] = createSignal<number[]>([]);
  const toggle = (i: number) => setPicked((p) => (p.includes(i) ? p.filter((x) => x !== i) : [...p, i]));
  const choice = () => {
    const i = ask().input;
    return i.type === "choice" ? i : null;
  };

  onMount(() => {
    // The tile's own ask takes the keyboard on desktop; the chat copy never steals it.
    if (props.where === "tile" && !coarsePointer()) requestAnimationFrame(() => field?.focus({ preventScroll: true }));
  });

  // A web view handed to you (`browser` `needs_user`): do it there, then say so.
  const handover = () => ask().input.type === "confirm" && (props.board.content.type === "url" || props.board.content.type === "local");
  const textInput = () => {
    const i = ask().input;
    return i.type === "text" ? i : null;
  };
  const Cancel = () => (
    <button type="button" class="btn btn-sm btn-ghost ask-cancel" disabled={!!busy()} onClick={() => void send({ type: "cancel" })} data-tip="Don't answer: the agent is told you declined">
      <Show when={busy() === "cancel"} fallback="Cancel">
        <Spinner size={12} />
      </Show>
    </button>
  );

  return (
    <div class="ask" classList={{ [`ask-${props.where}`]: true, [`ask-${ask().input.type}`]: true }} role="group" aria-label="The agent is asking you">
      <div class="ask-head">
        <span class="ask-mark" aria-hidden="true">
          <Icon name={ask().input.type === "signature" ? "pen" : "help"} size={14} />
        </span>
        <div class="ask-titles">
          <span class="ask-eyebrow">
            {handover()
              ? shownBy(props.board) === "the agent"
                ? "The agent needs you here"
                : `${shownBy(props.board)} needs you here`
              : shownBy(props.board) === "the agent"
                ? "The agent asks"
                : `${shownBy(props.board)} asks`}
            <Show when={props.where === "chat"}>
              {" · "}
              <button type="button" class="link-btn" onClick={() => props.onOpenBoard?.()}>
                {props.board.title}
              </button>
            </Show>
          </span>
          <p class="ask-prompt">{ask().prompt}</p>
        </div>
      </div>
      <Switch>
        <Match when={ask().input.type === "signature"}>
          <SignaturePad onChange={setInked} handle={(h) => (sig = h)} label={`Sign: ${ask().prompt}`}>
            <Cancel />
            <button type="button" class="btn btn-sm btn-primary" disabled={!inked() || !!busy()} onClick={sign}>
              <Show when={busy() === "signature"} fallback={<Icon name="check" size={14} stroke={2.2} />}>
                <Spinner size={12} />
              </Show>
              Sign
            </button>
          </SignaturePad>
        </Match>
        <Match when={textInput()}>
          {(inp) => {
            const i = inp();
            return (
              <div class="ask-text">
                <Show
                  when={i.multiline}
                  fallback={
                    <input
                      ref={(el) => (field = el)}
                      class="input"
                      value={text()}
                      placeholder={i.placeholder || "Your answer"}
                      onInput={(e) => setText(e.currentTarget.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && !e.isComposing) {
                          e.preventDefault();
                          sendText();
                        }
                      }}
                    />
                  }
                >
                  <textarea
                    ref={(el) => (field = el)}
                    class="input"
                    rows={3}
                    value={text()}
                    placeholder={i.placeholder || "Your answer"}
                    onInput={(e) => setText(e.currentTarget.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                        e.preventDefault();
                        sendText();
                      }
                    }}
                  />
                </Show>
                <div class="ask-actions">
                  <Cancel />
                  <button type="button" class="btn btn-sm btn-primary" disabled={!text().trim() || !!busy()} onClick={sendText}>
                    <Show when={busy() === "text"} fallback={<Icon name="arrowUp" size={14} stroke={2.2} />}>
                      <Spinner size={12} />
                    </Show>
                    Send
                  </button>
                </div>
              </div>
            );
          }}
        </Match>
        <Match when={choice()?.multi}>
          {/* Several may be picked: tick them, then send. */}
          <div class="ask-choices" role="group" aria-label={ask().prompt}>
            <For each={choice()!.options}>
              {(o, i) => (
                <button type="button" role="checkbox" aria-checked={picked().includes(i())} class="ask-choice" classList={{ on: picked().includes(i()) }} disabled={!!busy()} onClick={() => toggle(i())}>
                  <span class="ask-choice-n">
                    <Show when={picked().includes(i())} fallback={i() + 1}>
                      <Icon name="check" size={12} stroke={2.4} />
                    </Show>
                  </span>
                  <span class="ask-choice-label">{o}</span>
                </button>
              )}
            </For>
          </div>
          <div class="ask-actions">
            <Cancel />
            <span class="spacer" />
            <button
              type="button"
              class="btn btn-sm btn-primary"
              disabled={!picked().length || !!busy()}
              onClick={() => void send({ type: "choices", indexes: [...picked()].sort((a, b) => a - b) })}
            >
              <Show when={busy() === "choices"} fallback={<Icon name="arrowUp" size={14} stroke={2.2} />}>
                <Spinner size={12} />
              </Show>
              Send
            </button>
          </div>
        </Match>
        <Match when={choice()}>
          <div class="ask-choices" role="radiogroup" aria-label={ask().prompt}>
            <For each={choice()!.options}>
              {(o, i) => (
                <button type="button" role="radio" aria-checked={false} class="ask-choice" disabled={!!busy()} onClick={() => void send({ type: "choice", index: i() })}>
                  <span class="ask-choice-n">{i() + 1}</span>
                  <span class="ask-choice-label">{o}</span>
                  <Icon name="arrowRight" size={13} class="ask-choice-go" />
                </button>
              )}
            </For>
          </div>
          <div class="ask-actions"><Cancel /></div>
        </Match>
        <Match when={ask().input.type === "confirm"}>
          <div class="ask-actions">
            <Show when={!handover()}>
              <Cancel />
            </Show>
            <span class="spacer" />
            <button type="button" class="btn btn-sm" disabled={!!busy()} onClick={() => void send({ type: "confirm", yes: false })}>
              {handover() ? "I can't" : "No"}
            </button>
            <button type="button" class="btn btn-sm btn-primary" disabled={!!busy()} onClick={() => void send({ type: "confirm", yes: true })}>
              <Show when={busy() === "confirm"} fallback={<Icon name="check" size={14} stroke={2.2} />}>
                <Spinner size={12} />
              </Show>
              {handover() ? "Done" : "Yes"}
            </button>
          </div>
        </Match>
      </Switch>
    </div>
  );
}
