// Pictures in notes. The server keeps them by their content and a note
// refers to one as `![alt](assets/<name>)`. They come to this device in
// pieces over the app's own connection (`read_note_asset`, like a shared
// file) and are shown from blob: URLs: there is no picture address anyone
// else could open. A picture is scaled down (and turned upright) before it
// is sent, like one attached in a chat (lib/images.ts).
import type { FileChunk } from "../proto/FileChunk";
import type { NoteAsset } from "../proto/NoteAsset";
import { fileToAttachment } from "./images";
import { call, state } from "./store";
import { base64Blob } from "./util";

/** A picture's name in a note: `<16 hex>.<ext>` (what the server gives). */
const NAME = /^[0-9a-f]{16}\.(?:png|jpg|webp|gif)$/;
/** Every `assets/<name>` in a note's text. */
const REFS = /\bassets\/([0-9a-f]{16}\.(?:png|jpg|webp|gif))\b/g;

/** "assets/3f2a….jpg" → "3f2a….jpg" (else null: not one of ours). */
export function assetName(src: string): string | null {
  const m = /^(?:\.\/)?assets\/([^/?#]+)$/.exec(src.trim());
  return m && NAME.test(m[1]) ? m[1] : null;
}

/** The names of the pictures a note's text uses, each once. */
export function assetRefs(text: string): string[] {
  return [...new Set([...text.matchAll(REFS)].map((m) => m[1]))];
}

/** Markdown for a picture: `![alt](assets/name)` (the alt kept to one plain line). */
export function imageMarkdown(name: string, alt = ""): string {
  return `![${alt.replace(/[[\]\r\n]+/g, " ").trim()}](assets/${name})`;
}

/** A sensible alt text for a picked file: its name without the camera's numbering. */
export function altFor(file: File, fallback: string): string {
  const base = file.name.replace(/\.[a-z0-9]{1,5}$/i, "").replace(/[_-]+/g, " ").trim();
  if (!base || /^(img|image|dsc|pxl|photo|screenshot)?\s*[\d\s.]*$/i.test(base) || base === "image") return fallback;
  return base.slice(0, 80);
}

/**
 * The longest side a screenshot (PNG) keeps in a note: a 1440p screen's stays
 * as it is, so its text stays sharp. Photos go down to 2048 px (lib/images.ts).
 */
const SCREENSHOT_SIDE = 2560;

/** Send a picture to the server (scaled down first; as it is if this device can't draw it, the server checks it). */
export async function uploadPicture(file: File): Promise<NoteAsset> {
  let a: { mime: string; data: string };
  try {
    a = await fileToAttachment(file, file.type === "image/png" ? SCREENSHOT_SIDE : undefined);
  } catch (e) {
    if (!/^image\/(png|jpeg|webp|gif)$/.test(file.type) || file.size > 8 << 20) throw e;
    const bytes = new Uint8Array(await file.arrayBuffer());
    let bin = "";
    for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    a = { mime: file.type, data: btoa(bin) };
  }
  const asset = await putAsset(a.mime, a.data);
  // It's here already: no need to fetch it back.
  const url = URL.createObjectURL(base64Blob(a.data, asset.mime));
  remember(asset.name, Promise.resolve(url));
  return asset;
}

/**
 * Sent, and sent again once the connection is back if it was lost on the way:
 * a phone app coming back from the camera or the photo picker reconnects
 * first, and a picture handed over meanwhile shouldn't be lost.
 */
async function putAsset(mime: string, data: string): Promise<NoteAsset> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await call<NoteAsset>({ method: "put_note_asset", params: { mime, data } });
    } catch (e) {
      const err = e as { code?: string; message?: string };
      const lost = err.code === "transport" || /connection lost|not connected|timed out|reconnect/i.test(err.message ?? "");
      if (!lost || attempt >= 3) throw e;
      await connectedAgain(30_000);
    }
  }
}

async function connectedAgain(ms: number) {
  const until = Date.now() + ms;
  await new Promise((r) => setTimeout(r, 300));
  while (Date.now() < until && !(state.conn.state === "connected" && state.ready)) await new Promise((r) => setTimeout(r, 250));
}

// ── fetching, with a small cache of blob: URLs ───────────────────────────

const CHUNK = 1 << 20;
/** Pictures kept as blob: URLs here, newest last. */
const KEEP = 80;
const urls = new Map<string, Promise<string>>();

function remember(name: string, p: Promise<string>) {
  urls.delete(name);
  urls.set(name, p);
  while (urls.size > KEEP) {
    const [old, url] = urls.entries().next().value!;
    urls.delete(old);
    // Pictures already on screen stay drawn; a new <img> asks again.
    void url.then((u) => setTimeout(() => URL.revokeObjectURL(u), 30_000)).catch(() => {});
  }
}

/** A picture's bytes. */
export async function assetBytes(name: string): Promise<Blob> {
  if (!NAME.test(name)) throw new Error("not a picture of a note");
  const parts: Blob[] = [];
  let offset = 0;
  for (;;) {
    const c = await call<FileChunk>({ method: "read_note_asset", params: { name, offset, len: CHUNK } });
    const blob = base64Blob(c.data, "application/octet-stream");
    parts.push(blob);
    offset += blob.size;
    if (c.eof) break;
    if (blob.size === 0) throw new Error("the picture stopped arriving; try again");
  }
  const ext = name.split(".")[1];
  return new Blob(parts, { type: ext === "jpg" ? "image/jpeg" : `image/${ext}` });
}

/** A URL to show a picture with (fetched once, then cached). */
export function assetUrl(name: string): Promise<string> {
  const hit = urls.get(name);
  if (hit) {
    remember(name, hit);
    return hit;
  }
  const p = assetBytes(name).then((b) => URL.createObjectURL(b));
  remember(name, p);
  // A failure is not kept: the next look asks again.
  p.catch(() => urls.get(name) === p && urls.delete(name));
  return p;
}
