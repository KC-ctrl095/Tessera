import { For, Show, createEffect, createSignal, on } from "solid-js";
import type { DirEntry } from "../proto/DirEntry";
import { api, state } from "../lib/store";
import { dirBrowser, errorMessage, setDirBrowser } from "../lib/ui";
import { Icon, Spinner } from "./icons";
import { Modal } from "./primitives";

function parentOf(path: string): string {
  const p = path.replace(/\/+$/, "");
  const i = p.lastIndexOf("/");
  return i <= 0 ? "/" : p.slice(0, i);
}

export function DirBrowserHost() {
  return (
    <Show when={dirBrowser()} keyed>
      {(spec) => {
        // The server's folders, or this device's (spec.source, e.g. Settings → Devices → This computer).
        const home = () => (spec.source ? spec.source.home : state.server?.home);
        const list = async (p: string) => {
          const l = await (spec.source ? spec.source.list(p) : api.listDir(p));
          if (!l) throw new Error("Folders can't be listed here.");
          return l;
        };
        const [path, setPath] = createSignal(spec.start || home() || "/");
        const [input, setInput] = createSignal(path());
        const [entries, setEntries] = createSignal<DirEntry[] | null>(null);
        const [error, setError] = createSignal<string | null>(null);
        const [showFiles, setShowFiles] = createSignal(false);
        // "New folder": a name field at the top of the list, in the folder being browsed. A source
        // without `create` (This computer, for now) doesn't offer it.
        const canCreate = () => (spec.source ? !!spec.source.create : true);
        const [making, setMaking] = createSignal(false);
        const [newName, setNewName] = createSignal("");
        const [makeError, setMakeError] = createSignal<string | null>(null);
        const [makeBusy, setMakeBusy] = createSignal(false);
        let nameEl: HTMLInputElement | undefined;
        const openNew = () => {
          setNewName("");
          setMakeError(null);
          setMaking(true);
          requestAnimationFrame(() => nameEl?.focus());
        };
        const closeNew = () => {
          setMaking(false);
          setMakeError(null);
        };
        const submitNew = async () => {
          const name = newName().trim();
          if (!name || makeBusy()) return;
          setMakeBusy(true);
          setMakeError(null);
          try {
            const made = await (spec.source?.create ? spec.source.create(path(), name) : api.createDir(path(), name));
            closeNew();
            // Open it: "Use this folder" now picks the new one.
            setPath(made.path);
          } catch (e) {
            setMakeError(errorMessage(e));
          } finally {
            setMakeBusy(false);
          }
        };
        const close = (v: string | null) => {
          setDirBrowser(null);
          spec.resolve(v);
        };
        createEffect(
          on(path, async (p) => {
            setEntries(null);
            setError(null);
            setInput(p);
            closeNew();
            try {
              setEntries(await list(p));
            } catch (e) {
              setError(errorMessage(e));
            }
          }),
        );
        const crumbs = () => {
          const parts = path().split("/").filter(Boolean);
          return [{ name: "/", path: "/" }, ...parts.map((n, i) => ({ name: n, path: "/" + parts.slice(0, i + 1).join("/") }))];
        };
        const visible = () => (entries() ?? []).filter((e) => showFiles() || e.is_dir);
        return (
          <Modal onClose={() => close(null)} title={spec.title ?? "Choose a folder"} size="md" class="dirb">
            <div class="modal-body dirb-body">
              <form
                class="dirb-path"
                onSubmit={(e) => {
                  e.preventDefault();
                  const v = input().trim().replace(/^~(?=\/|$)/, home() ?? "~");
                  if (v) setPath(v);
                }}
              >
                <button type="button" class="icon-btn" aria-label="Parent folder" disabled={path() === "/"} onClick={() => setPath(parentOf(path()))}>
                  <Icon name="arrowUp" size={16} />
                </button>
                <input class="input mono" value={input()} onInput={(e) => setInput(e.currentTarget.value)} aria-label="Path" spellcheck={false} />
                <Show when={home()}>
                  <button type="button" class="btn btn-sm btn-ghost" onClick={() => setPath(home()!)}>
                    Home
                  </button>
                </Show>
              </form>
              <nav class="crumbs" aria-label="Path">
                <For each={crumbs()}>
                  {(c, i) => (
                    <>
                      <Show when={i() > 1}>
                        <Icon name="chevronRight" size={12} />
                      </Show>
                      <button type="button" class="crumb" onClick={() => setPath(c.path)}>
                        {c.name}
                      </button>
                    </>
                  )}
                </For>
              </nav>
              <div class="dirb-list" role="listbox" aria-label="Folders">
                <Show when={canCreate() && entries()}>
                  <div class="dirb-new">
                    <Show
                      when={making()}
                      fallback={
                        <button type="button" class="dirb-new-btn" onClick={openNew}>
                          <Icon name="plus" size={16} />
                          <span>New folder</span>
                        </button>
                      }
                    >
                      <form
                        class="dirb-new-form"
                        onSubmit={(e) => {
                          e.preventDefault();
                          void submitNew();
                        }}
                      >
                        <Icon name="folder" size={16} />
                        <input
                          ref={nameEl}
                          class="input dirb-new-name"
                          value={newName()}
                          placeholder="Folder name"
                          aria-label="Name of the new folder"
                          maxLength={255}
                          spellcheck={false}
                          autocomplete="off"
                          onInput={(e) => {
                            setNewName(e.currentTarget.value);
                            setMakeError(null);
                          }}
                          // Escape puts away the name field, not the whole picker (a native listener:
                          // the picker's own Escape is on the document, ahead of delegated handlers).
                          on:keydown={(e: KeyboardEvent) => {
                            if (e.key !== "Escape") return;
                            e.preventDefault();
                            e.stopPropagation();
                            closeNew();
                          }}
                        />
                        <button type="submit" class="btn btn-sm btn-primary" disabled={!newName().trim() || makeBusy()}>
                          Create
                        </button>
                        <button type="button" class="icon-btn" aria-label="Don't make a folder" onClick={closeNew}>
                          <Icon name="x" size={14} />
                        </button>
                      </form>
                      <Show when={makeError()}>
                        <p class="dirb-new-error danger-text" role="alert">
                          {makeError()}
                        </p>
                      </Show>
                    </Show>
                  </div>
                </Show>
                <Show when={entries()} fallback={<div class="empty-state">{error() ? <p class="danger-text">{error()}</p> : <Spinner size={18} />}</div>}>
                  <Show when={visible().length} fallback={<div class="empty-state"><p class="muted">Empty folder</p></div>}>
                    <For each={visible()}>
                      {(e) => (
                        <button
                          type="button"
                          class="dirb-item"
                          classList={{ file: !e.is_dir }}
                          disabled={!e.is_dir}
                          onClick={() => e.is_dir && setPath(e.path)}
                        >
                          <Icon name={e.is_dir ? "folder" : "file"} size={16} />
                          <span>{e.name}</span>
                          <Show when={e.is_dir}>
                            <Icon name="chevronRight" size={14} class="muted" />
                          </Show>
                        </button>
                      )}
                    </For>
                  </Show>
                </Show>
              </div>
              <div class="dirb-foot">
                <label class="inline-toggle small">
                  <input type="checkbox" checked={showFiles()} onChange={(e) => setShowFiles(e.currentTarget.checked)} /> Show files
                </label>
                <span class="spacer" />
                <button type="button" class="btn btn-ghost" onClick={() => close(null)}>
                  Cancel
                </button>
                <button type="button" class="btn btn-primary" disabled={!!error()} onClick={() => close(path())}>
                  Use this folder
                </button>
              </div>
            </div>
          </Modal>
        );
      }}
    </Show>
  );
}
