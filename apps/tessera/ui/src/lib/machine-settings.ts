// Machine settings copy stays in the lazy settings/bot panels, not the chat entry.
import type { MachineAsk } from "../proto/MachineAsk";
import type { MachinePreset } from "../proto/MachinePreset";
import type { MachineScope } from "../proto/MachineScope";
import type { IconName } from "../components/icons";
import { fmtLimit, isWindowsOs, machinePlatform, shortPath } from "./machines";

/** What a machine allows, as short plain lines (most important first). `sandboxOk: false`: its commands can't run. */
export function scopeLines(s: MachineScope, sandboxOk = true, home?: string | null, os?: string): { icon: IconName; text: string; warn?: boolean }[] {
  const out: ReturnType<typeof scopeLines> = [];
  const ro = s.folders.filter((f) => !f.write).map((f) => shortPath(f.path, home));
  const rw = s.folders.filter((f) => f.write).map((f) => shortPath(f.path, home));
  if (!s.folders.length) out.push({ icon: "folder", text: "No folders yet" });
  if (rw.length) out.push({ icon: "folder", text: `Read & change ${rw.join(", ")}`, warn: true });
  if (ro.length) out.push({ icon: "folder", text: `Read ${ro.join(", ")}` });
  out.push(
    !s.commands
      ? { icon: "terminal", text: "No commands" }
      : sandboxOk
        ? { icon: "terminal", text: isWindowsOs(os) ? "Can run commands with your account's access" : "Commands can also read other files your account can read, except protected passwords and keys", warn: true }
        : { icon: "terminal", text: "Commands on, but they can't run there (no sandbox)" },
  );
  if (s.desktop)
    out.push({
      icon: "monitor",
      text: isWindowsOs(os) ? "Desktop control: sees the screen, uses the mouse and keyboard" : "Desktop control: startup settings and the desktop session",
      warn: true,
    });
  if (s.webviews) out.push({ icon: "globe", text: "Web views can run here (they reach its local network)", warn: true });
  if (s.terminal && s.terminal !== "off") out.push({ icon: "terminal", text: `Your devices can open a terminal here (${s.terminal === "full" ? "full access" : "sandboxed"})`, warn: true });
  if (s.admin && s.admin !== "off" && s.commands)
    out.push({ icon: "shieldAlert", text: s.admin === "ask" ? "Administrator commands (root): each one asks first" : "Administrator commands (root): asking as each chat's mode says", warn: true });
  out.push({ icon: "shield", text: `Asks first: ${askInfo(s.ask).label.toLowerCase()}`, warn: s.ask === "chat" });
  out.push({ icon: "clock", text: `Up to ${fmtLimit(s.timeout_secs)} per action` });
  return out;
}

// ── presets and "Ask first" (the same words as `tessera node preset`) ────

export type PresetChoice = MachinePreset | "off";

export interface PresetInfo {
  value: PresetChoice;
  label: string;
  /** One plain sentence: what it allows. */
  sentence: string;
  icon: IconName;
  /** Amber: more than files you chose. */
  warn?: boolean;
  /** Red: close to the user's account. */
  danger?: boolean;
}

export const PRESETS: PresetInfo[] = [
  { value: "off", label: "Off", sentence: "Agents can't use this computer.", icon: "circleMinus" },
  { value: "look_only", label: "Look only", sentence: "Agents can read the folders you choose, and change nothing.", icon: "eye" },
  { value: "work_in_folders", label: "Work in folders", sentence: "Agents can read and change the folders you choose, and run commands there. Commands can also read other files your account can read, except protected passwords and keys. They can change only the chosen folders.", icon: "folder" },
  {
    value: "full_access",
    label: "Full access",
    sentence: "Agents can read and change everything in your home folder, run commands and open web views here. Startup files, passwords and keys stay protected.",
    icon: "home",
    warn: true,
  },
  {
    value: "desktop_control",
    label: "Desktop control",
    sentence: "Full access, plus your startup and window manager settings and your desktop session. This is close to giving the agent your account.",
    icon: "monitor",
    danger: true,
  },
];
const ADVANCED: PresetInfo = { value: "custom", label: "Advanced", sentence: "Your own folder rules.", icon: "sliders" };

/** `confined`: the node reports a working command sandbox, so the Windows "no sandbox" words don't apply. */
export const presetInfo = (p: PresetChoice, os?: string, confined = false): PresetInfo => {
  const info = PRESETS.find((x) => x.value === p) ?? ADVANCED;
  const windows = isWindowsOs(os) && (!confined || info.value === "desktop_control") ? WINDOWS_SENTENCES[info.value] : undefined;
  return windows ? { ...info, sentence: windows } : info;
};

/** Offer only the backends reported by this node, using its own OS for old nodes. */
export const presetsFor = (os = "", features?: string[] | null, sandboxOk?: boolean) => {
  const p = machinePlatform(os, features);
  return PRESETS.filter((x) =>
    (x.value !== "desktop_control" || p.desktop) &&
    (x.value !== "full_access" || !p.mac || p.sandbox)).map((x) =>
      x.value === "work_in_folders" && (!p.sandbox || sandboxOk === false)
        ? { ...x, sentence: "Agents can read and change the folders you choose. Commands aren't available with this choice on this computer." }
        : presetInfo(x.value, os, p.sandbox && sandboxOk !== false));
};

/**
 * On Windows (the same words as `preset_sentence_for` in crates/core): no command sandbox
 * there, so commands need Full access and then reach what the account can; desktop control
 * is the screen, the mouse and the keyboard.
 */
const WINDOWS_SENTENCES: Partial<Record<PresetChoice, string>> = {
  work_in_folders: "Agents can read and change the folders you choose. Commands need Full access on Windows (Tessera has no command sandbox there yet).",
  full_access: "Agents can read and change everything in your home folder and run commands here. On Windows commands run without a sandbox: they can reach everything your account can, including passwords and keys. This is close to giving the agent your account.",
  desktop_control: "Full access, plus seeing your screens and using your mouse and keyboard while you're signed in. This is close to giving the agent your account.",
  custom: "Your own folder rules. Commands need Full access on Windows (Tessera has no command sandbox there yet).",
};

export const ASK_CHOICES: { value: MachineAsk; label: string; help: string; warn?: boolean }[] = [
  { value: "always", label: "Every action", help: "Every action here waits for your approval, from any of your devices." },
  { value: "risky", label: "Only risky ones", help: "Asks before risky commands (deleting things, pushing, installing), changes to startup files, and screenshots or any use of the mouse or keyboard on the screen. The rest just runs." },
  { value: "chat", label: "Never", help: "Doesn't ask on its own: each chat's mode decides (YOLO chats don't ask; read-only chats never change anything).", warn: true },
];
export const askInfo = (a: MachineAsk) => ASK_CHOICES.find((x) => x.value === a) ?? ASK_CHOICES[0];
/** How much a choice lets through without asking (higher: less asking). */
export const askRank = (a: MachineAsk) => ASK_CHOICES.findIndex((x) => x.value === a);

