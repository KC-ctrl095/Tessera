// Files from a chat the user can download: shared by an agent
// (`share_file`), downloaded in a web view, or their own uploads (to get
// them on another device).
//
// * The apps save natively (`file_download`): a phone into Downloads, a
//   computer where the user picks (Linux) or into Downloads. The file comes
//   over the app's own encrypted connection; progress arrives as
//   `tessera://download` events.
// * A browser fetches the pieces itself and hands the result to the
//   browser's own download.
import { createStore } from "solid-js/store";
import type { Event } from "../proto/Event";
import type { FileChunk } from "../proto/FileChunk";
import type { SharedFile } from "../proto/SharedFile";
import { backend } from "./backend";
import { humanBytes } from "./images";
import { native } from "./native";
import { call } from "./store";
import { errorMessage, toast, toastError } from "./ui";
import { base64Blob, isTauri, isTauriMobile } from "./util";

export type { SharedFile };

export interface DownloadState {
  phase: "downloading" | "saved" | "error";
  received: number;
  size: number;
  /** Where it was saved (a path, or Android's `content://` address). */
  location?: string;
  /** "Downloads", or the folder it went to. */
  place?: string;
  name?: string;
  error?: string;
}

const [downloads, setDownloads] = createStore<Record<string, DownloadState>>({});
export { downloads };

/** A browser keeps this much in memory for one download, at most. */
const BROWSER_MAX = 512 << 20;
const CHUNK = 1 << 20;

interface Saved {
  name: string;
  location: string;
  place: string;
}

let listening = false;
async function listenProgress() {
  if (listening || !isTauri) return;
  listening = true;
  const { listen } = await import("@tauri-apps/api/event");
  await listen<{ id: string; received: number; size: number }>("tessera://download", (e) => {
    const p = e.payload;
    if (downloads[p.id]?.phase === "downloading") setDownloads(p.id, { received: p.received, size: p.size });
  });
}

/** Save a shared file on this device. */
export async function download(f: SharedFile): Promise<void> {
  if (downloads[f.id]?.phase === "downloading") return;
  setDownloads(f.id, { phase: "downloading", received: 0, size: f.size, error: undefined });
  try {
    if (isTauri) {
      await listenProgress();
      const r = await native<Saved>("file_download", { id: f.id, name: f.name, mime: f.mime });
      if (!r) throw new Error("This version of the app can't save files yet. Update it, or use the browser.");
      setDownloads(f.id, { phase: "saved", location: r.location, place: r.place, name: r.name, received: f.size });
      toast({
        level: "success",
        title: r.name,
        message: isTauriMobile ? "Saved to Downloads" : `Saved to ${r.place}`,
        action: { label: "Open", run: () => void openSaved(f) },
      });
    } else {
      await browserDownload(f);
      setDownloads(f.id, { phase: "saved", name: f.name, received: f.size });
    }
  } catch (e) {
    const msg = errorMessage(e);
    if (/^cancel/i.test(msg)) {
      // The user closed the save dialog: back to "Download".
      setDownloads(f.id, undefined!);
      return;
    }
    setDownloads(f.id, { phase: "error", error: msg });
    toastError(e, `Could not download ${f.name}`);
  }
}

async function browserDownload(f: SharedFile) {
  if (f.size > BROWSER_MAX)
    throw new Error(`${f.name} is ${humanBytes(f.size)}; a browser can download up to ${humanBytes(BROWSER_MAX)} here. Use the Tessera app for bigger files.`);
  const parts: Blob[] = [];
  let offset = 0;
  for (;;) {
    const c = await call<FileChunk>({ method: "read_shared_file", params: { id: f.id, offset, len: CHUNK } });
    const blob = base64Blob(c.data, "application/octet-stream");
    parts.push(blob);
    offset += blob.size;
    setDownloads(f.id, { received: offset, size: c.size });
    if (c.eof) break;
    if (blob.size === 0) throw new Error("the download stopped early; try again");
  }
  const url = URL.createObjectURL(new Blob(parts, { type: f.mime || "application/octet-stream" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = f.name;
  a.rel = "noopener";
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

/** Open a file saved here (`reveal`: show it in its folder; computers only). */
export async function openSaved(f: SharedFile, reveal = false): Promise<void> {
  const d = downloads[f.id];
  if (!d?.location) return;
  try {
    await native("file_open", { location: d.location, mime: f.mime, reveal });
  } catch (e) {
    toastError(e, `Could not open ${f.name}`);
  }
}

/**
 * Save text made here (a note as Markdown) on this device, like a download:
 * the apps save natively (a phone into Downloads), a browser through its
 * own download.
 */
export async function saveText(name: string, mime: string, text: string): Promise<void> {
  return saveBlob(name, mime, new Blob([text], { type: mime }));
}

/** Save bytes made here (a note with its pictures, a picture), like `saveText`. */
export async function saveBlob(name: string, mime: string, blob: Blob): Promise<void> {
  if (!isTauri) {
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = name;
    document.body.append(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 10_000);
    return;
  }
  try {
    const r = await native<Saved>("file_save", { name, mime, data: await blobBase64(blob) });
    if (!r) throw new Error("This version of the app can't save files yet. Update it, or use Copy.");
    toast({
      level: "success",
      title: r.name,
      message: isTauriMobile ? "Saved to Downloads" : `Saved to ${r.place}`,
      action: { label: "Open", run: () => void native("file_open", { location: r.location, mime, reveal: false }).catch((e) => toastError(e, `Could not open ${r.name}`)) },
    });
  } catch (e) {
    // The user closed the save dialog.
    if (/^cancel/i.test(errorMessage(e))) return;
    toastError(e, `Could not save ${name}`);
  }
}

/** A blob's bytes as base64. */
async function blobBase64(blob: Blob): Promise<string> {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

// ── files of a chat, by session ─────────────────────────────────────────

const [bySession, setBySession] = createStore<Record<string, SharedFile[]>>({});
const loading = new Set<string>();

/** A chat's downloadable files (loaded on first use, kept up to date). */
export function sessionFiles(sessionId: string): SharedFile[] {
  if (!(sessionId in bySession) && !loading.has(sessionId)) {
    loading.add(sessionId);
    call<SharedFile[]>({ method: "list_shared_files", params: { session_id: sessionId } })
      .then((list) => setBySession(sessionId, list))
      .catch(() => setBySession(sessionId, []))
      .finally(() => loading.delete(sessionId));
  }
  return bySession[sessionId] ?? [];
}

/** The downloadable copy of a file the user attached in this chat. */
export function uploadFile(sessionId: string | undefined, path: string | undefined): SharedFile | undefined {
  if (!sessionId || !path) return undefined;
  return sessionFiles(sessionId).find((f) => f.source === "upload" && f.path === path);
}

// ── web view downloads ──────────────────────────────────────────────────

const [webDownloads, setWebDownloads] = createStore<Record<string, SharedFile[]>>({});
export { webDownloads };

/** Hide a web view download from its tile (it stays in the chat's files). */
export function dismissWebDownload(boardId: string, id: string) {
  setWebDownloads(boardId, (l) => (l ?? []).filter((f) => f.id !== id));
}

function onEvent(e: Event) {
  if (e.type !== "file_shared") return;
  const f = e.file;
  if (f.session_id && f.session_id in bySession && !bySession[f.session_id].some((x) => x.id === f.id))
    setBySession(f.session_id, (l) => [f, ...l]);
  if (f.source === "web_view") {
    if (f.board_id) setWebDownloads(f.board_id, (l) => [f, ...(l ?? []).filter((x) => x.id !== f.id)].slice(0, 5));
    // The tile may be out of sight (or the agent's own browser fetched it).
    toast({
      level: "info",
      title: f.name,
      message: `Downloaded in the web view · ${fileMeta(f)}`,
      action: { label: isTauriMobile ? "Save to phone" : "Save", run: () => void download(f) },
      key: `web-download:${f.board_id ?? f.session_id ?? f.id}`,
      timeout: 10_000,
    });
  }
}

let watching = false;
/** Follow new files as they are shared. */
export function watchFiles() {
  if (watching) return;
  watching = true;
  backend.onEvent(onEvent);
}

/** "2.1 MB · PDF" */
export function fileMeta(f: Pick<SharedFile, "size" | "name" | "mime">): string {
  const ext = /\.([a-z0-9]{1,8})$/i.exec(f.name)?.[1]?.toUpperCase();
  return ext ? `${humanBytes(f.size)} · ${ext}` : humanBytes(f.size);
}
