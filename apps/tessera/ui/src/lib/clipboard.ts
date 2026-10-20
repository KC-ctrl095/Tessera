// What a paste brings into the composer, on every platform:
//
// * Browsers (Chromium, Firefox) and Android's web view put pasted images in
//   the paste event as files (some list them only under `items`).
// * WebKitGTK (the Linux desktop app) hands the page no image data at all:
//   the paste event is empty, or carries only the picture's markup. Files
//   copied in a file manager come as an empty `text/uri-list` (Nautilus,
//   Dolphin) or as their paths in plain text (Thunar), and files dragged in
//   from one only as links. The shell reads them instead: the image from GTK's
//   clipboard (`clipboard_image`), copied or dropped files from disk
//   (`clipboard_files`, `dropped_files`).
// * Android keyboards (Gboard's or Samsung's clipboard, GIF and sticker
//   keyboards) insert images through the platform's receive-content API,
//   which the web view refuses in a text box. The app's activity takes them
//   and hands them to the page as a `tessera:content` event (MainActivity.kt).
//
// An image or file wins over the link, markup or path beside it; plain text
// stays text, and big pastes become a "Pasted text" attachment instead of
// flooding the box.
import { MAX_FILE_BYTES, MAX_FILES } from "./images";
import { native, isDesktopApp } from "./native";
import { toast } from "./ui";
import { base64Blob } from "./util";

/** A paste longer than this (characters or lines) becomes a "Pasted text" attachment. */
export const BIG_PASTE_CHARS = 1500;
export const BIG_PASTE_LINES = 25;

export function lineCount(text: string): number {
  let n = 1;
  for (let i = text.indexOf("\n"); i >= 0 && i < text.length - 1; i = text.indexOf("\n", i + 1)) n++;
  return n;
}

export const isBigPaste = (text: string) => text.length > BIG_PASTE_CHARS || lineCount(text) > BIG_PASTE_LINES;

const EXT: Record<string, string> = { "image/jpeg": "jpg", "image/svg+xml": "svg" };
const extOf = (mime: string) => EXT[mime] ?? (mime.split("/")[1] || "bin").replace(/[^a-z0-9]/gi, "");

/** A file for pasted data, named after what it is ("Pasted image.png"). */
export function pastedFile(blob: Blob, mime = blob.type, name?: string): File {
  const kind = mime.startsWith("image/") ? "image" : "file";
  return new File([blob], name || `Pasted ${kind}.${extOf(mime)}`, { type: mime, lastModified: Date.now() });
}

/** The files a paste or drop carries (`files`, or file items where only those are listed). */
export function transferFiles(dt: DataTransfer | null | undefined): File[] {
  if (!dt) return [];
  const files = Array.from(dt.files ?? []);
  if (!files.length) {
    for (const item of Array.from(dt.items ?? [])) {
      if (item.kind !== "file") continue;
      const f = item.getAsFile();
      if (f) files.push(f);
    }
  }
  // Clipboard images often come unnamed or as a generic "image.png".
  return files.map((f) => (f.name && f.name !== "image.png" ? f : pastedFile(f, f.type || "application/octet-stream")));
}

/** The Linux desktop app, whose web view (WebKitGTK) keeps copied files and images from the page. */
const linuxApp = isDesktopApp && typeof navigator !== "undefined" && /Linux/.test(navigator.userAgent);

/** A line that is a file's path or link, or a web link (a copied picture's address). */
const LINK_OR_PATH = /^(?:file:\/\/\S|\/\S|~\/|https?:\/\/\S+$)/;

/**
 * Whether a paste may bring more than the page sees, so the composer asks
 * (`clipboardFiles`) before taking it as text: in the desktop app a paste
 * with nothing visible at all; on Linux also a file manager's copy (an empty
 * uri-list, or the files' paths as text) and a picture's markup or link
 * beside the picture itself.
 */
export function pasteMayHide(dt: DataTransfer | null | undefined, text: string): boolean {
  if (!text) return isDesktopApp;
  if (!linuxApp) return false;
  if (dt?.types.includes("text/uri-list") || /<img\b/i.test(dt?.getData("text/html") ?? "")) return true;
  const lines = text.trim().split(/\s*\n\s*/);
  return text.length <= 8192 && lines.length <= 50 && lines.every((l) => LINK_OR_PATH.test(l));
}

/** Whether a drop may carry files the page doesn't get (on Linux WebKitGTK gives a file manager's files as links). */
export const dropMayHide = (dt: DataTransfer | null | undefined) => linuxApp && !!dt?.types.includes("text/uri-list");

/** Files the shell read for a paste or drop (`clipboard_files`, `dropped_files`). */
interface PickedFiles {
  files: { name: string; mime: string; data: string }[];
  skipped: { name: string; reason: "folder" | "too large" | "unreadable" | "limit" }[];
}

const SKIPPED: Record<PickedFiles["skipped"][number]["reason"], string> = {
  folder: "Folders can't be attached; attach the files in it.",
  "too large": `Larger than ${MAX_FILE_BYTES >> 20} MB.`,
  unreadable: "This file couldn't be read.",
  limit: `Attach at most ${MAX_FILES} files at a time.`,
};

/** The files as `File`s; says which were left out. `null` when it named none. */
function picked(got: PickedFiles | null): File[] | null {
  if (!got || (!got.files.length && !got.skipped.length)) return null;
  for (const s of got.skipped) toast({ level: "warning", title: s.name, message: SKIPPED[s.reason] ?? "Not attached." });
  return got.files.map((f) => new File([base64Blob(f.data, f.mime)], f.name, { type: f.mime, lastModified: Date.now() }));
}

/** How long a paste waits for the web view's clipboard API (it may ask the user first, or never answer). */
const WEB_READ_MS = 1500;

/** Images the web view's clipboard API gives; read at once, while the paste is handled. */
async function webImages(): Promise<File[] | null> {
  const files: File[] = [];
  try {
    const timeout = new Promise<null>((resolve) => setTimeout(() => resolve(null), WEB_READ_MS));
    for (const item of (await Promise.race([navigator.clipboard.read(), timeout])) ?? []) {
      const type = item.types.find((t) => t.startsWith("image/"));
      if (type) files.push(pastedFile(await item.getType(type), type));
    }
  } catch {
    // Not allowed here, or nothing readable.
  }
  return files.length ? files : null;
}

/**
 * What a paste holds that its event didn't carry: files copied in a file
 * manager (attached as they are), else an image. In the Linux desktop app the
 * shell reads them; elsewhere the web view's clipboard API. `null` when there
 * is nothing to attach (paste the text, if any); `[]` when files were named
 * but none could be attached (the user was told). Only call it while handling
 * a paste: that is when the page may read the clipboard.
 */
export async function clipboardFiles(): Promise<File[] | null> {
  if (!linuxApp) return webImages();
  try {
    const copied = picked(await native<PickedFiles>("clipboard_files"));
    if (copied) return copied;
    const shot = await native<{ mime: string; data: string } | null>("clipboard_image");
    return shot?.data ? [pastedFile(base64Blob(shot.data, shot.mime), shot.mime)] : null;
  } catch (e) {
    console.warn("reading the clipboard failed", e);
    return null;
  }
}

/** Files dragged in from a file manager that came only as links (see `dropMayHide`); as `clipboardFiles`. */
export async function droppedFiles(): Promise<File[] | null> {
  try {
    return picked(await native<PickedFiles>("dropped_files"));
  } catch (e) {
    console.warn("reading the dropped files failed", e);
    return null;
  }
}

/** What Android's activity hands over: files a keyboard inserted (see MainActivity.kt). */
interface ReceivedContent {
  items: { mime: string; data: string; name?: string }[];
}

/**
 * Files inserted by a keyboard on Android, for the composer that has the
 * focus. `cb` returns true when it took them.
 */
export function onReceivedFiles(cb: (files: File[]) => boolean): () => void {
  const handler = (e: Event) => {
    const detail = (e as CustomEvent<ReceivedContent>).detail;
    if (!detail?.items?.length) return;
    const files = detail.items.filter((i) => i.data).map((i) => pastedFile(base64Blob(i.data, i.mime), i.mime, i.name));
    if (files.length && cb(files)) e.stopImmediatePropagation();
  };
  window.addEventListener("tessera:content", handler);
  return () => window.removeEventListener("tessera:content", handler);
}

/**
 * The text an input event added, when one input brought a lot at once (an
 * Android keyboard's clipboard commits text without a paste event): where it
 * starts and what it is. `before` is the value before the input.
 */
export function insertedChunk(before: string, after: string): { start: number; text: string } | null {
  const grew = after.length - before.length;
  if (grew <= 0) return null;
  let start = 0;
  const max = Math.min(before.length, after.length);
  while (start < max && before.charCodeAt(start) === after.charCodeAt(start)) start++;
  let end = 0;
  while (end < before.length - start && before.charCodeAt(before.length - 1 - end) === after.charCodeAt(after.length - 1 - end)) end++;
  const text = after.slice(start, after.length - end);
  return isBigPaste(text) ? { start, text } : null;
}
