// Addresses and extra headers for devices away from home (Connection and the setup wizard).
import { For, Show, createEffect, createResource, createSignal, on } from "solid-js";
import { createStore, produce, reconcile } from "solid-js/store";
import type { RemoteAccessInfo } from "../../proto/RemoteAccessInfo";
import { api } from "../../lib/store";
import { errorMessage, toast } from "../../lib/ui";
import { Icon, Spinner } from "../icons";
import { Advanced, LoadingBlock } from "./kit";
import { headerMap, type HeaderRow } from "./headers";

/**
 * Addresses and proxy headers devices should use away from home. The server
 * builds them into the pairing QR code; header values are write-only.
 */
export function RemoteAccessEditor() {
  const [info, { mutate }] = createResource(() => api.getRemoteAccess());
  const [urls, setUrls] = createStore<string[]>([]);
  // Header edits, applied one by one: nothing else stored is touched.
  const [removed, setRemoved] = createSignal<string[]>([]);
  const [replace, setReplace] = createStore<Record<string, string>>({});
  const [added, setAdded] = createStore<HeaderRow[]>([]);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const reset = (i: RemoteAccessInfo) => {
    setUrls(i.urls.length ? [...i.urls] : [""]);
    setRemoved([]);
    setReplace(reconcile({}));
    setAdded([]);
  };
  createEffect(on(info, (i) => i && reset(i)));
  const save = async () => {
    setError(null);
    const list = urls.map((u) => u.trim()).filter(Boolean);
    const bad = list.find((u) => !/^https?:\/\/[^\s]+$/i.test(u));
    if (bad) return setError(`“${bad}” isn't a web address (it should start with https://).`);
    const add: Record<string, string> = {};
    for (const [k, v] of Object.entries(replace)) if (v) add[k] = v;
    const m = headerMap(added);
    if (typeof m === "string") return setError(m);
    Object.assign(add, m);
    setBusy(true);
    try {
      const next = await api.setRemoteAccess(list, { add, remove: removed() });
      mutate(next);
      reset(next);
      toast({ level: "success", message: "Saved — new pairing QR codes include it", timeout: 2600 });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Show when={info()} fallback={<LoadingBlock error={info.error} />}>
      <div class="stack tight remote-access">
        <p class="field-help">
          Addresses a new device should use when it isn't at home. They go into the pairing QR code, so a phone is set up in one scan.
        </p>
        <div class="field">
          <span class="field-label">Addresses away from home</span>
          <For each={urls}>
            {(u, i) => (
              <div class="endpoint-row">
                <input
                  class="input mono"
                  type="url"
                  inputmode="url"
                  placeholder="https://tessera.example.com"
                  value={u}
                  aria-label={`Address away from home ${i() + 1}`}
                  autocomplete="off"
                  spellcheck={false}
                  onInput={(e) => setUrls(i(), e.currentTarget.value)}
                />
                <Show when={urls.length > 1}>
                  <button type="button" class="icon-btn" aria-label="Remove address" onClick={() => setUrls((l) => l.filter((_, j) => j !== i()))}>
                    <Icon name="x" size={15} />
                  </button>
                </Show>
              </div>
            )}
          </For>
          <button type="button" class="link-btn add-row" onClick={() => setUrls(urls.length, "")}>
            <Icon name="plus" size={14} /> Add address
          </button>
        </div>
        <Advanced label={`More: extra headers (for Cloudflare Access)${info()!.header_names.length ? ` · ${info()!.header_names.length} saved` : ""}`} open={!!added.length}>
          <div class="field">
            <span class="field-label">Extra headers</span>
            <span class="field-help">Some ways of reaching your server from outside (Cloudflare Access) need these. Their values are stored on the server and never shown again.</span>
            <Show when={info()!.header_names.length || added.length} fallback={<span class="muted small">None</span>}>
              <div class="stored-headers">
                <For each={info()!.header_names}>
                  {(n) => {
                    const gone = () => removed().includes(n);
                    const editing = () => n in replace;
                    return (
                      <div class="stored-header" classList={{ gone: gone() }}>
                        <span class="mono sh-name">
                          <Icon name="lock" size={11} /> {n}
                        </span>
                        <Show
                          when={editing()}
                          fallback={
                            <>
                              <Show when={/^CF-Access-Client-(Id|Secret)$/i.test(n)}>
                                <span class="muted small">Cloudflare Access</span>
                              </Show>
                              <span class="spacer" />
                              <Show
                                when={!gone()}
                                fallback={
                                  <button type="button" class="link-btn" onClick={() => setRemoved((l) => l.filter((x) => x !== n))}>
                                    Undo
                                  </button>
                                }
                              >
                                <button type="button" class="link-btn" onClick={() => setReplace(n, "")}>
                                  Replace value
                                </button>
                                <button type="button" class="icon-btn sm" aria-label={`Remove ${n}`} onClick={() => setRemoved((l) => [...l, n])}>
                                  <Icon name="x" size={14} />
                                </button>
                              </Show>
                            </>
                          }
                        >
                          <input
                            class="input mono"
                            type="password"
                            placeholder="New value"
                            value={replace[n] ?? ""}
                            aria-label={`New value for ${n}`}
                            autocomplete="new-password"
                            spellcheck={false}
                            onInput={(e) => setReplace(n, e.currentTarget.value)}
                          />
                          <button
                            type="button"
                            class="icon-btn sm"
                            aria-label="Keep the current value"
                            onClick={() =>
                              setReplace(
                                produce((r) => {
                                  delete r[n];
                                }),
                              )
                            }
                          >
                            <Icon name="x" size={14} />
                          </button>
                        </Show>
                      </div>
                    );
                  }}
                </For>
                <For each={added}>
                  {(r, i) => (
                    <div class="header-row">
                      <input class="input mono" placeholder="Header" value={r.key} aria-label="Header name" autocomplete="off" spellcheck={false} onInput={(e) => setAdded(i(), "key", e.currentTarget.value)} />
                      <input
                        class="input mono"
                        type="password"
                        placeholder="Value"
                        value={r.value}
                        aria-label={`Value for ${r.key || "header"}`}
                        autocomplete="new-password"
                        spellcheck={false}
                        onInput={(e) => setAdded(i(), "value", e.currentTarget.value)}
                      />
                      <button type="button" class="icon-btn" aria-label="Remove header" onClick={() => setAdded((l) => l.filter((_, j) => j !== i()))}>
                        <Icon name="x" size={15} />
                      </button>
                    </div>
                  )}
                </For>
              </div>
            </Show>
            <button type="button" class="link-btn add-row" onClick={() => setAdded(added.length, { key: "", value: "" })}>
              <Icon name="plus" size={14} /> Add header
            </button>
          </div>
        </Advanced>
        <Show when={error()}>
          <div class="form-error">{error()}</div>
        </Show>
        <div class="form-actions">
          <button type="button" class="btn btn-primary" disabled={busy()} onClick={save}>
            <Show when={busy()}>
              <Spinner size={14} />
            </Show>
            Save
          </button>
        </div>
      </div>
    </Show>
  );
}
