// Whiteboards: what an agent shows the user in a tile of its own. Their
// content is untrusted: HTML only ever runs in a sandboxed frame on another
// origin (the app's board protocol, or the server's board page for the web
// UI), web pages are framed without access to the app, and nothing from a
// frame is ever read or trusted by the app (there is no message listener).
// Asks (a signature, a choice…) are answered through the app's own UI.
import type { AskAnswer } from "../proto/AskAnswer";
import type { BoardContent } from "../proto/BoardContent";
import type { Whiteboard } from "../proto/Whiteboard";
import type { IconName } from "../components/icons";
import { backend, webAccess } from "./backend";
import { boardFrameUrl, boardLocalUrl, pinFrames } from "./native";
import { resolvedTheme } from "./prefs";
import { createStore, produce } from "solid-js/store";
import { api, botById, call, setState, state } from "./store";
import { captureTile } from "./snapshot";
import { confirmDialog, toast, toastError } from "./ui";
import { isTauri, openExternal } from "./util";
import { askServerToShowBeside, focusComposer, focusPane, getDraft, openBoard, openSession, paneShowing, paneShowingBoard, setDraft, stageAttachments, workspace } from "./workspace";

export type BoardKind = BoardContent["type"];

export const BOARD_KIND: Record<BoardKind, { label: string; icon: IconName }> = {
  html: { label: "Visual", icon: "cube" },
  markdown: { label: "Notes", icon: "file" },
  image: { label: "Image", icon: "image" },
  url: { label: "Web page", icon: "globe" },
  local: { label: "Web app", icon: "appWindow" },
};

export const kindOf = (b: Whiteboard | undefined): BoardKind => b?.content.type ?? "html";

/** "you", "Nova", "a subagent" or "the agent": who put this board up. */
export function shownBy(b: Whiteboard): string {
  if (b.by_user) return "you";
  const info = state.sessions[b.session_id]?.info;
  if (info?.kind === "subagent") {
    const parent = info.parent_id ? state.sessions[info.parent_id]?.info : undefined;
    const bot = botById(parent?.bot_id);
    return bot ? `${bot.name}'s subagent` : "a subagent";
  }
  const bot = info?.kind === "chat" ? botById(info.bot_id) : undefined;
  return bot?.name ?? "the agent";
}

/** The chat a board belongs with (subagents' boards go with their parent). */
export function boardChat(b: Whiteboard): string {
  const info = state.sessions[b.session_id]?.info;
  return info?.kind === "subagent" && info.parent_id ? info.parent_id : b.session_id;
}

const withQuery = (url: string, params: Record<string, string>) => {
  const u = new URL(url);
  for (const [k, v] of Object.entries(params)) u.searchParams.set(k, v);
  return u.toString();
};

/**
 * Where an HTML board's frame loads from: the app's separate board origin
 * (Tauri), or the server's board page (web UI, same server). Never srcdoc:
 * that would inherit the app's origin and policy.
 */
export async function htmlFrameSrc(b: Whiteboard): Promise<string> {
  const theme = resolvedTheme();
  if (isTauri) {
    const url = await boardFrameUrl(b.id, b.version);
    if (!url) throw new Error("This version of the app can't show visual whiteboards yet. Update Tessera to see it.");
    return theme === "light" ? withQuery(url, { theme: "light" }) : url;
  }
  // Board pages come only from the server's own computer, unencrypted:
  // through a remote address they'd leave Tessera's encryption.
  if (webAccess) throw new Error(WEB_NO_FRAMES);
  const { path } = await api.boardLink(b.id);
  const base = backend.sameOrigin ? location.origin : state.conn.endpoint ?? location.origin;
  // The version keeps a stale copy from being shown after an update.
  const url = withQuery(new URL(path, base).toString(), { theme, v: String(b.version) });
  pinFrames(url);
  return url;
}

/** Why a web browser at `/web/` shows no visual whiteboards or HTML previews. */
export const WEB_NO_FRAMES = "Visual whiteboards and HTML previews show in the Tessera app, not in a web browser.";

/** Where a `local` board's web app loads from: this device's proxy (app) or the server's machine (web UI). */
export async function localFrameSrc(b: Whiteboard, external = false): Promise<string> {
  if (b.content.type !== "local") throw new Error("not a web app");
  if (webAccess) throw new Error(WEB_NO_FRAMES);
  if (isTauri) {
    const url = await boardLocalUrl(b.id, external);
    if (!url) throw new Error("This version of the app can't show web apps from the server yet. Update Tessera to see it.");
    return url; // keep its query: the token authorises the proxy
  }
  return `http://127.0.0.1:${b.content.port}${b.content.path.startsWith("/") ? b.content.path : "/" + b.content.path}`;
}

/** Origins a web page board must never be: the app itself and the server it talks to. */
function ownOrigins(): Set<string> {
  const out = new Set<string>([location.origin]);
  const add = (u: string | null | undefined) => {
    try {
      if (u) out.add(new URL(u).origin);
    } catch {
      /* not a URL */
    }
  };
  add(state.conn.endpoint);
  for (const s of state.config?.servers ?? []) for (const e of s.endpoints) add(e);
  return out;
}

export type UrlCheck = { ok: true; url: URL } | { ok: false; reason: string };

/** Whether a `url` board may be framed at all (before asking the site). */
export function checkBoardUrl(raw: string): UrlCheck {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return { ok: false, reason: "That isn't a web address." };
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return { ok: false, reason: `Only web pages (http or https) can be shown here, not ${url.protocol.replace(/:$/, "")} links.` };
  if (ownOrigins().has(url.origin)) return { ok: false, reason: "This address is Tessera itself, so it isn't shown inside a whiteboard." };
  return { ok: true, url };
}

/** Whether a board has something to open in the browser (web pages and web apps). */
export function hasExternal(b: Whiteboard): boolean {
  return (b.content.type === "url" && checkBoardUrl(b.content.url).ok) || b.content.type === "local";
}

/** "Open in browser": the page, or the web app (through this device's proxy in the app). */
export async function openBoardExternally(b: Whiteboard): Promise<void> {
  try {
    if (b.content.type === "url" && checkBoardUrl(b.content.url).ok) return void (await openExternal(b.content.url));
    if (b.content.type === "local") return void (await openExternal(await localFrameSrc(b, true)));
  } catch (e) {
    toastError(e, "Could not open it");
  }
}

/** A short label for an answer, shown once the ask collapses. */
export function answerLabel(b: Whiteboard, a: AskAnswer): string {
  const input = b.ask?.input;
  switch (a.type) {
    case "signature":
      return "Signed";
    case "cancel":
      return "Declined";
    case "confirm":
      // A web view handed over (`needs_user`): done there, or not.
      if (b.content.type === "url" || b.content.type === "local") return a.yes ? "Done" : "Couldn't do it";
      return a.yes ? "Answered yes" : "Answered no";
    case "choice":
      return input?.type === "choice" ? `Chose “${input.options[a.index] ?? "?"}”` : "Answered";
    case "choices":
      return input?.type === "choice" ? `Chose ${a.indexes.map((i) => `“${input.options[i] ?? "?"}”`).join(", ")}` : "Answered";
    case "text":
      return "Answered";
  }
}

/** Answer a board's ask; resolves true when the server took it. */
export async function answerAsk(b: Whiteboard, a: AskAnswer): Promise<boolean> {
  const ask = b.ask;
  if (!ask) return false;
  // Noted first: the board's update (the ask cleared) may arrive before the reply.
  setState("answered", ask.id, { board: b.id, session: boardChat(b), prompt: ask.prompt, label: answerLabel(b, a) });
  const forget = () => setState("answered", produce((d) => void delete d[ask.id]));
  try {
    await api.answerBoard(b.id, ask.id, a);
    // A moment of feedback; the ask's own card in the chat keeps the outcome.
    setTimeout(forget, 6000);
    return true;
  } catch (e) {
    forget();
    toastError(e, a.type === "signature" ? "The signature wasn't sent" : "The answer wasn't sent");
    return false;
  }
}

export async function confirmDeleteBoard(b: Whiteboard): Promise<boolean> {
  const ok = await confirmDialog({
    title: "Delete this whiteboard?",
    body: `“${b.title}” is removed for good, on every device.${b.ask ? " The question on it is cancelled." : ""} The chat stays.`,
    confirmLabel: "Delete whiteboard",
    danger: true,
  });
  if (!ok) return false;
  try {
    await api.deleteBoard(b.id);
    return true;
  } catch (e) {
    toastError(e, "Could not delete the whiteboard");
    return false;
  }
}

/** The board a `whiteboard` tool call showed: from its result, its arguments, or the chat's boards by title. */
export function boardOfCall(sessionId: string, args: unknown, details: unknown): { id: string; title?: string; kind?: string } | null {
  const d = details as { board?: { id?: unknown; title?: unknown; kind?: unknown }; board_id?: unknown; id?: unknown } | undefined;
  const fromDetails = d?.board && typeof d.board.id === "string" ? d.board : typeof d?.board_id === "string" ? { id: d.board_id } : null;
  if (fromDetails && typeof fromDetails.id === "string")
    return { id: fromDetails.id, title: typeof (fromDetails as { title?: unknown }).title === "string" ? (fromDetails as { title: string }).title : undefined, kind: typeof (fromDetails as { kind?: unknown }).kind === "string" ? (fromDetails as { kind: string }).kind : undefined };
  const a = (typeof args === "string" ? safeJson(args) : args) as { id?: unknown; title?: unknown } | undefined;
  if (a && typeof a.id === "string") return { id: a.id };
  if (a && typeof a.title === "string") {
    const hit = Object.values(state.boards).find((b) => b.session_id === sessionId && b.title === a.title);
    if (hit) return { id: hit.id, title: hit.title };
  }
  return null;
}

function safeJson(s: string): unknown {
  try {
    return JSON.parse(s);
  } catch {
    return undefined;
  }
}

export const isBoardTool = (name: string) => /^(whiteboard|board|show_board)$/i.test(name);

/** What an answered `whiteboard` ask's result says, in a few words. */
export function askOutcome(result: string): string {
  if (/declined/i.test(result)) return "Declined";
  if (/signed/i.test(result)) return "Signed";
  const chose = /chose:\s*(.+)/i.exec(result);
  if (chose) return `Chose “${chose[1].trim()}”`;
  if (/said yes/i.test(result)) return "Answered yes";
  if (/said no/i.test(result)) return "Answered no";
  if (/^no answer/i.test(result)) return "No answer";
  return "Answered";
}

// ── boards the user puts up ──────────────────────────────────────────────

/**
 * What the user typed as an address: a bare host gets a scheme, like a
 * browser's address bar (http for this network — localhost, IP addresses,
 * .local/.lan names — https elsewhere).
 */
export function normalizeUrl(raw: string): string {
  const t = raw.trim();
  if (!t || /^[a-z][a-z0-9+.-]*:\/\//i.test(t) || /^(mailto|data|javascript|file|about):/i.test(t)) return t;
  const host = t.split(/[/?#]/)[0].replace(/:\d+$/, "").toLowerCase();
  const near = host === "localhost" || /^\d{1,3}(\.\d{1,3}){3}$/.test(host) || /^\[[0-9a-f:]+\]$/.test(host) || /\.(local|lan|home\.arpa|localhost)$/.test(host);
  return `${near ? "http" : "https"}://${t}`;
}

/** A short name for a web page board: its host and path, without the scheme. */
export function pageTitle(raw: string): string {
  try {
    const u = new URL(raw);
    const path = u.pathname === "/" ? "" : u.pathname.replace(/\/$/, "");
    return (u.host.replace(/^www\./, "") + path).slice(0, 80);
  } catch {
    return "Web page";
  }
}

/**
 * Show something beside a chat yourself (a web page, an image, notes): the
 * server puts its tile next to the chat's, and the agent can look at it.
 * Resolves with the board, or null when it was refused.
 */
export async function showBesideChat(sessionId: string, title: string, content: BoardContent): Promise<Whiteboard | null> {
  let b: Whiteboard;
  // Before asking: the server's tile can arrive before its reply does.
  askServerToShowBeside(sessionId);
  try {
    b = await api.createBoard(sessionId, title.trim(), content);
  } catch (e) {
    toastError(e, "It couldn't be shown");
    return null;
  }
  setState("boards", b.id, b);
  // The server adds the tile to the shared layout (it usually arrives with the
  // reply); when it didn't, or the chat's tile isn't shared, open one here.
  const reveal = (tries: number) => {
    const tile = paneShowingBoard(b.id);
    if (tile) {
      // One tile at a time on phones: go to it, or it would appear off screen.
      if (workspace.mode() === "mobile") focusPane(tile);
      return;
    }
    if (tries > 0) setTimeout(() => reveal(tries - 1), 300);
    else openBoard(b.id);
  };
  setTimeout(() => reveal(4), 60);
  return b;
}

/**
 * "Watch" on one of the agent's browser steps: its background tab becomes a
 * web view of yours (the page as it is; the agent carries on in it), opened
 * where you are: beside the chat in this mosaic, else in a new tile here.
 * A tab that is gone (closed, or the server restarted) opens its last
 * address instead.
 */
export async function watchTab(sessionId: string, tab: string, fallback?: { url: string; title?: string }): Promise<string | null> {
  let b: Whiteboard;
  try {
    b = await call<Whiteboard>({ method: "browser_watch", params: { session_id: sessionId, tab } });
  } catch (e) {
    if (fallback?.url) {
      const shown = await showBesideChat(sessionId, fallback.title || pageTitle(fallback.url), { type: "url", url: fallback.url });
      return shown?.id ?? null;
    }
    toastError(e, "That page can't be shown");
    return null;
  }
  setState("boards", b.id, b);
  setWatched(tab, b.id);
  const pane = openBoard(b.id);
  if (pane && workspace.mode() === "mobile") focusPane(pane);
  return b.id;
}

/** Background tabs watched from this device (tab → its web view's board). */
const [watched, setWatched] = createStore<Record<string, string>>({});
export const watchedBoard = (tab: string): string | undefined => watched[tab];

/** The reference "Ask about this" puts into the chat's composer. */
export const boardReference = (b: Whiteboard) => `About the “${b.title}” whiteboard (${b.id}): `;

// ── what the user sees of a board (snapshots) ────────────────────────────

/**
 * "Ask about this": a picture of the board as you see it (where the device
 * can take one) and a short reference to it, in its chat's message box.
 */
export async function askAboutBoard(b: Whiteboard, boardPane?: string | null) {
  // The picture first: on phones going to the chat moves the board off screen.
  // A web view already has one (its last frame); anything else asks the device.
  const shot = boardPane ? (webViewShot(boardPane) ?? (await captureTile(boardPane))) : null;
  const chat = boardChat(b);
  const pane = paneShowing(chat) ?? openSession(chat, { newPane: true });
  if (!pane) return;
  const ref = boardReference(b);
  const cur = getDraft(pane);
  if (!cur.includes(ref)) setDraft(pane, cur.trim() ? `${cur.replace(/\s+$/, "")}\n${ref}` : ref);
  if (shot) {
    const name = `${b.title.replace(/[^\w .-]+/g, "").trim() || "whiteboard"}.${shot.mime === "image/jpeg" ? "jpg" : shot.mime === "image/webp" ? "webp" : "png"}`;
    stageAttachments(pane, [
      {
        id: Math.random().toString(36).slice(2),
        kind: "image",
        name,
        mime: shot.mime,
        data: shot.data,
        preview: `data:${shot.mime};base64,${shot.data}`,
        width: shot.width,
        height: shot.height,
        size: Math.round((shot.data.length * 3) / 4),
      },
    ]);
  }
  focusPane(pane);
  focusComposer(pane);
  if (workspace.mode() === "mobile") toast({ level: "info", message: shot ? "Added with a picture of it to the chat's message box." : "Added to the chat's message box.", timeout: 1800 });
}

/** The page a web view tile shows, as it is on screen (its canvas). */
function webViewShot(paneId: string): { mime: string; data: string; width: number; height: number } | null {
  const c = document.querySelector<HTMLCanvasElement>(`[data-pane-id="${paneId}"] .wv-canvas.shown`);
  if (!c || !c.width || !c.height) return null;
  try {
    const url = c.toDataURL("image/jpeg", 0.85);
    return { mime: "image/jpeg", data: url.slice(url.indexOf(",") + 1), width: c.width, height: c.height };
  } catch {
    return null;
  }
}
