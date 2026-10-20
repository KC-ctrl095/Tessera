// What a tile shows, for the overview's cards, the all-mosaics page and tile
// previews. Apart from MosaicOverview.tsx so the overview can load on first use.
import type { Bot } from "../proto/Bot";
import type { SessionInfo } from "../proto/SessionInfo";
import { displayAddress, tabs } from "../lib/browser";
import { BOARD_KIND, kindOf } from "../lib/boards";
import { botById, state } from "../lib/store";
import { isPrivatePane, workspace } from "../lib/workspace";
import { sessionLabel } from "./bots";
import type { IconName } from "./icons";

/** What a tile shows, for cards, tabs and previews: title, icon or avatar, a line of preview. */
export function paneCard(id: string): {
  info: SessionInfo | undefined;
  bot: Bot | undefined;
  priv: boolean;
  title: string;
  preview: string;
  icon: IconName;
  /** Not a chat (a board, a note, a spawned tile). */
  shown: boolean;
} {
  const c = workspace.panes[id]?.content;
  const base = { info: undefined, bot: undefined, priv: false, shown: true };
  if (c?.type === "whiteboard") {
    const b = state.boards[c.boardId];
    // A web view says what page it's on.
    const t = tabs[c.boardId];
    if (b && t && (b.content.type === "url" || b.content.type === "local"))
      return { ...base, title: t.title || displayAddress(t.url), preview: displayAddress(t.url), icon: "globe" };
    return { ...base, title: b?.title ?? "Whiteboard", preview: b ? `${BOARD_KIND[kindOf(b)].label}${b.ask ? " · waiting for you" : ""}` : "Deleted", icon: BOARD_KIND[kindOf(b)].icon };
  }
  if (c?.type === "note") {
    const n = state.notes[c.noteId];
    return { ...base, title: n?.title ?? "Note", preview: n ? n.preview || "Empty note" : "", icon: "note" };
  }
  if (c?.type === "file") return { ...base, title: c.file.name, preview: "File", icon: "file" };
  if (c?.type === "terminal") return { ...base, title: "Terminal", preview: c.termId ? "Open" : "Choosing where", icon: "terminal" };
  if (c?.type === "draft") {
    const k = { url: ["New web view", "globe"], assist: ["Empty space", "addTile"] }[c.kind] as [string, IconName];
    return { ...base, title: k[0], preview: c.kind === "assist" ? "Pick what goes here" : "Waiting for an address", icon: k[1] };
  }
  if (c?.type === "other") return { ...base, title: "Tile", preview: "Made by a newer Tessera", icon: "mosaic" };
  const sid = c?.type === "chat" ? c.sessionId : null;
  const info = sid ? state.sessions[sid]?.info : undefined;
  const draftBot = c?.type === "chat" && !sid ? botById(c.draft?.botId) : undefined;
  const bot = info?.kind === "chat" ? botById(info.bot_id) : draftBot;
  const priv = isPrivatePane(id);
  const title = info ? sessionLabel(info) : priv ? "New private chat" : draftBot ? draftBot.name : "New chat";
  const preview = info?.preview || (info ? "" : priv ? "Nothing is saved" : "Empty");
  const icon: IconName = priv ? "veil" : info?.kind === "channel" ? "hash" : "chat";
  return { info, bot, priv, title, preview, icon, shown: false };
}
