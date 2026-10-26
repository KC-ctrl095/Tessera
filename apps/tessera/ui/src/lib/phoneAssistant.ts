// Loaded for an actual assist or its settings, never for an ordinary launch.
import { createSignal } from "solid-js";
import { api, ensureLoaded, state, upsertSession } from "./store";
import { lsGet, lsSet } from "./util";
import { openSession, workspace, workspaceServer } from "./workspace";
import { paneAction } from "./bus";
import { setDrawerOpen, setSettingsTab, setOverviewOpen, setPaletteOpen, toast } from "./ui";
import { setInboxOpen } from "./inboxState";
import { anyModalOpen } from "../components/primitives";
import { closeMosaics } from "./mosaicsPage";

const KEY = "tessera.phone-assistant";
const saved = lsGet<string>(`${KEY}.choice`, "recent");
export const [assistantChoice, choice] = createSignal(/^(recent|continue|bot:[A-Za-z0-9_-]{1,128})$/.test(saved) ? saved : "recent");
export function setAssistantChoice(value: string) { choice(value); lsSet(`${KEY}.choice`, value); }

type Last = { id: string; at: number };
/** `listen`: start the voice conversation (the system's assist gesture). Sent by
 * another app, the chat opens with the microphone one tap away instead. */
export async function openAssistant(listen = true) {
  const server = workspaceServer(), target = assistantChoice();
  if (!server || !state.ready || document.hidden) return;
  const botId = target.startsWith("bot:") ? target.slice(4) : null;
  if (botId && !state.bots.some((b) => b.id === botId)) throw new Error("Choose your assistant bot again in Web & voice settings.");
  const key = `${KEY}.${server}.${target}`;
  const last = lsGet<Last | null>(key, null);
  let id: string | undefined;
  if (last && (target !== "recent" || Date.now() - Math.max(last.at, state.sessions[last.id]?.info.updated_at ?? 0) < 5 * 60_000)) {
    try {
      await ensureLoaded(last.id);
      const info = state.sessions[last.id]?.info;
      if (info && !info.archived && !info.private && info.kind === "chat" && (info.bot_id ?? null) === botId) id = last.id;
    } catch (e) { if ((e as { code?: string }).code !== "not_found") throw e; }
  }
  if (server !== workspaceServer() || !state.ready || document.hidden) return;
  if (!id) {
    // No message, prompt or intent content is sent. The normal server default
    // chooses the model, tools and memory, just like a new chat from the + button.
    let info = await api.createSession({ title: "Assistant", botId });
    if (server !== workspaceServer() || !state.ready) return;
    // Spoken requests can come from the room or another app's audio: a chat the
    // assistant starts asks before risky actions even when new chats are YOLO.
    if (info.permission === "auto") info = await api.updateSession(info.id, { permission: "default" });
    upsertSession(info);
    id = info.id;
  }
  if (document.hidden) return;
  lsSet(key, { id, at: Date.now() });
  setSettingsTab(null); setDrawerOpen(false); setOverviewOpen(false); setPaletteOpen(false); setInboxOpen(false);
  closeMosaics();
  // Never put a live microphone behind an unrelated dialog or menu.
  if (anyModalOpen() || document.querySelector(".popover")) throw new Error("Close the open dialog or menu, then invoke your phone assistant again.");
  const pane = openSession(id);
  if (!pane) return;
  // Composer and its send binding mount before this ordinary pane action.
  queueMicrotask(() => {
    const content = workspace.panes[pane]?.content;
    if (!(state.ready && server === workspaceServer() && !document.hidden && content?.type === "chat" && content.sessionId === id)) return;
    if (listen) paneAction(pane, "voice");
    else toast({ level: "info", message: "Tap the microphone to talk.", timeout: 4000 });
  });
}
