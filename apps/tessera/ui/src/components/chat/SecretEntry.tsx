// Values go directly to the vault API, not Send or tool arguments.
import { Show, createResource, createSignal } from "solid-js";
import { call } from "../../lib/store";
import type { SecurityInfo } from "../../proto/SecurityInfo";
import { errorMessage, openSettings } from "../../lib/ui";
import { SecretInput } from "../SecretInput";
import { Icon } from "../icons";
import "../../styles/secret-notice.css";

export default function SecretEntry(props: { sessionId: string; callId: string; entry: Record<string, unknown> }) {
  const [security] = createResource(() => call<SecurityInfo>({ method: "get_security" }));
  const [value, setValue] = createSignal("");
  const [username, setUsername] = createSignal("");
  const [password, setPassword] = createSignal("");
  const [chatOnly, setChatOnly] = createSignal<boolean | undefined>();
  const [busy, setBusy] = createSignal(false);
  const [saved, setSaved] = createSignal(false);
  const [error, setError] = createSignal("");
  const login = () => props.entry.kind === "login";
  const name = () => String(props.entry.name ?? "");
  const kept = () => security()?.secret_saving === "chat_only" || (chatOnly() ?? security()?.secret_saving !== "auto");
  const save = async (e: Event) => {
    e.preventDefault();
    if (busy()) return;
    setBusy(true);
    setError("");
    try {
      const fields: Record<string, string> = login() ? { password: password() } : { value: value() };
      if (login() && username()) fields.username = username();
      if (login() && props.entry.url) fields.url = String(props.entry.url);
      await call({ method: "save_chat_secret", params: { session_id: props.sessionId, call_id: props.callId, fields, chat_only: kept() } });
      setValue(""); setUsername(""); setPassword("");
      setSaved(true);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div class="secret-ask" role="group" aria-label="Secure password entry">
      <div class="secret-ask-head"><Icon name="lock" size={14} /><strong>{String(props.entry.title ?? name())}</strong></div>
      <Show when={!saved() && props.entry.saved !== true} fallback={<p role="status">Saved securely as <code>{`$${name()}${login() ? "_PASSWORD" : ""}`}</code>. Its values are never shown to the model.</p>}>
        <p class="secret-ask-help">Enter it here, not in chat. The model gets only the variable names. Never enter Claude sign-in credentials.</p>
        <p class="secret-ask-help secret-ask-where"><Icon name="server" size={12} /> Usable on this server only. To use a new one on another computer, allow it in Passwords &amp; keys.</p>
        <form onSubmit={(e) => void save(e)}>
          <Show when={login()} fallback={<SecretInput label="Key value" placeholder="Key value" secret value={value()} onInput={setValue} />}>
            <SecretInput label="Username" placeholder="Username or email" value={username()} onInput={setUsername} />
            <SecretInput label="Password" placeholder="Password" secret value={password()} onInput={setPassword} />
          </Show>
          <label><input type="checkbox" checked={kept()} disabled={security()?.secret_saving === "chat_only"} onChange={(e) => setChatOnly(e.currentTarget.checked)} /> Keep for this chat only</label>
          <Show when={error()}><p role="alert">{error()}</p></Show>
          <div class="secret-ask-actions">
            <button class="btn btn-sm btn-primary" type="submit" disabled={busy() || !security() || (login() ? !password() : !value())}>Save securely</button>
            <button class="link-btn" type="button" onClick={() => openSettings("secrets")}>Manage passwords</button>
          </div>
        </form>
      </Show>
    </div>
  );
}
