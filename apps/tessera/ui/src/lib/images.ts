import type { ContentPart } from "../proto/ContentPart";

export const MAX_IMAGE_SIDE = 2048;

export interface Attachment {
  id: string;
  /** Images go in the message as image parts; other files are sent as attachments. */
  kind: "image" | "file";
  name: string;
  mime: string;
  /** Base64 without the data: prefix. */
  data: string;
  /** Object URL / data URL for previews (images). */
  preview: string;
  width: number;
  height: number;
  /** Bytes (of the original file). */
  size: number;
  /** A big paste turned into a file: its text, for the preview and to put it back in the message. */
  pasted?: string;
}

/** Limits shared with the server (see `attach()` in the engine). */
export const MAX_FILE_BYTES = 25 << 20;
export const MAX_FILES = 10;
/** Private chats keep attachments in memory: text files up to this size. */
export const MAX_PRIVATE_TEXT = 256 << 10;

const TEXT_EXT =
  /\.(txt|md|markdown|rst|csv|tsv|json|jsonl|ya?ml|toml|ini|cfg|conf|env|log|xml|html?|css|scss|js|mjs|cjs|jsx|ts|tsx|rs|py|rb|go|java|kt|swift|c|h|cc|cpp|hpp|cs|php|sh|bash|zsh|fish|sql|lua|pl|r|dart|vue|svelte|diff|patch|tex|srt|vtt)$/i;

/** Plain text we can send even to a private chat. */
export const isTextFile = (f: { name: string; mime: string }) =>
  /^text\//.test(f.mime) || /^application\/(json|xml|x-yaml|yaml|toml|javascript|x-sh)/.test(f.mime) || TEXT_EXT.test(f.name);

export function humanBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${u[i]}`;
}

/** Decode/encode only when the reader actually attaches a file. */
export const fileToFileAttachment = (file: File): Promise<Attachment> => import("./imageAttachment").then((m) => m.fileToFileAttachment(file));
export const fileToAttachment = (file: File, maxSide = MAX_IMAGE_SIDE): Promise<Attachment> => import("./imageAttachment").then((m) => m.fileToAttachment(file, maxSide));

/** Names of big pastes sent as files ("Pasted text.txt", "Pasted text 2.txt"). */
export const PASTED_NAME = /^Pasted text(?: \d+)?\.txt$/;

/** A big paste as a text file attachment ("Pasted text.txt"; `n` > 1 numbers it). */
export function pastedTextAttachment(text: string, n = 1): Attachment {
  const data = utf8Base64(text);
  return {
    id: Math.random().toString(36).slice(2),
    kind: "file",
    name: n > 1 ? `Pasted text ${n}.txt` : "Pasted text.txt",
    mime: "text/plain",
    data,
    preview: "",
    width: 0,
    height: 0,
    size: new TextEncoder().encode(text).length,
    pasted: text,
  };
}

export const attachmentToPart = (a: Attachment): ContentPart => ({ type: "image", mime: a.mime, data: a.data });

export const imageSrc = (p: Extract<ContentPart, { type: "image" }>) => `data:${p.mime};base64,${p.data}`;

/** What the user typed in a message, without the notes the server added for attached files. */
/** The note the server adds after an attached image (the image itself is shown). */
const ATTACHED_IMAGE = /^\[Attached image: [^\]\n]*\]\s*$/;

export function typedText(content: readonly ContentPart[]): string {
  return content
    .filter((p): p is Extract<ContentPart, { type: "text" }> => p.type === "text" && !parseAttachedFile(p.text) && !ATTACHED_IMAGE.test(p.text))
    .map((p) => p.text)
    .join("\n");
}

/** Text as base64 of its UTF-8 bytes. */
export function utf8Base64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

/**
 * A sent message's attachments, ready for the composer again: its images,
 * and the text files whose content the server inlined. Other files only
 * live on the server now; `missing` names them.
 */
export function messageAttachments(content: readonly ContentPart[]): { attachments: Attachment[]; missing: string[] } {
  const attachments: Attachment[] = [];
  const missing: string[] = [];
  let n = 0;
  for (const p of content) {
    if (p.type === "image") {
      n++;
      attachments.push({
        id: Math.random().toString(36).slice(2),
        kind: "image",
        name: `image-${n}.${p.mime.split("/")[1]?.replace("jpeg", "jpg") || "png"}`,
        mime: p.mime,
        data: p.data,
        preview: imageSrc(p),
        width: 0,
        height: 0,
        size: Math.floor((p.data.length * 3) / 4),
      });
      continue;
    }
    if (p.type !== "text") continue;
    const f = parseAttachedFile(p.text);
    if (!f) continue;
    if (f.text === undefined) {
      missing.push(f.name);
      continue;
    }
    const data = utf8Base64(f.text);
    attachments.push({
      id: Math.random().toString(36).slice(2),
      kind: "file",
      name: f.name,
      mime: "text/plain",
      data,
      preview: "",
      width: 0,
      height: 0,
      size: new TextEncoder().encode(f.text).length,
      ...(PASTED_NAME.test(f.name) ? { pasted: f.text } : {}),
    });
  }
  return { attachments, missing };
}

/** A file part the server added to a user message: `[Attached file: name · size · saved at /path]`. */
export interface AttachedFile {
  name: string;
  size: string;
  path?: string;
  /** Inlined text content, if any. */
  text?: string;
  note?: string;
}

export function parseAttachedFile(text: string): AttachedFile | null {
  const m = /^\[Attached file: (.+?) · ([^·\]]+?)(?: · saved at ([^\]]+))?\]/.exec(text);
  if (!m) return null;
  const rest = text.slice(m[0].length).replace(/^\n/, "");
  const fence = /^```[^\n]*\n([\s\S]*?)\n?```\s*$/.exec(rest);
  return {
    name: m[1],
    size: m[2].trim(),
    path: m[3]?.trim(),
    text: fence ? fence[1] : undefined,
    note: !fence && rest.trim() ? rest.trim().replace(/^\(|\)$/g, "") : undefined,
  };
}
