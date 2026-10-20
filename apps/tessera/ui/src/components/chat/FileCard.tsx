// A downloadable file in the chat: shared by an agent, downloaded in a web
// view, or one the user attached. One tap on Download saves it on this
// device (a phone: Downloads; a computer: where you pick), then opens it. A
// tap on its name opens it where Settings says (a text file: a tile by
// default; a picture: the picture viewer); its ⋯ menu (or a right-click, or
// a long press) offers every way (components/files/fileMenu.ts).
import { For, Show, createMemo } from "solid-js";
import { Dynamic } from "solid-js/web";
import { OPEN_LABEL, defaultOpen, isPictureFile, openFile, sharedViewFile, viewPicture } from "../../lib/fileOpen";
import { dismissWebDownload, download, downloads, fileMeta, openSaved, webDownloads, type SharedFile } from "../../lib/files";
import { isDesktopApp } from "../../lib/native";
import { state, type SessionState } from "../../lib/store";
import { argsObject } from "../../lib/tools";
import { basename, isTauri, isTauriMobile } from "../../lib/util";
import { openFileMenu } from "../files/fileMenu";
import { Icon, Tessellate } from "../icons";
import { longPress, type Anchor } from "../primitives";
import { FileIcon } from "./Messages";
import { ToolCard, toolView } from "./ToolCard";

const SOURCE: Record<string, string> = {
  agent: "Shared in the chat",
  web_view: "Downloaded in the web view",
  upload: "Attached",
};

export function FileCard(props: { file: SharedFile; compact?: boolean; onDismiss?: () => void }) {
  const f = () => props.file;
  // A private chat's file: its text is never kept on this device.
  const vf = () => ({ ...sharedViewFile(f()), ...(f().session_id && state.sessions[f().session_id!]?.info.private ? { private: true } : {}) });
  /** What a tap on the name does: open it (text, pictures), else nothing (Download is beside it). */
  const opens = () => isPictureFile(vf()) || defaultOpen(vf()) !== null;
  const open = () => {
    // A long press opened its menu: that tap was for the menu.
    if (document.querySelector(".menu-pop")) return;
    void (isPictureFile(vf()) ? viewPicture(vf()) : openFile(vf()));
  };
  const menu = (anchor: Anchor) => openFileMenu(anchor, vf(), () => void download(f()));
  const press = longPress((x, y) => menu({ x, y }));
  const d = () => downloads[f().id];
  const pct = () => {
    const s = d();
    return s && s.size > 0 ? Math.min(100, Math.round((s.received / s.size) * 100)) : 0;
  };
  const saved = () => d()?.phase === "saved";
  const busy = () => d()?.phase === "downloading";
  const where = () => {
    const s = d();
    if (!saved()) return null;
    if (!isTauri) return "Downloaded";
    return isTauriMobile ? "Saved to Downloads" : `Saved to ${s?.place ?? "your computer"}`;
  };
  return (
    <div
      class="shared-file"
      classList={{ compact: !!props.compact, saved: saved(), busy: busy() }}
      data-file={f().id}
      {...press}
      onContextMenu={(e) => {
        e.preventDefault();
        menu({ x: e.clientX, y: e.clientY });
      }}
    >
      <Dynamic
        component={opens() ? "button" : "span"}
        {...(opens() ? { type: "button", "aria-label": `Open ${f().name}`, title: isPictureFile(vf()) ? "View" : OPEN_LABEL[defaultOpen(vf())!], onClick: open } : {})}
        class="shared-file-main"
        classList={{ opens: opens() }}
      >
        <span class="shared-file-icon">
          <FileIcon name={f().name} size={18} />
        </span>
        <span class="shared-file-text">
          <span class="shared-file-name" title={f().name}>
            {f().name}
          </span>
          <span class="shared-file-meta">
            {where() ?? (busy() ? `Downloading… ${pct()}%` : `${fileMeta(f())}${props.compact ? "" : ` · ${SOURCE[f().source] ?? "File"}`}`)}
          </span>
        </span>
      </Dynamic>
      <span class="shared-file-actions">
        <Show
          when={saved() && isTauri}
          fallback={
            <button
              type="button"
              class="btn btn-sm shared-file-get"
              disabled={busy()}
              aria-label={`Download ${f().name}`}
              title={isTauriMobile ? "Save to Downloads" : isTauri ? "Save on this computer" : "Download"}
              onClick={() => void download(f())}
            >
              <Show when={busy()} fallback={<Icon name="download" size={14} />}>
                <span class="shared-file-pct">{pct()}%</span>
              </Show>
              <Show when={!props.compact && !busy()}>
                <span>{saved() ? "Again" : "Download"}</span>
              </Show>
            </button>
          }
        >
          <button type="button" class="btn btn-sm" onClick={() => void openSaved(f())} aria-label={`Open ${f().name}`}>
            <Icon name="external" size={14} />
            <Show when={!props.compact}>
              <span>Open</span>
            </Show>
          </button>
          <Show when={isDesktopApp}>
            <button type="button" class="btn btn-sm" title="Show in folder" aria-label="Show in folder" onClick={() => void openSaved(f(), true)}>
              <Icon name="folder" size={14} />
            </button>
          </Show>
        </Show>
        <button type="button" class="icon-btn shared-file-more" aria-label={`More for ${f().name}`} aria-haspopup="menu" data-tip="Open with…, download" onClick={(e) => menu(e.currentTarget)}>
          <Icon name="more" size={15} />
        </button>
        <Show when={props.onDismiss}>
          <button type="button" class="icon-btn shared-file-x" aria-label="Dismiss" onClick={() => props.onDismiss?.()}>
            <Icon name="x" size={13} />
          </button>
        </Show>
      </span>
      <Show when={busy()}>
        <span class="shared-file-bar" style={{ width: `${pct()}%` }} />
      </Show>
    </div>
  );
}

/** The file details a `share_file` result carries. */
function sharedOf(details: unknown): SharedFile | null {
  const f = (details as { file?: SharedFile } | null | undefined)?.file;
  return f && typeof f.id === "string" && typeof f.name === "string" ? f : null;
}

/**
 * A `share_file` call in the transcript: the file's card once it is shared
 * (never folded into a steps group), a quiet placeholder while it is
 * copied, and the usual tool card when it was refused.
 */
export function SharedFileCall(props: { session: SessionState; callId: string; args: unknown }) {
  const result = () => props.session.results[props.callId];
  const file = createMemo(() => sharedOf(result()?.details));
  const path = () => {
    const p = argsObject(props.args)?.path;
    return typeof p === "string" ? basename(p) : "file";
  };
  return (
    <Show
      when={file()}
      fallback={
        <Show
          when={result()}
          fallback={
            <div class="shared-file busy tool-file" aria-busy="true">
              <span class="shared-file-icon">
                <Tessellate size={16} />
              </span>
              <span class="shared-file-text">
                <span class="shared-file-name">{path()}</span>
                <span class="shared-file-meta">Getting the file ready…</span>
              </span>
            </div>
          }
        >
          <ToolCard view={toolView(props.session, props.callId, "share_file", props.args)} session={props.session} />
        </Show>
      }
    >
      {(f) => (
        <div class="tool-file">
          <FileCard file={f()} />
        </div>
      )}
    </Show>
  );
}

/**
 * What the server's browser downloaded in this web view, as a slim bar
 * between the address bar and the page: save it here with one tap.
 */
export function WebDownloads(props: { boardId: string }) {
  const list = () => webDownloads[props.boardId] ?? [];
  return (
    <Show when={list().length}>
      <div class="wv-downloads" aria-label="Downloads">
        <For each={list()}>{(f) => <FileCard file={f} compact onDismiss={() => dismissWebDownload(props.boardId, f.id)} />}</For>
      </div>
    </Show>
  );
}
