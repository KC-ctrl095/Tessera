// Settings → Connection: which server this app talks to, and how devices reach it at home and away.
import { For, Show, createSignal } from "solid-js";
import { activeServer, isLocalEndpoint, pairAgain, removeServer, switchServer } from "../../lib/app";
import { backend } from "../../lib/backend";
import { state } from "../../lib/store";
import { confirmDialog, setSettingsTab, toastError } from "../../lib/ui";
import { plural } from "../../lib/util";
import { SecurityNote } from "../Connection";
import { Icon } from "../icons";
import { LanToggle, RemoteAccessPanel, WebAccessPanel } from "../RemoteAccess";
import { ServerForm } from "../Onboarding";
import { Card } from "./kit";

export function ServersTab() {
  const [editing, setEditing] = createSignal<string | null>(null);
  const [adding, setAdding] = createSignal(false);
  const servers = () => state.config?.servers ?? [];

  const remove = async (id: string, name: string) => {
    const ok = await confirmDialog({
      title: backend.sameOrigin ? "Sign this browser out?" : `Remove ${name}?`,
      body: backend.sameOrigin ? "You'll need a new pairing code to use Tessera here again." : "This device forgets the server. You can pair again later.",
      confirmLabel: backend.sameOrigin ? "Sign out" : "Remove",
      danger: true,
    });
    if (!ok) return;
    try {
      await removeServer(id);
      if (!state.config?.servers.length) setSettingsTab(null);
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <div class="stack">
      <Show when={state.conn.state === "connected"}>
        <SecurityNote />
      </Show>
      <For each={servers()}>
        {(s) => {
          const active = () => activeServer()?.id === s.id;
          return (
            <Card class="server-card">
              <Show when={editing() !== s.id} fallback={<ServerForm mode="edit" initial={s} onDone={() => setEditing(null)} onCancel={() => setEditing(null)} />}>
                <div class="row gap">
                  <span class="conn-dot lg" data-state={active() ? state.conn.state : "disconnected"} />
                  <div class="grow">
                    <div class="strong">
                      {s.name}
                      <Show when={active()}>
                        <span class="badge badge-accent">In use</span>
                      </Show>
                    </div>
                    <div class="muted small">
                      As “{s.deviceName}”
                      <Show when={Object.keys(s.headers).length}> · {plural(Object.keys(s.headers).length, "extra header", "extra headers")}</Show>
                      <Show when={active() && state.server}> · Tessera {state.server!.version}</Show>
                    </div>
                  </div>
                  <Show when={!active()}>
                    <button type="button" class="btn btn-sm" onClick={() => switchServer(s.id)}>
                      Connect
                    </button>
                  </Show>
                  <Show when={!backend.sameOrigin}>
                    <button type="button" class="icon-btn" aria-label={`Edit ${s.name}`} data-tip="Edit addresses" onClick={() => setEditing(s.id)}>
                      <Icon name="pencil" size={15} />
                    </button>
                  </Show>
                  <button type="button" class="icon-btn danger-text" aria-label={`Remove ${s.name}`} data-tip={backend.sameOrigin ? "Sign out" : "Remove"} onClick={() => remove(s.id, s.name)}>
                    <Icon name={backend.sameOrigin ? "logOut" : "trash"} size={15} />
                  </button>
                </div>
                <Show when={backend.kind === "tauri"}>
                  <Show
                    when={s.fingerprint}
                    fallback={
                      <div class="fp-row warn">
                        <Icon name="lockOpen" size={14} />
                        <span class="grow">Not end-to-end encrypted (paired before encryption).</span>
                        <button type="button" class="btn btn-sm" onClick={() => void pairAgain(s.id)}>
                          Pair again
                        </button>
                      </div>
                    }
                  >
                    <div class="fp-row">
                      <Icon name="lock" size={14} />
                      <span>
                        Security code <span class="mono fp">{s.fingerprint}</span>
                      </span>
                      <span class="muted small">
                        Matches <code>tessera pair</code> on your server
                      </span>
                    </div>
                  </Show>
                </Show>
                <Show when={!backend.sameOrigin}>
                  <ul class="endpoint-list">
                    <For each={s.endpoints}>
                      {(ep) => (
                        <li classList={{ active: state.conn.endpoint === ep && active() }}>
                          <Icon name={isLocalEndpoint(ep) ? "home" : "cloud"} size={13} />
                          <span class="grow ellipsis">{ep}</span>
                          <Show when={state.conn.endpoint === ep && active()} fallback={<span class="ep-kind">{isLocalEndpoint(ep) ? "At home" : "Away"}</span>}>
                            <span class="badge badge-accent">in use</span>
                          </Show>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
              </Show>
            </Card>
          );
        }}
      </For>
      <Show when={state.ready}>
        <section class="settings-section">
          <h4 class="section-title">Home network</h4>
          <LanToggle />
        </section>
        <section class="settings-section">
          <h4 class="section-title">Away from home</h4>
          <RemoteAccessPanel onPair={() => setSettingsTab("devices")} />
        </section>
        <section class="settings-section">
          <h4 class="section-title">Web browsers</h4>
          <WebAccessPanel />
        </section>
      </Show>
      <Show when={!backend.sameOrigin}>
        <Show
          when={adding()}
          fallback={
            <div>
              <button type="button" class="btn" onClick={() => setAdding(true)}>
                <Icon name="plus" size={15} /> Add another server
              </button>
            </div>
          }
        >
          <Card class="form-card">
            <ServerForm mode="pair" onDone={() => setAdding(false)} onCancel={() => setAdding(false)} />
          </Card>
        </Show>
      </Show>
    </div>
  );
}
