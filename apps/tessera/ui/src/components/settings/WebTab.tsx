// Settings → Web & voice: how agents search the web (backup search keys under More), the browser
// behind web views (its finer choices under More), and dictating messages (VoiceTab).
import { For, Show, createEffect, createResource, createSignal, on } from "solid-js";
import { createStore } from "solid-js/store";
import type { WebSearchInfo } from "../../proto/WebSearchInfo";
import { api, call } from "../../lib/store";
import { errorMessage, toast, toastError } from "../../lib/ui";
import { plural } from "../../lib/util";
import { humanBytes } from "../../lib/images";
import { Icon, Spinner } from "../icons";
import { Segmented, Toggle } from "../primitives";
import { Advanced, Card, LoadingBlock, Row } from "./kit";
import { HeaderEditor, headerMap, type HeaderRow } from "./headers";
import { VoiceTab } from "./VoiceTab";
import { isAndroidApp } from "../../lib/native";
import { PhoneAssistant } from "./PhoneAssistant";

/**
 * What the server says about its browser. Coded against `get_browser` /
 * `set_browser { headless }`; hidden while the server doesn't have them.
 */
interface BrowserInfo {
  /** Web views may run in a browser on this server (absent: yes). */
  enabled?: boolean;
  /** This server's memory (bytes) when it is small: its own browser is off unless turned on. */
  low_memory?: number | null;
  /** A Chromium-family browser was found. */
  found: boolean;
  /** Its path, e.g. /usr/bin/chromium. */
  path?: string | null;
  /** Its name, e.g. "Chromium". */
  name?: string | null;
  /** Running now (it stops itself after a while with nothing to do). */
  running?: boolean;
  /** No window on the server's desktop. */
  headless: boolean;
  /** Open tabs (web views and the agent's). */
  tabs?: number;
  /** How sharp pictures are while pages move. */
  quality?: BrowserQuality;
  /** Sharp frames a second the server draws (measured; 60 = fast). */
  speed?: number | null;
}
type BrowserQuality = "auto" | "sharp" | "smooth";
const QUALITY_HELP: Record<BrowserQuality, string> = {
  auto: "Each device gets pictures as sharp as this server can draw smoothly.",
  sharp: "Always your screen's full sharpness. Scrolling may stutter if the server is slow.",
  smooth: "Normal resolution while a page moves, the least work for the server. It turns sharp once the page stops.",
};

export function BrowserCard() {
  const [info, { mutate }] = createResource(async () => {
    try {
      return await call<BrowserInfo>({ method: "get_browser" } as never);
    } catch {
      // An older server: no browser settings to show.
      return null;
    }
  });
  const [busy, setBusy] = createSignal(false);
  const set = async (params: { enabled?: boolean; headless?: boolean; quality?: BrowserQuality }) => {
    setBusy(true);
    try {
      mutate(await call<BrowserInfo>({ method: "set_browser", params } as never));
    } catch (e) {
      toastError(e, "Couldn't change the browser");
    } finally {
      setBusy(false);
    }
  };
  const setHeadless = (headless: boolean) => set({ headless });
  /** The server draws pages slowly (typical of virtual machines without a fast CPU). */
  const slow = (b: BrowserInfo) => b.speed != null && b.speed < 30;
  return (
    <Show when={info()}>
      {(b) => (
        <Card>
          <div class="setting-title">Web view browser</div>
          <Show when={b().enabled === false || b().low_memory}>
            <Row
              title="Web views on this server"
              help={
                <span class="webview-low-memory">
                  {b().low_memory
                    ? `This server has ${humanBytes(b().low_memory!)} of memory and a browser needs about 400 MB of it, so web views here are off unless you turn them on.`
                    : "Turned off in the server's settings."}{" "}
                  Web views can also open on one of your computers that runs Tessera.
                </span>
              }
            >
              <Toggle checked={b().enabled !== false} disabled={busy()} onChange={(v) => void set({ enabled: v })} label="Run web views on this server" />
            </Row>
          </Show>
          <Show when={b().enabled !== false}>
            <Row
              title={b().found ? `Found ${b().name || "a browser"}` : "No browser found"}
              help={
                b().found ? (
                  <>{b().running ? `Running${b().tabs ? `, ${plural(b().tabs!, "tab", "tabs")}` : ""}` : "Starts when a web view opens"}</>
                ) : (
                  <>
                    Web views and the agent's browsing need Chromium, Chrome, Brave or Edge on the server. Install one, or set <span class="mono">[browser] path</span> in the server's config.
                  </>
                )
              }
            />
            <Show when={b().found}>
              <Advanced label="More: the picture while pages move, its window on the server">
              <Show when={b().path}>
                <Row title="Where it is" help={<span class="mono">{b().path}</span>} />
              </Show>
              <Show when={b().found && b().headless && b().quality}>
                <Row
                  title="Picture while pages move"
                  help={
                    <>
                      {QUALITY_HELP[b().quality!]}
                      <Show when={slow(b())}>
                        <span class="webview-slow">
                          {" "}
                          This server draws web pages slowly (about {Math.max(1, Math.round(b().speed!))} sharp frames a second), so Auto keeps them smooth here.
                        </span>
                      </Show>
                    </>
                  }
                >
                  <Segmented<BrowserQuality>
                    label="Picture while pages move"
                    value={b().quality!}
                    onChange={(q) => void set({ quality: q })}
                    options={[
                      { value: "auto", label: "Auto" },
                      { value: "sharp", label: "Sharp" },
                      { value: "smooth", label: "Smooth" },
                    ]}
                  />
                </Row>
              </Show>
              <Show when={b().found}>
                <Row title="Show its window on the server" help="Off: it runs without a window (the default). On: you can watch it on the server's own screen.">
                  <Toggle checked={!b().headless} disabled={busy()} onChange={(v) => void setHeadless(!v)} label="Show the browser window on the server" />
                </Row>
              </Show>
              </Advanced>
            </Show>
          </Show>
        </Card>
      )}
    </Show>
  );
}

export function WebSearchTab() {
  const [info, { mutate }] = createResource(() => api.getWebSearch());
  const [search, setSearch] = createSignal("duckduckgo");
  const [url, setUrl] = createSignal("");
  const [rows, setRows] = createStore<HeaderRow[]>([]);
  const [replacing, setReplacing] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const reset = (i: WebSearchInfo) => {
    setSearch(i.search === "searxng" ? "searxng" : "duckduckgo");
    setUrl(i.searxng_url ?? "");
    setReplacing(false);
    setRows([]);
  };
  createEffect(on(info, (i) => i && reset(i)));
  const dirty = () => {
    const i = info();
    if (!i) return false;
    return search() !== (i.search === "searxng" ? "searxng" : "duckduckgo") || (search() === "searxng" && url().trim() !== (i.searxng_url ?? "")) || replacing();
  };
  const save = async () => {
    setError(null);
    let headers: Record<string, string> | undefined;
    if (search() === "searxng") {
      const u = url().trim();
      if (!/^https?:\/\/[^\s]+$/i.test(u)) return setError("Enter the address of your SearXNG, like https://search.example.com");
    }
    if (replacing()) {
      const m = headerMap(rows);
      if (typeof m === "string") return setError(m);
      headers = m;
    }
    setBusy(true);
    try {
      const next = await api.saveWebSearch(search(), search() === "searxng" ? url().trim() : undefined, headers);
      mutate(next);
      toast({ level: "success", message: "Web search saved", timeout: 1800 });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Show when={info()} fallback={<LoadingBlock error={info.error} />}>
      <div class="stack">
        <Card>
          <p class="field-help">
            Private chats and the agent search through this. A self-hosted SearXNG keeps your searches private and is the most reliable (DuckDuckGo often
            rate-limits servers).
          </p>
          <div class="search-choice" role="radiogroup" aria-label="Search service">
            <button type="button" role="radio" class="choice-card" aria-checked={search() === "duckduckgo"} classList={{ on: search() === "duckduckgo" }} onClick={() => setSearch("duckduckgo")}>
              <span class="choice-top">
                <Icon name="globe" size={15} />
                <span class="choice-label">DuckDuckGo</span>
              </span>
              <span class="choice-desc">Works out of the box. Searches leave your server for DuckDuckGo.</span>
            </button>
            <button type="button" role="radio" class="choice-card" aria-checked={search() === "searxng"} classList={{ on: search() === "searxng" }} onClick={() => setSearch("searxng")}>
              <span class="choice-top">
                <Icon name="server" size={15} />
                <span class="choice-label">Your SearXNG</span>
              </span>
              <span class="choice-desc">A metasearch engine you host. Private and dependable.</span>
            </button>
          </div>
          <Show when={search() === "searxng"}>
            <div class="stack tight">
              <label class="field">
                <span class="field-label">SearXNG address</span>
                <input
                  class="input mono"
                  type="url"
                  inputmode="url"
                  placeholder="https://search.example.com"
                  value={url()}
                  autocomplete="off"
                  spellcheck={false}
                  onInput={(e) => setUrl(e.currentTarget.value)}
                />
              </label>
              <HeaderEditor
                label="Headers"
                names={info()!.header_names}
                replacing={replacing()}
                rows={rows}
                setRows={setRows}
                onReplace={() => {
                  setRows(info()!.header_names.map((key) => ({ key, value: "" })));
                  setReplacing(true);
                }}
                onKeep={() => {
                  setReplacing(false);
                  setRows([]);
                }}
              />
            </div>
          </Show>
          <Show when={error()}>
            <div class="form-error">{error()}</div>
          </Show>
          <div class="form-actions">
            <Show when={dirty()}>
              <button type="button" class="btn btn-ghost" onClick={() => reset(info()!)}>
                Discard
              </button>
            </Show>
            <button type="button" class="btn btn-primary" disabled={busy() || !dirty()} onClick={save}>
              <Show when={busy()}>
                <Spinner size={14} />
              </Show>
              Save
            </button>
          </div>
        </Card>
        <Card class="search-keys-card">
          <Advanced label="More: backup search keys (Brave, Tavily)">
            <p class="field-help search-keys-lede">Used when SearXNG isn't set up or doesn't answer; DuckDuckGo is always tried last.</p>
            <For each={SEARCH_KEYS}>
              {(k) => <SearchKeyRow provider={k} set={(info()!.keys ?? []).includes(k.id)} onSaved={(i) => mutate(i ? { ...info()!, ...i } : info())} />}
            </For>
          </Advanced>
        </Card>
      </div>
    </Show>
  );
}

const SEARCH_KEYS: { id: "brave" | "tavily"; name: string; hint: string }[] = [
  { id: "brave", name: "Brave Search", hint: "api.search.brave.com" },
  { id: "tavily", name: "Tavily", hint: "tavily.com" },
];

/** One write-only key: it's never shown again once saved, only whether it's set. */
function SearchKeyRow(props: { provider: (typeof SEARCH_KEYS)[number]; set: boolean; onSaved: (i: WebSearchInfo | null) => void }) {
  const [key, setKey] = createSignal("");
  const [busy, setBusy] = createSignal<"save" | "remove" | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const store = async (value: string) => {
    setError(null);
    setBusy(value ? "save" : "remove");
    try {
      const next = await api.setSearchKey(props.provider.id, value);
      setKey("");
      props.onSaved(next && typeof next === "object" && Array.isArray((next as WebSearchInfo).keys) ? next : await api.getWebSearch().catch(() => null));
      toast({ level: "success", message: value ? `${props.provider.name} key saved` : `${props.provider.name} key removed`, timeout: 1800 });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };
  return (
    <div class="search-key">
      <div class="search-key-top">
        <span class="search-key-name">{props.provider.name}</span>
        <span class="search-key-host">{props.provider.hint}</span>
        <span class="spacer" />
        <Show when={props.set} fallback={<span class="search-key-state">Not set</span>}>
          <span class="search-key-state on">
            <Icon name="check" size={12} stroke={2.4} /> Saved
          </span>
        </Show>
      </div>
      <form
        class="search-key-form"
        onSubmit={(e) => {
          e.preventDefault();
          if (key().trim()) void store(key().trim());
        }}
      >
        <input
          class="input mono"
          type="password"
          value={key()}
          placeholder={props.set ? "Saved — paste a new key to replace it" : "Paste an API key"}
          aria-label={`${props.provider.name} API key`}
          autocomplete="off"
          spellcheck={false}
          onInput={(e) => setKey(e.currentTarget.value)}
        />
        <button type="submit" class="btn" disabled={!key().trim() || !!busy()}>
          <Show when={busy() === "save"}>
            <Spinner size={13} />
          </Show>
          {props.set ? "Replace" : "Save"}
        </button>
        <Show when={props.set}>
          <button type="button" class="btn btn-ghost" disabled={!!busy()} onClick={() => void store("")}>
            <Show when={busy() === "remove"}>
              <Spinner size={13} />
            </Show>
            Remove
          </button>
        </Show>
      </form>
      <Show when={error()}>
        <div class="form-error">{error()}</div>
      </Show>
    </div>
  );
}

/** The section: web search, the browser behind web views, then voice typing. */
export function WebTab() {
  return (
    <div class="stack">
      <WebSearchTab />
      <BrowserCard />
      <Show when={isAndroidApp}><PhoneAssistant /></Show>
      <section class="settings-section" id="set-voice">
        <h4 class="section-title">Voice</h4>
        <VoiceTab />
      </section>
    </div>
  );
}
