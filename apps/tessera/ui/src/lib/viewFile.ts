// A file from a chat as a tile shows it (lib/fileOpen.ts opens it,
// components/files/FileTile.tsx shows it). Kept apart so the workspace can
// keep such tiles without loading the rest.

/** A file from a chat: an agent's shared file, a web view download, or one the user attached. */
export interface ViewFile {
  name: string;
  mime?: string;
  /** Bytes, when known. */
  size?: number;
  /** A shared file's id (its bytes come with `read_shared_file`). */
  id?: string;
  /** Its text, when the chat has it already (an attached text file's). */
  text?: string;
  /** Where it is on the server (shown, never needed to read it). */
  path?: string;
  /** The chat it is in. */
  sessionId?: string;
  /** From a private chat: its text is never written to this device's storage. */
  private?: boolean;
}

/** Text kept with a tile across restarts, at most (more is read again, or opened again from the chat). */
const KEEP_TEXT = 64 << 10;

/** A file tile's file as it may be stored on this device. */
export function storedFile(f: ViewFile): ViewFile {
  const keep = f.text !== undefined && !f.private && f.text.length <= KEEP_TEXT;
  const { text: _text, ...rest } = f;
  return keep ? { ...rest, text: f.text } : rest;
}

/** A stored file tile's file, checked. */
export function viewFileOf(o: Partial<ViewFile>): ViewFile {
  const str = (v: unknown) => (typeof v === "string" ? v : undefined);
  const num = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : undefined);
  const f: ViewFile = { name: str(o.name) ?? "File" };
  const fields: [keyof ViewFile, unknown][] = [
    ["mime", str(o.mime)],
    ["size", num(o.size)],
    ["id", str(o.id)],
    ["text", str(o.text)],
    ["path", str(o.path)],
    ["sessionId", str(o.sessionId)],
    ["private", o.private === true ? true : undefined],
  ];
  for (const [k, v] of fields) if (v !== undefined) (f as unknown as Record<string, unknown>)[k] = v;
  return f;
}
