import { Show, createEffect, createResource, createSignal } from "solid-js";
import { activeServer } from "../../lib/app";
import { native } from "../../lib/native";
import { errorMessage } from "../../lib/ui";
import type { AppUpdate } from "../../proto/AppUpdate";
import { Card, Row } from "./kit";

interface Checked { serverId: string; build: AppUpdate | null }

export function AppUpdates() {
  const [available] = createResource(() => native<boolean>("app_update_available").catch(() => null));
  const [checked, setChecked] = createSignal<Checked | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [message, setMessage] = createSignal("");
  const [error, setError] = createSignal("");
  let generation = 0;
  createEffect(() => {
    activeServer()?.id;
    generation++;
    setChecked(null);
    setMessage("");
    setError("");
  });
  const check = async () => {
    const turn = generation;
    setBusy(true); setError(""); setMessage(""); setChecked(null);
    try {
      const result = await native<Checked>("app_update_check");
      if (turn !== generation) return;
      if (!result) throw new Error("Update checks aren't available in this app.");
      setChecked(result);
      if (!result.build) setMessage("No newer app is offered by your server.");
    } catch (e) { if (turn === generation) setError(errorMessage(e)); }
    finally { setBusy(false); }
  };
  const install = async () => {
    const offer = checked();
    if (!offer?.build) return;
    const turn = generation;
    setBusy(true); setError(""); setMessage("");
    try {
      const started = await native<boolean>("app_update_install", {
        serverId: offer.serverId, id: offer.build.id, sha256: offer.build.sha256,
      });
      if (turn === generation) {
        if (started === null) throw new Error("Installing isn't available in this app.");
        setMessage(started ? "The setup installer opened. Follow its steps to finish." : "Cancelled; nothing was installed.");
        if (started) setChecked(null);
      }
    } catch (e) { if (turn === generation) setError(errorMessage(e)); }
    finally { setBusy(false); }
  };
  return <Show when={available()}>
    <Card>
      <div class="app-updates">
        <Row title="App updates" help="Your server offers the setup. Nothing runs until you confirm in the Windows app; your chats and settings stay.">
          <button type="button" class="btn app-update-check" disabled={busy()} onClick={() => void check()}>Check for updates</button>
        </Row>
        <Show when={checked()?.build}>{(b) => <>
          <p class="small">Tessera {b().version} · {(b().size / 1048576).toFixed(1)} MB</p>
          <Show when={b().notes}><p class="muted small">{b().notes}</p></Show>
          <button type="button" class="btn app-update-install" disabled={busy()} onClick={() => void install()}>Install update…</button>
        </>}</Show>
        <Show when={busy()}><p class="muted small">Checking or downloading; the installer may close and restart Tessera.</p></Show>
        <Show when={message()}><p class="muted small app-update-message" role="status">{message()}</p></Show>
        <Show when={error()}><p class="form-error" role="alert">{error()}</p></Show>
      </div>
    </Card>
  </Show>;
}
