// The bot editor's choices: machine access, colours, personalities and tool
// sets. Apart from bots.tsx (which the sidebar and chats need at once) so they
// load with the editor and the settings that use them.
import type { Bot } from "../../proto/Bot";
import type { MachineAccess } from "../../proto/MachineAccess";
import type { MachineInfo } from "../../proto/MachineInfo";
import type { MachineScope } from "../../proto/MachineScope";
import type { ToolPreset } from "../../proto/ToolPreset";
import { state } from "../../lib/store";

// ── machines (Bot::machines) ─────────────────────────────────────────────

/** The choices per machine, in the editor's words. */
export const ACCESS_CHOICES: { value: MachineAccess; label: string }[] = [
  { value: "no_access", label: "No access" },
  { value: "read_only", label: "Read only" },
  { value: "allowed", label: "As allowed" },
];

/** One line: what a choice means on `machine` (null: machines in general). */
export function accessHelp(level: MachineAccess, machine: string | null): string {
  const on = machine ?? "them";
  if (level === "no_access") return machine ? `Can't use ${machine}.` : "Can't use them.";
  if (level === "read_only") return `Can read files and list folders on ${on}; no commands, changes or web views.`;
  return machine ? `Whatever ${machine} allows.` : "Whatever each machine allows.";
}

/** What a bot may do on machine `id` before the machine's own say (bots made before the setting: as it allows). */
export function botMachineAccess(bot: Bot, id: string): MachineAccess {
  const m = bot.machines;
  if (!m) return "allowed";
  return m.each?.[id] ?? m.default ?? "no_access";
}

/** The machine's own "who can use it" lets this bot in (by id, or by handle as set in its terminal). */
export function machineLetsBot(scope: MachineScope, botId: string | null, handle: string): boolean {
  if (scope.bots == null) return true;
  const h = handle.replace(/^@/, "").toLowerCase();
  return scope.bots.some((x) => (!!botId && x === botId) || (!!h && x.replace(/^@/, "").toLowerCase() === h));
}

/** The bots that may use a machine, by name ("Nova, Forge (read only)"); null when there are no bots. */
export function machineBots(m: MachineInfo): string | null {
  if (!state.bots.length) return null;
  const may = [...state.bots]
    .sort((a, b) => a.name.localeCompare(b.name))
    .filter((b) => machineLetsBot(m.scope, b.id, b.handle))
    .map((b) => ({ b, level: botMachineAccess(b, m.id) }))
    .filter((x) => x.level !== "no_access");
  if (!may.length) return "none";
  return may.map(({ b, level }) => (level === "read_only" ? `${b.name} (read only)` : b.name)).join(", ");
}

// ── editor presets ───────────────────────────────────────────────────────

export const BOT_EMOJIS = [
  "✨", "🔧", "🔭", "🤖", "🧠", "🦊", "🐙", "🦉", "🐝", "🦄",
  "🐧", "🐼", "🐱", "🐶", "🦁", "🐸", "🐢", "🦜", "🐬", "🚀",
  "🛰️", "🔬", "🧪", "📚", "📝", "📅", "📬", "💡", "🎯", "🧭",
  "🛠️", "⚙️", "🛡️", "🎨", "🎵", "🍀", "🌙", "☀️", "⚡", "🔥",
  "🌊", "🧩", "💬", "🕵️",
];

// Mineral pigments that sit well in the mosaic (any hex still works as a custom colour).
export const BOT_COLORS = ["#9d8bd6", "#7d9be0", "#5fa9b3", "#6fb08e", "#d4af5f", "#e07a56", "#c98aa6", "#bdb4a6"];

export const PERSONALITY_PRESETS: { label: string; text: string }[] = [
  { label: "Warm & concise", text: "Warm, friendly and to the point. Gives the short answer first and details on request. Plain language, never lectures." },
  { label: "Witty", text: "Playful and quick-witted with a dry sense of humour, but never at the expense of being useful. Keeps jokes short." },
  { label: "Formal", text: "Polished, precise and courteous. Writes complete sentences, avoids slang, and structures answers clearly." },
  { label: "Blunt engineer", text: "Terse and technical. Leads with the answer or the diff, states trade-offs plainly, skips pleasantries and hedging." },
  { label: "Cheerful", text: "Upbeat and encouraging. Celebrates progress, keeps the energy high and makes chores feel lighter." },
];

export const TOOL_PRESETS: { value: ToolPreset; label: string; desc: string }[] = [
  { value: "full", label: "Full", desc: "Everything, including tools you have connected" },
  { value: "coding", label: "Coding", desc: "Files, shell (also on your machines), search, web, subagents" },
  { value: "research", label: "Research", desc: "Read-only files, web and history" },
  { value: "assistant", label: "Assistant", desc: "Web, notes, memory, scheduling and messaging bots — no shell" },
  { value: "read_only", label: "Read-only", desc: "Read-only files only" },
  { value: "custom", label: "Custom", desc: "Pick exactly which tools" },
];

export const TOOL_NAMES: { name: string; desc: string }[] = [
  { name: "read", desc: "Read files" },
  { name: "write", desc: "Create files" },
  { name: "edit", desc: "Edit files" },
  { name: "grep", desc: "Search file contents" },
  { name: "glob", desc: "Find files" },
  { name: "bash", desc: "Run shell commands" },
  { name: "machines", desc: "Use your other computers (machines)" },
  { name: "desktop", desc: "See and use a computer's screen (desktop control)" },
  { name: "job", desc: "Background processes" },
  { name: "web_fetch", desc: "Fetch web pages" },
  { name: "web_search", desc: "Search the web" },
  { name: "browser", desc: "Use a web browser" },
  { name: "task", desc: "Task ledger" },
  { name: "memory", desc: "Long-term memory" },
  { name: "note", desc: "Read and write notes" },
  { name: "whiteboard", desc: "Show things beside the chat" },
  { name: "skill", desc: "Use skills" },
  { name: "skill_manage", desc: "Create and edit skills" },
  { name: "recall", desc: "Search past chats" },
  { name: "docs", desc: "Read Tessera's own docs" },
  { name: "agent", desc: "Hand parts of a task to helpers" },
  { name: "agents", desc: "Check on helpers" },
  { name: "schedule", desc: "Schedule jobs" },
  { name: "message_bot", desc: "Message other bots" },
];

/** Approximate tool sets (crates/core/src/bots.rs), used to prefill "Custom". The server is authoritative. */
export const PRESET_TOOLS: Record<Exclude<ToolPreset, "custom">, string[]> = {
  full: TOOL_NAMES.map((t) => t.name),
  coding: ["read", "write", "edit", "grep", "glob", "bash", "machines", "desktop", "job", "web_fetch", "web_search", "browser", "task", "memory", "skill", "skill_manage", "recall", "agent", "agents", "message_bot", "whiteboard", "note", "docs"],
  research: ["read", "grep", "glob", "web_fetch", "web_search", "browser", "task", "memory", "skill", "recall", "agent", "agents", "message_bot", "whiteboard", "note", "docs"],
  assistant: ["read", "write", "edit", "grep", "glob", "web_fetch", "web_search", "browser", "task", "memory", "skill", "skill_manage", "recall", "schedule", "message_bot", "whiteboard", "note", "docs"],
  read_only: ["read", "grep", "glob", "recall", "task", "skill", "docs"],
};
