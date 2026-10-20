import { For, Match, Show, Switch, createEffect, createMemo, createSignal, on, onCleanup, onMount, type JSX } from "solid-js";
import type { Bot } from "../proto/Bot";
import type { Job } from "../proto/Job";
import type { SearchHit } from "../proto/SearchHit";
import type { SessionInfo } from "../proto/SessionInfo";
import { deleteBot, deleteJob, deleteSession, forkAndOpen, renameSession, runJobNow, setArchived, setPinned } from "../lib/actions";
import { dragItem } from "../lib/drag";
import { prefs, setPrefs } from "../lib/prefs";
import { api, botById, isBusy, pendingAsks, refreshArchived, state } from "../lib/store";
import {
  openBotEditor,
  openChannelEditor,
  openJobEditor,
  openSettings,
  setArchivedOpen,
  setDrawerOpen,
  setJumpTarget,
  setPaletteOpen,
  toastError,
} from "../lib/ui";
import { lsGet, lsSet, plural, relTime, startOfToday, touchUi } from "../lib/util";
import { BP_TILE, openBot, openSession, paneShowing, usedWorkspaces, workspace, type OpenOpts } from "../lib/workspace";
import { AvatarStack, BotAvatar, botBusy, dmLabel, sessionLabel, botColor } from "./bots";
import { openBotProfile } from "./BotEditor";
import { ConnectionBadge } from "./Connection";
import { Brand, Icon, Logo, Spinner, Tessellate, type IconName } from "./icons";
import { Snippet } from "./Snippet";
import { NoteRow, NotesList, markNotesSeen, noteList, noteMatches, unseenNotes } from "./notes/NotesList";
import { WindowedRows } from "./WindowedRows";
import { longPress, openMenu, type MenuItem } from "./primitives";
import { MOD, WorkspaceStrip } from "./TopBar";
import { NotificationBell } from "./NotificationBell";
import { openMosaics } from "../lib/mosaicsPage";
// Answers agents' requests to look at a whiteboard tile on this screen.
import "../lib/snapshot";
import "../styles/sidebar.css";

// ── tabs ─────────────────────────────────────────────────────────────────
// One section at a time, chosen in a row of tabs under the search box, so a
// long chat history never pushes the rest out of reach. The section shown
// gets the sidebar's whole height. The choice is remembered per device.

export type SidebarTab = "chats" | "bots" | "channels" | "jobs" | "notes";
const TABS: { id: SidebarTab; label: string; icon: IconName }[] = [
  { id: "chats", label: "Chats", icon: "chat" },
  { id: "bots", label: "Bots", icon: "bot" },
  { id: "channels", label: "Channels", icon: "hash" },
  { id: "jobs", label: "Scheduled", icon: "clock" },
  { id: "notes", label: "Notes", icon: "note" },
];
/** Chats and Notes always show; the drawer also keeps Bots reachable before the first bot. */
const ALWAYS: SidebarTab[] = ["chats", "notes"];
const TAB_KEY = "tessera.sidebar.tab";
const isTab = (v: unknown): v is SidebarTab => TABS.some((t) => t.id === v);
const [tab, setTab] = createSignal<SidebarTab>(((v) => (isTab(v) ? v : "chats"))(lsGet<unknown>(TAB_KEY, "chats")));
/** The sidebar's section on screen. */
export const sidebarTab = tab;
/** Show one section of the sidebar (remembered on this device). */
export function showSidebarTab(t: SidebarTab) {
  setTab(t);
  lsSet(TAB_KEY, t);
}

/** Bring the sidebar into view at one section: the drawer on phones and narrow windows, else the sidebar itself. */
export function revealSidebarTab(t: SidebarTab) {
  showSidebarTab(t);
  if (workspace.mode() !== "tile" || window.innerWidth < BP_TILE) setDrawerOpen(true);
  else setPrefs("sidebarCollapsed", false);
}

/** A scheduled job this close counts as due (a dot on the tab). */
const DUE_SOON_MS = 15 * 60_000;
/** Height of a day/month label in the chat list (see sidebar.css). */
const SUB_H = 26;

/**
 * A chat picked here opens in a new tile (a tile already showing it just gets
 * the focus). The folded phone shows one tile at a time: there it takes the
 * place of the one on screen.
 */
const placement = (): OpenOpts => ({ newPane: workspace.mode() !== "mobile" });

const needsYou = (s: SessionInfo) => s.status === "awaiting_approval" || pendingAsks(s.id).length > 0;
const jobRunning = (j: Job) => !!j.last_session && isBusy(state.sessions[j.last_session]?.info);
/** Its last run needs a look: it failed, stopped short, or the server stopped under it. */
const jobFailed = (j: Job) => ["error", "paused", "interrupted"].includes(j.last_status ?? "");

/** The chat list, flattened: day/month labels and rows. */
type ChatItem = { k: "sub"; label: string } | { k: "row"; s: SessionInfo };

/** Where a chat goes by its last activity: Today, Yesterday, This week, then months. */
function dayGroup(ts: number, today: number): string {
  if (ts >= today) return "Today";
  if (ts >= today - 86_400_000) return "Yesterday";
  if (ts >= today - 6 * 86_400_000) return "This week";
  const d = new Date(ts);
  const n = new Date(today);
  if (d.getFullYear() === n.getFullYear() && d.getMonth() === n.getMonth()) return "Earlier this month";
  return d.toLocaleDateString(undefined, d.getFullYear() === n.getFullYear() ? { month: "long" } : { month: "long", year: "numeric" });
}

export function Sidebar(props: { variant: "full" | "drawer" }) {
  const [query, setQuery] = createSignal("");
  const [hits, setHits] = createSignal<SearchHit[] | null>(null);
  const [searching, setSearching] = createSignal(false);
  const [rowH, setRowH] = createSignal(30);
  let searchInput!: HTMLInputElement;
  let scroller!: HTMLDivElement;
  const ids = (t: SidebarTab) => ({ tab: `sb-tab-${props.variant}-${t}`, panel: `sb-panel-${props.variant}` });

  const closeDrawer = () => props.variant === "drawer" && setDrawerOpen(false);
  const q = () => query().trim().toLowerCase();

  const infos = createMemo(() => Object.values(state.sessions).map((s) => s.info));
  const allChats = createMemo(() =>
    infos()
      .filter((i) => i.kind === "chat" && !i.archived && !i.private)
      .sort((a, b) => Number(b.pinned) - Number(a.pinned) || b.updated_at - a.updated_at),
  );
  const chats = createMemo(() => allChats().filter((s) => !q() || sessionLabel(s).toLowerCase().includes(q()) || s.preview.toLowerCase().includes(q())));
  /** Private chats live only in the server's memory; they are listed, never searched. */
  const privates = createMemo(() =>
    q()
      ? []
      : infos()
          .filter((i) => i.private && i.kind === "chat")
          .sort((a, b) => b.updated_at - a.updated_at),
  );
  const allChannels = createMemo(() =>
    infos()
      .filter((i) => i.kind === "channel" && !i.archived)
      // Rooms first, then private bot DMs; newest first within each.
      .sort((a, b) => Number(!!a.bot_id) - Number(!!b.bot_id) || b.updated_at - a.updated_at),
  );
  const channels = createMemo(() => allChannels().filter((i) => !q() || sessionLabel(i).toLowerCase().includes(q())));
  const bots = createMemo(() =>
    state.bots
      .filter((b) => !q() || b.name.toLowerCase().includes(q()) || b.handle.includes(q()) || b.role.toLowerCase().includes(q()))
      .slice()
      .sort((a, b) => a.name.localeCompare(b.name)),
  );
  const jobs = createMemo(() =>
    [...state.jobs]
      .filter((j) => !q() || j.name.toLowerCase().includes(q()) || j.prompt.toLowerCase().includes(q()))
      .sort((a, b) => Number(!a.enabled) - Number(!b.enabled) || (a.next_run ?? Infinity) - (b.next_run ?? Infinity)),
  );
  const notes = createMemo(() => noteList().filter((n) => noteMatches(n, q())));
  /** Channels and Scheduled appear once there is one; the drawer always offers Bots. */
  const has: Record<SidebarTab, () => boolean> = {
    chats: () => true,
    notes: () => true,
    bots: () => state.bots.length > 0,
    channels: () => allChannels().length > 0,
    jobs: () => state.jobs.length > 0,
  };
  const tabs = createMemo(() => TABS.filter((t) => ALWAYS.includes(t.id) || (props.variant === "drawer" && t.id === "bots") || has[t.id]()));
  /** The section on screen, falling back to Chats if its tab has gone. */
  const current = () => (tabs().some((t) => t.id === tab()) ? tab() : "chats");
  // The archived chats' line under the list needs their number: fetched once the list is in, when idle.
  createEffect(() => {
    if (!state.sessionsLoaded || state.archived) return;
    const later = typeof requestIdleCallback === "function" ? (f: () => void) => requestIdleCallback(f, { timeout: 4000 }) : (f: () => void) => setTimeout(f, 1500);
    later(() => void (state.archived ? null : refreshArchived().catch(() => {})));
  });
  const archivedCount = () => (state.archived ?? []).filter((s) => !s.private).length;

  /** Pinned, then by day and month, as one flat list of labels and rows (windowed). */
  // Items keep their identity between updates, so only rows that change re-render.
  const rowItems = new Map<string, ChatItem>();
  const subItems = new Map<string, ChatItem>();
  const chatItems = createMemo(() => {
    const today = startOfToday();
    const out: ChatItem[] = [];
    let last: string | null | undefined;
    const seen = new Set<string>();
    for (const s of chats()) {
      const label = s.pinned ? null : dayGroup(s.updated_at, today);
      if (label !== last && label) {
        if (!subItems.has(label)) subItems.set(label, { k: "sub", label });
        out.push(subItems.get(label)!);
      }
      last = label;
      let it = rowItems.get(s.id);
      if (!it || (it.k === "row" && it.s !== s)) rowItems.set(s.id, (it = { k: "row", s }));
      seen.add(s.id);
      out.push(it);
    }
    for (const id of rowItems.keys()) if (!seen.has(id)) rowItems.delete(id);
    return out;
  });
  const itemHeight = (it: ChatItem) => (it.k === "sub" ? SUB_H : rowH());

  // "Due soon" changes with the clock: one timer for the next job to cross the line, none at idle.
  const [dueTick, setDueTick] = createSignal(0);
  let dueTimer: ReturnType<typeof setTimeout> | undefined;
  createEffect(() => {
    void dueTick();
    const now = Date.now();
    const next = state.jobs
      .filter((j) => j.enabled && j.next_run != null)
      .map((j) => j.next_run! - DUE_SOON_MS)
      .filter((t) => t > now)
      .reduce((m, t) => Math.min(m, t), Infinity);
    clearTimeout(dueTimer);
    if (next !== Infinity) dueTimer = setTimeout(() => setDueTick((n) => n + 1), Math.min(next - now + 50, 2 ** 31 - 1));
  });
  onCleanup(() => clearTimeout(dueTimer));

  // ── what each tab says about itself ──
  const status = createMemo(() => {
    void dueTick();
    const now = Date.now();
    const chatInfos = infos().filter((i) => i.kind === "chat" && !i.archived);
    const chanInfos = allChannels();
    return {
      chats: { need: chatInfos.filter(needsYou).length, busy: chatInfos.some((i) => isBusy(i) && !i.internal) },
      bots: { need: 0, busy: state.bots.some((b) => botBusy(b.id)) },
      channels: { need: chanInfos.filter(needsYou).length, busy: chanInfos.some((i) => isBusy(i)) },
      jobs: {
        need: 0,
        busy: state.jobs.some(jobRunning),
        failed: state.jobs.some((j) => j.enabled && jobFailed(j)),
        due: state.jobs.some((j) => j.enabled && j.next_run != null && j.next_run - now < DUE_SOON_MS),
      },
      notes: { need: 0, busy: false, fresh: unseenNotes() },
    } as Record<SidebarTab, { need: number; busy: boolean; failed?: boolean; due?: boolean; fresh?: number }>;
  });
  /** Matches per tab while searching. */
  const matches = (t: SidebarTab) =>
    t === "chats" ? chats().length : t === "bots" ? bots().length : t === "channels" ? channels().length : t === "jobs" ? jobs().length : notes().length;

  // The notes list on screen: its news is seen.
  createEffect(() => {
    if (current() === "notes" && !q()) markNotesSeen();
  });
  // A fresh tab starts at the top.
  createEffect(
    on(tab, () => {
      if (scroller && !q()) scroller.scrollTop = 0;
    }, { defer: true }),
  );
  // Row height follows density and touch (the CSS variable).
  const readRowH = () => {
    const v = scroller ? parseFloat(getComputedStyle(scroller).getPropertyValue("--row-h")) : NaN;
    if (Number.isFinite(v) && v > 0) setRowH(v);
  };
  onMount(readRowH);
  createEffect(on(() => prefs.density, () => requestAnimationFrame(readRowH), { defer: true }));

  // Server-side full-text search (debounced).
  let timer: ReturnType<typeof setTimeout> | undefined;
  let searchGen = 0;
  createEffect(
    on(query, (v) => {
      clearTimeout(timer);
      const needle = v.trim();
      if (needle.length < 2) {
        setHits(null);
        setSearching(false);
        return;
      }
      setSearching(true);
      const gen = ++searchGen;
      timer = setTimeout(async () => {
        try {
          const r = await api.search(needle, 30);
          if (gen === searchGen) setHits(r);
        } catch (e) {
          if (gen === searchGen) setHits([]);
          console.warn(e);
        } finally {
          if (gen === searchGen) setSearching(false);
        }
      }, 260);
    }),
  );
  onCleanup(() => clearTimeout(timer));

  const pickTab = (t: SidebarTab) => {
    showSidebarTab(t);
    // Searching shows every section: jump to this one.
    if (q()) requestAnimationFrame(() => scroller?.querySelector(`[data-group="${t}"]`)?.scrollIntoView({ block: "start", behavior: "smooth" }));
  };
  const onTabKey = (e: KeyboardEvent) => {
    const list = tabs();
    const i = list.findIndex((t) => t.id === current());
    const to = e.key === "ArrowRight" ? i + 1 : e.key === "ArrowLeft" ? i - 1 : e.key === "Home" ? 0 : e.key === "End" ? list.length - 1 : null;
    if (to === null) return;
    e.preventDefault();
    const t = list[(to + list.length) % list.length].id;
    pickTab(t);
    document.getElementById(ids(t).tab)?.focus();
  };

  const open = (id: string) => {
    openSession(id, placement());
    closeDrawer();
  };
  /**
   * The ⋯ menu's other way to open: into the selected tile (as a tap did
   * before), or on the folded phone, where a tap already does that, a new tile.
   */
  const otherPlace = (go: (opts: OpenOpts) => unknown): MenuItem =>
    workspace.mode() === "mobile"
      ? { label: "Open in a new tile", icon: "addTile", onSelect: () => (go({ newPane: true }), closeDrawer()) }
      : { label: "Open in selected tile", icon: "swap", onSelect: () => (go({}), closeDrawer()) };

  const openHit = (h: SearchHit) => {
    const pane = openSession(h.session_id, placement());
    if (pane) setJumpTarget({ paneId: pane, sessionId: h.session_id, seq: h.seq });
    closeDrawer();
  };

  const focused = () => {
    const f = workspace.focused();
    return f ? workspace.panes[f]?.content : undefined;
  };
  const focusedSession = () => {
    const c = focused();
    return c?.type === "chat" ? c.sessionId : null;
  };
  const focusedNote = () => {
    const c = focused();
    return c?.type === "note" ? c.noteId : null;
  };

  const menuFor = (s: SessionInfo): MenuItem[] =>
    s.private
      ? [
          otherPlace((o) => openSession(s.id, o)),
          { separator: true },
          { label: "Delete for good…", icon: "trash", danger: true, onSelect: () => void deleteSession(s.id) },
        ]
      : [
          otherPlace((o) => openSession(s.id, o)),
          { separator: true },
          { label: "Rename", icon: "pencil", onSelect: () => renameSession(s.id) },
          { label: s.pinned ? "Unpin" : "Pin to top", icon: "pin", onSelect: () => setPinned(s.id, !s.pinned) },
          ...(s.kind === "chat" ? [{ label: "Branch into a new chat", icon: "fork" as const, onSelect: () => forkAndOpen(s.id) }] : []),
          { label: s.archived ? "Unarchive" : "Archive", icon: "archive", onSelect: () => setArchived(s.id, !s.archived) },
          { separator: true },
          { label: "Delete…", icon: "trash", danger: true, onSelect: () => void deleteSession(s.id) },
        ];

  const botMenu = (b: Bot): MenuItem[] => [
    { label: `Chat with ${b.name}`, icon: "chat", onSelect: () => (openBot(b.id, placement()), closeDrawer()) },
    { label: "Profile", icon: "user", onSelect: () => (openBotProfile(b.id), closeDrawer()) },
    otherPlace((o) => openBot(b.id, o)),
    { label: "Start a fresh chat", icon: "compose", onSelect: () => (openBot(b.id, { newPane: true, fresh: true }), closeDrawer()) },
    { label: `New channel with ${b.name}`, icon: "hash", onSelect: () => openChannelEditor([b.id]) },
    { separator: true },
    { label: "Edit bot…", icon: "pencil", onSelect: () => openBotEditor(b.id) },
    { label: "Delete bot…", icon: "trash", danger: true, onSelect: () => void deleteBot(b) },
  ];

  const SessionRow = (p: { s: SessionInfo }) => {
    const lp = longPress((x, y) => openMenu({ x, y }, menuFor(p.s)));
    const bot = () => (p.s.kind === "chat" || p.s.kind === "job" ? botById(p.s.bot_id) : undefined);
    const dm = () => p.s.kind === "channel" && !!p.s.bot_id;
    const room = () => p.s.kind === "channel" && !p.s.bot_id;
    const busy = () => isBusy(p.s);
    const needs = () => needsYou(p.s);
    /** Forks and "(rewound)" copies: which chat this one branched from. */
    const branchedFrom = () => {
      if (p.s.kind !== "chat" || !p.s.parent_id) return null;
      const parent = state.sessions[p.s.parent_id]?.info ?? state.archived?.find((a) => a.id === p.s.parent_id);
      return parent ? `Branched from “${sessionLabel(parent)}”` : "Branched from a chat that's no longer here";
    };
    return (
      <li
        class="srow"
        classList={{ active: focusedSession() === p.s.id, shown: !!paneShowing(p.s.id), dm: dm(), private: p.s.private }}
        onContextMenu={(e) => {
          e.preventDefault();
          openMenu({ x: e.clientX, y: e.clientY }, menuFor(p.s));
        }}
        {...lp}
        onPointerDown={(e) => {
          lp.onPointerDown(e);
          dragItem(e, { kind: "session", id: p.s.id }, sessionLabel(p.s));
        }}
      >
        <button
          type="button"
          class="srow-main"
          onClick={() => open(p.s.id)}
          onAuxClick={(e) => e.button === 1 && open(p.s.id)}
          aria-label={dm() ? `${dmLabel(p.s)} (private, read-only)` : sessionLabel(p.s)}
          title={dm() ? "Private conversation between bots — read-only" : [branchedFrom(), p.s.preview || sessionLabel(p.s)].filter(Boolean).join("\n")}
        >
          <span class="srow-lead">
            <Show
              when={bot()}
              fallback={
                <Show when={room()} fallback={<Icon name={p.s.private ? "veil" : dm() ? "lock" : branchedFrom() ? "fork" : "chat"} size={14} />}>
                  <Icon name="hash" size={14} />
                </Show>
              }
            >
              <BotAvatar bot={bot()} size={18} />
            </Show>
          </span>
          <span class="srow-title">{dm() ? dmLabel(p.s) : sessionLabel(p.s)}</span>
          <Show when={room() && !busy()}>
            <AvatarStack ids={p.s.members} size={15} max={3} />
          </Show>
          <Show when={p.s.pinned && !busy() && !needs()}>
            <Icon name="pin" size={12} class="srow-pin" aria-label="Pinned" />
          </Show>
          <Show when={needs()}>
            <span class="sq sq-approval" role="img" aria-label="Needs you" title={pendingAsks(p.s.id).length ? "Waiting for your answer on a whiteboard" : "Needs your approval"} />
          </Show>
          <Show when={busy() && !needs()}>
            <Tessellate size={8} label="Working" />
          </Show>
          <Show when={p.s.status === "error"}>
            <span class="sq sq-error" role="img" aria-label="Error" />
          </Show>
        </button>
        <button
          type="button"
          class="icon-btn sm srow-more"
          aria-label={`Options for ${sessionLabel(p.s)}`}
          aria-haspopup="menu"
          onClick={(e) => openMenu(e.currentTarget, menuFor(p.s), "bottom-end")}
        >
          <Icon name="more" size={15} />
        </button>
      </li>
    );
  };

  const BotRow = (p: { b: Bot }) => {
    const lp = longPress((x, y) => openMenu({ x, y }, botMenu(p.b)));
    const busy = () => botBusy(p.b.id);
    const showing = () => {
      const f = focusedSession();
      const info = f ? state.sessions[f]?.info : undefined;
      return info?.kind === "chat" && info.bot_id === p.b.id;
    };
    return (
      <li
        class="srow brow"
        classList={{ active: showing() }}
        style={{ "--bot": botColor(p.b.color) }}
        onContextMenu={(e) => {
          e.preventDefault();
          openMenu({ x: e.clientX, y: e.clientY }, botMenu(p.b));
        }}
        {...lp}
        onPointerDown={(e) => {
          lp.onPointerDown(e);
          dragItem(e, { kind: "bot", id: p.b.id }, p.b.name);
        }}
      >
        <button
          type="button"
          class="srow-main"
          onClick={() => {
            openBot(p.b.id, placement());
            closeDrawer();
          }}
          onAuxClick={(e) => {
            if (e.button !== 1) return;
            openBot(p.b.id, placement());
            closeDrawer();
          }}
          title={`${p.b.name} — ${p.b.role}`}
        >
          <span class="brow-av">
            <BotAvatar bot={p.b} size={20} />
          </span>
          <span class="srow-title">{p.b.name}</span>
          <Show when={p.b.permission === "auto"}>
            <Icon name="shieldOff" size={12} class="brow-yolo" aria-label="YOLO: never asks" />
          </Show>
          <Show when={busy()} fallback={<span class="brow-role">{p.b.role}</span>}>
            <Tessellate size={8} label="Working" />
          </Show>
        </button>
        <button type="button" class="icon-btn sm srow-more" aria-label={`Options for ${p.b.name}`} aria-haspopup="menu" onClick={(e) => openMenu(e.currentTarget, botMenu(p.b), "bottom-end")}>
          <Icon name="more" size={15} />
        </button>
      </li>
    );
  };

  /** A scheduled task's other actions: its last run, run it now, pause it, delete it. */
  const jobMenu = (j: Job): MenuItem[] => [
    { label: "Edit…", icon: "pencil", onSelect: () => (openJobEditor(j), closeDrawer()) },
    ...(j.last_session ? [{ label: "Open the last run", icon: "chat" as const, onSelect: () => (openSession(j.last_session!, placement()), closeDrawer()) }] : []),
    { label: "Run now", icon: "play", onSelect: () => void runJobNow(j) },
    { label: j.enabled ? "Pause" : "Resume", icon: j.enabled ? "pause" : "play", onSelect: () => void api.saveJob({ ...j, enabled: !j.enabled }).catch((e) => toastError(e)) },
    { separator: true },
    { label: "Delete…", icon: "trash", danger: true, onSelect: () => void deleteJob(j) },
  ];

  const JobRow = (p: { j: Job }) => {
    const lp = longPress((x, y) => openMenu({ x, y }, jobMenu(p.j)));
    return (
      <li
        class="srow"
        classList={{ paused: !p.j.enabled }}
        onContextMenu={(e) => {
          e.preventDefault();
          openMenu({ x: e.clientX, y: e.clientY }, jobMenu(p.j));
        }}
        {...lp}
      >
        <button
          type="button"
          class="srow-main"
          onClick={() => {
            openJobEditor(p.j);
            closeDrawer();
          }}
          title="Edit this scheduled task"
        >
          <span class="srow-lead">
            <Show when={botById(p.j.bot_id)} fallback={<Icon name="clock" size={14} />}>
              <BotAvatar bot={botById(p.j.bot_id)} size={18} />
            </Show>
          </span>
          <span class="srow-title">{p.j.name}</span>
          <Show when={jobFailed(p.j)}>
            <span class="srow-err" role="img" aria-label={`Last run ${p.j.last_status === "error" ? "failed" : p.j.last_status}`} title={`Last run ${p.j.last_status === "error" ? "failed" : p.j.last_status}`} />
          </Show>
          <Show when={jobRunning(p.j)} fallback={<span class="srow-meta">{!p.j.enabled ? (p.j.ended ? "ended" : "paused") : p.j.next_run ? relTime(p.j.next_run) : ""}</span>}>
            <Tessellate size={8} label="Running" />
          </Show>
        </button>
        <button type="button" class="icon-btn sm srow-more" aria-label={`Options for ${p.j.name}`} aria-haspopup="menu" onClick={(e) => openMenu(e.currentTarget, jobMenu(p.j), "bottom-end")}>
          <Icon name="more" size={15} />
        </button>
      </li>
    );
  };

  /** The line above a section: what is in it, and a drawer's direct way to add a bot. */
  const PanelHead = (p: { text: JSX.Element; action?: JSX.Element }) => (
    <div class="sb-panel-head" classList={{ "with-action": !!p.action }}>
      <span class="sb-panel-count">{p.text}</span>
      {p.action}
    </div>
  );

  /** A group of search results. */
  const Group = (p: { id: SidebarTab | "messages"; label: string; count?: number; children: JSX.Element }) => (
    <section class="sb-sec sb-group" data-group={p.id}>
      <div class="sb-sec-head">
        <span class="sb-sec-toggle static">
          <span>{p.label}</span>
          <Show when={p.count}>
            <span class="count">{p.count}</span>
          </Show>
        </span>
      </div>
      <div class="sb-sec-body">{p.children}</div>
    </section>
  );

  const nextJob = createMemo(() => state.jobs.filter((j) => j.enabled && j.next_run).sort((a, b) => a.next_run! - b.next_run!)[0]);

  const ChatsPanel = () => (
    <>
      <Show when={allChats().length || privates().length}>
        <PanelHead text={plural(allChats().length, "chat", "chats")} />
      </Show>
      <Show when={privates().length}>
        <div class="sb-sub private">Private · this device only, in memory</div>
        <ul class="srows">
          <For each={privates()}>{(s) => <SessionRow s={s} />}</For>
        </ul>
      </Show>
      <Show
        when={allChats().length}
        fallback={
          <Show when={!privates().length}>
            <p class="sb-empty sb-first">
              No chats yet. Your chats will show up here. Start one with <Icon name="compose" size={13} /> {props.variant === "drawer" && workspace.mode() === "mobile" ? "New" : "New chat"} at the top right.
            </p>
          </Show>
        }
      >
        <WindowedRows items={chatItems()} scroller={() => scroller} height={itemHeight}>
          {(it) =>
            it.k === "sub" ? (
              <li class="sb-sub vsub" role="presentation">
                {it.label}
              </li>
            ) : (
              <SessionRow s={it.s} />
            )
          }
        </WindowedRows>
      </Show>
      <Show when={archivedCount()}>
        <button
          type="button"
          class="sb-archived"
          onClick={() => {
            setArchivedOpen(true);
            closeDrawer();
          }}
        >
          <Icon name="archive" size={13} />
          <span>Archived chats · {archivedCount()}</span>
        </button>
      </Show>
    </>
  );

  const BotsPanel = () => (
    <>
      <PanelHead
        text={plural(state.bots.length, "bot", "bots")}
        action={props.variant === "drawer" ? (
          <button
            type="button"
            class="btn btn-sm sb-panel-add"
            aria-label="Add new bot"
            onClick={() => {
              closeDrawer();
              openBotEditor();
            }}
          >
            <Icon name="plus" size={14} /> Add new bot
          </button>
        ) : undefined}
      />
      <ul class="srows">
        <For each={bots()}>{(b) => <BotRow b={b} />}</For>
      </ul>
    </>
  );

  const ChannelsPanel = () => (
    <>
      <PanelHead text={plural(allChannels().length, "channel", "channels")} />
      <ul class="srows">
        <For each={channels()}>{(s) => <SessionRow s={s} />}</For>
      </ul>
    </>
  );

  const JobsPanel = () => (
    <>
      <PanelHead
        text={
          <>
            {plural(state.jobs.length, "scheduled task", "scheduled tasks")}
            <Show when={nextJob()}>{(j) => <span class="sb-panel-sub"> · next {relTime(j().next_run!)}</span>}</Show>
          </>
        }
      />
      <ul class="srows">
        <For each={jobs()}>{(j) => <JobRow j={j} />}</For>
      </ul>
    </>
  );

  const NotesPanel = () => (
    <>
      <PanelHead text={plural(noteList().length, "note", "notes")} />
      <NotesList onPick={closeDrawer} focusedNote={focusedNote()} />
    </>
  );

  /** Searching: every section's matches, then messages. */
  const Results = () => (
    <>
      <Show when={chats().length}>
        <Group id="chats" label="Chats" count={chats().length}>
          <ul class="srows">
            <For each={chats()}>{(s) => <SessionRow s={s} />}</For>
          </ul>
        </Group>
      </Show>
      <Show when={bots().length}>
        <Group id="bots" label="Bots" count={bots().length}>
          <ul class="srows">
            <For each={bots()}>{(b) => <BotRow b={b} />}</For>
          </ul>
        </Group>
      </Show>
      <Show when={channels().length}>
        <Group id="channels" label="Channels" count={channels().length}>
          <ul class="srows">
            <For each={channels()}>{(s) => <SessionRow s={s} />}</For>
          </ul>
        </Group>
      </Show>
      <Show when={jobs().length}>
        <Group id="jobs" label="Scheduled" count={jobs().length}>
          <ul class="srows">
            <For each={jobs()}>{(j) => <JobRow j={j} />}</For>
          </ul>
        </Group>
      </Show>
      <Show when={notes().length}>
        <Group id="notes" label="Notes" count={notes().length}>
          <ul class="srows">
            <For each={notes()}>{(n) => <NoteRow note={n} onPick={closeDrawer} active={focusedNote() === n.id} query={q()} />}</For>
          </ul>
        </Group>
      </Show>
      <Show when={hits()}>
        <Group id="messages" label="Messages">
          <Show when={!hits()!.length && !searching()}>
            <p class="sb-empty">No messages match “{query()}”.</p>
          </Show>
          <ul class="hits">
            <For each={hits()!}>
              {(h) => (
                <li>
                  <button type="button" class="hit" onClick={() => openHit(h)}>
                    <span class="hit-title">{h.session_title || "Untitled chat"}</span>
                    <span class="hit-snippet">
                      <Snippet text={h.snippet} query={q()} />
                    </span>
                  </button>
                </li>
              )}
            </For>
          </ul>
        </Group>
      </Show>
      <Show when={!hits() && !searching() && !tabs().some((t) => matches(t.id))}>
        <p class="sb-empty">Nothing matches “{query()}”.</p>
      </Show>
    </>
  );

  return (
    <nav class="sidebar" classList={{ drawer: props.variant === "drawer" }} aria-label="Sidebar">
      <div class="sb-top">
        <Brand size={22} />
        <span class="spacer" />
        <Show when={props.variant === "full"}>
          <button type="button" class="icon-btn" aria-label="Hide sidebar" data-tip="Hide sidebar" data-kbd={`${MOD} B`} onClick={() => setPrefs("sidebarCollapsed", true)}>
            <Icon name="sidebar" size={17} />
          </button>
        </Show>
        <Show when={props.variant === "drawer"}>
          <button type="button" class="icon-btn" aria-label="Close sidebar" onClick={() => setDrawerOpen(false)}>
            <Icon name="x" />
          </button>
        </Show>
      </div>

      <label class="sb-search">
        <Icon name="search" size={15} />
        <input
          ref={searchInput}
          type="search"
          placeholder="Search"
          value={query()}
          aria-label="Search chats, bots, notes and messages"
          onInput={(e) => setQuery(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              setQuery("");
              searchInput.blur();
            }
          }}
        />
        <Show when={searching()}>
          <Spinner size={13} />
        </Show>
        <Show when={!searching() && !query() && !touchUi()}>
          <button type="button" class="sb-kbd" aria-label="Command palette" data-tip="Command palette" onClick={() => setPaletteOpen(true)}>
            <kbd>{MOD}</kbd>
            <kbd>K</kbd>
          </button>
        </Show>
        <Show when={query()}>
          <button type="button" class="icon-btn sm" aria-label="Clear search" onClick={() => setQuery("")}>
            <Icon name="x" size={13} />
          </button>
        </Show>
      </label>

      {/* Phones: the mosaics, once there are two (a new one is made with New → More → Mosaic). */}
      <Show when={props.variant === "drawer" && workspace.mode() !== "tile" && usedWorkspaces().length > 1}>
        <div class="sb-ws">
          <span class="sb-ws-label">Mosaics</span>
          <WorkspaceStrip onPick={closeDrawer} always add={false} />
          <button
            type="button"
            class="sb-ws-all"
            onClick={() => {
              closeDrawer();
              openMosaics();
            }}
          >
            <Icon name="workspaces" size={14} /> All
          </button>
        </div>
      </Show>

      <div class="sb-tabs" role="tablist" aria-label="Sidebar sections" classList={{ searching: !!q() }} onKeyDown={onTabKey}>
        <For each={tabs()}>
          {(t) => {
            const on = () => current() === t.id;
            const s = () => status()[t.id];
            const n = () => (q() ? matches(t.id) : 0);
            const hint = () => {
              const x = s();
              const bits: string[] = [];
              if (q()) bits.push(plural(n(), "match", "matches"));
              if (x.need) bits.push(`${x.need} need${x.need === 1 ? "s" : ""} you`);
              if (x.busy) bits.push(t.id === "jobs" ? "running" : "working");
              if (x.failed) bits.push("a run failed");
              if (x.due) bits.push("one is due soon");
              if (x.fresh) bits.push(`${x.fresh} new from agents`);
              return bits.length ? `${t.label} · ${bits.join(", ")}` : t.label;
            };
            return (
              <button
                type="button"
                role="tab"
                id={ids(t.id).tab}
                class="sb-tab"
                classList={{ on: on(), empty: !!q() && !n() }}
                aria-selected={on()}
                aria-controls={ids(t.id).panel}
                aria-label={hint()}
                data-tip={on() ? undefined : hint()}
                tabIndex={on() ? 0 : -1}
                onClick={() => pickTab(t.id)}
              >
                <span class="sb-tab-icon">
                  <Icon name={t.icon} size={15} />
                  <Show when={s().need}>
                    <span class="sb-tab-badge need">{s().need}</span>
                  </Show>
                  <Show when={!s().need && s().fresh}>
                    <span class="sb-tab-badge fresh">{s().fresh}</span>
                  </Show>
                  <Show when={!s().need && !s().fresh && (s().failed || s().due)}>
                    <span class="sb-tab-dot" classList={{ failed: !!s().failed }} />
                  </Show>
                </span>
                <span class="sb-tab-label">{t.label}</span>
                <Show when={q()}>
                  <span class="sb-tab-n">{n()}</span>
                </Show>
                <Show when={!q() && s().busy}>
                  <Tessellate size={6} class="sb-tab-busy" />
                </Show>
              </button>
            );
          }}
        </For>
      </div>

      <div ref={scroller} class="sb-scroll" role="tabpanel" id={ids(current()).panel} aria-labelledby={ids(current()).tab}>
        <Show
          when={state.sessionsLoaded}
          fallback={
            <div class="sb-loading" aria-busy="true">
              <div class="sk" />
              <div class="sk w70" />
              <div class="sk w80" />
              <div class="sk w60" />
            </div>
          }
        >
          <Show when={!q()} fallback={<Results />}>
            <Switch>
              <Match when={current() === "chats"}>
                <ChatsPanel />
              </Match>
              <Match when={current() === "bots"}>
                <BotsPanel />
              </Match>
              <Match when={current() === "channels"}>
                <ChannelsPanel />
              </Match>
              <Match when={current() === "jobs"}>
                <JobsPanel />
              </Match>
              <Match when={current() === "notes"}>
                <NotesPanel />
              </Match>
            </Switch>
          </Show>
        </Show>
      </div>

      <div class="sb-foot">
        <ConnectionBadge />
        {/* A folded phone has no top bar: the bell is here, beside Settings. */}
        <Show when={workspace.mode() === "mobile"}>
          <NotificationBell class="icon-btn" />
        </Show>
        <button
          type="button"
          class="icon-btn"
          aria-label="Settings"
          data-tip="Settings"
          data-kbd={`${MOD} ,`}
          onClick={() => {
            openSettings();
            closeDrawer();
          }}
        >
          <Icon name="gear" />
        </button>
      </div>
    </nav>
  );
}

/** Narrow icon rail shown when the desktop sidebar is collapsed. */
export function Rail() {
  return (
    <nav class="rail" aria-label="Sidebar (collapsed)">
      <button type="button" class="rail-logo" aria-label="Show sidebar" data-tip="Show sidebar" data-kbd={`${MOD} B`} onClick={() => setPrefs("sidebarCollapsed", false)}>
        <Logo size={22} />
      </button>
      <button type="button" class="icon-btn" aria-label="Search" data-tip="Search" data-kbd={`${MOD} K`} onClick={() => setPaletteOpen(true)}>
        <Icon name="search" />
      </button>
      <span class="rail-sep" />
      <For each={state.bots}>
        {(b) => (
          <button
            type="button"
            class="icon-btn rail-bot"
            aria-label={`Chat with ${b.name}`}
            data-tip={b.name}
            onPointerDown={(e) => dragItem(e, { kind: "bot", id: b.id }, b.name)}
            onClick={() => openBot(b.id, placement())}
          >
            <BotAvatar bot={b} size={24} />
            <Show when={botBusy(b.id)}>
              <Tessellate size={7} class="rail-busy" />
            </Show>
          </button>
        )}
      </For>
      <span class="spacer" />
      <ConnectionBadge compact />
      <button type="button" class="icon-btn" aria-label="Settings" data-tip="Settings" onClick={() => openSettings()}>
        <Icon name="gear" />
      </button>
    </nav>
  );
}
