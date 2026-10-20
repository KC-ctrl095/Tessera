// New tiles, at once and without a dialog: a chat, a web view or a note (the
// top bar's New chat button and its menu, a phone tile's "new" menu, the
// palette). A web view starts as an empty browser tile on this device; given
// an address it becomes a `url` board (`create_board`) in that very tile, and
// the server keeps a live browser tab for it (components/board/WebView.tsx).
// A note is made at once (`save_note`) and opens in its editor. A terminal opens
// on the server at once, or asks in the tile where (when a machine allows one
// too); see components/terminal.
import { api, setState, state, upsertSession } from "./store";
import { checkBoardUrl, pageTitle } from "./boards";
import { toAddress } from "./browser";
import { newNote } from "./notes";
import { toastError } from "./ui";
import type { BoardContent } from "../proto/BoardContent";
import { adoptSpawnedBoard, focusComposer, newChat, openPane, paneSession, workspace } from "./workspace";

export type SpawnKind = "chat" | "url" | "note" | "terminal";

export const SPAWN: { kind: SpawnKind; label: string; icon: "chat" | "globe" | "note" | "terminal"; desc: string }[] = [
  { kind: "chat", label: "Chat", icon: "chat", desc: "A new chat" },
  { kind: "url", label: "Web view", icon: "globe", desc: "A browser tile the agent can use too" },
  { kind: "note", label: "Note", icon: "note", desc: "A note you write in Markdown" },
  { kind: "terminal", label: "Terminal", icon: "terminal", desc: "A command line on your server or a machine, for you only" },
];

/** The chat a spawned tile belongs with: the focused one, else a chat in this mosaic. */
function ownerHere(): string | null {
  const f = workspace.focused();
  const own = f ? paneSession(f)?.info : undefined;
  if (own && !own.private && own.kind !== "subagent") return own.id;
  for (const id of workspace.order()) {
    const info = paneSession(id)?.info;
    if (info && !info.private && (info.kind === "chat" || info.kind === "channel")) return info.id;
  }
  return null;
}

/** Spawn a tile beside the focused one. */
export function spawnTile(kind: SpawnKind): string | null {
  if (kind === "chat") {
    const id = newChat({ newPane: true });
    if (id) focusComposer(id);
    return id;
  }
  if (kind === "note") {
    void newNote();
    return null;
  }
  if (kind === "terminal") return openPane({ type: "terminal" });
  return openPane({ type: "draft", kind: "url", sessionId: ownerHere() });
}

/**
 * The chat a board made from a spawned tile goes with (`create_board` needs
 * one): the tile's own, a chat in its mosaic, the latest chat, or a new one.
 */
async function ownerFor(paneId: string): Promise<string> {
  const c = workspace.panes[paneId]?.content;
  const want = c?.type === "draft" ? c.sessionId : null;
  if (want && state.sessions[want]) return want;
  const here = ownerHere();
  if (here) return here;
  const latest = Object.values(state.sessions)
    .map((s) => s.info)
    .filter((i) => i.kind === "chat" && !i.private && !i.archived && !i.internal)
    .sort((a, b) => b.updated_at - a.updated_at)[0];
  if (latest) return latest.id;
  const info = await api.createSession({ title: "Web views" });
  upsertSession(info);
  setState("sessions", info.id, "loaded", true);
  return info.id;
}

/** Tiles waiting for their board: a second "Go" (Enter and the button, a double tap) never makes a second one. */
const filling = new Set<string>();

/** Give a spawned tile its content: it becomes a board, in place. Resolves false when refused. */
export async function fillDraft(paneId: string, title: string, content: BoardContent): Promise<boolean> {
  if (filling.has(paneId)) return false;
  filling.add(paneId);
  try {
    const owner = await ownerFor(paneId);
    const b = await api.createBoard(owner, title, content, true);
    setState("boards", b.id, b);
    adoptSpawnedBoard(paneId, b.id);
    return true;
  } catch (e) {
    toastError(e, "It couldn't be shown");
    return false;
  } finally {
    filling.delete(paneId);
  }
}

/** What a web view's address field was given: an error, or the board's content. */
export function urlContent(raw: string): { title: string; content: BoardContent } | string {
  const u = toAddress(raw);
  if (!u) return "Type an address, or words to search for.";
  const c = checkBoardUrl(u);
  if (!c.ok) return c.reason;
  return { title: pageTitle(c.url.toString()), content: { type: "url", url: c.url.toString() } };
}
