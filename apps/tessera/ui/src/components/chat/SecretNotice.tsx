// A key or login typed into the chat went to Passwords & keys (or stays
// with this chat), and the conversation keeps only its `$NAME`: one calm
// line in the transcript, with the variables and a way to manage them.
// With "Ask me first" it is a small card instead, asking whether to save it
// (Save / Don't save); the server then rewrites the notice to say which.
// The server says all this in plain-text notices (engine/security.rs
// `listed_notice`, `ASK_PREFIX`), read here.
import { For, Show, createSignal } from "solid-js";
import type { ChatSecretChoice } from "../../proto/ChatSecretChoice";
import type { SecretKind } from "../../proto/SecretKind";
import { api } from "../../lib/store";
import { openSettings, toastError } from "../../lib/ui";
import { Icon, Spinner } from "../icons";
import "../../styles/secret-notice.css";

export interface SavedSecrets {
  /** "saved": moved to Passwords & keys now; "known": it was stored already; "kept": this chat
   * keeps it (not in Passwords & keys); "ask": this chat keeps it until the user answers. */
  how: "saved" | "known" | "kept" | "ask";
  /** Stored names, e.g. GITHUB (a login) or OPENAI_API_KEY. */
  names: string[];
  /** What it is, for a single one ("OpenAI API key"); empty when only its kind is known. */
  what: string;
  kind: SecretKind;
  /** The variables the chat and the agent use instead, without `$`. */
  vars: string[];
  /** The agent saved it from earlier messages (rather than as it arrived). */
  byAgent: boolean;
}

// Older servers (and chats saved by them) said "Secrets & logins".
const SAVED = ["Saved to Passwords & keys: ", "Saved to Secrets & logins: "];
const KEPT = "Kept for this chat only: ";
const ASK = "Save to Passwords & keys? ";
const VAR = /\$([A-Z_][A-Z0-9_]*)/g;
const KINDS: Record<string, SecretKind> = { token: "token", login: "login", custom: "custom" };

/**
 * "Saved to Passwords & keys: NAME (what), …. It was taken out of your
 * message; the agent uses it as $VAR, … without seeing the value." (or "Kept
 * for this chat only: …"), "Save to Passwords & keys? NAME (what) was taken
 * out of your message and is kept for this chat only until you decide; …" or
 * "Your message held the stored secret $VAR; …" → what to show; null for any
 * other notice.
 */
export function savedSecrets(entry: { level: string; text: string }): SavedSecrets | null {
  if (entry.level !== "info") return null;
  const t = entry.text;
  const prefix = SAVED.find((p) => t.startsWith(p)) ?? (t.startsWith(KEPT) ? KEPT : null);
  if (prefix) {
    const body = t.slice(prefix.length);
    const cut = body.search(/\. (?:It was|They were) taken out of /);
    if (cut < 0) return null;
    const items = [...body.slice(0, cut).matchAll(/([A-Z_][A-Z0-9_]*) \(([^)]*)\)/g)].map((m) => ({ name: m[1], what: m[2] }));
    if (!items.length) return null;
    const one = items.length === 1 ? items[0] : null;
    return {
      how: prefix === KEPT ? "kept" : "saved",
      names: items.map((i) => i.name),
      what: one && !KINDS[one.what] ? one.what : "",
      kind: (one && KINDS[one.what]) || "token",
      vars: [...body.slice(cut).matchAll(VAR)].map((m) => m[1]),
      byAgent: /taken out of the conversation/.test(body),
    };
  }
  if (t.startsWith(ASK)) {
    const m = /^([A-Z_][A-Z0-9_]*) \(([^)]*)\) was taken out of your message[^;]*; the agent uses it as (.+) without seeing the value\.$/.exec(t.slice(ASK.length));
    if (!m) return null;
    return { how: "ask", names: [m[1]], what: KINDS[m[2]] ? "" : m[2], kind: KINDS[m[2]] ?? "token", vars: [...m[3].matchAll(VAR)].map((x) => x[1]), byAgent: false };
  }
  const known = /^Your message held the stored secrets? (.+?); the agent got the name instead\.?$/.exec(t);
  if (known) {
    const vars = [...known[1].matchAll(VAR)].map((m) => m[1]);
    return vars.length ? { how: "known", names: vars, what: "", kind: "token", vars, byAgent: false } : null;
  }
  return null;
}

const LABEL: Record<SavedSecrets["how"], string> = {
  saved: "Saved to Passwords & keys",
  known: "Kept out of the chat",
  kept: "Kept for this chat only",
  ask: "Save to Passwords & keys?",
};
const GONE: Record<SavedSecrets["how"], string> = {
  saved: "removed from this chat",
  known: "already stored",
  kept: "not saved to Passwords & keys",
  ask: "",
};

/** `sessionId`: the chat it's in (needed to answer a card). */
export function SecretNotice(props: { saved: SavedSecrets; text: string; sessionId?: string }) {
  const s = () => props.saved;
  /** What was saved: its label, else how many, else its kind. */
  const what = () =>
    s().what || (s().names.length > 1 ? `${s().names.length} secrets` : s().how === "known" ? "" : s().kind === "login" ? "Login" : s().kind === "custom" ? "Values" : "Token");
  const chips = () => (s().vars.length ? s().vars : s().names);
  return (
    <Show when={s().how === "ask" && props.sessionId} fallback={<SecretLine {...props} what={what()} chips={chips()} />}>
      <SecretAsk {...props} sessionId={props.sessionId!} what={what()} chips={chips()} />
    </Show>
  );
}

function SecretLine(props: { saved: SavedSecrets; text: string; what: string; chips: string[] }) {
  const s = () => props.saved;
  return (
    <div class="secret-saved" classList={{ kept: s().how === "kept" }} role="note" title={props.text}>
      <span class="secret-saved-glyph" aria-hidden="true">
        <Icon name="lock" size={12} />
      </span>
      <span class="secret-saved-label">{LABEL[s().how]}</span>
      <Show when={props.what}>
        <span class="secret-saved-what">{props.what}</span>
      </Show>
      <span class="secret-saved-names">
        <For each={props.chips}>{(v) => <code class="var-chip">${v}</code>}</For>
      </span>
      <Show when={GONE[s().how]}>
        <span class="secret-saved-gone">{GONE[s().how]}</span>
      </Show>
      <button type="button" class="link-btn" onClick={() => openSettings("secrets")}>
        Manage
      </button>
    </div>
  );
}

/** "Ask me first": save it to Passwords & keys, or leave it with this chat. */
function SecretAsk(props: { saved: SavedSecrets; text: string; sessionId: string; what: string; chips: string[] }) {
  const [busy, setBusy] = createSignal<ChatSecretChoice | null>(null);
  const decide = async (choice: ChatSecretChoice) => {
    if (busy()) return;
    setBusy(choice);
    try {
      // The server rewrites this notice (entries_changed), which replaces the card.
      await api.decideChatSecret(props.sessionId, props.saved.names[0], choice);
    } catch (e) {
      toastError(e, "Couldn't do that");
      setBusy(null);
    }
  };
  return (
    <div class="secret-ask" role="group" aria-label="Save to Passwords & keys?" title={props.text}>
      <div class="secret-ask-head">
        <span class="secret-saved-glyph" aria-hidden="true">
          <Icon name="lock" size={12} />
        </span>
        <span class="secret-ask-title">Save to Passwords &amp; keys?</span>
      </div>
      <div class="secret-ask-what">
        <Show when={props.what}>
          <span class="secret-saved-what">{props.what}</span>
        </Show>
        <span class="secret-saved-names">
          <For each={props.chips}>{(v) => <code class="var-chip">${v}</code>}</For>
        </span>
      </div>
      <p class="secret-ask-help">Taken out of your message. Until you choose, only this chat can use it; Don't save keeps it that way.</p>
      <div class="secret-ask-actions">
        <button type="button" class="btn btn-sm btn-primary" disabled={!!busy()} onClick={() => void decide("save")}>
          <Show when={busy() === "save"} fallback={<Icon name="lock" size={13} />}>
            <Spinner size={13} />
          </Show>
          Save
        </button>
        <button type="button" class="btn btn-sm" disabled={!!busy()} onClick={() => void decide("chat_only")}>
          <Show when={busy() === "chat_only"}>
            <Spinner size={13} />
          </Show>
          Don't save
        </button>
      </div>
    </div>
  );
}
