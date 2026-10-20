// A text file in a window of its own (lib/fileWindow.ts): the app's page
// started as a file window. It shows the file it was handed and nothing
// else: no connection to the server, no tiles, nothing saved. Find, wrap,
// copy and save work as in the tile.
import { Match, Show, Suspense, Switch, createEffect, createResource, createSignal, lazy } from "solid-js";
import { takeWindowFile } from "../../lib/fileWindow";
import { Icon } from "../icons";
import type { TextViewHandle } from "./TextView";

const TextView = lazy(() => import("./TextView"));

const NO_WRAP = /\.(csv|tsv|json|jsonl)$/i;

export function FileWindow(props: { token: string }) {
  const [file] = createResource(() => takeWindowFile(props.token));
  const [wrap, setWrap] = createSignal(true);
  const [view, setView] = createSignal<TextViewHandle>();
  const [copied, setCopied] = createSignal(false);
  const name = () => file()?.name ?? "File";
  createEffect(() => {
    const f = file();
    if (!f) return;
    document.title = `${f.name} — Tessera`;
    setWrap(!NO_WRAP.test(f.name));
  });
  const copy = async () => {
    const t = view()?.text() ?? file()?.text ?? "";
    try {
      await navigator.clipboard.writeText(t);
      setCopied(true);
      setTimeout(() => setCopied(false), 1400);
    } catch {
      /* the user can still select and copy */
    }
  };
  const save = () => {
    const f = file();
    if (!f) return;
    // A download of what's already here (nothing is fetched).
    const url = URL.createObjectURL(new Blob([f.text], { type: f.mime || "text/plain" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = f.name;
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 10_000);
  };
  return (
    <div class="file-window">
      <header class="file-window-head">
        <span class="file-window-name" title={name()}>
          <Icon name="file" size={15} />
          <span>{name()}</span>
        </span>
        <Show when={file()}>
          <button type="button" class="icon-btn" aria-label="Find" title="Find (Ctrl F)" onClick={() => view()?.find()}>
            <Icon name="search" size={15} />
          </button>
          <button type="button" class="icon-btn" aria-label="Wrap long lines" aria-pressed={wrap()} title="Wrap long lines" onClick={() => setWrap(!wrap())}>
            <Icon name="cornerDownLeft" size={15} />
          </button>
          <button type="button" class="icon-btn" aria-label="Copy" title={copied() ? "Copied" : "Copy (the selection, or all)"} onClick={() => void copy()}>
            <Icon name={copied() ? "check" : "copy"} size={15} />
          </button>
          <Show when={!("__TAURI_INTERNALS__" in window)}>
            <button type="button" class="icon-btn" aria-label="Download" title="Download" onClick={save}>
              <Icon name="download" size={15} />
            </button>
          </Show>
        </Show>
      </header>
      <main class="file-window-body">
        <Switch>
          <Match when={file.loading}>
            <div class="file-window-note">Opening…</div>
          </Match>
          <Match when={!file()}>
            <div class="file-window-note">This window's file is gone. Open it again from its chat.</div>
          </Match>
          <Match when={file()}>
            <Suspense>
              <TextView text={file()!.text} name={file()!.name} wrap={wrap()} touch={false} handle={setView} />
            </Suspense>
          </Match>
        </Switch>
      </main>
    </div>
  );
}
