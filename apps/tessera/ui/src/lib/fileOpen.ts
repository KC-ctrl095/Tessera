// Opening a file from a chat (an agent's shared file, a web view download,
// a file the user attached) where the user wants it: in a tile here, in a
// window of its own, or with this device's app for it. Settings → Appearance
// says what a tap on a file's name does ("Open files in"); each file's menu
// offers the other ways and Download, which stays as it was. Nothing here
// opens a browser tab: a window is the app's own page (lib/fileWindow.ts).
//
// Text comes over the app's own connection (`read_shared_file`, as a
// download does) or from the chat itself (an attached text file's text).
// Programs are never handed to the device to open (the app refuses them too,
// see apps/tessera/src-tauri/src/files.rs `opens_as`).
import type { SharedFile } from "../proto/SharedFile";
import type { FileChunk } from "../proto/FileChunk";
import { openBrowserWindow } from "./fileWindow";
import { download, downloads, openSaved, saveBlob } from "./files";
import { humanBytes, isTextFile, utf8Base64 } from "./images";
import { isDesktopApp, native } from "./native";
import { NOTE_MAX, openNote, rememberNote } from "./notes";
import { prefs } from "./prefs";
import { api, call } from "./store";
import { toast, toastError } from "./ui";
import { base64Blob, isTauri, isTauriMobile, touchUi } from "./util";
import { openViewer } from "./viewer";
import type { ViewFile } from "./viewFile";
import { focusPane, openPane, workspace } from "./workspace";

export type { ViewFile };

/** Where a file opens: a tile here, a window of its own, or this device's app. */
export type OpenIn = "tile" | "window" | "app";

/** The most a tile or window reads of a file (bigger ones: download them). */
export const VIEW_MAX = 8 << 20;
const CHUNK = 1 << 20;

const PICTURE = /\.(png|jpe?g|gif|webp|bmp|avif)$/i;

/** Text to read: by its type or name, or because the chat has its text. */
export const isTextLike = (f: ViewFile) => f.text !== undefined || isTextFile({ name: f.name, mime: f.mime ?? "" });
/** A picture the viewer shows. */
export const isPictureFile = (f: ViewFile) => /^image\/(png|jpeg|gif|webp|bmp|avif)$/.test(f.mime ?? "") || PICTURE.test(f.name);

/** Programs, installers and shortcuts (as the apps' `opens_as`): never opened from Tessera. */
const PROGRAM =
  /\.(exe|msi|msix|msixbundle|appx|appxbundle|msp|com|scr|pif|cpl|dll|sys|drv|ocx|lnk|url|website|appref-ms|application|gadget|hta|reg|inf|scf|msc|diagcab|settingcontent-ms|library-ms|jar|jnlp|app|pkg|mpkg|dmg|deb|rpm|apk|aab|xapk|appimage|snap|flatpak|flatpakref|flatpakrepo|run|bin|elf|out|desktop|workflow|action|scpt|xpi|crx|pyc|pyo|class)[. ]*$/i;
export const isProgram = (f: Pick<ViewFile, "name">) => PROGRAM.test(f.name);

/** Where files can open on this device. */
export function openPlaces(f: ViewFile): Record<OpenIn, boolean> {
  const text = isTextLike(f);
  return {
    tile: text,
    // The desktop app makes a window of its own; a browser on a computer opens the app's page in one. Not on phones.
    window: text && (isDesktopApp || (!isTauri && !touchUi())),
    // The apps hand it to the system (a browser could only download it).
    app: isTauri && !isProgram(f),
  };
}

/** What a tap on the file opens: the user's choice where it can, else a tile; null when nothing opens (download it). */
export function defaultOpen(f: ViewFile): OpenIn | null {
  const can = openPlaces(f);
  if (can[prefs.openFiles]) return prefs.openFiles;
  if (can.tile) return "tile";
  return can.app && prefs.openFiles === "app" ? "app" : null;
}

/** A shared file as a file to open. */
export function sharedViewFile(f: SharedFile): ViewFile {
  return { name: f.name, mime: f.mime, size: f.size, id: f.id, path: f.path, sessionId: f.session_id ?? undefined };
}

function asShared(f: ViewFile): SharedFile {
  return { id: f.id!, name: f.name, mime: f.mime || "application/octet-stream", size: f.size ?? 0, created_at: 0, source: "agent", path: f.path ?? "" };
}

/** The file's bytes (at most `max`; null when it is bigger). */
async function bytesOf(f: ViewFile, max: number): Promise<Blob | null> {
  if (!f.id) return f.text !== undefined ? new Blob([f.text], { type: f.mime || "text/plain" }) : null;
  if ((f.size ?? 0) > max) return null;
  const parts: Blob[] = [];
  let offset = 0;
  for (;;) {
    const c = await call<FileChunk>({ method: "read_shared_file", params: { id: f.id, offset, len: CHUNK } });
    if (c.size > max) return null;
    const blob = base64Blob(c.data, "application/octet-stream");
    parts.push(blob);
    offset += blob.size;
    if (c.eof) break;
    if (blob.size === 0) throw new Error("the file stopped arriving; try again");
  }
  return new Blob(parts, { type: f.mime || "application/octet-stream" });
}

/** Why a file can't be read here. */
export class NotText extends Error {}
export class TooBig extends Error {}

/** A file's text: the chat's copy, or read from the server (UTF-8). */
export async function loadText(f: ViewFile): Promise<string> {
  if (f.text !== undefined) return f.text;
  const b = await bytesOf(f, VIEW_MAX);
  if (!b) throw new TooBig(`${f.name} is ${humanBytes(f.size ?? VIEW_MAX + 1)}; files up to ${humanBytes(VIEW_MAX)} open here. Download it instead.`);
  const bytes = new Uint8Array(await b.arrayBuffer());
  // A zero byte near the start: not text (a picture, an archive…).
  if (bytes.subarray(0, 8000).includes(0)) throw new NotText(`${f.name} isn't a text file. Download it to open it.`);
  return new TextDecoder("utf-8").decode(bytes);
}

/** Open `f` the way asked (else the user's way). */
export async function openFile(f: ViewFile, how: OpenIn | null = defaultOpen(f)): Promise<void> {
  if (!how) return;
  if (how === "tile") return void openFileTile(f);
  if (how === "window") return openFileWindow(f);
  return openWithApp(f);
}

/** Show a picture from a chat in the picture viewer. */
export async function viewPicture(f: ViewFile): Promise<void> {
  try {
    const b = await bytesOf(f, 64 << 20);
    if (!b) throw new Error("It is too big to show here. Download it instead.");
    openViewer([{ src: URL.createObjectURL(b), alt: f.name }]);
  } catch (e) {
    toastError(e, `Couldn't show ${f.name}`);
  }
}

/** The tile showing this file, if any. */
function tileShowing(f: ViewFile): string | null {
  for (const p of Object.values(workspace.panes)) {
    if (p.content.type !== "file") continue;
    const g = p.content.file;
    if (f.id ? g.id === f.id : !g.id && g.name === f.name && g.sessionId === f.sessionId && g.text === f.text) return p.id;
  }
  return null;
}

/** Read it in a tile beside the one in front (or focus the tile already showing it). */
export function openFileTile(f: ViewFile): string | null {
  const have = tileShowing(f);
  if (have) {
    focusPane(have);
    return have;
  }
  const id = openPane({ type: "file", file: { ...f } }, workspace.focused());
  if (!id) toast({ level: "warning", message: "There's no room for another tile here. Close one, or use another mosaic." });
  return id;
}

/** In a window of its own: the desktop app's, or (a browser) one of the app's own pages. */
export async function openFileWindow(f: ViewFile): Promise<void> {
  let text: string;
  try {
    text = await loadText(f);
  } catch (e) {
    return toastError(e, `Couldn't open ${f.name}`);
  }
  const file = { name: f.name, mime: f.mime ?? "text/plain", text };
  if (isDesktopApp) {
    try {
      const done = await native("file_window", file);
      if (done !== null) return;
    } catch (e) {
      return toastError(e, `Couldn't open ${f.name} in a window`);
    }
    // An app from before windows: a tile instead.
    toast({ level: "info", message: "This version of the app can't open windows yet; it opened in a tile." });
    openFileTile(f);
    return;
  }
  if (!openBrowserWindow(file)) {
    toast({ level: "info", message: "The browser blocked the window (allow pop-ups for Tessera); it opened in a tile." });
    openFileTile(f);
  }
}

/** With this device's app for it (the desktop app: without a save dialog; a phone: saved to Downloads first). */
export async function openWithApp(f: ViewFile): Promise<void> {
  if (isProgram(f)) return void toast({ level: "info", title: f.name, message: "Tessera doesn't open programs or installers. Download it and open it yourself if you trust it." });
  try {
    if (isDesktopApp) {
      const done = await native("file_open_with", f.id ? { id: f.id, name: f.name } : { name: f.name, data: utf8Base64(await loadText(f)) });
      if (done !== null) return;
    }
    if (f.id) {
      // Saved to this device first (as Download does), then opened.
      const s = asShared(f);
      if (downloads[s.id]?.phase !== "saved" || !downloads[s.id]?.location) await download(s);
      if (downloads[s.id]?.location) await openSaved(s);
      return;
    }
    const saved = await native<{ location: string; name: string }>("file_save", { name: f.name, mime: f.mime || "text/plain", data: utf8Base64(await loadText(f)) });
    if (saved) await native("file_open", { location: saved.location, mime: f.mime || "text/plain", reveal: false });
  } catch (e) {
    if (/^cancel/i.test(String((e as { message?: string })?.message ?? e))) return;
    toastError(e, `Couldn't open ${f.name}`);
  }
}

/** Save it on this device: a shared file as Download does; text the chat has, as a file. */
export async function saveFile(f: ViewFile): Promise<void> {
  if (f.id) return download(asShared(f));
  if (f.text !== undefined) return saveBlob(f.name, f.mime || "text/plain", new Blob([f.text], { type: f.mime || "text/plain" }));
}

const LANG: Record<string, string> = { py: "python", rs: "rust", ts: "ts", tsx: "tsx", js: "js", mjs: "js", sh: "sh", yml: "yaml", md: "markdown", kt: "kotlin", rb: "ruby" };

/** A note made from a text file: Markdown as it is, anything else as a code block. */
export async function saveAsNote(f: ViewFile, text?: string): Promise<void> {
  try {
    const t = text ?? (await loadText(f));
    const ext = /\.([a-z0-9]{1,8})$/i.exec(f.name)?.[1]?.toLowerCase() ?? "";
    let content = t;
    if (!/^(md|markdown)$/.test(ext) && !/^text\/markdown/.test(f.mime ?? "")) {
      const longest = Math.max(2, ...(t.match(/`+/g) ?? []).map((r) => r.length));
      const fence = "`".repeat(longest + 1);
      content = `${fence}${ext === "txt" || ext === "log" ? "" : (LANG[ext] ?? ext)}\n${t.replace(/\n$/, "")}\n${fence}\n`;
    }
    if (new TextEncoder().encode(content).length > NOTE_MAX) throw new Error(`Notes hold up to ${humanBytes(NOTE_MAX)}; this file is bigger.`);
    const note = await api.saveNote({ id: null, title: f.name.replace(/\.(md|markdown|txt)$/i, "").slice(0, 120) || "File", content, base_version: null, session_id: f.sessionId ?? null });
    rememberNote(note);
    openNote(note.id, { newPane: true });
  } catch (e) {
    toastError(e, "Couldn't make a note of it");
  }
}

/** The labels the menus and Settings use. */
export const OPEN_LABEL: Record<OpenIn, string> = {
  tile: "Open in a tile",
  window: "Open in a new window",
  app: isTauriMobile ? "Open with an app on this phone" : "Open with this computer's app",
};
