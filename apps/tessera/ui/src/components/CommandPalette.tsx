import { For, Show, createEffect, createMemo, createSignal, on, onCleanup } from "solid-js";
import "../styles/palette.css";
import type { SearchHit } from "../proto/SearchHit";
import type { Bot } from "../proto/Bot";
import { compactSession, confirmDeleteMosaic, fastAvailable, toggleMosaicLocal, forkAndOpen, newPrivateChat, paneSessionId, paneSettings, patchPane, requestClosePane, setPaneFast, toggleYolo } from "../lib/actions";
import { paneAction } from "../lib/bus";
import { prefs, resolvedTheme, setPrefs } from "../lib/prefs";
import { canRewind, lastUserSeq, rewindLast } from "../lib/rewind";
import { api, findModel, state } from "../lib/store";
import { shortLabel } from "../lib/thinking";
import { openBotEditor, openChannelEditor, openJobEditor, openSettings, setArchivedOpen, setCheatsheetOpen, setJumpTarget, setPaletteOpen } from "../lib/ui";
import { SPAWN, spawnTile } from "../lib/spawn";
import { relTime, touchUi } from "../lib/util";
import { openMosaics } from "../lib/mosaicsPage";
import {
  LAYOUTS,
  cycleLayout,
  firstFreeWorkspace,
  moveToWorkspace,
  newChat,
  newWorkspace,
  openBot,
  openSession,
  setLayoutKind,
  splitPane,
  switchWorkspace,
  toggleMaximize,
  usedWorkspaces,
  workspace,
} from "../lib/workspace";
import { BotAvatar, sessionLabel } from "./bots";
import { Icon, Spinner, type IconName } from "./icons";
import { Modal } from "./primitives";
import { ALT, LAYOUT_ICON, MOD } from "./TopBar";
import { revealSidebarTab } from "./Sidebar";

interface Cmd {
  id: string;
  label: string;
  icon: IconName;
  bot?: Bot;
  hint?: string;
  sub?: string;
  run: () => void;
  /** Ctrl/Cmd+Enter: open in a new tile. */
  runAlt?: () => void;
}

function score(q: string, s: string): number {
  if (!q) return 1;
  const t = s.toLowerCase();
  const i = t.indexOf(q);
  if (i === 0) return 3;
  if (i > 0) return 2;
  // subsequence match
  let j = 0;
  for (const c of t) if (c === q[j]) j++;
  return j === q.length ? 1 : 0;
}

export function CommandPalette() {
  const [q, setQ] = createSignal("");
  const [sel, setSel] = createSignal(0);
  const [hits, setHits] = createSignal<SearchHit[]>([]);
  const [searching, setSearching] = createSignal(false);
  let list!: HTMLDivElement;
  const close = () => setPaletteOpen(false);
  const touch = touchUi();
  const focusedSid = () => {
    const f = workspace.focused();
    return f ? paneSessionId(f) : null;
  };

  const actions = (): Cmd[] => {
    const sid = focusedSid();
    const f = workspace.focused();
    const a: Cmd[] = [
      { id: "new", label: "New chat", icon: "compose", hint: `${MOD} N`, run: () => newChat({ newPane: true }) },
      { id: "private", label: "New private chat", icon: "veil", sub: "Nothing is saved", run: () => newPrivateChat() },
      { id: "split-r", label: "Split right", icon: "splitRight", run: () => splitPane("right") },
      { id: "split-d", label: "Split down", icon: "splitDown", run: () => splitPane("bottom") },
      { id: "close", label: "Close tile", icon: "x", hint: `${ALT} W`, run: () => void requestClosePane() },
      { id: "max", label: workspace.maximized() ? "Restore layout" : "Maximize tile", icon: "maximize", hint: `${ALT} F`, run: () => toggleMaximize() },
      { id: "layout-next", label: "Next layout", icon: "layoutDwindle", hint: `${ALT} Space`, run: () => cycleLayout() },
      ...(touchUi() ? [] : [{ id: "keys", label: "Keyboard shortcuts", icon: "keyboard" as const, hint: `${ALT} /`, run: () => setCheatsheetOpen(true) }]),
      { id: "bot-new", label: "New bot…", icon: "bot", run: () => openBotEditor() },
      { id: "channel-new", label: "New channel…", icon: "hash", run: () => openChannelEditor() },
      { id: "job-new", label: "New scheduled task…", icon: "clock", run: () => openJobEditor() },
      { id: "switcher", label: "All mosaics", icon: "workspaces", hint: touchUi() ? undefined : `${MOD} Tab`, run: () => openMosaics() },
      {
        id: "ws-local",
        label: workspace.localOf(workspace.active()) ? "Share this mosaic with my other devices" : "Keep this mosaic only on this device",
        icon: "lock",
        sub: workspace.localOf(workspace.active()) ? "Your other devices show it again" : "Your other devices stop showing it",
        run: () => toggleMosaicLocal(workspace.active()),
      },
      ...LAYOUTS.map((l): Cmd => ({ id: `layout-${l.id}`, label: `Layout: ${l.label}`, icon: LAYOUT_ICON[l.id], sub: l.desc, run: () => setLayoutKind(l.id) })),
      ...usedWorkspaces()
        .filter((i) => i !== workspace.active())
        .map(
          (i): Cmd => ({
            id: `ws-${i}`,
            label: `Go to Mosaic ${i + 1}`,
            icon: "mosaic",
            hint: `${ALT} ${i + 1}`,
            sub: `${workspace.orderOf(i).length} tiles`,
            run: () => switchWorkspace(i),
          }),
        ),
      ...(usedWorkspaces().length > 1
        ? [
            {
              id: "ws-del",
              label: `Delete this mosaic (Mosaic ${workspace.active() + 1})`,
              icon: "trash" as const,
              sub: "Its tiles close; the chats stay",
              run: () => void confirmDeleteMosaic(workspace.active()),
            },
          ]
        : []),
      ...(firstFreeWorkspace() >= 0
        ? [
            { id: "ws-new", label: "New mosaic", icon: "mosaic" as const, hint: `${ALT} ${firstFreeWorkspace() + 1}`, run: () => void newWorkspace() },
            {
              id: "mv-new",
              label: "Move tile to a new mosaic",
              icon: "swap" as const,
              hint: `${ALT} ⇧ ${firstFreeWorkspace() + 1}`,
              run: () => moveToWorkspace(workspace.focused(), firstFreeWorkspace()),
            },
          ]
        : []),
      ...usedWorkspaces()
        .filter((i) => i !== workspace.active())
        .map(
          (i): Cmd => ({
            id: `mv-${i}`,
            label: `Move tile to Mosaic ${i + 1}`,
            icon: "swap",
            hint: `${ALT} ⇧ ${i + 1}`,
            run: () => moveToWorkspace(workspace.focused(), i),
          }),
        ),
      { id: "model", label: "Change model…", icon: "sparkles", run: () => f && paneAction(f, "model") },
      { id: "perm", label: "Change permissions…", icon: "shield", run: () => f && paneAction(f, "permission") },
      {
        id: "yolo",
        label: f && paneSettings(f).permission === "auto" ? "Turn YOLO off for this chat" : "Turn YOLO on for this chat",
        icon: "shieldOff",
        run: () => f && void toggleYolo(f),
      },
      { id: "think", label: "Change thinking…", icon: "brain", run: () => f && paneAction(f, "thinking") },
      ...(f
        ? (findModel(paneSettings(f).model)?.thinking ?? []).map(
            (o): Cmd => ({
              id: `think-${o.level}`,
              label: `Thinking: ${shortLabel(o.label)}`,
              icon: "brain",
              sub: o.hint,
              run: () => void patchPane(f, { thinking: o.level }),
            }),
          )
        : []),
      {
        id: "theme",
        label: resolvedTheme() === "dark" ? "Switch to light theme" : "Switch to dark theme",
        icon: resolvedTheme() === "dark" ? "sun" : "moon",
        run: () => setPrefs("theme", resolvedTheme() === "dark" ? "light" : "dark"),
      },
      { id: "sidebar", label: prefs.sidebarCollapsed ? "Show sidebar" : "Hide sidebar", icon: "sidebar", hint: `${MOD} B`, run: () => setPrefs("sidebarCollapsed", !prefs.sidebarCollapsed) },
      { id: "settings", label: "Settings", icon: "gear", hint: `${MOD} ,`, run: () => openSettings() },
      { id: "bots", label: "Your bots", icon: "bot", run: () => (state.bots.length ? revealSidebarTab("bots") : openBotEditor()) },
      { id: "providers", label: "Models", icon: "sparkles", run: () => openSettings("models") },
      { id: "voice", label: state.server?.voice ? "Voice typing settings" : "Set up voice typing", icon: "mic", run: () => openSettings("voice") },
      { id: "secrets", label: "Passwords & keys", icon: "key", run: () => openSettings("secrets") },
      { id: "security", label: "Privacy & safety", icon: "shieldCheck", run: () => openSettings("safety") },
      { id: "commands", label: "What commands can reach (sandbox, folders, terminal)", icon: "terminal", run: () => openSettings("commands") },
      { id: "archived", label: "Archived chats", icon: "archive", run: () => setArchivedOpen(true) },
      { id: "search", label: "Web & voice", icon: "globe", run: () => openSettings("web") },
      { id: "jobs", label: "Scheduled tasks", icon: "clock", run: () => (state.jobs.length ? revealSidebarTab("jobs") : openJobEditor()) },
      { id: "pair", label: "Pair a new device", icon: "phone", run: () => openSettings("devices") },
      { id: "machines", label: "Computers agents can use", icon: "laptop", run: () => openSettings("devices") },
    ];
    if (sid && f) {
      a.splice(9, 0,
        { id: "fork", label: "Branch into a new chat (fork)", icon: "fork", run: () => forkAndOpen(sid) },
        ...(canRewind(state.sessions[sid]?.info) && (lastUserSeq(sid) !== null || state.sessions[sid]?.hasOlderHistory)
          ? [{ id: "rewind", label: "Rewind to before your last message", icon: "rewind" as const, run: () => rewindLast(sid, f) }]
          : []),
        { id: "compact", label: "Summarize the chat to make room (compact)", icon: "compress", run: () => compactSession(sid) },
        { id: "rename", label: "Rename chat", icon: "pencil", run: () => paneAction(f, "rename") },
      );
    }
    // "+": new tiles of every kind, at once.
    for (const k of SPAWN.slice(1)) a.push({ id: `spawn-${k.kind}`, label: `New ${k.label.toLowerCase()} tile`, icon: k.icon, sub: k.desc, run: () => void spawnTile(k.kind) });
    if (f && fastAvailable(f)) {
      const on = paneSettings(f).fast;
      a.push({ id: "fast", label: on ? "Turn fast mode off" : "Turn fast mode on", icon: "fast", sub: "Quicker replies, uses more credits", run: () => void setPaneFast(f, !on) });
    }
    return a;
  };

  const items = createMemo(() => {
    const raw = q().trim().toLowerCase();
    const at = raw.startsWith("@");
    const needle = at ? raw.slice(1) : raw;
    const botCmds = state.bots
      .map((b) => ({ b, s: Math.max(score(needle, b.handle), score(needle, b.name.toLowerCase())) }))
      .filter((x) => x.s > 0 && (at || needle))
      .sort((x, y) => y.s - x.s)
      .map(
        ({ b }): Cmd => ({
          id: "b:" + b.id,
          label: `Chat with ${b.name}`,
          icon: "bot",
          bot: b,
          sub: `@${b.handle} · ${b.role}`,
          hint: `${MOD} ↵ new tile`,
          run: () => openBot(b.id),
          runAlt: () => openBot(b.id, { newPane: true }),
        }),
      );
    if (at) return botCmds.length ? [{ title: "Bots", items: botCmds }] : [];
    const acts = actions()
      .map((c) => ({ c, s: score(needle, c.label) }))
      .filter((x) => x.s > 0)
      .sort((a, b) => b.s - a.s)
      .map((x) => x.c);
    const sessions = Object.values(state.sessions)
      .map((s) => s.info)
      .filter((i) => (i.kind === "chat" || i.kind === "channel") && !i.private && !i.archived)
      .map((i) => ({ i, s: score(needle, sessionLabel(i).toLowerCase()) }))
      .filter((x) => x.s > 0)
      .sort((a, b) => b.s - a.s || b.i.updated_at - a.i.updated_at)
      .slice(0, needle ? 12 : 6)
      .map(
        ({ i }): Cmd => ({
          id: "s:" + i.id,
          label: sessionLabel(i),
          icon: i.kind === "channel" ? (i.bot_id ? "lock" : "hash") : "chat",
          bot: i.kind === "chat" ? state.bots.find((b) => b.id === i.bot_id) : undefined,
          sub: relTime(i.updated_at),
          run: () => openSession(i.id),
          runAlt: () => openSession(i.id, { newPane: true }),
        }),
      );
    const hitCmds = hits().map(
      (h): Cmd => ({
        id: `h:${h.session_id}:${h.seq}`,
        label: h.snippet,
        icon: "search",
        sub: h.session_title,
        run: () => {
          const pane = openSession(h.session_id);
          if (pane) setJumpTarget({ paneId: pane, sessionId: h.session_id, seq: h.seq });
        },
      }),
    );
    const groups: { title: string; items: Cmd[] }[] = [];
    if (needle) {
      if (botCmds.length) groups.push({ title: "Bots", items: botCmds });
      if (sessions.length) groups.push({ title: "Chats", items: sessions });
      if (acts.length) groups.push({ title: "Actions", items: acts });
      if (hitCmds.length) groups.push({ title: "Messages", items: hitCmds });
    } else {
      groups.push({ title: "Actions", items: acts.slice(0, 8) });
      if (sessions.length) groups.push({ title: "Recent chats", items: sessions });
    }
    return groups;
  });
  const flat = createMemo(() => items().flatMap((g) => g.items));

  let timer: ReturnType<typeof setTimeout> | undefined;
  createEffect(
    on(q, (v) => {
      setSel(0);
      clearTimeout(timer);
      const needle = v.trim();
      if (needle.length < 3 || !state.ready) {
        setHits([]);
        setSearching(false);
        return;
      }
      setSearching(true);
      timer = setTimeout(async () => {
        try {
          setHits(await api.search(needle, 8));
        } catch {
          setHits([]);
        } finally {
          setSearching(false);
        }
      }, 250);
    }),
  );
  onCleanup(() => clearTimeout(timer));

  const run = (c: Cmd, alt = false) => {
    close();
    (alt && c.runAlt ? c.runAlt : c.run)();
  };

  const onKey = (e: KeyboardEvent) => {
    const n = flat().length;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSel((i) => (i + 1) % Math.max(1, n));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSel((i) => (i - 1 + n) % Math.max(1, n));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const c = flat()[sel()];
      if (c) run(c, e.ctrlKey || e.metaKey);
    }
  };

  createEffect(() => {
    const i = sel();
    list?.querySelector(`[data-idx="${i}"]`)?.scrollIntoView({ block: "nearest" });
  });

  return (
    <Modal onClose={close} title="Command palette" size="md" class="palette" hideHeader>
      <div class="palette-input">
        <Icon name="search" size={18} />
        <input
          value={q()}
          onInput={(e) => setQ(e.currentTarget.value)}
          onKeyDown={onKey}
          placeholder="Search chats and messages, run a command — @ for bots"
          aria-label="Command"
          autofocus
        />
        <Show when={searching()}>
          <Spinner size={14} />
        </Show>
        <Show when={!touch}>
          <kbd>Esc</kbd>
        </Show>
      </div>
      <div class="palette-list" ref={list} role="listbox">
        <Show when={flat().length} fallback={<div class="empty-state small"><p class="muted">No results</p></div>}>
          <For each={items()}>
            {(g) => (
              <div class="palette-group">
                <div class="menu-heading">{g.title}</div>
                <For each={g.items}>
                  {(c) => {
                    const idx = () => flat().indexOf(c);
                    return (
                      <button
                        type="button"
                        role="option"
                        class="palette-item"
                        data-idx={idx()}
                        classList={{ active: sel() === idx() }}
                        aria-selected={sel() === idx()}
                        onMouseMove={() => setSel(idx())}
                        onClick={(e) => run(c, e.ctrlKey || e.metaKey)}
                      >
                        <Show when={c.bot} fallback={<Icon name={c.icon} size={16} />}>
                          <BotAvatar bot={c.bot} size={18} />
                        </Show>
                        <span class="palette-label">{c.label}</span>
                        <Show when={c.sub}>
                          <span class="palette-sub">{c.sub}</span>
                        </Show>
                        <Show when={c.hint && !touch}>
                          <kbd>{c.hint}</kbd>
                        </Show>
                      </button>
                    );
                  }}
                </For>
              </div>
            )}
          </For>
        </Show>
      </div>
      <div class="palette-foot" hidden={touch}>
        <span>
          <kbd>↑</kbd>
          <kbd>↓</kbd> move
        </span>
        <span>
          <kbd>↵</kbd> open
        </span>
        <span>
          <kbd>{MOD} ↵</kbd> in a new tile
        </span>
        <span>
          <kbd>@</kbd> bots
        </span>
      </div>
    </Modal>
  );
}
