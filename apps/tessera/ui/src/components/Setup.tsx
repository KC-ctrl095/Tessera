// First-run setup, and Settings → Run setup again. The same few steps as
// `tessera setup` in a terminal, in the same words.
//
// · SetupStart (desktop app, before any server): run Tessera on this computer
//   (the shell starts it and connects, no code) or connect to another one.
// · SetupWizard (once connected): your AI, where you'll use it, your phone,
//   and a plain-words security summary (web search and voice are one tap
//   away from there). A mosaic above the steps fills in as they are done:
//   each step owns a band of tesserae that settle into place when it
//   completes.
import { newChatFreedom } from "../lib/freedom";
import { For, Match, Show, Switch, createEffect, createMemo, createResource, createSignal, onCleanup, onMount } from "solid-js";
import "../styles/settings.css";
import "../styles/setup.css";
import type { OAuthStart } from "../proto/OAuthStart";
import type { SetupStatus } from "../proto/SetupStatus";
import { adoptLocalServer } from "../lib/app";
import { localServerCancel, localServerDetect, localServerStart, onLocalServerStep } from "../lib/native";
import { INSTALL_COMMAND, installCommandHere } from "../lib/repo";
import { api, defaultModel, modelLabel, modelReady, refreshProviders, setState, state } from "../lib/store";
import { errorMessage, openSettings, toast, toastError, type SettingsTab } from "../lib/ui";
import { copyText, lsGet, lsSet, openExternal, plural, reducedMotion, touchUi } from "../lib/util";
import { focusComposer, isUnusedPane, newChat, setDraft, workspace } from "../lib/workspace";
import { Icon, Logo, Spinner, Tessellate, type IconName } from "./icons";
import { SettleTiles } from "./Mosaic";
import { useBackToClose } from "./primitives";
import { LanToggle, RemoteAccessPanel, TunnelLine, useRemoteStatus } from "./RemoteAccess";
import { AnthropicForm, OAuthDialog, OpenAIForm } from "./settings/ModelsTab";
import { PairPanel } from "./settings/PairPanel";
import { startTour } from "./Tour";

// ── When to show it ─────────────────────────────────────────────────────────

/** (Moved to lib/setupWatch.ts so the wizard can load on demand.) */
export { watchSetup } from "../lib/setupWatch";

// ── The mosaic that fills in ───────────────────────────────────────────────

function rng(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

interface Piece {
  d: string;
  tint: string;
  op: number;
  band: number;
  delay: number;
}

/** Tesserae in waving rows, fading at the rim; `band` = which step lays them. */
function pieces(W: number, H: number, bands: number, seed = 5): Piece[] {
  const r = rng(seed);
  const cell = 13;
  const gap = 2.2;
  const out: Piece[] = [];
  const period = W / 2.1;
  const amp = cell * 0.5;
  for (let row = 0; row * cell < H + cell; row++) {
    let x = -r() * cell;
    const y0 = row * cell;
    while (x < W) {
      const w = cell * (0.7 + r() * 0.65);
      const x1 = x;
      const x2 = x + w - gap;
      x += w;
      const wave = (xx: number) => Math.sin((xx / period) * Math.PI * 2 + row * 0.2) * amp;
      const h = cell - gap;
      const dx = ((x1 + x2) / 2 - W / 2) / (W * 0.5);
      const dy = (y0 + h / 2 - H * 0.55) / (H * 0.62);
      const keep = 1 - Math.hypot(dx, dy);
      if (keep <= 0 || r() > keep + 0.3) continue;
      const j = (v: number) => (r() - 0.5) * v;
      const ya = y0 + wave(x1);
      const yb = y0 + wave(x2);
      const pts = [
        [x1 + j(1.4), ya + j(1.2)],
        [x2 + j(1.4), yb + j(1.2)],
        [x2 + j(1.4), yb + h + j(1.2)],
        [x1 + j(1.4), ya + h + j(1.2)],
      ];
      const roll = r();
      out.push({
        d: "M" + pts.map(([a, b]) => `${a.toFixed(1)} ${b.toFixed(1)}`).join("L") + "Z",
        tint: roll < 0.055 ? "g" : roll < 0.08 ? "l" : `s${Math.floor(r() * 4)}`,
        op: Math.max(0.25, Math.min(1, keep * 1.3)),
        band: Math.min(bands - 1, Math.max(0, Math.floor(((x1 + x2) / 2 / W) * bands))),
        delay: Math.round(r() * 420),
      });
    }
  }
  return out;
}

/** The setup mosaic: bands of tesserae settle in as their steps are done. */
function SetupMosaic(props: { bands: number; done: (band: number) => boolean; width?: number; height?: number }) {
  const W = () => props.width ?? 640;
  const H = () => props.height ?? 130;
  const list = createMemo(() => pieces(W(), H(), props.bands));
  return (
    <svg class="setup-mosaic" classList={{ still: reducedMotion() }} viewBox={`0 0 ${W()} ${H()}`} aria-hidden="true" preserveAspectRatio="xMidYMid meet">
      <For each={list()}>
        {(p) => <path class={`${p.tint} ${props.done(p.band) ? "on" : "off"}`} d={p.d} style={{ "--o": String(p.op), "--d": `${p.delay}ms` }} />}
      </For>
    </svg>
  );
}

// ── Step 0: where should Tessera run? (desktop app) ────────────────────────

/** The desktop app runs on Windows: the server it starts here does too. */
const onWindows = () => /win/i.test(navigator.platform);
/** What "Stop waiting" says (the desktop shell says the same). */
const STOPPED_WAITING =
  "Stopped waiting. Tessera may still be starting in the background: press “Try again” in a moment to connect to it.";

export function SetupStart(props: { onRemote: () => void }) {
  const [info, { refetch }] = createResource(() => localServerDetect().catch(() => null));
  const [autostart, setAutostart] = createSignal(true);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // While it starts: what it is doing, for how long, and a way out.
  const [step, setStep] = createSignal<string | null>(null);
  const [seconds, setSeconds] = createSignal(0);
  const [stopping, setStopping] = createSignal(false);
  let stopFollowing: (() => void) | undefined;
  let ticker: ReturnType<typeof setInterval> | undefined;
  const settle = () => {
    stopFollowing?.();
    stopFollowing = undefined;
    if (ticker) clearInterval(ticker);
    ticker = undefined;
  };
  onCleanup(settle);
  // This try: when it began (the shell knows it by that) and whether "Stop
  // waiting" came before the start even reached the shell.
  let attempt = 0;
  let stopped = false;
  const start = async () => {
    setError(null);
    setStep(null);
    setSeconds(0);
    setStopping(false);
    setBusy(true);
    const began = Date.now();
    attempt = began;
    stopped = false;
    ticker = setInterval(() => setSeconds(Math.floor((Date.now() - began) / 1000)), 1000);
    stopFollowing = await onLocalServerStep((s) => setStep(s.message)).catch(() => undefined);
    try {
      if (stopped) throw new Error(STOPPED_WAITING);
      const id = await localServerStart(autostart(), began);
      if (!id) throw new Error("This version of the app can't start a server yet.");
      await adoptLocalServer(id);
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      settle();
      setBusy(false);
    }
  };
  const stopWaiting = async () => {
    stopped = true;
    setStopping(true);
    await localServerCancel(attempt).catch(() => null);
  };
  const copy = async (t: string) => {
    if (await copyText(t)) toast({ level: "success", message: "Copied", timeout: 1400 });
  };
  return (
    <div class="setup setup-start">
      <div class="setup-hero">
        <SetupMosaic bands={2} done={(b) => b === 0 || busy()} />
      </div>
      <div class="setup-card">
        <div class="setup-head">
          <Logo size={34} />
          <div>
            <p class="inscription">Welcome</p>
            <h1 class="serif">Where should Tessera run?</h1>
          </div>
        </div>
        <p class="setup-lede">Tessera is a server your devices connect to. It can live right here, or on a machine that's always on.</p>
        <div class="setup-choices">
          <div class="setup-choice recommended">
            <div class="choice-top">
              <Icon name="laptop" size={16} />
              <span class="choice-label">On this computer</span>
              <span class="badge badge-accent">Recommended</span>
            </div>
            <p class="choice-desc">The simplest way: it starts in the background and this app connects to it right away — no code to type.</p>
            <Show when={info.loading}>
              <div class="muted small row gap">
                <Tessellate size={9} /> Looking for Tessera on this computer…
              </div>
            </Show>
            <Show when={!info.loading && info()}>
              {(i) => (
                <Show
                  when={i().available}
                  fallback={
                    <div class="stack tight">
                      <p class="field-help">The Tessera server isn't installed on this computer yet. One command in a terminal installs it (no admin rights needed):</p>
                      <div class="cmd-line">
                        <code>{installCommandHere()}</code>
                        <button type="button" class="icon-btn sm" aria-label="Copy command" data-tip="Copy" onClick={() => void copy(installCommandHere())}>
                          <Icon name="copy" size={14} />
                        </button>
                      </div>
                      <p class="field-help">Then come back here and check again.</p>
                      <div>
                        <button type="button" class="btn btn-sm" onClick={() => void refetch()}>
                          <Icon name="refresh" size={14} /> Check again
                        </button>
                      </div>
                    </div>
                  }
                >
                  <label class="inline-toggle">
                    <input type="checkbox" checked={autostart()} onChange={(e) => setAutostart(e.currentTarget.checked)} />
                    {onWindows() ? "Keep Tessera running: start it when I sign in to Windows" : "Start Tessera when I log in"}
                  </label>
                  <Show when={onWindows()}>
                    <p class="field-help setup-keep-on">
                      {autostart()
                        ? "It keeps running with this window closed, and this PC stays awake while it's plugged in, so your phone can reach it any time. The screen still turns off."
                        : "It runs until you restart or shut down this PC. Your phone can't reach it while the PC is asleep or off."}
                    </p>
                  </Show>
                  <button type="button" class="btn btn-primary btn-lg" disabled={busy()} onClick={() => void start()}>
                    <Show when={busy()} fallback={<Icon name={error() ? "refresh" : "play"} size={16} />}>
                      <Spinner size={14} />
                    </Show>
                    {busy() ? "Starting Tessera…" : error() ? "Try again" : i().running ? "Connect to it" : "Start Tessera here"}
                  </button>
                  <Show when={busy()}>
                    <div class="setup-start-status" role="status" aria-live="polite">
                      <span class="setup-start-step">{step() ?? "Starting…"}</span>
                      <span class="muted small setup-start-time">{seconds()} s</span>
                      <button type="button" class="btn btn-sm" disabled={stopping()} onClick={() => void stopWaiting()}>
                        {stopping() ? "Stopping…" : "Stop waiting"}
                      </button>
                    </div>
                  </Show>
                </Show>
              )}
            </Show>
            <Show when={!info.loading && !info()}>
              <p class="field-help">This app can't start a server here. Choose “On another computer” to connect to one.</p>
            </Show>
            <Show when={error()}>
              <div class="form-error">{error()}</div>
            </Show>
          </div>
          <button type="button" class="setup-choice" onClick={props.onRemote}>
            <div class="choice-top">
              <Icon name="server" size={16} />
              <span class="choice-label">On another computer</span>
            </div>
            <p class="choice-desc">A home server, a Proxmox VM or a VPS that's always on. Your phone can reach it too.</p>
            <ol class="setup-mini-steps">
              <li>
                Install Tessera there with one command: <code class="setup-cmd">{INSTALL_COMMAND}</code>
              </li>
              <li>Its setup ends with a pairing link (and a QR code for your phone).</li>
              <li>Paste the link here.</li>
            </ol>
            <span class="link-btn">
              Connect to a server <Icon name="arrowRight" size={13} />
            </span>
          </button>
        </div>
      </div>
    </div>
  );
}

// ── The guided setup (connected) ───────────────────────────────────────────

type StepId = "models" | "reach" | "phone" | "done";

interface StepDef {
  id: StepId;
  label: string;
  title: string;
  icon: IconName;
  optional?: boolean;
}

/** The same steps as `tessera setup`, in the same words (its service step is the desktop app's "Start Tessera here"). */
const STEPS: StepDef[] = [
  { id: "models", label: "Your AI", title: "How should Tessera get its AI?", icon: "sparkles" },
  { id: "reach", label: "Where", title: "Where you'll use it", icon: "home", optional: true },
  touchUi()
    ? { id: "phone", label: "Devices", title: "Add another device", icon: "laptop", optional: true }
    : { id: "phone", label: "Your phone", title: "Pair your phone", icon: "phone", optional: true },
  { id: "done", label: "Done", title: "You're set", icon: "shieldCheck" },
];

/** Where setup was left on this device, per server: the step and those passed or skipped. Only step names (nothing typed in a step). */
interface SetupProgress {
  v: 1;
  step: StepId;
  marked: StepId[];
}
const progressKey = () => `tessera.setup.${state.conn.serverId || "here"}`;
const isStep = (id: unknown): id is StepId => STEPS.some((s) => s.id === id);
/** What was kept, checked: anything else (another version, a hand edit) is set aside and setup starts over.
 *  `none`: nothing was kept at all (a first run, or an update from a version that didn't keep it). */
function savedProgress(): { step: StepId; marked: StepId[]; none: boolean } {
  const raw: unknown = lsGet<unknown>(progressKey(), null);
  const o = raw && typeof raw === "object" && !Array.isArray(raw) ? (raw as Record<string, unknown>) : null;
  // Kept before it had a version: the same two fields, taken as they are.
  const before = !!o && o.v === undefined && typeof o.step === "string" && Array.isArray(o.marked);
  if (!o || (o.v !== 1 && !before)) {
    let none = false;
    try {
      none = localStorage.getItem(progressKey()) === null;
    } catch {
      /* no storage: like a first run every time */
    }
    if (!none) lsSet(progressKey(), undefined);
    return { step: "models", marked: [], none };
  }
  return {
    step: isStep(o.step) ? o.step : "models",
    marked: Array.isArray(o.marked) ? o.marked.filter(isStep) : [],
    none: false,
  };
}

export function SetupWizard(props: { onClose: () => void }) {
  const [status, { refetch }] = createResource(() => api.setupStatus().catch(() => null));
  // Closing the app, or an update restarting it, picks setup up where it was left.
  const saved = savedProgress();
  const [step, setStep] = createSignal<StepId>(saved.step);
  const [marked, setMarked] = createSignal<Set<StepId>>(new Set(saved.marked));
  createEffect(() => lsSet(progressKey(), { v: 1, step: step(), marked: [...marked()] } satisfies SetupProgress));
  const forgetProgress = () => lsSet(progressKey(), undefined);
  const [finishing, setFinishing] = createSignal(false);
  const idx = () => STEPS.findIndex((s) => s.id === step());
  const def = () => STEPS[idx()];
  const mark = (id: StepId) => setMarked((m) => new Set([...m, id]));

  /** A step counts as done when it's configured (per the server) or passed here. */
  const isDone = (id: StepId): boolean => {
    const s = status();
    if (marked().has(id)) return true;
    if (!s) return false;
    if (id === "models") return s.has_model;
    if (id === "reach") return s.lan || s.remote;
    if (id === "phone") return s.devices > 1;
    return false;
  };
  // Nothing kept here (an update from a version that didn't keep its place): open at the first step the
  // server doesn't have yet, once it says, unless the user has started on this one already.
  let placed = !saved.none;
  const touch = () => (placed = true);
  createEffect(() => {
    const s = status();
    if (placed || !s) return;
    placed = true;
    if (step() !== "models" || marked().size) return;
    setStep(STEPS.find((d) => d.id !== "done" && !isDone(d.id))?.id ?? "done");
  });

  const go = (id: StepId) => {
    setStep(id);
    void refetch();
  };
  const next = () => {
    mark(step());
    const n = STEPS[idx() + 1];
    if (n) go(n.id);
  };
  const back = () => {
    const p = STEPS[idx() - 1];
    if (p) go(p.id);
  };

  // Escape never loses setup halfway: it only closes from the last step or "Later".
  // The phone's back gesture closes it (it comes back on the next launch).
  onMount(() => {
    onCleanup(useBackToClose(props.onClose));
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && step() === "done") props.onClose();
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
  });

  /** Done: a first prompt to try, or a settings page to open (web search, voice…). */
  const finish = async (prompt?: string, settings?: SettingsTab) => {
    setFinishing(true);
    try {
      await api.completeSetup();
      forgetProgress();
    } catch (e) {
      toastError(e, "Could not save setup");
      return;
    } finally {
      setFinishing(false);
    }
    props.onClose();
    if (settings) return openSettings(settings);
    if (prompt) {
      const f = workspace.focused();
      const id = f && isUnusedPane(f) ? f : newChat({ newPane: true });
      if (id) {
        setDraft(id, prompt);
        focusComposer(id);
      }
    }
    setTimeout(startTour, 500);
  };

  const later = async () => {
    // Skipping entirely still counts as "seen": Settings → Run setup again brings it back.
    try {
      await api.completeSetup();
      forgetProgress();
    } catch {
      /* older server */
    }
    props.onClose();
  };

  return (
    <div class="setup setup-wizard" role="dialog" aria-modal="true" aria-label="Set up Tessera" onPointerDown={touch} onKeyDown={touch}>
      <div class="setup-hero">
        <SetupMosaic bands={STEPS.length} done={(b) => isDone(STEPS[b].id) || (STEPS[b].id === "done" && step() === "done")} />
      </div>
      <div class="setup-card wide">
        <nav class="setup-steps" aria-label="Setup steps">
          <For each={STEPS}>
            {(s, i) => (
              <button
                type="button"
                class="setup-step"
                classList={{ on: step() === s.id, done: isDone(s.id) }}
                aria-current={step() === s.id ? "step" : undefined}
                onClick={() => go(s.id)}
              >
                <span class="setup-step-sq">
                  <Show when={isDone(s.id)} fallback={<span class="n">{i() + 1}</span>}>
                    <Icon name="check" size={11} stroke={2.2} />
                  </Show>
                </span>
                <span class="setup-step-label">{s.label}</span>
              </button>
            )}
          </For>
        </nav>
        <div class="setup-body">
          <div class="setup-head">
            <div>
              <p class="inscription">
                Step {idx() + 1} of {STEPS.length}
                <Show when={def().optional}> · optional</Show>
              </p>
              <h1 class="serif">{def().title}</h1>
            </div>
            <Show when={step() !== "done"}>
              <button type="button" class="btn btn-sm btn-ghost setup-later" onClick={() => void later()}>
                Finish later
              </button>
            </Show>
          </div>
          <div class="setup-content">
            <Switch>
              <Match when={step() === "models"}>
                <ModelsStep status={status()} onChanged={() => void refetch()} />
              </Match>
              <Match when={step() === "reach"}>
                <p class="setup-lede">
                  At home, your phone and laptop connect over your Wi-Fi; away from home, through a private tunnel. Each device pairs once, and
                  everything is end-to-end encrypted either way.
                </p>
                <div class="stack">
                  <LanToggle status={status() ?? null} onChanged={() => void refetch()} />
                  <h4 class="section-title setup-away">
                    From anywhere <span class="opt">optional</span>
                  </h4>
                  <RemoteAccessPanel onPair={() => { mark("reach"); go("phone"); }} onChanged={() => void refetch()} />
                </div>
              </Match>
              <Match when={step() === "phone"}>
                <PhoneStep status={status()} onChanged={() => void refetch()} onReach={() => go("reach")} />
              </Match>
              <Match when={step() === "done"}>
                <DoneStep status={status()} busy={finishing()} onFinish={(p, tab) => void finish(p, tab)} />
              </Match>
            </Switch>
          </div>
          <Show when={step() !== "done"}>
            {/* A guided path, not a second home: each step says where its settings live. */}
            <p class="setup-later-hint">
              You can change this later in Settings → {step() === "models" ? "Models" : step() === "reach" ? "Connection" : "Devices"}.
            </p>
            <div class="setup-foot">
              <Show when={idx() > 0}>
                <button type="button" class="btn btn-ghost" onClick={back}>
                  <Icon name="arrowLeft" size={14} /> Back
                </button>
              </Show>
              <span class="spacer" />
              <Show when={def().optional && !isDone(step())}>
                <button type="button" class="btn btn-ghost" onClick={next}>
                  Skip
                </button>
              </Show>
              <button type="button" class="btn btn-primary" disabled={step() === "models" && !defaultModel()} onClick={next}>
                Continue <Icon name="arrowRight" size={14} />
              </button>
            </div>
          </Show>
        </div>
      </div>
    </div>
  );
}

/** A server on this computer itself. */
const onThisComputer = (url: string) => /^https?:\/\/(localhost|127\.|\[::1\])/i.test(url);
/** A model server on this computer or the home network, as opposed to a hosted API. */
const ownComputer = (url: string) => /^https?:\/\/(localhost|127\.|\[::1\]|10\.|192\.168\.|172\.(1[6-9]|2\d|3[01])\.|[^/:]*\.local\b)/i.test(url);

/**
 * Your AI, the ways `tessera setup` offers: a plan you already pay for
 * (Claude through your own Claude Code, ChatGPT), a model on your own
 * computers (looked for at once, on this computer and its network), or an
 * API key; then the model new chats use.
 */
function ModelsStep(props: { status: SetupStatus | null | undefined; onChanged: () => void }) {
  const [looking, setLooking] = createSignal(false);
  const [looked, setLooked] = createSignal(false);
  const [lookError, setLookError] = createSignal<string | null>(null);
  const [keyForm, setKeyForm] = createSignal<"anthropic" | "other" | null>(null);
  const [oauth, setOauth] = createSignal<{ provider: string; start: OAuthStart } | null>(null);
  const [signingIn, setSigningIn] = createSignal(false);
  const models = createMemo(() => state.providers.filter((p) => p.authenticated).flatMap((p) => p.models.map((m) => ({ ...m, provider: p.id, providerName: p.name }))));
  const current = () => {
    const ref = props.status?.default_model ?? state.server?.default_model ?? null;
    return modelReady(ref) ? ref : null;
  };
  const found = createMemo(() => state.providers.filter((p) => p.kind === "openai_chat" && p.authenticated && p.models.length > 0 && ownComputer(p.base_url)));
  const codex = () => state.providers.find((p) => p.kind === "openai_codex");
  const pick = async (provider: string, model: string) => {
    try {
      await api.setDefaultModel({ provider, model });
      setState("server", "default_model", { provider, model });
      props.onChanged();
    } catch (e) {
      toastError(e, "Could not set the default model");
    }
  };
  const look = async () => {
    setLooking(true);
    setLookError(null);
    try {
      const list = await api.detectLocalModels();
      setState("providers", list);
      // One on this computer can answer at once; one elsewhere on the network may be someone else's, so it waits for a click.
      const here = list.find((p) => p.authenticated && p.models.length && onThisComputer(p.base_url));
      if (!current() && here) await pick(here.id, here.models[0].id);
    } catch (e) {
      setLookError(errorMessage(e));
    } finally {
      setLooking(false);
      setLooked(true);
    }
  };
  const signIn = async () => {
    const id = codex()?.id ?? "openai-codex";
    setSigningIn(true);
    try {
      const start = await api.startOauth(id);
      setOauth({ provider: id, start });
      if (!start.user_code && /^https:\/\//i.test(start.url)) void openExternal(start.url);
    } catch (e) {
      toastError(e, "Could not start sign-in");
    } finally {
      setSigningIn(false);
    }
  };
  void refreshProviders();
  // Like `tessera setup`: see what's there before asking anything.
  onMount(() => {
    if (!current()) void look();
  });
  return (
    <div class="stack">
      <p class="setup-lede">A plan you already pay for, a free model on your own computers, or an API key. Keys stay on your server; the AI never sees them.</p>
      <div class="setup-model-now" classList={{ ok: !!current() }}>
        <Show when={current()} fallback={<><Icon name="alert" size={15} /><span>No AI is connected yet. Sign in or add an API key below to start chatting.</span></>}>
          <Icon name="check" size={15} />
          <span>
            New chats use <b>{modelLabel(current())}</b>.
          </span>
        </Show>
      </div>
      <div class="setup-ways">
        <ClaudeNote />
        <div class="setup-way setup-way-chatgpt" classList={{ ok: !!codex()?.authenticated }}>
          <Icon name={codex()?.authenticated ? "check" : "user"} size={15} />
          <div class="grow">
            <b>ChatGPT · your Plus or Pro plan</b>
            <div class="setup-way-sub">{codex()?.authenticated ? "Signed in. Its models are in the list below." : "Sign in once, in a browser on any device."}</div>
          </div>
          <Show when={!codex()?.authenticated}>
            <button type="button" class="btn btn-sm" disabled={signingIn()} onClick={() => void signIn()}>
              <Show when={signingIn()}>
                <Spinner size={13} />
              </Show>
              Sign in
            </button>
          </Show>
        </div>
        <div class="setup-way setup-way-own" classList={{ ok: found().length > 0 }}>
          <Icon name={found().length ? "check" : "cpu"} size={15} />
          <div class="grow">
            <b>Your own computers</b> <span class="setup-way-tag">private and free</span>
            <Show
              when={found().length}
              fallback={
                <div class="setup-way-sub" aria-live="polite">
                  <Show when={looking()} fallback={lookError() ?? (looked() ? "Nothing answered on this computer or your network. Start llama.cpp, Ollama or LM Studio, then look again." : "llama.cpp, Ollama or LM Studio, on this computer or another one at home.")}>
                    <span class="row gap">
                      <Spinner size={12} /> Looking on this computer and your network…
                    </span>
                  </Show>
                </div>
              }
            >
              <ul class="setup-found">
                <For each={found()}>
                  {(p) => (
                    <li>
                      <span class="grow">
                        {p.name} <span class="muted">· {plural(p.models.length, "model", "models")}</span>
                      </span>
                      <Show when={current()?.provider === p.id} fallback={<button type="button" class="btn btn-sm" onClick={() => void pick(p.id, p.models[0].id)}>Use</button>}>
                        <span class="badge badge-ok">In use</span>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </div>
          <button type="button" class="btn btn-sm btn-ghost" disabled={looking()} onClick={() => void look()}>
            <Icon name="refresh" size={13} /> {looked() || found().length ? "Look again" : "Look"}
          </button>
        </div>
        <div class="setup-way setup-way-key">
          <Icon name="key" size={15} />
          <div class="grow">
            <b>An API key</b>
            <div class="setup-way-sub">OpenRouter (hundreds of models with one key), OpenAI, Anthropic, or another server's address.</div>
            <div class="setup-way-chips">
              <button type="button" class="chip" classList={{ active: keyForm() === "other" }} onClick={() => setKeyForm(keyForm() === "other" ? null : "other")}>
                OpenRouter, OpenAI or another
              </button>
              <button type="button" class="chip" classList={{ active: keyForm() === "anthropic" }} onClick={() => setKeyForm(keyForm() === "anthropic" ? null : "anthropic")}>
                Anthropic
              </button>
            </div>
          </div>
        </div>
        <Show when={keyForm() === "other"}>
          <OpenAIForm onDone={() => setKeyForm(null)} />
        </Show>
        <Show when={keyForm() === "anthropic"}>
          <AnthropicForm onDone={() => setKeyForm(null)} />
        </Show>
      </div>
      <Show when={models().length}>
        <label class="field">
          <span class="field-label">Model for new chats</span>
          <select
            class="input"
            value={current() ? `${current()!.provider}/${current()!.model}` : ""}
            onChange={(e) => {
              const [p, ...m] = e.currentTarget.value.split("/");
              if (p && m.length) void pick(p, m.join("/"));
            }}
          >
            <Show when={!current()}>
              <option value="">Choose a model…</option>
            </Show>
            <For each={models()}>{(m) => <option value={`${m.provider}/${m.id}`}>{`${m.name} — ${m.providerName}`}</option>}</For>
          </select>
          <span class="field-help">Each chat can still switch. More choices (a cheaper model for small jobs, fallbacks) are in Settings → Models.</span>
        </label>
      </Show>
      <Show when={oauth()} keyed>
        {(o) => <OAuthDialog provider={o.provider} start={o.start} onClose={() => setOauth(null)} />}
      </Show>
    </div>
  );
}

const ANTHROPIC_KEYS_URL = "https://console.anthropic.com/settings/keys";

const CLAUDE_CODE_SETUP_URL = "https://code.claude.com/docs/en/setup";

/** A server sentence with `commands` in backticks, shown as code. */
const withCode = (text: string) => text.split("`").map((part, i) => (i % 2 ? <code>{part}</code> : part));

/**
 * Claude, the allowed ways: a Pro/Max plan through the user's own Claude Code
 * on the server (Tessera runs it; it never signs in to Claude itself), or an
 * Anthropic API key. Asking once also lists Claude Code's models when found.
 */
function ClaudeNote() {
  const ask = async (refresh: boolean) => {
    const info = await api.claudeCodeStatus(refresh);
    if (info.installed) await refreshProviders();
    return info;
  };
  // Older servers don't know the request: then only the guidance shows.
  const [info, { mutate }] = createResource(() => ask(false).catch(() => null));
  const [checking, setChecking] = createSignal(false);
  const check = async () => {
    setChecking(true);
    try {
      mutate(await ask(true));
    } catch (e) {
      toastError(e, "Could not check Claude Code");
    } finally {
      setChecking(false);
    }
  };
  const ready = () => !!info()?.signed_in && !info()?.api_key;
  const plan = () => (info()?.plan ? ` (${info()!.plan!.replace(/^./, (c) => c.toUpperCase())})` : "");
  const link = (href: string, label: string) => (
    <button type="button" class="link-btn" onClick={() => void openExternal(href)}>
      {label} <Icon name="external" size={11} />
    </button>
  );
  return (
    <div class="claude-note" classList={{ ok: ready() }}>
      <Icon name={ready() ? "check" : "sparkles"} size={15} />
      <div class="grow">
        <Show
          when={ready()}
          fallback={
            <>
              <b>Claude · your Pro or Max plan.</b>{" "}
              <Show
                when={info()}
                fallback={
                  <>
                    Use your plan through your own Claude Code: install it on the computer running Tessera and sign in once (run <code>claude</code>);
                    chats then pick a “Claude (your subscription)” model.
                  </>
                }
              >
                {(i) => <>{withCode(i().hint)}</>}
              </Show>{" "}
              Or add an Anthropic API key below. {link(ANTHROPIC_KEYS_URL, "Get an API key")}
              <span class="claude-actions">
                <Show when={!info()?.installed}>{link(CLAUDE_CODE_SETUP_URL, "Install Claude Code")}</Show>
                <Show when={info() !== null && !info.loading}>
                  <button type="button" class="link-btn" disabled={checking()} onClick={() => void check()}>
                    {checking() ? "Checking…" : "Check again"}
                  </button>
                </Show>
              </span>
            </>
          }
        >
          <b>Claude is ready{plan()}</b> — your subscription, through your own Claude Code
          <Show when={info()?.account}> ({info()!.account})</Show>. Pick a “Claude (your subscription)” model for new chats, or keep the one above.
          Tessera's notes, whiteboards, memory and other tools come built in.
        </Show>
      </div>
    </div>
  );
}

/** Pairing the phone: the QR code, and where the phone can reach this server from. */
function PhoneStep(props: { status: SetupStatus | null | undefined; onChanged: () => void; onReach: () => void }) {
  const [remote, { refetch }] = useRemoteStatus();
  const [checking, setChecking] = createSignal(false);
  const check = async () => {
    setChecking(true);
    await refetch();
    setChecking(false);
  };
  const home = () => !props.status || props.status.lan;
  const away = () => !!props.status?.remote || !!remote()?.urls.length;
  const tunnel = () => remote()?.tunnel;
  // On a phone this step adds a computer or tablet, not "your phone".
  const who = touchUi() ? "Your other devices" : "Your phone";
  return (
    <div class="stack">
      <p class="setup-lede">
        {touchUi()
          ? "To use Tessera on a computer or tablet too, open the app there and enter this code (or scan it)."
          : !home() && !away()
            ? "Once your phone can reach this computer (see below), open the Tessera app on it and scan the QR code that appears here."
            : "Open the Tessera app on your phone and scan this code. It pairs your phone and sets up the encrypted connection in one go."}
      </p>
      <Switch>
        <Match when={!home() && !away()}>
          <div class="reach-note warn">
            <Icon name="alert" size={14} />
            <span>
              Right now only this computer can reach Tessera. Let devices on your network connect below, or{" "}
              <button type="button" class="link-btn" onClick={() => props.onReach()}>
                set up remote access
              </button>
              .
            </span>
          </div>
        </Match>
        <Match when={tunnel() && !tunnel()!.connected}>
          <div class="reach-note warn">
            <Icon name="cloud" size={14} />
            <div class="grow">
              Away from home, {touchUi() ? "devices connect" : "your phone connects"} through <b>https://{tunnel()!.hostname}</b>, which works once the
              tunnel connects.
              {home() ? " Pairing on this network works now." : ""}
              <TunnelLine tunnel={tunnel()!} onCheck={() => void check()} checking={checking()} />
            </div>
          </div>
        </Match>
        <Match when={!away()}>
          <div class="reach-note">
            <Icon name="home" size={14} />
            <span>
              {who} can pair while on this network (Wi-Fi). To use Tessera away from home too,{" "}
              <button type="button" class="link-btn" onClick={() => props.onReach()}>
                set up remote access
              </button>
              .
            </span>
          </div>
        </Match>
        <Match when={away()}>
          <div class="reach-note ok">
            <Icon name="globe" size={14} />
            <span>
              {who} can reach Tessera {home() ? "at home and " : ""}from anywhere
              <Show when={remote()?.urls[0]}>
                {" "}(<span class="mono">{remote()!.urls[0]}</span>)
              </Show>
              . The code brings the addresses and keys along.
            </span>
          </div>
        </Match>
      </Switch>
      {/* Set in the step before; here only when nothing can reach the server yet. */}
      <Show when={!home() && !away()}>
        <LanToggle
          status={props.status ?? null}
          onChanged={() => {
            props.onChanged();
            void refetch();
          }}
        />
      </Show>
      {/* A fresh code whenever the ways in change: its QR code carries the addresses that work now. */}
      <Show when={`${home()}·${away()}`} keyed>
        <PairPanel autostart />
      </Show>
    </div>
  );
}

/** The summary: what keeps them safe, in plain words, and a first thing to try. */
function DoneStep(props: { status: SetupStatus | null | undefined; busy: boolean; onFinish: (prompt?: string, settings?: SettingsTab) => void }) {
  const s = () => props.status;
  const sandbox = () => s()?.sandbox ?? "";
  const items = (): { icon: IconName; title: string; body: string; tone?: "warn" }[] => [
    {
      icon: "lock",
      title: "Encrypted end to end",
      body: "Your devices talk to Tessera with their own keys. Even through a tunnel, nobody in between can read what you send.",
    },
    sandbox().startsWith("on")
      ? { icon: "shieldCheck", title: "Commands run in a sandbox", body: "They can't touch Tessera's own settings, your passwords store or your desktop session." }
      : sandbox().startsWith("partial")
        ? { icon: "shield", title: "Commands run in a partial sandbox", body: "Most of the system is fenced off, but this kernel can't lock everything down.", tone: "warn" }
        : { icon: "shieldAlert", title: "No sandbox on this system", body: "Commands run as you. Keep approvals on and review what the agent wants to do.", tone: "warn" },
    newChatFreedom() === "auto"
      ? { icon: "hand", title: "Approvals are off (YOLO)", body: "New chats act without asking. Change it in Settings → Privacy & safety.", tone: "warn" }
      : newChatFreedom() === "ask"
        ? {
            icon: "hand",
            title: "It asks before it changes anything",
            body: "Every command and every change to your files waits for your OK. You can give it more freedom in Settings → Privacy & safety.",
          }
        : newChatFreedom() === "read_only"
          ? { icon: "hand", title: "It only reads", body: "New chats can look things up and answer, but not change files or run commands. Change it in Settings → Privacy & safety." }
          : {
              icon: "hand",
              title: "It asks before risky actions",
              body: "Deleting files, installing software and publishing changes need your OK. You can change this for any chat.",
            },
    { icon: "key", title: "Secrets stay secret", body: "The AI never sees your passwords and tokens; a command gets one only when it names it." },
    { icon: "veil", title: "Private chats leave nothing behind", body: "They're held in memory only and stay on the device you start them on — no history, no memory, nothing on disk." },
    s()?.remote
      ? { icon: "globe", title: "Reachable from anywhere", body: "Behind Cloudflare Access or your own tunnel — which only ever sees encrypted traffic." }
      : s()?.lan
        ? { icon: "home", title: "Only on your home network", body: "Nothing is exposed to the internet. Add remote access any time in Settings → Connection." }
        : { icon: "laptop", title: "Only on this computer", body: "Other devices can't connect yet. Add your home network or remote access in Settings → Connection." },
  ];
  const MORE: { tab: SettingsTab; icon: IconName; label: string }[] = [
    { tab: "search", icon: "globe", label: s()?.web_search === "searxng" ? "Web search (SearXNG)" : "Web search (DuckDuckGo)" },
    { tab: "voice", icon: "mic", label: s()?.voice ? "Voice typing (on)" : "Voice typing" },
    { tab: "models", icon: "sparkles", label: "Models" },
  ];
  const PROMPTS = ["Explain how this project is organized", "Plan my week and suggest what to do first", "Research a topic and give me the short version"];
  return (
    <div class="stack">
      <div class="done-mark">
        <SettleTiles size={11} />
      </div>
      <ul class="safety-list">
        <For each={items()}>
          {(it) => (
            <li classList={{ warn: it.tone === "warn" }}>
              <span class="safety-icon">
                <Icon name={it.icon} size={16} />
              </span>
              <div>
                <div class="safety-title">{it.title}</div>
                <div class="safety-body">{it.body}</div>
              </div>
            </li>
          )}
        </For>
      </ul>
      <div class="field">
        <span class="field-label">Try one to start</span>
        <div class="setup-prompts">
          <For each={PROMPTS}>
            {(p) => (
              <button type="button" class="suggestion" disabled={props.busy} onClick={() => props.onFinish(p)}>
                <Icon name="arrowRight" size={13} />
                <span>{p}</span>
              </button>
            )}
          </For>
        </div>
      </div>
      <div class="field">
        <span class="field-label">More, any time</span>
        <div class="setup-more">
          <For each={MORE}>
            {(m) => (
              <button type="button" class="chip" disabled={props.busy} onClick={() => props.onFinish(undefined, m.tab)}>
                <Icon name={m.icon} size={13} />
                {m.label}
              </button>
            )}
          </For>
        </div>
      </div>
      <div class="setup-foot">
        <span class="spacer" />
        <button type="button" class="btn btn-primary btn-lg" disabled={props.busy} onClick={() => props.onFinish()}>
          <Show when={props.busy} fallback={<Icon name="check" size={16} />}>
            <Spinner size={14} />
          </Show>
          Start using Tessera
        </button>
      </div>
    </div>
  );
}
