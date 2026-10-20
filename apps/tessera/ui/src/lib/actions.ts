// Higher-level user actions shared by the sidebar, pane headers, slash
// commands and the command palette.
import type { Attachment } from "../proto/Attachment";
import { newChatFreedom } from "./freedom";
import type { ContentPart } from "../proto/ContentPart";
import type { ModelRef } from "../proto/ModelRef";
import type { PermissionMode } from "../proto/PermissionMode";
import type { ThinkingLevel } from "../proto/ThinkingLevel";
import type { Bot } from "../proto/Bot";
import type { Job } from "../proto/Job";
import type { MenuItem } from "../components/primitives";
import { prefs, setPrefs } from "./prefs";
import { api, botById, defaultModel, findModel, sendMessage, setState, state, upsertSession } from "./store";
import { clampThinking, thinkingOptions } from "./thinking";
import { browseDir, confirmDialog, promptDialog, toast, toastError } from "./ui";
import { syncLayoutNow } from "./mosaicSync";
import { closePane, deleteWorkspace, focusComposer, isPrivatePane, newChat, openPane, openSession, setMosaicLocal, setMosaicName, setPaneContent, updateChatDraft, workspace } from "./workspace";

export interface PaneSettings {
  model: ModelRef | null;
  thinking: ThinkingLevel;
  permission: PermissionMode;
  cwd: string;
  /** Fast mode is on (it only applies when the model offers it, see `fastAvailable`). */
  fast: boolean;
  daybreak: boolean;
  /** The agent may start subagents (on unless turned off for this chat). */
  subagents: boolean;
}

/** Effective settings for a pane: the session's, or the pending new-chat draft. */
export function paneSettings(paneId: string): PaneSettings {
  const p = workspace.panes[paneId];
  const c = p?.content;
  if (c?.type === "chat" && c.sessionId) {
    const s = state.sessions[c.sessionId];
    if (s) return { model: s.info.model, thinking: s.info.thinking, permission: s.info.permission, cwd: s.info.cwd, fast: !!s.info.fast, daybreak: !!s.info.daybreak, subagents: s.info.subagents !== false };
  }
  const d = c?.type === "chat" ? c.draft : undefined;
  const bot = botById(d?.botId);
  const model = d?.model ?? bot?.model ?? defaultModel();
  const info = findModel(model);
  // Like the server: the bot's level, else the one last picked for this model.
  const want = d?.thinking ?? bot?.thinking ?? info?.last_thinking ?? info?.default_thinking ?? "auto";
  return {
    model,
    thinking: clampThinking(want, thinkingOptions(info), info?.default_thinking),
    permission: d?.permission ?? bot?.permission ?? newChatFreedom(),
    cwd: d?.cwd ?? bot?.cwd ?? "",
    fast: !!d?.fast,
    daybreak: !!d?.daybreak,
    subagents: d?.subagents !== false,
  };
}

export function paneSessionId(paneId: string): string | null {
  const c = workspace.panes[paneId]?.content;
  return c?.type === "chat" ? c.sessionId : null;
}

export async function patchPane(
  paneId: string,
  patch: { model?: ModelRef; thinking?: ThinkingLevel; permission?: PermissionMode; cwd?: string; fast?: boolean; subagents?: boolean },
) {
  const sid = paneSessionId(paneId);
  if (!sid) {
    // A new chat: keep the chosen thinking level valid for the chosen model
    // (the level last picked for that model wins, as when a chat switches).
    const c = workspace.panes[paneId]?.content;
    const d = c?.type === "chat" ? c.draft : undefined;
    if (patch.model && d?.thinking && !patch.thinking) {
      const info = findModel(patch.model);
      patch = { ...patch, thinking: clampThinking(info?.last_thinking ?? d.thinking, thinkingOptions(info), info?.default_thinking) };
    }
    if (patch.model) {
      const programs = findModel(patch.model)?.cyber_access_programs;
      if (!["standard", "daybreak_blue"].every((p) => programs?.includes(p))) updateChatDraft(paneId, { daybreak: false });
    }
    updateChatDraft(paneId, patch);
    return;
  }
  try {
    upsertSession(await api.updateSession(sid, patch));
  } catch (e) {
    toastError(e, "Could not update chat");
  }
}

/** Create the session for a new-chat pane, apply its draft settings, then send. */
export async function startChat(paneId: string, content: ContentPart[], attachments: Attachment[] = []) {
  const c = workspace.panes[paneId]?.content;
  const draft = c?.type === "chat" ? c.draft ?? {} : {};
  const want = paneSettings(paneId);
  const info = draft.private
    ? await api.createSession({ model: draft.model ?? null, private: true })
    : await api.createSession({ cwd: draft.cwd || null, model: draft.model ?? null, title: null, botId: draft.botId ?? null });
  upsertSession(info);
  setState("sessions", info.id, "loaded", true);
  const patch: { thinking?: ThinkingLevel; permission?: PermissionMode; fast?: boolean; subagents?: boolean } = {};
  // Only send a thinking level the user actually picked; otherwise the
  // server starts where it would (the level last picked for the model, or
  // the model's default).
  if (draft.thinking && want.thinking !== info.thinking) patch.thinking = want.thinking;
  // A private chat has no file tools: its permission mode is moot.
  if (!draft.private && want.permission !== info.permission) patch.permission = want.permission;
  if (draft.fast && !info.fast) patch.fast = true;
  if (draft.daybreak) upsertSession(await api.setSessionDaybreak(info.id, true));
  if (draft.subagents === false && info.subagents !== false) patch.subagents = false;
  if (Object.keys(patch).length) {
    try {
      upsertSession(await api.updateSession(info.id, patch));
    } catch (e) {
      toastError(e);
    }
  }
  setPaneContent(paneId, { type: "chat", sessionId: info.id });
  // The tile goes to the server before the message does (a board the agent
  // shows is put beside its chat's tile, so the server must know it).
  await syncLayoutNow();
  await sendMessage(info.id, content, "auto", attachments);
}

export async function renameSession(id: string, title?: string) {
  const s = state.sessions[id];
  const next =
    title ??
    (await promptDialog({ title: "Rename chat", value: s?.info.title ?? "", placeholder: "Chat title", confirmLabel: "Rename" }));
  if (next == null || !next.trim()) return;
  try {
    upsertSession(await api.updateSession(id, { title: next.trim() }));
  } catch (e) {
    toastError(e, "Rename failed");
  }
}

export async function forkAndOpen(id: string, atSeq?: number) {
  try {
    const info = await api.forkSession(id, atSeq);
    upsertSession(info);
    openSession(info.id, { newPane: true });
  } catch (e) {
    toastError(e, "Couldn't branch the chat");
  }
}

export async function setPinned(id: string, pinned: boolean) {
  try {
    upsertSession(await api.updateSession(id, { pinned }));
  } catch (e) {
    toastError(e);
  }
}

export async function setArchived(id: string, archived: boolean) {
  try {
    upsertSession(await api.updateSession(id, { archived }));
    if (archived) {
      toast({
        level: "info",
        message: "Chat archived",
        action: { label: "Undo", run: () => void setArchived(id, false) },
      });
    }
  } catch (e) {
    toastError(e);
  }
}

export async function deleteSession(id: string): Promise<boolean> {
  const s = state.sessions[id];
  const ok = s?.info.private
    ? await confirmPrivateDelete()
    : await confirmDialog({
        title: "Delete chat?",
        body: `“${s?.info.title || "Untitled chat"}” and its history will be permanently deleted.`,
        confirmLabel: "Delete",
        danger: true,
      });
  if (!ok) return false;
  try {
    await api.deleteSession(id);
    return true;
  } catch (e) {
    toastError(e, "Delete failed");
    return false;
  }
}

const confirmPrivateDelete = () =>
  confirmDialog({
    title: "Delete this private chat?",
    body: "It can't be recovered. Nothing of it was saved anywhere.",
    confirmLabel: "Delete",
    danger: true,
  });

/** A new private chat in a new tile (or in `paneId`). */
export function newPrivateChat(opts: { pane?: string } = {}) {
  const content = { type: "chat" as const, sessionId: null, draft: { private: true } };
  if (opts.pane) {
    setPaneContent(opts.pane, content);
    return opts.pane;
  }
  return openPane(content);
}

/**
 * Close a tile. A private chat is deleted on the server first (after
 * confirming), because closing it is the only way back to it being gone.
 */
export async function requestClosePane(paneId: string | null = workspace.focused()) {
  if (!paneId) return;
  if (!(await leavePrivate(paneId))) return;
  closePane(paneId);
}

/** A fresh chat in this tile, in place of what it showed (a private chat goes, as when closed). */
export async function replaceWithNewChat(paneId: string | null = workspace.focused()) {
  if (!paneId) return;
  if (!(await leavePrivate(paneId))) return;
  newChat({ pane: paneId });
  focusComposer();
}

/** A private chat leaving its tile is deleted (after asking); false when the user keeps it. */
async function leavePrivate(paneId: string): Promise<boolean> {
  if (!isPrivatePane(paneId)) return true;
  const c = workspace.panes[paneId]?.content;
  const sid = c?.type === "chat" ? c.sessionId : null;
  if (!sid || !state.sessions[sid]) return true;
  if (!(await confirmPrivateDelete())) return false;
  try {
    await api.deleteSession(sid);
    return true;
  } catch (e) {
    toastError(e, "Delete failed");
    return false;
  }
}

/** Set (or with "" clear) the goal of a chat. */
export async function setSessionGoal(id: string, goal: string) {
  try {
    upsertSession(await api.setGoal(id, goal.trim()));
    if (!goal.trim()) toast({ level: "info", message: "Goal cleared", timeout: 1600 });
  } catch (e) {
    toastError(e, "Could not set the goal");
  }
}

export async function compactSession(id: string, instructions?: string) {
  try {
    await api.compact(id, instructions);
  } catch (e) {
    toastError(e, "Compaction failed");
  }
}

export async function abortSession(id: string) {
  try {
    await api.abort(id);
  } catch (e) {
    toastError(e, "Could not stop");
  }
}

export async function chooseCwd(paneId: string) {
  const cur = paneSettings(paneId).cwd || state.server?.home || "/";
  const path = await browseDir(cur, "Working directory");
  if (path) await patchPane(paneId, { cwd: path });
}

/** Permission modes as users see them. `auto` is shown as YOLO. */
export const PERMISSIONS: { value: PermissionMode; label: string; short: string; desc: string; icon: "shield" | "shieldOff" | "hand" | "eye" }[] = [
  { value: "default", label: "Default", short: "Asks before risky actions", desc: "Works on its own, but asks before deleting things, running unusual commands or going online", icon: "shield" },
  { value: "ask", label: "Ask me", short: "Asks before every change", desc: "Checks with you before it edits a file or runs a command", icon: "hand" },
  { value: "auto", label: "YOLO", short: "Never asks", desc: "Never asks for approval. Catastrophic commands are still blocked", icon: "shieldOff" },
  { value: "read_only", label: "Read-only", short: "Looks, never touches", desc: "Can read and plan, but can't change anything", icon: "eye" },
];

/** Options for a permission picker; YOLO is styled as a warning. */
export const permissionOptions = () => PERMISSIONS.map((p) => ({ value: p.value, label: p.label, icon: p.icon, warn: p.value === "auto" }));

export const permissionInfo = (m: PermissionMode) => PERMISSIONS.find((p) => p.value === m) ?? PERMISSIONS[0];

/** Parse a user-typed mode ("yolo", "read-only", …). */
export function parsePermission(s: string): PermissionMode | null {
  const v = s.trim().toLowerCase().replace(/[\s-]/g, "_");
  if (v === "yolo") return "auto";
  if (v === "ask_me" || v === "askme") return "ask";
  const p = PERMISSIONS.find((x) => x.value === v || x.label.toLowerCase().replace(/[\s-]/g, "_") === v);
  return p?.value ?? null;
}

/**
 * The first time YOLO is switched on anywhere, explain what it means.
 * Resolves true when it may be applied.
 */
export async function confirmYolo(mode: PermissionMode): Promise<boolean> {
  if (mode !== "auto" || prefs.yoloAck) return true;
  const ok = await confirmDialog({
    title: "Turn on YOLO mode?",
    body:
      "The agent will run commands, edit files and use the network without ever asking for approval. " +
      "Catastrophic commands (like rm -rf /, formatting disks or fork bombs) are still refused. " +
      "Use it for trusted work in a sandbox or a repo you can restore.",
    confirmLabel: "Enable YOLO",
    danger: true,
  });
  if (ok) setPrefs("yoloAck", true);
  return ok;
}

/** Set a tile's permission mode (YOLO asks for confirmation once). */
export async function setPanePermission(paneId: string, mode: PermissionMode) {
  if (!(await confirmYolo(mode))) return;
  await patchPane(paneId, { permission: mode });
  if (mode === "auto") toast({ level: "warning", message: "YOLO on — this chat will not ask before acting.", timeout: 2600 });
}

export async function toggleYolo(paneId: string) {
  const cur = paneSettings(paneId).permission;
  await setPanePermission(paneId, cur === "auto" ? "default" : "auto");
}

// ── bots & channels ──────────────────────────────────────────────────────

export async function saveBot(bot: Bot): Promise<Bot> {
  const saved = await api.saveBot(bot);
  const i = state.bots.findIndex((b) => b.id === saved.id);
  if (i >= 0) setState("bots", i, saved);
  else setState("bots", (l) => [...l, saved]);
  return saved;
}

export async function deleteBot(bot: Bot): Promise<boolean> {
  const ok = await confirmDialog({
    title: `Delete ${bot.name}?`,
    body: `@${bot.handle} and its duties will be removed. Its past chats stay in your history.`,
    confirmLabel: "Delete bot",
    danger: true,
  });
  if (!ok) return false;
  try {
    await api.deleteBot(bot.id);
    setState("bots", (l) => l.filter((b) => b.id !== bot.id));
    return true;
  } catch (e) {
    toastError(e, "Delete failed");
    return false;
  }
}

export async function createChannel(title: string, members: string[], opts: { newPane?: boolean } = {}) {
  const info = await api.createChannel(title, members);
  upsertSession(info);
  setState("sessions", info.id, "loaded", true);
  openSession(info.id, { newPane: opts.newPane ?? true });
  return info;
}

export async function setChannelMembers(sessionId: string, members: string[]) {
  try {
    upsertSession(await api.updateSession(sessionId, { members }));
  } catch (e) {
    toastError(e, "Could not update members");
  }
}

/** Change a tile's model (a new chat keeps its thinking choice valid). */
export async function setPaneModel(paneId: string, model: ModelRef) {
  await patchPane(paneId, { model });
}


/** The tile's model offers fast mode (priority processing). */
export const fastAvailable = (paneId: string) => !!findModel(paneSettings(paneId).model)?.fast;

/** Turn fast mode on or off for a tile's chat (or its new chat). */
export async function setPaneFast(paneId: string, on: boolean) {
  const model = findModel(paneSettings(paneId).model);
  if (on && !model?.fast) {
    toast({ level: "info", message: `${model?.name ?? "This model"} has no fast mode.`, timeout: 2600 });
    return;
  }
  await patchPane(paneId, { fast: on });
  // Claude Code's fast mode is paid from the account's usage credits, outside the plan.
  const claude = state.providers.some((p) => p.id === model?.provider && p.kind === "claude_code");
  const msg = !on ? "Fast mode off." : claude ? "Fast mode on: quicker replies, paid from your Claude usage credits (not your plan)." : "Fast mode on: quicker replies, uses more credits.";
  toast({ level: "info", message: msg, timeout: claude && on ? 4000 : 2200 });
}

/** Give a mosaic a name (empty: back to "Mosaic N"). */
export async function renameMosaic(index: number) {
  const next = await promptDialog({
    title: `Rename Mosaic ${index + 1}`,
    value: workspace.nameOf(index) ?? "",
    placeholder: `Mosaic ${index + 1}`,
    confirmLabel: "Rename",
  });
  if (next !== null) setMosaicName(index, next);
}

/** Keep a mosaic on this device only, or share it with your other devices again (with Undo). */
export function toggleMosaicLocal(index: number) {
  const on = !workspace.localOf(index);
  setMosaicLocal(index, on);
  toast({
    level: "info",
    message: on
      ? `Mosaic ${index + 1} is only on this device now. Your other devices no longer show it.`
      : `Mosaic ${index + 1} is shared with your other devices again.`,
    action: { label: "Undo", run: () => setMosaicLocal(index, !on) },
    timeout: 5000,
  });
}

/** A mosaic's options (strip, overviews): open it, rename it, keep it on this device, delete it. */
export function mosaicMenuItems(i: number, open: () => void): MenuItem[] {
  const local = workspace.localOf(i);
  return [
    { heading: workspace.nameOf(i) ? `${i + 1} · ${workspace.nameOf(i)}` : `Mosaic ${i + 1}` },
    { label: "Open", icon: "mosaic", onSelect: open },
    { label: "Rename…", icon: "pencil", onSelect: () => void renameMosaic(i) },
    {
      label: "Only on this device",
      icon: "lock",
      checked: local,
      description: local ? "Your other devices don't show it" : "Your other devices stop showing it",
      onSelect: () => toggleMosaicLocal(i),
    },
    { separator: true },
    { label: "Delete mosaic…", icon: "trash", danger: true, onSelect: () => void confirmDeleteMosaic(i) },
  ];
}

/** Let the agent start subagents in this chat, or not. */
export async function setPaneSubagents(paneId: string, on: boolean) {
  await patchPane(paneId, { subagents: on });
}

/** Delete a mosaic after confirming. Its tiles close; the chats stay in the sidebar. */
export async function confirmDeleteMosaic(index: number) {
  const n = workspace.orderOf(index).length;
  const ok = await confirmDialog({
    title: `Delete Mosaic ${index + 1}?`,
    body:
      (n > 0
        ? `Its ${n === 1 ? "tile closes" : `${n} tiles close`}. The chats themselves aren't deleted — they stay in the sidebar, and you can open them again any time.`
        : "It's empty, so nothing else changes.") + (workspace.localOf(index) ? " It's only on this device, so your other devices don't change." : ""),
    confirmLabel: "Delete mosaic",
    danger: true,
  });
  if (ok) deleteWorkspace(index);
}

// ── scheduled tasks ──────────────────────────────────────────────────────

export const runJobNow = (j: Job): Promise<boolean> => import("./jobActions").then((m) => m.runJobNow(j));
export const deleteJob = (j: Job): Promise<boolean> => import("./jobActions").then((m) => m.deleteJob(j));
