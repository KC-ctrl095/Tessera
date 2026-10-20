// Settings → Devices: one entry per computer, phone or browser.
//
// A computer often has several logins with the server: its desktop app, its
// terminal (`tessera`) and its machine (`tessera node`). The server works
// out which run on the same computer (`DeviceInfo.computer`, from what each
// program reported, `device_hello`/`node_hello`) and every client shows the
// same groups. This module turns devices and machines into those entries,
// picks each one's name, and reports what this device is.
import type { DeviceClient } from "../proto/DeviceClient";
import type { DeviceInfo } from "../proto/DeviceInfo";
import type { MachineInfo } from "../proto/MachineInfo";
import type { IconName } from "../components/icons";
import { isDesktopApp, native } from "./native";
import { call } from "./store";
import { isTauriMobile } from "./util";

export type { DeviceClient };

/** Rename a device and the others shown on its computer (never a machine's own name). */
export const renameDevice = (id: string, name: string) => call<DeviceInfo[]>({ method: "rename_device", params: { id, name } });

/** What a device is, by what it said, else by its name (older servers and programs). */
export function clientOf(d: DeviceInfo): DeviceClient {
  if (d.client && d.client !== "unknown") return d.client;
  const n = d.name.toLowerCase();
  if (/^terminal on |^terminal \(this machine\)$|\bterminal\b|\bcli\b/.test(n)) return "terminal";
  if (/\bbrowser\b|^(chrome|firefox|edge|safari)\b/.test(n)) return "browser";
  if (/phone|pixel|android|fold|flip|galaxy|iphone|ipad|ios|tablet/.test(n)) return "phone";
  if (/desktop app|\(desktop app\)|^(linux|windows|macos) desktop$/.test(n)) return "app";
  return "unknown";
}

/** How each kind of login reads inside an entry, and in its one-line summary. */
export const ROLE: Record<DeviceClient, { label: string; word: string; icon: IconName }> = {
  app: { label: "Tessera app", word: "Tessera app", icon: "monitor" },
  phone: { label: "Phone app", word: "phone app", icon: "phone" },
  browser: { label: "Browser", word: "browser", icon: "globe" },
  terminal: { label: "Terminal", word: "terminal", icon: "terminal" },
  machine: { label: "Machine sign-in", word: "", icon: "cpu" },
  unknown: { label: "Paired device", word: "paired device", icon: "laptop" },
};
const ORDER: Record<DeviceClient, number> = { app: 0, phone: 1, browser: 2, terminal: 3, unknown: 4, machine: 5 };

/**
 * A name one of Tessera's own programs made up when pairing (not one the
 * user typed): "Terminal on Desktop", "Linux desktop", "Android tablet"…, or
 * the computer's own name.
 */
export function isMadeUpName(name: string, host?: string | null): boolean {
  const n = name.trim().toLowerCase();
  if (host && n === host.trim().toLowerCase()) return true;
  return (
    /^terminal on .+$|^terminal \(this machine\)$|\(desktop app\)$|^desktop app( on .+)?$/.test(n) ||
    /^(linux|windows|macos) (desktop|pc|laptop|computer)$|^desktop$|^device$/.test(n) ||
    /^(chrome|firefox|edge|safari|browser)( on (windows|macos|linux))?$/.test(n) ||
    /^android (phone|tablet|device)$|^ios device$/.test(n)
  );
}

export interface Computer {
  /** Stable while the list lasts: the server's group, else the device's or machine's id. */
  id: string;
  name: string;
  /** Its logins with the server, the app first. */
  devices: DeviceInfo[];
  /** Its machines (usually one; two OS accounts could each run one). */
  machines: MachineInfo[];
  /** This device is one of its logins. */
  here: boolean;
  /** A phone, a lone browser, or a computer. */
  kind: "phone" | "browser" | "computer";
  icon: IconName;
  online: boolean;
  lastSeen: number;
  /** Logins the user may rename from here (a machine's own name only changes on it). */
  renamable: DeviceInfo[];
}

/** The name an entry goes by: one the user chose, else one typed when pairing, else what agents call it. */
export function computerName(devices: DeviceInfo[], machines: MachineInfo[]): string {
  const sorted = [...devices].sort((a, b) => ORDER[clientOf(a)] - ORDER[clientOf(b)]);
  const host = devices.find((d) => d.host)?.host ?? null;
  const people = sorted.filter((d) => clientOf(d) !== "machine");
  return (
    sorted.find((d) => d.named)?.name ??
    people.find((d) => !isMadeUpName(d.name, host))?.name ??
    machines[0]?.name ??
    host ??
    sorted[0]?.name ??
    "Device"
  );
}

/** Devices and machines as one entry per computer, phone or browser: this device first, then online, then latest seen. */
export function computers(devices: DeviceInfo[], machines: MachineInfo[]): Computer[] {
  const groups = new Map<string, { devices: DeviceInfo[]; machines: MachineInfo[] }>();
  const groupOf = new Map<string, string>();
  for (const d of devices) {
    const g = d.computer ?? d.id;
    groupOf.set(d.id, g);
    if (!groups.has(g)) groups.set(g, { devices: [], machines: [] });
    groups.get(g)!.devices.push(d);
  }
  for (const m of machines) {
    const g = groupOf.get(m.id) ?? m.id;
    if (!groups.has(g)) groups.set(g, { devices: [], machines: [] });
    groups.get(g)!.machines.push(m);
  }
  const out: Computer[] = [];
  for (const [id, g] of groups) {
    g.devices.sort((a, b) => ORDER[clientOf(a)] - ORDER[clientOf(b)] || a.created_at - b.created_at);
    const kinds = new Set(g.devices.map(clientOf));
    const kind: Computer["kind"] = g.machines.length || kinds.has("app") || kinds.has("terminal") || kinds.has("machine") ? "computer" : kinds.has("phone") ? "phone" : kinds.has("browser") ? "browser" : "computer";
    const name = computerName(g.devices, g.machines);
    const serverish = /server|srv|nas|vps|pve|proxmox|pi\b|raspberry/i.test(name) || g.machines.some((m) => /server|nas/i.test(m.name));
    const icon: IconName =
      kind === "phone" ? "phone" : kind === "browser" ? "globe" : serverish ? "server" : kinds.has("app") ? "monitor" : kinds.has("terminal") && !g.machines.length ? "terminal" : "laptop";
    out.push({
      id,
      name,
      devices: g.devices,
      machines: g.machines,
      here: g.devices.some((d) => d.current),
      kind,
      icon,
      online: g.devices.some((d) => d.online) || g.machines.some((m) => m.online),
      lastSeen: Math.max(0, ...g.devices.map((d) => d.last_seen ?? 0), ...g.machines.map((m) => m.last_seen ?? 0)),
      renamable: g.devices.filter((d) => clientOf(d) !== "machine"),
    });
  }
  return out.sort((a, b) => Number(b.here) - Number(a.here) || Number(b.online) - Number(a.online) || b.lastSeen - a.lastSeen);
}

/** "Tessera app, terminal and machine for agents": what an entry is used for, plainly. */
export function usedFor(c: Computer): string {
  const words: string[] = [];
  const add = (w: string) => w && !words.includes(w) && words.push(w);
  for (const d of c.devices) {
    const k = clientOf(d);
    // A sign-in that never said what it is adds nothing beside the others.
    add(k === "unknown" ? (c.kind === "phone" ? ROLE.phone.word : "") : ROLE[k].word);
  }
  if (c.machines.length) add("machine for agents");
  if (!words.length) return "Paired device";
  const text = words.length === 1 ? words[0] : `${words.slice(0, -1).join(", ")} and ${words[words.length - 1]}`;
  return text.charAt(0).toUpperCase() + text.slice(1);
}

// ── naming this device ─────────────────────────────────────────────────

/** Samsung's model numbers (SM-F946B…) in the names people know. */
const SAMSUNG: [RegExp, string][] = [
  [/^SM-F9/i, "Galaxy Z Fold"],
  [/^SM-F7/i, "Galaxy Z Flip"],
  [/^SM-[SG]9/i, "Galaxy S"],
  [/^SM-N9/i, "Galaxy Note"],
  [/^SM-A/i, "Galaxy A"],
  [/^SM-M/i, "Galaxy M"],
  [/^SM-[XTP]/i, "Galaxy Tab"],
];

/** A phone's maker and model ("samsung", "SM-F966B") as people say it ("Galaxy Z Fold"). */
export function friendlyModel(maker: string | null | undefined, model: string | null | undefined): string | null {
  const m = (model ?? "").trim();
  const mk = (maker ?? "").trim();
  if (!m || /^k$/i.test(m)) return null;
  for (const [re, name] of SAMSUNG) if (re.test(m)) return name;
  if (/^SM-/i.test(m)) return "Galaxy";
  if (!mk || m.toLowerCase().startsWith(mk.toLowerCase()) || /^(pixel|galaxy|iphone|ipad)/i.test(m)) return m;
  return `${mk.charAt(0).toUpperCase()}${mk.slice(1)} ${m}`;
}

/** The model an Android browser or app shows in its user agent ("SM-F966B"), if any. */
export function uaModel(ua = navigator.userAgent): string | null {
  const m = /Android [\d.]+; ([^;)]+?)(?: Build\/[^;)]*)?[;)]/.exec(ua);
  const model = m?.[1]?.trim();
  return model && !/^(k|wv|mobile)$/i.test(model) ? model : null;
}

/** A computer's own name, unless it is one Windows made up ("DESKTOP-EXAMPLE"). */
export function computerNameFromHost(host: string | null | undefined): string | null {
  const h = (host ?? "").trim();
  if (!h) return null;
  if (/^DESKTOP-[A-Z0-9]{5,}$/i.test(h)) return "Windows PC";
  if (/^LAPTOP-[A-Z0-9]{5,}$/i.test(h)) return "Windows laptop";
  return h;
}

/** What the shell says about this device (`device_report`; the desktop and phone apps). */
interface DeviceReport {
  computer: string | null;
  host: string | null;
  manufacturer: string | null;
  model: string | null;
}
let report: Promise<DeviceReport | null> | null = null;
const deviceReport = () => (report ??= native<DeviceReport>("device_report").catch(() => null));

/**
 * The name to suggest when pairing this device: the phone's model ("Galaxy
 * Z Fold"), the computer's name ("Desktop"), else what kind of device it is.
 * Never guessed from the screen's size (an unfolded foldable is no tablet).
 */
export async function suggestedName(fallback: string): Promise<string> {
  const r = await deviceReport();
  if (isTauriMobile || /Android/i.test(navigator.userAgent)) {
    return friendlyModel(r?.manufacturer, r?.model) ?? friendlyModel(null, uaModel()) ?? fallback;
  }
  if (isDesktopApp) return computerNameFromHost(r?.host) ?? fallback;
  return fallback;
}

/** Tell the server what this device is (once per connection; it never renames anything). This device as the server has it. */
export async function sayHello(): Promise<DeviceInfo | null> {
  const r = await deviceReport();
  const client: DeviceClient = isTauriMobile ? "phone" : isDesktopApp ? "app" : "browser";
  const model = isTauriMobile || /Android/i.test(navigator.userAgent) ? (friendlyModel(r?.manufacturer, r?.model) ?? friendlyModel(null, uaModel())) : null;
  return call<DeviceInfo>({
    method: "device_hello",
    params: { client, ...(r?.computer ? { computer: r.computer } : {}), ...(r?.host ? { host: r.host } : {}), ...(model ? { model } : {}) },
  }).catch(() => null); // an older server doesn't know it
}
