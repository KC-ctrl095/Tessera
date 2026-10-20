// Device-local board IDs only. Private/unknown chats never touch storage.
import { createEffect, createRoot, createSignal } from "solid-js";
import { privateIds, state } from "./store";

type Pages = { bookmarks: string[]; groups: Record<string, string[]> };
type Scope = { key: string; storage: string | null; session: string | null };
const saved = new Map<string, Pages>();
const ephemeral = new Map<string, { pages: Pages; scope: Scope }>();
const [revision, setRevision] = createSignal(0);
const changed = () => setRevision((n) => n + 1);
const server = () => state.conn.serverId ?? state.conn.endpoint;
const scopeOf = (id: string): Scope => {
  const b = state.boards[id];
  const info = b ? state.sessions[b.session_id]?.info : undefined;
  const chat = info?.kind === "subagent" && info.parent_id ? info.parent_id : b?.session_id;
  const session = chat ? state.sessions[chat]?.info : undefined;
  const ordinary = !!session && !session.private && !privateIds.has(chat!);
  const connection = server();
  const storage = ordinary && connection ? `tessera-web-pages:${encodeURIComponent(connection)}` : null;
  return { key: JSON.stringify([connection, ordinary ? "ordinary" : chat ?? id]), storage, session: ordinary ? null : chat ?? null };
};
const ids = (v: unknown, limit: number): string[] => Array.isArray(v) ? [...new Set(v.filter((s): s is string => typeof s === "string" && s.length > 0 && s.length < 100))].slice(-limit) : [];
const empty = (): Pages => ({ bookmarks: [], groups: {} });
const load = (key: string): Pages => {
  try {
    const v = JSON.parse(localStorage.getItem(key) ?? "null");
    if (!v || typeof v !== "object") return empty();
    const groups = v.groups && typeof v.groups === "object" && !Array.isArray(v.groups) ? Object.entries(v.groups).slice(-128) : [];
    return { bookmarks: ids(v.bookmarks, 32), groups: Object.fromEntries(groups.filter(([k]) => k.length < 100).map(([k, a]) => [k, ids(a, 16)])) };
  } catch { return empty(); }
};
const usable = (scope: Scope) => !!scope.storage || (state.conn.state === "connected" && !!scope.session && !!state.sessions[scope.session]);
const pages = (scope: Scope) => {
  revision();
  if (!usable(scope)) return empty();
  if (scope.storage) {
    if (!saved.has(scope.key)) saved.set(scope.key, load(scope.storage));
    return saved.get(scope.key)!;
  }
  if (!ephemeral.has(scope.key)) ephemeral.set(scope.key, { pages: empty(), scope });
  return ephemeral.get(scope.key)!.pages;
};
const persist = (scope: Scope, value: Pages) => {
  if (scope.storage) { try { localStorage.setItem(scope.storage, JSON.stringify(value)); } catch {} }
  changed();
};
const visible = (list: string[], scope: Scope) => list.filter((id) => state.boards[id] && scopeOf(id).key === scope.key);
export const webPageScope = (id: string) => scopeOf(id).key;
export const webBookmarks = (id: string) => { const scope = scopeOf(id); return visible(pages(scope).bookmarks, scope); };
export function bookmarkPage(id: string) {
  const scope = scopeOf(id);
  if (!usable(scope)) return;
  const value = pages(scope);
  value.bookmarks = value.bookmarks.includes(id) ? value.bookmarks.filter((s) => s !== id) : [...value.bookmarks.slice(-31), id];
  persist(scope, value);
}
export const webTabGroup = (id: string) => { const scope = scopeOf(id); return visible(pages(scope).groups[id] ?? [], scope); };
export function rememberTabGroup(list: string[]) {
  const scope = scopeOf(list[0] ?? ""), group = visible(ids(list, 16), scope);
  if (!group.length || !usable(scope)) return;
  const value = pages(scope);
  if (group.every((id) => JSON.stringify(value.groups[id]) === JSON.stringify(group))) return;
  value.groups = Object.fromEntries([...Object.entries(value.groups), ...group.map((id) => [id, group] as const)].slice(-128));
  persist(scope, value);
}
// Legacy preferences had no connection/privacy identity. Discard that metadata,
// not the actual saved boards, instead of attributing it to the wrong chat.
try { localStorage.removeItem("tessera-web-bookmarks"); localStorage.removeItem("tessera-web-tab-groups"); } catch {}
createRoot(() => createEffect(() => {
  revision(); // New ephemeral entries must subscribe to their session's lifetime.
  const connected = state.conn.state === "connected", connection = server();
  for (const [key, { scope }] of ephemeral) {
    if (!connected || scope.key !== JSON.stringify([connection, scope.session]) || (scope.session && !state.sessions[scope.session])) {
      ephemeral.delete(key); changed();
    }
  }
}));
