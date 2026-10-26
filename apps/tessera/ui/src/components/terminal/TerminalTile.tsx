// A terminal in a tile, for the user alone: on the server, or on one of
// their machines that allows it (decided on that computer). It opens at once
// on the server when that is the only place; otherwise the tile asks where
// (in the tile, no dialog). The terminal belongs to this device: the tile is
// kept here only, and closing the tile ends it. Terminals are off on a
// server until turned on; the tile says so plainly and offers it, with what
// it means. The terminal itself (xterm.js) loads only when one opens.
import "../../styles/terminal.css";
import { For, Match, Show, Suspense, Switch, createEffect, createResource, createSignal, lazy, onCleanup } from "solid-js";
import { ensureMachines, listMachines, machinePlatform, onMachinesChanged, type MachineInfo } from "../../lib/machines";
import { closeTerminal, ctrlOf, fullAccessWarning, KEYS, setTerminalSettings, terminalStatus, terminalTheme, setTerminalTheme, TERMINAL_THEMES, type TerminalInfo, type TerminalStatus, type TerminalTheme } from "../../lib/terminal";
import { resolvedTheme } from "../../lib/prefs";
import { confirmDialog, setDrawerOpen, toast } from "../../lib/ui";
import { copyText, touchUi } from "../../lib/util";
import { closePane, registerComposer, setPaneContent, workspace } from "../../lib/workspace";
import { Icon, Tessellate } from "../icons";
import { usePane } from "../PaneContext";
import { openMenu, type MenuItem } from "../primitives";
import { RestoreButton } from "../RestoreButton";
import { ALT } from "../TopBar";
import type { TermViewHandle } from "./TermView";
import { NewTileButton, newInHeader } from "../NewMenu";

const TermView = lazy(() => import("./TermView"));

type Content = { type: "terminal"; machine?: string | null; termId?: string };

/** What a terminal can do, in plain words (also in Settings → Privacy & safety → What commands can reach). */
export const TERMINAL_EXPLAINED =
  "A terminal is a command line on the computer, as your account there: whatever you type runs with that account's rights. Only you see it, on this device; agents never get one.";

export function TerminalTile(props: { paneId: string }) {
  const pane = usePane();
  const touch = touchUi();
  const content = (): Content => {
    const c = workspace.panes[props.paneId]?.content;
    return c?.type === "terminal" ? c : { type: "terminal" };
  };
  const set = (c: Omit<Content, "type">) => setPaneContent(props.paneId, { type: "terminal", ...c });

  const [status, { refetch: refetchStatus, mutate: setStatus }] = createResource(() => terminalStatus().catch((e) => ({ error: String((e as { message?: string })?.message ?? e) }) as const));
  const [machines, { refetch: refetchMachines }] = createResource(() => listMachines().catch(() => [] as MachineInfo[]));
  onCleanup(onMachinesChanged(() => void refetchMachines()));
  ensureMachines();

  const [info, setInfo] = createSignal<TerminalInfo | null>(null);
  const [ended, setEnded] = createSignal<{ code: number | null; reason: string } | null>(null);
  const [failed, setFailed] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal(false);
  /** Bumped by "Start again": a fresh terminal view. */
  const [run, setRun] = createSignal(0);
  const [view, setView] = createSignal<TermViewHandle>();
  const [ctrl, setCtrl] = createSignal(false);
  const [alt, setAlt] = createSignal(false);
  /** The find bar (Ctrl+Shift+F, or Find in the ⋯ menu). */
  const [finding, setFinding] = createSignal(false);
  const [needle, setNeedle] = createSignal("");
  const [missed, setMissed] = createSignal(false);
  let findInput: HTMLInputElement | undefined;
  const [title, setTitle] = createSignal("");
  const colorTheme = (): "dark" | "light" => {
    const theme = terminalTheme();
    return theme === "match" ? resolvedTheme() : theme;
  };
  const colors = () => TERMINAL_THEMES[colorTheme()];

  // Moving the focus here (Alt+arrows) puts the keyboard in the terminal, and so
  // does typing over the tile with the mouse on it (like a chat's message box).
  onCleanup(
    registerComposer(
      props.paneId,
      () => view()?.focus(),
      (text) => {
        view()?.focus();
        view()?.send(text);
      },
    ),
  );

  const st = (): TerminalStatus | null => {
    const s = status();
    return s && !("error" in s) ? s : null;
  };
  /** Old servers don't know terminals. */
  const unsupported = () => {
    const s = status();
    return !!s && "error" in s;
  };
  const serverOn = () => !!st() && st()!.supported && st()!.settings.access !== "off";
  const usable = (): MachineInfo[] => (machines() ?? []).filter((m) => m.online && !m.paused && machinePlatform(m.os, m.features).terminal && !!m.scope.terminal && m.scope.terminal !== "off");
  /** Every place a terminal can open now: null is the server. */
  const places = (): (string | null)[] => [...(serverOn() ? [null] : []), ...usable().map((m) => m.id)];
  const chosen = () => content().machine !== undefined;
  const machineName = (id: string | null | undefined) => (id ? (machines() ?? []).find((m) => m.id === id)?.name ?? "the machine" : "this server");

  // One place only: open there at once (no question to answer).
  const autoPick = () => {
    if (chosen() || content().termId || status.loading || machines.loading) return;
    const p = places();
    if (p.length === 1) set({ machine: p[0] });
  };

  createEffect(() => {
    if (!status.loading && !machines.loading) autoPick();
  });

  const pick = (machine: string | null) => {
    setEnded(null);
    setFailed(null);
    set({ machine });
  };

  const turnOn = async (access: "sandboxed" | "full") => {
    const s = st();
    if (!s) return;
    // Full access is asked about here as in Settings → What commands can reach.
    if (access === "full" && !(await confirmDialog({ title: "Terminals with full access?", body: fullAccessWarning(s.sandbox_ok), confirmLabel: "Turn on", danger: true }))) return;
    setBusy(true);
    try {
      const next = await setTerminalSettings({ ...s.settings, access });
      setStatus(next);
      if (!usable().length) set({ machine: null });
    } catch (e) {
      toast({ level: "error", message: String((e as { message?: string })?.message ?? e) });
    } finally {
      setBusy(false);
    }
  };

  const startAgain = () => {
    setEnded(null);
    setFailed(null);
    setInfo(null);
    setTitle("");
    set({ machine: content().machine });
    setRun((n) => n + 1);
  };

  // The tile closed (or became something else): the terminal ends with it.
  let openId: string | undefined;
  onCleanup(() => {
    const c = workspace.panes[props.paneId]?.content;
    const still = c?.type === "terminal" && c.termId === openId;
    if (openId && !still) void closeTerminal(openId);
  });

  const modify = (data: string): string => {
    let out = data;
    if (ctrl()) {
      const c = ctrlOf(data);
      if (c) out = c;
      setCtrl(false);
    }
    if (alt()) {
      out = "\x1b" + out;
      setAlt(false);
    }
    return out;
  };
  const key = (data: string) => {
    view()?.send(alt() ? "\x1b" + data : data);
    setAlt(false);
    setCtrl(false);
    view()?.focus();
  };

  const copy = async () => {
    const t = view()?.copy() ?? "";
    if (!t) return toast({ level: "info", message: "Select some text first" });
    if (await copyText(t)) toast({ level: "success", message: "Copied" });
  };
  const paste = () => view()?.pasteClipboard() ?? Promise.resolve();

  const sub = () => {
    const i = info();
    if (!chosen()) return "Choose where";
    const place = i?.place ?? machineName(content().machine);
    if (!i) return place;
    return `${place} · ${i.sandboxed ? "sandboxed" : "full access"}`;
  };

  /** Open a terminal on `machine` in this tile instead (the one open here ends, after asking). */
  const switchTo = async (machine: string | null) => {
    const live = !!openId && !ended();
    if (live && !(await confirmDialog({ title: `Open a terminal on ${machineName(machine)}?`, body: `The terminal on ${machineName(content().machine)} ends, with anything running in it.`, confirmLabel: "Switch" }))) return;
    if (openId) void closeTerminal(openId);
    openId = undefined;
    setInfo(null);
    setTitle("");
    setEnded(null);
    setFailed(null);
    set({ machine });
    setRun((n) => n + 1);
  };

  const openFind = () => {
    setFinding(true);
    requestAnimationFrame(() => {
      findInput?.focus();
      findInput?.select();
    });
  };
  const find = (back = false) => {
    const t = needle();
    if (!t) return;
    setMissed(!view()?.find(t, back));
  };
  const closeFind = () => {
    setFinding(false);
    setMissed(false);
    view()?.findDone();
  };

  const menu = (anchor: HTMLElement) => {
    const items: MenuItem[] = [];
    if (info() && !ended()) {
      items.push({ label: "Copy", icon: "copy", hint: touch ? undefined : "Ctrl Shift C", onSelect: () => void copy() });
      items.push({ label: "Paste", hint: touch ? undefined : "Ctrl Shift V", onSelect: () => void paste() });
      items.push({ label: "Select all", onSelect: () => view()?.selectAll() });
      items.push({ label: "Find…", icon: "search", hint: touch ? undefined : "Ctrl Shift F", onSelect: openFind });
      items.push({ label: "Clear the screen", onSelect: () => view()?.clear() });
      items.push({
        label: "Text size",
        description: touch ? undefined : "Ctrl = and Ctrl −",
        more: [
          { label: "Bigger", hint: touch ? undefined : "Ctrl =", onSelect: () => view()?.zoom(1) },
          { label: "Smaller", hint: touch ? undefined : "Ctrl −", onSelect: () => view()?.zoom(-1) },
          { label: "As it was", hint: touch ? undefined : "Ctrl 0", onSelect: () => view()?.zoom(0) },
        ],
      });
      items.push({ separator: true });
    }
    items.push({
      label: "Colours",
      more: (["dark", "light", "match"] as TerminalTheme[]).map((theme) => ({
        label: theme === "match" ? "Match app" : theme === "dark" ? "Dark" : "Light",
        description: theme === "dark" ? "Default" : undefined,
        checked: terminalTheme() === theme,
        onSelect: () => setTerminalTheme(theme),
      })),
    });
    if (ended() || failed()) items.push({ label: "Start again", icon: "refresh", onSelect: startAgain });
    // Every place a terminal may open now: this server (if on) and each machine that allows it itself.
    const here = chosen() ? (content().machine ?? null) : undefined;
    const list = places();
    if (list.length > 1 || (here !== undefined && !list.includes(here))) {
      items.push({ heading: "Open on" });
      for (const p of list)
        items.push({
          label: p === null ? "This server" : machineName(p),
          icon: p === null ? "server" : "monitor",
          checked: p === here && !!info() && !ended(),
          onSelect: () => {
            if (p === here && info() && !ended()) return;
            void switchTo(p);
          },
        });
    }
    items.push({ label: "Close terminal", icon: "x", description: "Ends what runs in it", hint: workspace.mode() === "tile" ? `${ALT} W` : undefined, onSelect: () => closePane(props.paneId) });
    openMenu(anchor, items, "bottom-end");
  };

  return (
    <div class="board term-tile" data-term-tile={props.paneId}>
      <header
        class="pane-head board-head"
        onPointerDown={(e) => {
          if ((e.target as HTMLElement).closest("button, input, a, [data-nodrag]")) return;
          pane.startDrag(e);
        }}
      >
        <Show when={pane.showMenuButton()}>
          <button type="button" class="icon-btn head-menu" data-keep-focus aria-label="Open sidebar" onClick={() => setDrawerOpen(true)}>
            <Icon name="menu" />
          </button>
        </Show>
        <Show when={(workspace.mode() === "mobile" && pane.count() > 1) || (workspace.mode() === "dual" && pane.count() > 2)}>
          <div class="page-dots" aria-label={`Tile ${pane.index() + 1} of ${pane.count()}`}>
            <For each={Array.from({ length: pane.count() })}>{(_, i) => <i classList={{ on: i() === pane.index() }} />}</For>
          </div>
        </Show>
        <div class="head-title">
          <span class="board-kind" aria-hidden="true">
            <Icon name="terminal" size={15} />
          </span>
          <div class="title-stack">
            <div class="title-line">
              <span class="title-static" title={title() || "Terminal"}>{title() || "Terminal"}</span>
            </div>
            <span class="head-sub board-sub">
              <span class="sub-text term-sub">{sub()}</span>
            </span>
          </div>
        </div>
        <div class="head-controls">
          <Show when={info() && !ended() && !touch}>
            <button type="button" class="icon-btn board-act" aria-label="Copy" data-tip="Copy (Ctrl Shift C)" onClick={() => void copy()}>
              <Icon name="copy" size={15} />
            </button>
          </Show>
          <button type="button" class="icon-btn" aria-label="Terminal options" aria-haspopup="menu" data-tip="More" onClick={(e) => menu(e.currentTarget)}>
            <Icon name="more" size={17} />
          </button>
          <RestoreButton />
          {/* Phones: New at the top right; Close is in the ⋯ menu. */}
          <Show
            when={newInHeader() && pane.showMenuButton()}
            fallback={
              <button type="button" class="icon-btn head-close" aria-label="Close terminal" data-tip="Close (ends what runs in it)" data-kbd={`${ALT} W`} onClick={() => closePane(props.paneId)}>
                <Icon name="x" size={15} />
              </button>
            }
          >
            <NewTileButton />
          </Show>
        </div>
      </header>
      <div class="board-body term-body" data-term-theme={(chosen() || content().termId) && !failed() ? colorTheme() : undefined} style={{ "--term-bg": colors().background, "--term-fg": colors().foreground }}>
        <Switch>
          <Match when={status.loading || machines.loading}>
            <div class="board-loading">
              <Tessellate size={10} />
            </div>
          </Match>
          <Match when={unsupported()}>
            <div class="term-note">
              <Icon name="terminal" size={22} />
              <p>This server doesn't have terminals yet. Update Tessera on it to use them.</p>
            </div>
          </Match>
          <Match when={!chosen() && !content().termId}>
            <Picker
              status={st()}
              machines={usable()}
              busy={busy()}
              onPick={pick}
              onTurnOn={(a) => void turnOn(a)}
              onRefresh={() => {
                void refetchStatus();
                void refetchMachines();
              }}
            />
          </Match>
          <Match when={failed()}>
            <div class="term-note" role="alert">
              <Icon name="alert" size={22} />
              <p>{failed()}</p>
              <div class="term-note-actions">
                <button type="button" class="btn btn-sm" onClick={startAgain}>
                  Try again
                </button>
                <Show when={places().length > 1}>
                  <button type="button" class="btn btn-sm" onClick={() => setPaneContent(props.paneId, { type: "terminal" })}>
                    Somewhere else
                  </button>
                </Show>
              </div>
            </div>
          </Match>
          <Match when={true}>
            <Suspense
              fallback={
                <div class="board-loading">
                  <Tessellate size={10} />
                </div>
              }
            >
              <Show when={run() + 1} keyed>
                {(_) => (
                  <TermView
                    termId={content().termId}
                    machine={content().machine ?? null}
                    touch={touch}
                    theme={colorTheme()}
                    onTitle={setTitle}
                    modify={modify}
                    handle={setView}
                    onFind={openFind}
                    onOpened={(i) => {
                      setInfo(i);
                      openId = i.id;
                      if (content().termId !== i.id) set({ machine: content().machine ?? null, termId: i.id });
                    }}
                    onExit={(code, reason) => {
                      setEnded({ code, reason });
                      openId = undefined;
                      // Gone for good: the tile no longer points at it.
                      set({ machine: content().machine ?? null });
                    }}
                    onError={(m) => {
                      setFailed(m);
                      set({ machine: content().machine ?? null });
                    }}
                  />
                )}
              </Show>
            </Suspense>
            <Show when={finding()}>
              <div class="term-find" role="search" data-nodrag>
                <Icon name="search" size={14} />
                <input
                  ref={findInput}
                  type="search"
                  placeholder="Find in the terminal"
                  aria-label="Find in the terminal"
                  value={needle()}
                  classList={{ missed: missed() }}
                  onInput={(e) => {
                    setNeedle(e.currentTarget.value);
                    setMissed(false);
                    find();
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      find(e.shiftKey);
                    } else if (e.key === "Escape") {
                      e.preventDefault();
                      e.stopPropagation();
                      closeFind();
                    }
                  }}
                />
                <Show when={missed()}>
                  <span class="term-find-none">Not found</span>
                </Show>
                <button type="button" class="icon-btn sm" aria-label="Earlier" data-tip="Earlier (Shift Enter)" onClick={() => find(true)}>
                  <Icon name="arrowUp" size={14} />
                </button>
                <button type="button" class="icon-btn sm" aria-label="Later" data-tip="Later (Enter)" onClick={() => find(false)}>
                  <Icon name="arrowDown" size={14} />
                </button>
                <button type="button" class="icon-btn sm" aria-label="Close find" onClick={closeFind}>
                  <Icon name="x" size={14} />
                </button>
              </div>
            </Show>
            <Show when={ended()}>
              <div class="term-ended" role="status">
                <span>
                  {ended()!.reason ? ended()!.reason.charAt(0).toUpperCase() + ended()!.reason.slice(1) : "It ended"}
                </span>
                <span class="term-ended-actions">
                  <button type="button" class="btn btn-sm btn-primary" onClick={startAgain}>
                    Start again
                  </button>
                  <button type="button" class="btn btn-sm" onClick={() => closePane(props.paneId)}>
                    Close
                  </button>
                </span>
              </div>
            </Show>
            <Show when={touch && info() && !ended()}>
              <div class="term-keys" role="toolbar" aria-label="Extra keys" data-nodrag onPointerDown={(e) => e.preventDefault()}>
                <button type="button" onClick={() => key(KEYS.esc)}>
                  Esc
                </button>
                <button type="button" onClick={() => key(KEYS.tab)}>
                  Tab
                </button>
                <button type="button" classList={{ on: ctrl() }} aria-pressed={ctrl()} onClick={() => setCtrl(!ctrl())}>
                  Ctrl
                </button>
                <button type="button" classList={{ on: alt() }} aria-pressed={alt()} onClick={() => setAlt(!alt())}>
                  Alt
                </button>
                <button type="button" aria-label="Left" onClick={() => key(KEYS.left)}>
                  ←
                </button>
                <button type="button" aria-label="Up" onClick={() => key(KEYS.up)}>
                  ↑
                </button>
                <button type="button" aria-label="Down" onClick={() => key(KEYS.down)}>
                  ↓
                </button>
                <button type="button" aria-label="Right" onClick={() => key(KEYS.right)}>
                  →
                </button>
                <button type="button" class="term-key-word" onClick={() => void paste()}>
                  Paste
                </button>
              </div>
            </Show>
          </Match>
        </Switch>
      </div>
    </div>
  );
}

/** Where to open it, or why nowhere yet (and turning it on for this server). */
function Picker(props: {
  status: TerminalStatus | null;
  machines: MachineInfo[];
  busy: boolean;
  onPick: (machine: string | null) => void;
  onTurnOn: (access: "sandboxed" | "full") => void;
  onRefresh: () => void;
}) {
  const s = () => props.status;
  const serverOn = () => !!s()?.supported && s()?.settings.access !== "off";
  return (
    <div class="term-pick">
      <Show when={s() && !s()!.private}>
        <p class="term-warn">
          <Icon name="shieldAlert" size={15} /> This connection isn't end-to-end encrypted, so terminals can't open over it. Use the Tessera app, or open Tessera on the server itself.
        </p>
      </Show>
      <Show when={serverOn() || props.machines.length}>
        <p class="term-pick-title">Open a terminal on</p>
        <div class="term-places">
          <Show when={serverOn()}>
            <button type="button" class="term-place" onClick={() => props.onPick(null)}>
              <Icon name="server" size={18} />
              <span>
                <b>This server</b>
                <small>{s()!.settings.access === "sandboxed" ? "Sandboxed" : "Full access"}</small>
              </span>
            </button>
          </Show>
          <For each={props.machines}>
            {(m) => (
              <button type="button" class="term-place" onClick={() => props.onPick(m.id)}>
                <Icon name="monitor" size={18} />
                <span>
                  <b>{m.name}</b>
                  <small>{m.scope.terminal === "sandboxed" ? "Sandboxed" : "Full access"}</small>
                </span>
              </button>
            )}
          </For>
        </div>
      </Show>
      <Show when={s() && !serverOn()}>
        <div class="term-off">
          <p class="term-pick-title">
            <Icon name="shield" size={15} /> Terminals are off on this server
          </p>
          <Show
            when={s()!.supported}
            fallback={<p class="term-help">Terminals aren't available on this server's platform.</p>}
          >
            <p class="term-help">{TERMINAL_EXPLAINED}</p>
            <Show
              when={s()!.sandbox_ok}
              fallback={
                <p class="term-help term-warn-text">
                  The command sandbox is {s()!.sandbox.startsWith("off") ? "off" : "not available"} on this server, so a terminal here would have your account's full rights.
                </p>
              }
            >
              <p class="term-help">Sandboxed, Tessera's own files (its keys and saved passwords) stay out of reach.</p>
            </Show>
            <div class="term-note-actions">
              <Show when={s()!.sandbox_ok}>
                <button type="button" class="btn btn-sm btn-primary" disabled={props.busy} onClick={() => props.onTurnOn("sandboxed")}>
                  Turn on, sandboxed
                </button>
              </Show>
              <Show when={!s()!.strict}>
                <button type="button" class={"btn btn-sm" + (s()!.sandbox_ok ? "" : " btn-primary")} disabled={props.busy} onClick={() => props.onTurnOn("full")}>
                  {s()!.sandbox_ok ? "Turn on, full access" : "Turn on"}
                </button>
              </Show>
            </div>
            <p class="term-help term-small">You can turn it off again in Settings → Privacy &amp; safety → What commands can reach.</p>
          </Show>
        </div>
      </Show>
      <Show when={!props.machines.length}>
        <p class="term-help term-small">
          A machine can allow terminals too: on that computer, in Settings → Devices → this computer.{" "}
          <button type="button" class="link-btn" onClick={() => props.onRefresh()}>
            Look again
          </button>
        </p>
      </Show>
    </div>
  );
}
