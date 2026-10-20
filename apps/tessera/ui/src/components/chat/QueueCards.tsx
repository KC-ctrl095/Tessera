// Messages waiting for the busy agent, above the composer: queued follow-ups
// (sent when the agent is done), steering (read at its next step) and what
// is being sent now (the agent stops for it). Each can be edited in place,
// steered, sent now, or removed. Cards glide in, out and to new places.
import { For, Show, createEffect, createSignal, on, onCleanup, type Accessor, type Setter } from "solid-js";
import type { QueuedMessage } from "../../proto/QueuedMessage";
import { api, isBusy, type SessionState } from "../../lib/store";
import { toastError } from "../../lib/ui";
import { reducedMotion, touchUi } from "../../lib/util";
import { Icon } from "../icons";

interface Card {
  /** Which item of the queue this is (its text and which copy): changes when edited. */
  key: string;
  msg: Accessor<QueuedMessage>;
  setMsg: Setter<QueuedMessage>;
  leaving: Accessor<boolean>;
  setLeaving: Setter<boolean>;
  el?: HTMLElement;
}

const EASE = "cubic-bezier(0.16, 0.84, 0.3, 1)";

/** Queue items have no ids: the same text is the same card (its nth copy). */
function keyed(list: readonly QueuedMessage[]): { key: string; msg: QueuedMessage }[] {
  const seen = new Map<string, number>();
  return list.map((msg) => {
    const n = (seen.get(msg.text) ?? 0) + 1;
    seen.set(msg.text, n);
    return { key: `${msg.text}\u0000${n}`, msg };
  });
}

export function QueueCards(props: { session: SessionState }) {
  const [cards, setCards] = createSignal<Card[]>([]);
  const [editing, setEditing] = createSignal<Card | null>(null);
  /** Cards by key, so each keeps its element, edit state and animations. */
  const pool = new Map<string, Card>();
  /** An edit just saved: its card keeps its place when the new text arrives. */
  let renamed: { card: Card; text: string; preview: string } | null = null;
  const sid = () => props.session.info.id;

  createEffect(
    on(
      () => props.session.queued.map((q) => ({ ...q })),
      (queued) => {
        const next = keyed(queued);
        if (renamed) {
          const r = renamed;
          renamed = null;
          const hit = next.find((c) => (c.msg.text === r.text || c.msg.text === r.preview) && !pool.has(c.key));
          if (hit && pool.get(r.card.key) === r.card) {
            pool.delete(r.card.key);
            r.card.key = hit.key;
            pool.set(hit.key, r.card);
          }
        }
        // FLIP: where the cards are now, before anything moves.
        const before = new Map<Card, DOMRect>();
        for (const c of cards()) if (c.el?.isConnected) before.set(c, c.el.getBoundingClientRect());
        const out: Card[] = next.map(({ key, msg }) => {
          let c = pool.get(key);
          if (!c) {
            const [m, setMsg] = createSignal(msg);
            const [l, setLeaving] = createSignal(false);
            c = { key, msg: m, setMsg, leaving: l, setLeaving };
            pool.set(key, c);
          } else {
            c.setMsg(msg);
            c.setLeaving(false);
          }
          return c;
        });
        // Cards that went away stay a moment where they were, fading out.
        cards().forEach((c, i) => {
          if (out.includes(c)) return;
          if (!c.leaving()) {
            c.setLeaving(true);
            setTimeout(() => {
              if (!c.leaving()) return;
              if (pool.get(c.key) === c) pool.delete(c.key);
              setCards((l) => l.filter((x) => x !== c));
            }, reducedMotion() ? 0 : 200);
          }
          out.splice(Math.min(i, out.length), 0, c);
        });
        if (editing() && !next.some((x) => x.key === editing()!.key)) setEditing(null);
        setCards(out);
        if (reducedMotion()) return;
        requestAnimationFrame(() => {
          for (const [c, o] of before) {
            if (!c.el?.isConnected || c.leaving()) continue;
            const dy = o.top - c.el.getBoundingClientRect().top;
            if (Math.abs(dy) > 1) c.el.animate([{ transform: `translateY(${dy}px)` }, { transform: "none" }], { duration: 220, easing: EASE });
          }
        });
      },
    ),
  );

  const indexOf = (c: Card) => keyed(props.session.queued).findIndex((x) => x.key === c.key);

  const remove = (c: Card) => {
    const i = indexOf(c);
    if (i < 0) return;
    api.dequeue(sid(), i).catch((e) => toastError(e, "Could not remove it"));
  };
  const steer = (c: Card) => {
    const i = indexOf(c);
    if (i < 0) return;
    api.steerQueued(sid(), i).catch((e) => toastError(e, "Could not steer with it"));
  };
  const sendNow = (c: Card) => {
    const i = indexOf(c);
    if (i < 0) return;
    api.interruptQueued(sid(), i).catch((e) => toastError(e, "Could not send it now"));
  };
  const save = async (c: Card, text: string) => {
    const i = indexOf(c);
    setEditing(null);
    const cur = props.session.queued[i];
    if (i < 0 || !cur || text.trim() === cur.text.trim()) return;
    // Emptied, a message with attachments is just them; without, it goes.
    if (!text.trim() && !cur.attachments) return remove(c);
    // The server sends the whole text (an older one only its first line).
    renamed = { card: c, text: text.trim(), preview: text.trim().split("\n").map((l) => l.trim()).find(Boolean) ?? text.trim() };
    try {
      await api.editQueued(sid(), i, text.trim());
    } catch (e) {
      renamed = null;
      toastError(e, "Could not change it");
    }
  };

  return (
    <div class="qcards" role="list" aria-label="Waiting for the agent">
      <For each={cards()}>
        {(c) => (
          <QueueCard
            card={c}
            busy={isBusy(props.session.info)}
            subagent={!!props.session.info.parent_id}
            editing={editing() === c}
            onEdit={() => setEditing(c)}
            onCancel={() => setEditing(null)}
            onSave={(t) => void save(c, t)}
            onSteer={() => steer(c)}
            onSendNow={() => sendNow(c)}
            onRemove={() => remove(c)}
          />
        )}
      </For>
    </div>
  );
}

function QueueCard(props: {
  card: Card;
  busy: boolean;
  /** A subagent's turns belong to its parent: it can be steered, not stopped. */
  subagent: boolean;
  editing: boolean;
  onEdit: () => void;
  onCancel: () => void;
  onSave: (text: string) => void;
  onSteer: () => void;
  onSendNow: () => void;
  onRemove: () => void;
}) {
  const m = () => props.card.msg();
  let el!: HTMLDivElement;
  let input: HTMLTextAreaElement | undefined;
  const [draft, setDraft] = createSignal("");
  const touch = touchUi();
  const files = () => (m().attachments === 1 ? "1 attachment" : `${m().attachments} attachments`);

  // Rise in when it appears.
  queueMicrotask(() => {
    props.card.el = el;
    if (!reducedMotion() && el && !props.card.leaving()) el.animate([{ opacity: 0, transform: "translateY(8px) scale(0.98)" }, { opacity: 1, transform: "none" }], { duration: 220, easing: EASE });
  });
  // Sink away when it goes (and back, should it return meanwhile).
  createEffect(
    on(
      () => props.card.leaving(),
      (leaving, was) => {
        if (!leaving && was) for (const a of el?.getAnimations() ?? []) a.cancel();
        if (!leaving || !el || reducedMotion()) return;
        const h = el.getBoundingClientRect().height;
        el.animate(
          [
            { opacity: 1, transform: "none", height: `${h}px` },
            { opacity: 0, transform: "translateY(-4px) scale(0.98)", height: "0px", marginTop: "-4px" },
          ],
          { duration: 190, easing: EASE, fill: "forwards" },
        );
      },
    ),
  );
  // The tag flips to "Steering" with a short glow.
  createEffect(
    on(
      () => m().steer,
      (steer, was) => {
        if (steer && was === false && el && !reducedMotion()) el.animate([{ boxShadow: "inset 0 0 0 1.5px rgba(var(--accent-rgb), 0.9)" }, { boxShadow: "inset 0 0 0 1px transparent" }], { duration: 700, easing: EASE });
      },
    ),
  );

  const fit = () => {
    if (!input) return;
    input.style.height = "0px";
    input.style.height = `${Math.min(160, input.scrollHeight)}px`;
  };
  createEffect(
    on(
      () => props.editing,
      (on) => {
        if (!on) return;
        setDraft(m().text);
        queueMicrotask(() => {
          if (!input) return;
          input.focus();
          input.setSelectionRange(input.value.length, input.value.length);
          fit();
        });
      },
    ),
  );
  let blurTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(blurTimer));

  return (
    <div
      ref={el}
      class="qcard"
      role="listitem"
      classList={{ steer: m().steer, now: !!m().interrupt, leaving: props.card.leaving(), editing: props.editing }}
      aria-hidden={props.card.leaving() ? "true" : undefined}
      aria-busy={m().interrupt ? "true" : undefined}
    >
      <span class="qtag" title={m().interrupt ? "The agent is stopping to read it now" : m().steer ? "Read at the agent's next step" : "Sent when the agent is done"}>
        <Icon name={m().interrupt ? "send" : m().steer ? "steer" : "clock"} size={11} />
        {m().interrupt ? "Sending" : m().steer ? "Steering" : "Queued"}
      </span>
      <Show
        when={props.editing}
        fallback={
          <p class="qtext" title={m().text} onDblClick={() => !props.card.leaving() && !m().interrupt && props.onEdit()}>
            <Show when={m().attachments}>
              <span class="qatt" aria-label={files()}>
                <Icon name="paperclip" size={11} />
                {m().attachments}
              </span>
            </Show>
            {m().text || <span class="qatt-only">{files()}</span>}
          </p>
        }
      >
        <div class="qedit">
          <textarea
            ref={input}
            class="qinput"
            value={draft()}
            rows={1}
            aria-label="Edit the queued message"
            onInput={(e) => {
              setDraft(e.currentTarget.value);
              fit();
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
                e.preventDefault();
                props.onSave(draft());
              } else if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                props.onCancel();
              }
            }}
            onBlur={() => {
              // Clicking Save/Cancel blurs first; let the click decide.
              blurTimer = setTimeout(() => props.editing && props.onSave(draft()), 150);
            }}
          />
          <span class="qedit-hint">{[m().attachments ? `Keeps ${files()}` : "", touch ? "" : "↵ save · Esc cancel"].filter(Boolean).join(" · ")}</span>
        </div>
      </Show>
      <div class="qacts">
        <Show
          when={!props.editing && !m().interrupt}
          fallback={
            <Show when={props.editing}>
              <button type="button" class="qbtn" onMouseDown={(e) => e.preventDefault()} onClick={() => props.onCancel()}>
                Cancel
              </button>
              <button type="button" class="qbtn primary" onMouseDown={(e) => e.preventDefault()} onClick={() => props.onSave(draft())}>
                Save
              </button>
            </Show>
          }
        >
          <button type="button" class="qbtn icon" aria-label="Edit" data-tip="Edit" onClick={() => props.onEdit()} disabled={props.card.leaving()}>
            <Icon name="pencil" size={13} />
          </button>
          <Show when={!m().steer && props.busy}>
            <button type="button" class="qbtn steer-now" aria-label="Steer" data-tip="The agent reads it at its next step, without stopping" onClick={() => props.onSteer()} disabled={props.card.leaving()}>
              <Icon name="steer" size={13} />
              <span>Steer</span>
            </button>
          </Show>
          <Show when={!(props.busy && props.subagent)}>
            <button
              type="button"
              class="qbtn send-now"
              aria-label="Send now"
              data-tip={props.busy ? "Stop the agent and send this now" : "Send this now"}
              onClick={() => props.onSendNow()}
              disabled={props.card.leaving()}
            >
              <Icon name="send" size={13} />
              <span>Send now</span>
            </button>
          </Show>
          <button type="button" class="qbtn icon" aria-label="Remove" data-tip="Remove" onClick={() => props.onRemove()} disabled={props.card.leaving()}>
            <Icon name="x" size={13} />
          </button>
        </Show>
      </div>
    </div>
  );
}
