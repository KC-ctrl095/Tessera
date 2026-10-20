// Lightweight tab state and address labels. Frame decoding and input handlers
// live in browserView.ts and load only when a web-view tile is opened.
import { createStore, produce } from "solid-js/store";
import type { BrowserTab } from "../proto/BrowserTab";
import { backend } from "./backend";

export type { BrowserTab };
/** The live state of each tab this device knows about, by board id. */
export const [tabs, setTabs] = createStore<Record<string, BrowserTab>>({});
export const keepTab = (t: BrowserTab | null | undefined) => {
  if (t && typeof t.board_id === "string") setTabs(t.board_id, { ...t, file_chooser: t.file_chooser, file_multiple: !!t.file_multiple });
  return t ?? null;
};
export const forgetTab = (boardId: string) => setTabs(produce((d) => void delete d[boardId]));
backend.onEvent((e) => { if (e.type === "browser_state") keepTab(e.tab); });

/** An address as typed (https:// added, http:// for local names), or words to search for. */
export function toAddress(raw: string): string {
  const t = raw.trim();
  if (!t) return "";
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(t) || /^(mailto|data|javascript|file|about):/i.test(t)) return t;
  const host = t.split(/[/?#]/)[0].replace(/:\d+$/, "").toLowerCase();
  const near = host === "localhost" || /^\d{1,3}(\.\d{1,3}){3}$/.test(host) || /^\[[0-9a-f:]+\]$/.test(host) || /\.(local|lan|home\.arpa|localhost)$/.test(host);
  const looksLikeHost = near || (/^[^\s]+\.[a-z]{2,}$/i.test(host) && !/\s/.test(t)) || /^[^\s/]+:\d+(\/|$)/.test(t);
  if (!looksLikeHost) return `https://duckduckgo.com/?q=${encodeURIComponent(t)}`;
  return `${near ? "http" : "https"}://${t}`;
}

/** An address as shown in the field: without https:// and a lone trailing slash. */
export function displayAddress(url: string): string {
  try {
    const u = new URL(url);
    if (u.protocol !== "https:" && u.protocol !== "http:") return url;
    const rest = `${u.pathname === "/" ? "" : u.pathname}${u.search}${u.hash}`;
    return (u.protocol === "http:" ? "http://" : "") + u.host + rest;
  } catch {
    return url;
  }
}
