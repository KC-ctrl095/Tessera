import { Match, Show, Switch, createEffect, createSignal, lazy, onCleanup, onMount } from "solid-js";
import { requestClosePane } from "./lib/actions";
import { phase, startApp } from "./lib/app";
import { prefs, setPrefs } from "./lib/prefs";
import { state } from "./lib/store";
import { inboxOpen, setInboxOpen } from "./lib/inboxState";
import { portraits } from "./components/bots";
import { subagentParts } from "./components/chat/SubagentGate";
import {
  addServerOpen,
  archivedOpen,
  botEditor,
  channelEditor,
  cheatsheetOpen,
  dialog,
  dirBrowser,
  drawerOpen,
  jobEditor,
  openSettings,
  overviewOpen,
  paletteOpen,
  rewindDialog,
  setAddServerOpen,
  setCheatsheetOpen,
  setDrawerOpen,
  setOverviewOpen,
  setPaletteOpen,
  settingsTab,
  setSetupOpen,
  setupOpen,
  tourStarted,
  SETTINGS_ELSEWHERE,
  setArchivedOpen,
  setSettingsTab,
  openBotEditor,
  openJobEditor,
} from "./lib/ui";
import { isDesktopApp } from "./lib/native";
import {
  BP_TILE,
  cycleLayout,
  focusComposer,
  moveFocus,
  moveToWorkspace,
  newChat,
  resizeFocused,
  swapWith,
  switchWorkspace,
  toggleMaximize,
  workspace,
  type Direction,
} from "./lib/workspace";
import { BotEditor } from "./components/BotEditor";
import { ConnectScreen, OfflineBanner } from "./components/Connection";
import { Logo } from "./components/icons";
import { DialogHost, MenuHost, Modal, Toasts, TooltipHost, anyModalOpen, useBackToClose } from "./components/primitives";
import { watchSetup } from "./lib/setupWatch";
import { Rail, Sidebar, revealSidebarTab } from "./components/Sidebar";
import { mosaicsKeyDown, mosaicsOpen } from "./lib/mosaicsPage";
import { installHoverTyping } from "./lib/hoverType";
import { installViewerClicks, viewer } from "./lib/viewer";
import { TopBar } from "./components/TopBar";
import { NotificationOverlay } from "./components/NotificationBell";
import { Workspace, noteTile } from "./components/Workspace";
import { loadBoardPane } from "./components/board/BoardSlot";
import { RewindHost } from "./components/chat/RewindHost";

// Opened on demand: loaded the first time they're needed (first run, pairing,
// settings, editors, dialogs), so a phone starts on less.
const MosaicsPage = lazy(() => import("./components/MosaicsPage").then((m) => ({ default: m.MosaicsPage })));
const DirBrowserHost = lazy(() => import("./components/DirBrowser").then((m) => ({ default: m.DirBrowserHost })));
const SettingsModal = lazy(() => import("./components/Settings").then((m) => ({ default: m.SettingsModal })));
const Lightbox = lazy(() => import("./components/Lightbox"));
const Onboarding = lazy(() => import("./components/Onboarding").then((m) => ({ default: m.Onboarding })));
const ServerForm = lazy(() => import("./components/Onboarding").then((m) => ({ default: m.ServerForm })));
const SetupStart = lazy(() => import("./components/Setup").then((m) => ({ default: m.SetupStart })));
const SetupWizard = lazy(() => import("./components/Setup").then((m) => ({ default: m.SetupWizard })));
const ChannelEditor = lazy(() => import("./components/ChannelEditor").then((m) => ({ default: m.ChannelEditor })));
const JobEditorView = lazy(() => import("./components/JobEditorView").then((m) => ({ default: m.JobEditorView })));
const ArchivedView = lazy(() => import("./components/ArchivedView").then((m) => ({ default: m.ArchivedView })));

const Cheatsheet = lazy(() => import("./components/Cheatsheet").then((m) => ({ default: m.Cheatsheet })));
// Started from Setup or Settings, which load it; shown from then on.
const Tour = lazy(() => import("./components/Tour").then((m) => ({ default: m.Tour })));
// Opened with a key or a gesture, so they load once the app has settled (preloadSoon): the
// first Ctrl+K or pinch finds them ready.
const CommandPalette = lazy(() => import("./components/CommandPalette").then((m) => ({ default: m.CommandPalette })));
const MosaicOverview = lazy(() => import("./components/MosaicOverview").then((m) => ({ default: m.MosaicOverview })));

/** Load parts that must open at once when the main thread is idle after start (not before the first paint). */
function preloadSoon(load: () => unknown) {
  const run = () => void load();
  if (typeof requestIdleCallback === "function") requestIdleCallback(run, { timeout: 3000 });
  else setTimeout(run, 1500);
}

const DIRS: Record<string, Direction> = {
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
  KeyH: "left",
  KeyL: "right",
  KeyK: "up",
  KeyJ: "down",
};

export function App() {
  const [pairElsewhere, setPairElsewhere] = createSignal(false);
  onMount(() => {
    watchSetup();
    void startApp();
    window.addEventListener("keydown", onGlobalKey);
    // The all-mosaics page sees its keys first (Ctrl+Tab would otherwise reach a text box).
    window.addEventListener("keydown", onMosaicsKey, true);
    onCleanup(() => {
      window.removeEventListener("keydown", onGlobalKey);
      window.removeEventListener("keydown", onMosaicsKey, true);
    });
    trackViewport();
    installHoverTyping();
    onCleanup(installViewerClicks());
  });

  return (
    <div class="app">
      <Switch>
        <Match when={phase() === "boot"}>
          <div class="splash">
            <Logo size={48} />
          </div>
        </Match>
        <Match when={phase() === "onboarding"}>
          <Show when={isDesktopApp && !pairElsewhere()} fallback={<Onboarding onBack={isDesktopApp ? () => setPairElsewhere(false) : undefined} />}>
            <SetupStart onRemote={() => setPairElsewhere(true)} />
          </Show>
        </Match>
        <Match when={phase() === "app"}>
          <Show when={state.everConnected} fallback={<ConnectScreen />}>
            <Shell />
          </Show>
        </Match>
      </Switch>

      <Show when={setupOpen() && phase() === "app" && state.everConnected}>
        <SetupWizard onClose={() => setSetupOpen(false)} />
      </Show>
      <Show when={phase() === "app" && state.everConnected && tourStarted()}>
        <Tour />
      </Show>
      <Show when={settingsTab() && !SETTINGS_ELSEWHERE.includes(settingsTab()!)}>
        <SettingsModal />
      </Show>
      <Show when={paletteOpen()}>
        <CommandPalette />
      </Show>
      <Show when={botEditor()} keyed>
        {(b) => <BotEditor id={b.id} />}
      </Show>
      <Show when={channelEditor()} keyed>
        {(c) => <ChannelEditor members={c.members} />}
      </Show>
      <Show when={jobEditor()} keyed>
        {(j) => <JobEditorView job={j.job} />}
      </Show>
      <Show when={archivedOpen()}>
        <ArchivedView />
      </Show>
      <Show when={rewindDialog()} keyed>
        {(spec) => <RewindHost spec={spec} />}
      </Show>
      <Show when={cheatsheetOpen()}>
        <Cheatsheet />
      </Show>
      <Show when={addServerOpen()} keyed>
        {(spec) => {
          const old = spec.repair ? state.config?.servers.find((x) => x.id === spec.repair) : undefined;
          return (
            <Modal title={old ? `Pair again with ${old.name}` : "Add a server"} onClose={() => setAddServerOpen(false)} size="md">
              <div class="modal-body">
                <Show when={old}>
                  <p class="muted small modal-lede">Get a new code with <code>tessera pair</code> on the server. This turns on end-to-end encryption and replaces the old pairing.</p>
                </Show>
                <ServerForm mode="pair" initial={old} repair={!!old} onDone={() => setAddServerOpen(false)} onCancel={() => setAddServerOpen(false)} />
              </div>
            </Modal>
          );
        }}
      </Show>
      <Show when={viewer()}>
        <Lightbox />
      </Show>
      <Show when={dirBrowser()}>
        <DirBrowserHost />
      </Show>
      <DialogHost />
      <MenuHost />
      <TooltipHost />
      <Toasts />
    </div>
  );
}

function Shell() {
  const wide = () => workspace.mode() === "tile" && window.innerWidth >= BP_TILE;
  const sidebar = () => (wide() ? (prefs.sidebarCollapsed ? "rail" : "full") : "drawer");
  createEffect(() => {
    if (sidebar() !== "drawer" && drawerOpen()) setDrawerOpen(false);
  });
  createEffect(() => {
    if (workspace.mode() === "tile" && overviewOpen()) setOverviewOpen(false);
  });
  onMount(() => {
    preloadSoon(CommandPalette.preload);
    preloadSoon(loadBoardPane);
  });
  // The tile overview is for phones and foldables (a pinch opens it).
  createEffect(() => {
    if (workspace.mode() !== "tile") preloadSoon(MosaicOverview.preload);
  });
  // Older links to Settings' Bots, Scheduled and Archived sections: their homes are elsewhere now.
  createEffect(() => {
    const t = settingsTab();
    if (!t || !SETTINGS_ELSEWHERE.includes(t)) return;
    setSettingsTab(null);
    if (t === "archived") setArchivedOpen(true);
    else if (t === "bots") state.bots.length ? revealSidebarTab("bots") : openBotEditor();
    else state.jobs.length ? revealSidebarTab("jobs") : openJobEditor();
  });
  // Parts some people never see, once there is something for them to show: bots' portraits,
  // note tiles, helpers' cards and reports.
  createEffect(() => {
    if (state.bots.length) preloadSoon(portraits.load);
  });
  createEffect(() => {
    if (!noteTile.get() && Object.keys(state.notes).length) preloadSoon(noteTile.load);
  });
  createEffect(() => {
    if (!subagentParts.get() && Object.values(state.sessions).some((s) => s.info.kind === "subagent")) preloadSoon(subagentParts.load);
  });

  return (
    <div class="shell">
      <Show when={sidebar() === "full"}>
        <Sidebar variant="full" />
      </Show>
      <Show when={sidebar() === "rail"}>
        <Rail />
      </Show>
      <main class="main">
        <OfflineBanner />
        <Show when={workspace.mode() !== "mobile"}>
          <TopBar drawer={sidebar() === "drawer"} />
        </Show>
        <Workspace />
        <Show when={sidebar() === "drawer"}>
          <EdgeSwipe />
        </Show>
      </main>
      <Show when={sidebar() === "drawer"}>
        <Drawer />
      </Show>
      <Show when={overviewOpen() && workspace.mode() !== "tile"}>
        <MosaicOverview />
      </Show>
      <Show when={mosaicsOpen()}>
        <MosaicsPage />
      </Show>
      <NotificationOverlay />
    </div>
  );
}

/** Slide-over sidebar for narrow layouts; swipe left to close. */
function Drawer() {
  const [dx, setDx] = createSignal(0);
  let start: { x: number; y: number; id: number; lock: boolean | null } | null = null;
  // The back gesture closes the open drawer.
  let releaseBack: (() => void) | null = null;
  createEffect(() => {
    if (drawerOpen()) releaseBack ??= useBackToClose(() => {
      releaseBack = null;
      setDrawerOpen(false);
    });
    else if (releaseBack) {
      const r = releaseBack;
      releaseBack = null;
      r();
    }
  });
  return (
    <div class="drawer-layer" classList={{ open: drawerOpen(), dragging: dx() !== 0 }} aria-hidden={!drawerOpen()}>
      <div class="drawer-backdrop" onClick={() => setDrawerOpen(false)} style={{ opacity: drawerOpen() ? Math.max(0, 1 + dx() / 300) : 0 }} />
      <div
        class="drawer-panel"
        style={{ transform: drawerOpen() ? `translateX(${Math.min(0, dx())}px)` : undefined }}
        onPointerDown={(e) => {
          if (e.pointerType === "touch") start = { x: e.clientX, y: e.clientY, id: e.pointerId, lock: null };
        }}
        onPointerMove={(e) => {
          if (!start || e.pointerId !== start.id) return;
          const ddx = e.clientX - start.x;
          const ddy = e.clientY - start.y;
          if (start.lock === null && Math.hypot(ddx, ddy) > 10) start.lock = Math.abs(ddx) > Math.abs(ddy);
          if (start.lock) setDx(Math.min(0, ddx));
        }}
        onPointerUp={() => {
          if (dx() < -70) setDrawerOpen(false);
          setDx(0);
          start = null;
        }}
        onPointerCancel={() => {
          setDx(0);
          start = null;
        }}
      >
        <Show when={drawerOpen()}>
          <Sidebar variant="drawer" />
        </Show>
      </div>
    </div>
  );
}

/** Invisible strip on the left edge: swipe right to open the drawer. */
function EdgeSwipe() {
  let sx = 0;
  let id = -1;
  return (
    <div
      class="edge-swipe"
      onPointerDown={(e) => {
        if (e.pointerType !== "touch") return;
        sx = e.clientX;
        id = e.pointerId;
        e.currentTarget.setPointerCapture(e.pointerId);
      }}
      onPointerMove={(e) => {
        if (e.pointerId === id && e.clientX - sx > 36) {
          id = -1;
          setDrawerOpen(true);
        }
      }}
      onPointerUp={() => (id = -1)}
    />
  );
}

/** Ctrl+Tab / Alt+`: all mosaics (see lib/mosaicsPage.ts), never over a dialog. */
function onMosaicsKey(e: KeyboardEvent) {
  if (phase() !== "app" || !state.everConnected) return;
  if (!mosaicsOpen() && (anyModalOpen() || viewer() || paletteOpen() || dialog() || dirBrowser() || rewindDialog() || cheatsheetOpen() || document.querySelector(".popover"))) return;
  // While the remote page keyboard has focus, Ctrl+Tab belongs to its saved
  // web tabs. Alt+` still opens mosaics, and an already-open switcher owns keys.
  if (!mosaicsOpen() && e.key === "Tab" && e.ctrlKey && (e.target as Element | null)?.closest?.(".wv-keys")) return;
  mosaicsKeyDown(e);
}

function onGlobalKey(e: KeyboardEvent) {
  if (e.defaultPrevented || phase() !== "app") return;
  const mod = e.ctrlKey || e.metaKey;
  const key = e.key.toLowerCase();
  if (inboxOpen()) {
    // The notification center sits above the toasts: the palette or Settings take its place
    // rather than opening beneath it.
    if (mod && !e.altKey && !e.shiftKey && (key === "k" || key === ",")) {
      e.preventDefault();
      setInboxOpen(false);
      if (key === "k") setPaletteOpen(true);
      else openSettings();
    }
    return;
  }
  if (mod && !e.altKey && !e.shiftKey && key === "k") {
    e.preventDefault();
    setPaletteOpen(!paletteOpen());
    return;
  }
  if (settingsTab() || paletteOpen() || dialog() || dirBrowser() || addServerOpen() || botEditor() || channelEditor() || jobEditor() || archivedOpen() || rewindDialog()) return;
  if (cheatsheetOpen()) {
    if (e.altKey && (e.code === "Slash" || key === "/" || key === "?")) {
      e.preventDefault();
      setCheatsheetOpen(false);
    }
    return;
  }
  if (e.key === "Escape" && drawerOpen()) {
    setDrawerOpen(false);
    return;
  }
  if (!state.everConnected) return;
  if (mod && !e.altKey && !e.shiftKey) {
    if (key === "n") {
      e.preventDefault();
      newChat({ newPane: true });
      focusComposer();
    } else if (key === ",") {
      e.preventDefault();
      openSettings();
    } else if (key === "b") {
      e.preventDefault();
      if (window.innerWidth >= BP_TILE) setPrefs("sidebarCollapsed", !prefs.sidebarCollapsed);
      else setDrawerOpen(!drawerOpen());
    }
    return;
  }
  if (!e.altKey) return;
  // Workspaces: Alt+1…9 switch, Alt+Shift+1…9 move the focused tile there.
  const digit = /^Digit([1-9])$/.exec(e.code) ?? /^Numpad([1-9])$/.exec(e.code);
  if (digit && !mod) {
    e.preventDefault();
    const i = Number(digit[1]) - 1;
    if (e.shiftKey) moveToWorkspace(workspace.focused(), i);
    else switchWorkspace(i);
    focusComposer();
    return;
  }
  if (!mod && (e.code === "Slash" || key === "/" || key === "?")) {
    e.preventDefault();
    setCheatsheetOpen(true);
    return;
  }
  if (!mod && e.code === "Space") {
    e.preventDefault();
    cycleLayout(e.shiftKey ? -1 : 1);
    return;
  }
  if (!mod && !e.shiftKey) {
    if (e.key === "Enter") {
      e.preventDefault();
      if (newChat({ newPane: true })) focusComposer();
      return;
    }
    if (e.code === "KeyW") {
      e.preventDefault();
      void requestClosePane();
      focusComposer();
      return;
    }
    if (e.code === "KeyF") {
      e.preventDefault();
      toggleMaximize();
      return;
    }
  }
  const dir = DIRS[e.code] ?? DIRS[e.key];
  if (dir) {
    e.preventDefault();
    if (mod) resizeFocused(dir);
    else if (e.shiftKey) swapWith(dir);
    else {
      moveFocus(dir);
      focusComposer();
    }
  }
}

/** Keep the app sized to the visual viewport (on-screen keyboards). */
function trackViewport() {
  const vv = window.visualViewport;
  if (!vv) return;
  const apply = () => {
    if (vv.scale > 1.01) return;
    document.documentElement.style.setProperty("--app-h", `${Math.round(vv.height)}px`);
  };
  vv.addEventListener("resize", apply);
  apply();
}
