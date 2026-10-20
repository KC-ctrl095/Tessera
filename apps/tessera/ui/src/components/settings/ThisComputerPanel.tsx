// Settings → Devices → This computer (the desktop app only): whether
// Tessera's agents may use this computer, and what they may do here. All of
// it goes through the app's local commands, which run `tessera node` on
// this computer; none of it is a server request, so no other device can
// turn this on or widen it. Choices that allow more ask first; choices that
// allow less apply at once.
import { For, Show, createMemo, createResource, createSignal, onCleanup } from "solid-js";
import type { FolderAccess } from "../../proto/FolderAccess";
import {
  ADMIN_CHOICES,
  ADMIN_WARNING,
  machinePlatform,
  TERMINAL_HELP,
  TIME_LIMITS,
  WEBVIEWS_HELP,
  isWindowsOs,
  compactLog,
  fmtLimit,
  localHome,
  localListDir,
  logLine,
  nodeAccess,
  nodeAdminPassword,
  nodeEnable,
  nodeLog,
  nodePreset,
  nodeRestore,
  nodeService,
  nodeSetScope,
  nodeStartupChanges,
  nodeGuardInstall,
  nodeStop,
  setActivityFocus,
  type MachineAsk,
  type NodeStatus,
  type MachineInfo,
  type MachineScope,
  type NodeLogEntry,
  type ThisComputer,
} from "../../lib/machines";
import { ASK_CHOICES, presetsFor, askInfo, askRank, presetInfo, type PresetChoice } from "../../lib/machine-settings";
import { browseDir, confirmDialog, errorMessage, promptDialog, setSettingsTab, toast, toastError } from "../../lib/ui";
import { fmtDateTime, fmtDuration, relTime, tildify } from "../../lib/util";
import { Icon, Spinner, type IconName } from "../icons";
import { CopyButton, Segmented } from "../primitives";
import { machineBots } from "../bot/presets";
import { ACCESS_WORDS, AccessPicker } from "./AccessPicker";
import { SwitchRow } from "./kit";

function Cmd(props: { text: string }) {
  return (
    <span class="sec-cmd">
      <code>{props.text}</code>
      <CopyButton text={() => props.text} label="Copy command" size={13} class="sm" />
    </span>
  );
}

export function statusLine(s: NodeStatus): { icon: IconName; tone: "ok" | "warn" | "bad" | "off"; title: string; detail: string } {
  if (s.revoked) return { icon: "shieldOff", tone: "bad", title: "Agents were stopped from another device", detail: "Agents can't use this computer. Turn it on again here to let them." };
  if (!s.enabled) return { icon: "laptop", tone: "off", title: "Off", detail: "Pick below what they may do here to let them use it." };
  if (s.scope.stopped) return { icon: "hand", tone: "bad", title: "Stopped here", detail: "You stopped every agent on this computer. Nothing runs until you resume it here." };
  if (s.paused) return { icon: "pause", tone: "warn", title: "Paused from another device", detail: "Agents can't use it until it's resumed (on any device, under Devices)." };
  if (!s.running) return { icon: "alertCircle", tone: "warn", title: "On, but not running", detail: s.error || "Keep this app open, or turn on “Keep it on in the background”." };
  if (!s.connected) return { icon: "wifiOff", tone: "warn", title: "Connecting…", detail: s.error || (s.server ? `Trying to reach ${s.server}.` : "Waiting for the server.") };
  const busy = s.calls ? ` ${s.calls === 1 ? "One action is" : `${s.calls} actions are`} running now.` : "";
  return { icon: "shieldCheck", tone: "ok", title: "On", detail: `Your agents can use ${s.name}${s.server ? ` through ${s.server}` : ""}, as far as you allow below.${busy}` };
}

/** Presets from least to most: switching up asks first. */
const ORDER: PresetChoice[] = ["off", "look_only", "work_in_folders", "full_access", "desktop_control"];

// Kept here (Settings loads when opened), not with the rest of lib/machines (first load).
/** The warning before desktop control (the same text as the terminal's), on Windows. */
const DESKTOP_WARNING_WINDOWS =
  "Desktop control is close to giving the agent your account. Agents can see your screens and use your mouse and keyboard while you're signed in: open programs, click, type and press shortcuts as you would, in anything you can reach yourself. They can't see or use the lock screen or Windows security prompts, or programs running as administrator, and it stays off while Tessera itself runs as administrator. Commands on Windows aren't sandboxed in any case (they need Full access). What agents type and the screenshots they take go to the chat's model and stay in the chat, like anything a tool returns; this computer's activity log and the Stop bar say only what kind of action it was. A bar at the top of your screen always shows what they do; its Stop button, or the Stop key it shows (usually Ctrl+Alt+Esc), stops every agent here at once until you resume. Turn it on only for a computer you'd let this agent use as you.";

/** The warning before desktop control (the same text as the terminal's). */
const DESKTOP_WARNING =
  "Desktop control is close to giving the agent your account. Agents could change what runs when you log in (shell startup files, autostart, your window manager's settings) and use your desktop session: open windows and programs, reload your window manager, change settings. Programs they start that way run as you, outside Tessera's sandbox: such a program could read your passwords and keys and change this computer's machine settings. Commands themselves still can't, and every startup file they change is backed up first so you can put it back. Turn it on only for a computer you'd let this agent use as you.";

export function ThisComputerPanel(props: {
  info: ThisComputer;
  setInfo: (s: ThisComputer) => void;
  refetch: () => void;
  /** How the server sees this computer as a machine (recent activity). */
  machine: MachineInfo | null;
}) {
  const [home] = createResource(() => localHome().catch(() => null));
  /** What is being saved: "on", "preset", "folders", "ask", "who", "service", "advanced". */
  const [busy, setBusy] = createSignal<string | null>(null);
  const tilde = (p: string) => tildify(p, home() ?? props.info.node?.home ?? undefined);

  const poll = setInterval(() => {
    const s = props.info.node;
    if (s && s.enabled && (!s.connected || !s.running) && !busy()) props.refetch();
  }, 8000);
  onCleanup(() => clearInterval(poll));

  const run = async (what: string, fn: () => Promise<ThisComputer | null>, fail: string) => {
    setBusy(what);
    try {
      const s = await fn();
      if (s) props.setInfo(s);
      return true;
    } catch (e) {
      toastError(e, fail);
      return false;
    } finally {
      setBusy(null);
    }
  };

  const node = () => props.info.node!;
  const scope = () => node().scope;
  const preset = (): PresetChoice => (node().enabled ? node().preset : "off");
  const canTurnOn = () => node().paired || !!props.info.appServer;
  const platform = () => machinePlatform(node().os, node().features);
  const workCommands = () => platform().sandbox && node().sandbox_ok;
  const choices = () => presetsFor(node().os, node().features, node().sandbox_ok);

  // ── on / off and the preset ──
  const setOn = (on: boolean) => run("on", () => nodeEnable(on), on ? "Couldn't turn it on" : "Couldn't turn it off");
  // After a Stop (the Stop bar, its key or `tessera node stop`): only here.
  const resume = () => run("resume", () => nodeStop(false), "Couldn't resume it");
  const installGuard = () => run("guard", () => nodeGuardInstall(), "Couldn't install Tessera Guard");

  /** The choice being applied (its spinner). */
  // This computer runs Windows: desktop control means its screen, mouse and keyboard there.
  const win = () => isWindowsOs(node().os || "");
  const desktopOk = () => platform().desktop;
  const deskWarning = () => (win() ? DESKTOP_WARNING_WINDOWS : DESKTOP_WARNING);
  const [choosing, setChoosing] = createSignal<PresetChoice | null>(null);
  const choosePreset = async (p: PresetChoice) => {
    if (!choices().some((x) => x.value === p) && p !== "custom") return;
    setChoosing(p);
    if (p === preset() && p !== "custom" && !(p === "work_in_folders" && !scope().folders.length)) return;
    if (p === "off") return void setOn(false);
    if (p === "custom") return void setAdvanced(true);
    const up = node().preset === "custom" || ORDER.indexOf(p) > ORDER.indexOf(preset());
    if (p === "desktop_control") {
      if (!desktopOk()) return;
      const ok = await confirmDialog({ title: "Turn on desktop control?", body: deskWarning(), confirmLabel: "Turn on desktop control", danger: true });
      if (!ok) return;
    } else if (up) {
      const info = choices().find((x) => x.value === p) ?? presetInfo(p, node().os);
      const ok = await confirmDialog({
        title: `Switch to ${info.label}?`,
        body:
          info.sentence +
          (p === "full_access" ? " Agents could change any of your own files, so choose this only for an agent you trust with them." : "") +
          " You can switch back here at any time.",
        confirmLabel: `Use ${info.label}`,
        danger: !!info.warn,
      });
      if (!ok) return;
    }
    if (p === "work_in_folders" && !scope().folders.length) return void (await addFolder(p));
    if (!node().enabled && !(await run("on", () => nodeEnable(true), "Couldn't turn it on"))) return;
    await run("preset", () => nodePreset(p as Exclude<PresetChoice, "off" | "custom">), `Couldn't switch to ${presetInfo(p).label}`);
  };

  // ── folders (Look only, Work in folders) ──
  const folderPreset = () => preset() === "look_only" || preset() === "work_in_folders";
  const addFolder = async (forPreset: "look_only" | "work_in_folders" = preset() as "look_only" | "work_in_folders") => {
    const h = home() ?? undefined;
    const path = await browseDir(h ?? "/", "A folder agents may use on this computer", { list: localListDir, home: h });
    if (!path) return;
    const clean = path.replace(/\/+$/, "") || "/";
    if (scope().folders.some((f) => f.path === clean)) return;
    const work = forPreset === "work_in_folders";
    const broad = clean === "/" || (!!h && (clean === h || h.startsWith(clean + "/")));
    const ok = await confirmDialog({
      title: work ? `Let agents work in ${tilde(clean)}?` : `Let agents read ${tilde(clean)}?`,
      body: broad
        ? `That's ${clean === "/" ? "everything on this computer" : "all of your files"}. ${work ? "Agents could read and change any of it" : "Agents could read any of it"} (Tessera's own files stay out of reach). A project folder is usually enough.`
        : work
          ? "Agents could read, change and delete files in it. " + (workCommands() ? "They can also run commands there. " : "Commands aren't available with this choice on this computer. ") + "Tessera's own files stay out of reach."
          : "Agents could read any file in it. Tessera's own files stay out of reach.",
      confirmLabel: work ? "Allow" : "Allow reading",
      danger: broad || work,
    });
    if (!ok) return;
    if (!node().enabled && !(await run("on", () => nodeEnable(true), "Couldn't turn it on"))) return;
    await run("folders", () => nodePreset(forPreset, [...scope().folders.map((f) => f.path), clean]), "Couldn't add the folder");
  };
  const removeFolder = (path: string) =>
    run(
      "folders",
      () =>
        folderPreset()
          ? nodePreset(preset() as "look_only" | "work_in_folders", scope().folders.filter((f) => f.path !== path).map((f) => f.path))
          : nodeSetScope({ ...scope(), folders: scope().folders.filter((f) => f.path !== path) }),
      "Couldn't remove the folder",
    );

  // ── ask first ──
  const setAsk = async (ask: MachineAsk) => {
    if (ask === scope().ask) return;
    if (askRank(ask) > askRank(scope().ask)) {
      const desk = scope().desktop;
      const ok = await confirmDialog({
        title: ask === "chat" ? "Never ask first?" : "Ask only before risky actions?",
        body:
          (ask === "chat"
            ? "This computer then asks nothing on its own: each chat's mode decides, so chats in YOLO mode, and bots set that way, act here without checking with you."
            : "Ordinary actions (reading, editing files in your folders, everyday commands) then run without asking; deleting things, pushing, installing and changing startup files still ask.") +
          (desk ? " Desktop control is on, so that includes your desktop session." : ""),
        confirmLabel: ask === "chat" ? "Never ask" : "Only risky ones",
        danger: ask === "chat" || desk,
      });
      if (!ok) return;
    }
    await run("ask", () => nodePreset(null, undefined, ask), "Couldn't change when it asks");
  };

  // ── web views here (an extra with Work in folders; included in Full access) ──
  const setWebviews = async (on: boolean) => {
    if (
      on &&
      !(await confirmDialog({
        title: "Let web views run on this computer?",
        body: WEBVIEWS_HELP + " Only chats and bots that may use this computer can open them.",
        confirmLabel: "Allow web views",
        danger: true,
      }))
    )
      return;
    await run("webviews", () => (preset() === "custom" ? nodeSetScope({ ...scope(), webviews: on }) : nodePreset(null, undefined, undefined, on)), "Couldn't change web views");
  };

  // ── terminal from my devices (the user's own, not agents'; any preset) ──
  const terminalChoices = () => [
    { value: "off", label: "Off" },
    ...(node().sandbox_ok && !platform().windows ? [{ value: "sandboxed", label: "Sandboxed" }] : []),
    { value: "full", label: "Full access", warn: true },
  ];
  const setTerminal = async (v: string) => {
    if (!platform().terminal) return;
    const want = v as NonNullable<MachineScope["terminal"]>;
    const was = scope().terminal ?? "off";
    if (want === was) return;
    const looser = (want === "full" && was !== "full") || (want === "sandboxed" && was === "off");
    if (
      looser &&
      !(await confirmDialog({
        title: want === "full" ? "Terminals here with full access?" : "Let your devices open a terminal here?",
        body:
          TERMINAL_HELP +
          (want === "full"
            ? " With full access it can also reach Tessera's own files on this computer." + (platform().windows ? " Windows terminals are not sandboxed: they can reach your account's files, including keys and saved passwords." : "")
            : " Sandboxed, Tessera's own files on this computer stay out of reach."),
        confirmLabel: "Allow terminals",
        danger: want === "full",
      }))
    )
      return;
    await run("terminal", () => nodeSetScope({ ...scope(), terminal: want }), "Couldn't change terminals");
  };

  // ── administrator (root with sudo, outside the sandbox; only set here) ──
  const adminNow = () => scope().admin ?? "off";
  const setAdmin = async (v: string) => {
    if (!platform().admin) return;
    const want = v as NonNullable<MachineScope["admin"]>;
    const was = adminNow();
    if (want === was) return;
    const looser = (want === "ask" && was === "off") || (want === "chat" && was !== "chat");
    if (
      looser &&
      !(await confirmDialog({
        title: want === "chat" ? "Administrator commands without asking?" : "Let agents work here as administrator?",
        body: want === "chat" ? ADMIN_WARNING + " With “As the chat says”, chats in YOLO mode run them without asking you." : ADMIN_WARNING,
        confirmLabel: want === "chat" ? "Don't ask" : "Allow administrator",
        danger: true,
      }))
    )
      return;
    await run("admin", () => nodeSetScope({ ...scope(), admin: want }), "Couldn't change administrator commands");
  };
  const savePassword = async () => {
    const p = await promptDialog({
      title: "This computer's password",
      body: "The password sudo asks for here. Tessera checks it with sudo and keeps it on this computer only: it is never sent anywhere, and agents never see it.",
      label: "Password",
      secret: true,
      confirmLabel: "Save",
    });
    if (!p) return;
    if (await run("admin-pw", () => nodeAdminPassword(p), "Couldn't save the password")) toast({ level: "success", message: "Saved: sudo accepted it" });
  };
  const forgetPassword = () => run("admin-pw", () => nodeAdminPassword(null), "Couldn't forget the password");

  // ── who can use it ──
  /** "Only these bots" picked but no bot yet: kept here until one is (nothing saved). */
  const [pendingOnly, setPendingOnly] = createSignal(false);
  const who = () => (pendingOnly() ? [] : (scope().bots ?? null));
  const setWho = async (v: string[] | null) => {
    if (v != null && v.length === 0) {
      // Picking "Only these bots" first: nothing changes until a bot is picked.
      setPendingOnly(true);
      return;
    }
    const old = scope().bots ?? null;
    const looser = old != null && (v == null || v.some((b) => !old.includes(b)));
    if (looser) {
      const ok = await confirmDialog({
        title: v == null ? "Let any chat use this computer?" : "Let this bot use this computer too?",
        body: v == null ? "Every chat could then use it (asking first as you chose)." : "It could then use this computer as you allow here, also in its scheduled duties.",
        confirmLabel: "Allow",
      });
      if (!ok) return;
    }
    if (await run("who", () => nodeAccess(v), "Couldn't change who can use it")) setPendingOnly(false);
  };

  // ── advanced: folder rules, commands, time limit ──
  const [advanced, setAdvanced] = createSignal(false);
  const showAdvanced = () => advanced() || preset() === "custom";
  const setScope = (what: string, next: MachineScope) => run(what, () => nodeSetScope(next), "Couldn't change what Tessera may do here");
  const setWrite = async (f: FolderAccess, write: boolean) => {
    if (f.write === write) return;
    if (
      write &&
      !(await confirmDialog({
        title: `Let agents change files in ${tilde(f.path)}?`,
        body: "They could create, edit and delete files there. Tessera's own files stay out of reach.",
        confirmLabel: "Allow changes",
        danger: true,
      }))
    )
      return;
    await setScope("advanced", { ...scope(), folders: scope().folders.map((x) => (x.path === f.path ? { ...x, write } : x)) });
  };
  const addAdvancedFolder = async () => {
    const h = home() ?? undefined;
    const path = await browseDir(h ?? "/", "A folder agents may use on this computer", { list: localListDir, home: h });
    if (!path) return;
    const clean = path.replace(/\/+$/, "") || "/";
    if (scope().folders.some((f) => f.path === clean)) return;
    const ok = await confirmDialog({ title: `Let agents read ${tilde(clean)}?`, body: "Agents could read any file in it. You can let them change files there afterwards.", confirmLabel: "Allow reading" });
    if (ok) await setScope("advanced", { ...scope(), folders: [...scope().folders, { path: clean, write: false }] });
  };
  const setCommands = async (on: boolean) => {
    if (
      on &&
      !(await confirmDialog({
        title: "Let agents run commands here?",
        body: win()
          ? "Commands run as you with your full rights (Windows has no command sandbox yet): they can reach everything your account can, passwords and keys included. They need Tessera Guard here."
          : "Commands run as you in Tessera's sandbox, starting in one of your folders. They can also read other files your account can read, except protected passwords and keys. They can use the network and change files only in read & write folders.",
        confirmLabel: "Allow commands",
        danger: true,
      }))
    )
      return;
    await setScope("advanced", { ...scope(), commands: on });
  };
  const setDesktop = async (on: boolean) => {
    if (on && !platform().desktop) return;
    if (on && !(await confirmDialog({ title: "Turn on desktop control?", body: deskWarning(), confirmLabel: "Turn on desktop control", danger: true }))) return;
    await setScope("advanced", { ...scope(), desktop: on });
  };
  const setLimit = async (secs: number) => {
    if (secs === scope().timeout_secs) return;
    if (secs > scope().timeout_secs && !(await confirmDialog({ title: `Let actions run for up to ${fmtLimit(secs)}?`, body: "A command or file action is stopped when it runs longer than this.", confirmLabel: "Allow" }))) return;
    await setScope("advanced", { ...scope(), timeout_secs: secs });
  };
  const limits = () => {
    const cur = scope().timeout_secs;
    const all: number[] = [...TIME_LIMITS];
    if (!all.includes(cur)) all.push(cur);
    return all.sort((a, b) => a - b).map((s) => ({ value: String(s), label: fmtLimit(s) }));
  };

  const st = createMemo(() => statusLine(node()));
  return (
    <div class="stack tight mach-local-tab">
      <Show when={props.info.serverHere && !node().enabled}>
        <div class="form-error warn sec-hint mach-here">
          <Icon name="info" size={14} />
          <span>
            Your Tessera server runs on this computer, so your agents can already work here with their usual tools.{" "}
            <button type="button" class="link-btn" onClick={() => setSettingsTab("commands")}>
              What commands can reach
            </button>{" "}
            (in Privacy &amp; safety) sets what they may do.
          </span>
        </div>
      </Show>

      {/* On / off */}
      <div class="sec-status mach-status" data-tone={st().tone} role="status">
        <span class="sec-status-icon">
          <Icon name={st().icon} size={18} />
        </span>
        <div class="grow">
          <div class="setting-title">Agents on this computer</div>
          <div class="setting-help">
            <span class="mach-state">{st().title}</span> · {st().detail}
          </div>
        </div>
        <Show when={busy() === "on" || busy() === "resume"}>
          <Spinner size={16} />
        </Show>
        <Show when={node().enabled && scope().stopped && busy() !== "resume"}>
          <button type="button" class="btn btn-sm btn-primary mach-resume" disabled={!!busy()} onClick={() => void resume()}>
            Resume
          </button>
        </Show>
      </div>
      <Show when={!node().enabled && !canTurnOn()}>
        <div class="form-error warn sec-hint">
          <Icon name="info" size={14} />
          <span>
            Connect this app to your server first (Settings → Connection), or pair this computer's terminal: <Cmd text="tessera connect <address> --code <code>" />
          </span>
        </div>
      </Show>
      <Show when={scope().commands && !node().sandbox_ok}>
        <div class="form-error warn sec-hint">
          <Icon name="shieldAlert" size={14} />
          <span>Commands can't run here: this system can't sandbox them ({node().sandbox}). File actions still work.</span>
        </div>
      </Show>

      {/* What agents may do: one choice */}
      <div class="mach-block">
        <div class="mach-block-title">What agents may do here</div>
        <div class="mach-presets" role="radiogroup" aria-label="What agents may do here">
          <For each={choices()}>
            {(p) => (
              <button
                type="button"
                role="radio"
                class="mach-preset"
                classList={{ warn: !!p.warn, danger: !!p.danger }}
                aria-checked={preset() === p.value}
                disabled={p.value !== "off" && !node().enabled && !canTurnOn()}
                aria-disabled={!!busy() || (p.value !== "off" && !node().enabled && !canTurnOn())}
                onClick={() => !busy() && void choosePreset(p.value)}
              >
                <span class="mach-preset-icon">
                  <Show when={choosing() === p.value && !!busy()} fallback={<Icon name={p.icon} size={16} />}>
                    <Spinner size={14} />
                  </Show>
                </span>
                <span class="mach-preset-text">
                  <span class="mach-preset-name">{p.label}</span>
                  <span class="mach-preset-desc">{p.sentence}</span>
                </span>
                <span class="mach-preset-radio" aria-hidden="true" />
              </button>
            )}
          </For>
          <Show when={preset() === "custom"}>
            <div class="mach-preset custom" role="radio" aria-checked="true">
              <span class="mach-preset-icon">
                <Icon name="sliders" size={16} />
              </span>
              <span class="mach-preset-text">
                <span class="mach-preset-name">Advanced</span>
                <span class="mach-preset-desc">Your own folder rules, below. Pick a choice above to go back to a simple one.</span>
              </span>
              <span class="mach-preset-radio" aria-hidden="true" />
            </div>
          </Show>
        </div>

        {/* The folders of Look only / Work in folders */}
        {/* The folders of Look only / Work in folders (Advanced, when open, has the full list instead). */}
        <Show when={node().enabled && folderPreset() && !showAdvanced()}>
          <div class="mach-folders">
            <ul class="sec-folders">
              <Show when={!scope().folders.length}>
                <li class="sec-folder fixed mach-nofolder">
                  <Icon name="folder" size={15} />
                  <span class="sec-folder-path">No folders yet: add the ones agents may {preset() === "work_in_folders" ? "work in" : "read"}.</span>
                </li>
              </Show>
              <For each={scope().folders}>
                {(f) => (
                  <li class="sec-folder">
                    <Icon name="folder" size={15} />
                    <span class="sec-folder-path mono" title={f.path}>
                      {tilde(f.path)}
                    </span>
                    <span class="badge">{f.write ? "Read & change" : "Read only"}</span>
                    <button type="button" class="icon-btn sm danger" aria-label={`Remove ${tilde(f.path)}`} data-tip="Remove" disabled={!!busy()} onClick={() => void removeFolder(f.path)}>
                      <Icon name="x" size={14} />
                    </button>
                  </li>
                )}
              </For>
            </ul>
            <div class="sec-add">
              <button type="button" class="btn btn-sm" classList={{ "btn-primary": !scope().folders.length }} aria-disabled={!!busy()} onClick={() => !busy() && void addFolder()}>
                <Show when={busy() === "folders"} fallback={<Icon name="plus" size={14} />}>
                  <Spinner size={13} />
                </Show>
                Add a folder
              </button>
            </div>
          </div>
        </Show>
        <Show when={node().enabled && preset() === "work_in_folders" && platform().webviews}>
          <SwitchRow title="Web views here" help={WEBVIEWS_HELP} checked={!!scope().webviews} saving={busy() === "webviews"} disabled={!!busy()} onChange={(v) => void setWebviews(v)} />
        </Show>
        <Show when={node().enabled && (preset() === "full_access" || preset() === "desktop_control")}>
          <p class="mach-fine mach-homeline">
            <Icon name="home" size={13} />
            <span>
              <Show
                when={win()}
                fallback={
                  <>
                    Your home folder <code>{tilde(node().home || "~")}</code>. Tessera's own files, your passwords and keys{preset() === "full_access" ? " and your startup files" : ""} stay out of reach.
                  </>
                }
              >
                Your home folder <code>{tilde(node().home || "~")}</code>. File actions never read or change Tessera's own files or your passwords and keys, nor change your startup files; commands run as you without a sandbox and can.
              </Show>
            </span>
          </p>
          <Show when={scope().webviews}>
            <p class="mach-fine mach-homeline">
              <Icon name="globe" size={13} />
              <span>Web views can run here: {WEBVIEWS_HELP.charAt(0).toLowerCase() + WEBVIEWS_HELP.slice(1)}</span>
            </p>
          </Show>
        </Show>
        <Show when={node().enabled && scope().desktop}>
          <div
            class="mach-desk"
            classList={{
              "form-error warn sec-hint": !node().desktop_session || (!win() && !node().bus_filter),
              "mach-fine": !!node().desktop_session && (win() || node().bus_filter),
            }}
          >
            <Icon name="monitor" size={14} />
            <span>
              <Show
                when={win()}
                fallback={node().desktop_session ? `Commands get your desktop session: ${node().desktop_session}.` : "No desktop session is running right now; commands get it once one is."}
              >
                {node().desktop_session
                  ? `Agents can see and use your screen while you're signed in: ${node().desktop_session}. Not the lock screen, Windows security prompts or programs running as administrator.`
                  : "No signed-in desktop right now; agents can see and use the screen once you're signed in."}{" "}
                {node().stop_bar
                  ? `The Stop bar at the top of your screen shows what they do and stops them: ${node().stop_bar}.`
                  : "The Stop bar at the top of your screen shows what they do and stops them."}
              </Show>
              <Show when={!win() && !node().bus_filter}>
                {" "}
                Notifications and desktop settings also need <code>xdg-dbus-proxy</code> installed here (it keeps your passwords out of reach).
              </Show>
            </span>
          </div>
        </Show>
      </div>

      {/* Windows: the user's Stop kept where agents' programs can't undo it. */}
      <Show when={win() && node().guard}>
        {(g) => (
          <div class="mach-block mach-guard">
            <div class="mach-block-title">Tessera Guard</div>
            <Show
              when={g().installed}
              fallback={
                <>
                  <p class="setting-help">
                    Keeps your Stop where agents' programs can't undo it without your approval, and ends what this machine started when you stop it. Commands and desktop control need it on Windows. Windows asks an administrator once.
                  </p>
                  <button type="button" class="btn btn-sm mach-guard-install" disabled={!!busy()} onClick={() => void installGuard()}>
                    <Show when={busy() === "guard"} fallback={<Icon name="shieldCheck" size={14} />}>
                      <Spinner size={13} />
                    </Show>
                    Install Tessera Guard
                  </button>
                </>
              }
            >
              <p class="setting-help">
                {g().running
                  ? "On: your Stop is kept where agents' programs can't undo it without your approval, and resuming asks Windows for an administrator's approval."
                  : "Installed but not answering, so nothing runs here until it does (restarting the computer starts it again)."}
              </p>
              <Show when={g().blocked}>
                <p class="form-error warn sec-hint">{g().blocked}</p>
              </Show>
            </Show>
          </div>
        )}
      </Show>

      <Show when={node().enabled}>
        {/* Ask first */}
        <div class="mach-block">
          <div class="mach-block-title">Ask first</div>
          <Segmented value={scope().ask} options={ASK_CHOICES.map((a) => ({ value: a.value, label: a.label, warn: a.warn }))} onChange={(v) => void setAsk(v)} label="Ask first" class="mach-ask" />
          <p class="setting-help">{askInfo(scope().ask).help}</p>
        </div>

        {/* Who can use it */}
        <div class="mach-block">
          <AccessPicker
            value={who()}
            onChange={(v) => void setWho(v)}
            anyLabel={ACCESS_WORDS.any}
            anyHelp="Every chat can use this computer, asking first as you chose above."
            onlyHelp="Only the bots you pick, in their chats, scheduled duties and the helpers they start. Other chats don't see this computer."
            disabled={!!busy()}
          />
          <Show when={props.machine && machineBots(props.machine)}>
            {(bots) => (
              <span class="field-help mach-bots">
                <Icon name="bot" size={13} /> Bots that may use it: {bots()}. Each bot can be given less in its settings (Machines).
              </span>
            )}
          </Show>
        </div>

        {/* Terminal from my devices */}
        <div class="mach-block" data-mach-terminal>
          <div class="mach-block-title">Terminal from my devices</div>
          <Show when={platform().terminal} fallback={<p class="setting-help" aria-disabled="true">{platform().windows ? "Not on Windows yet" : "Not available on this computer"}</p>}>
            <Segmented value={scope().terminal ?? "off"} options={terminalChoices()} onChange={(v) => void setTerminal(v)} label="Terminal from my devices" class="mach-term" />
          <p class="setting-help">
            {(scope().terminal ?? "off") === "off"
              ? "Off: no terminal can open on this computer. " + TERMINAL_HELP
              : TERMINAL_HELP + ((scope().terminal ?? "off") === "full" ? " Full access: Tessera's own files here are reachable too." : " Sandboxed: Tessera's own files here stay out of reach.")}
          </p>
          </Show>
        </div>

        {/* Administrator */}
        <Show when={platform().admin}>
        <div class="mach-block" data-mach-admin>
          <div class="mach-block-title">Administrator</div>
          <Show
            when={scope().commands}
            fallback={
              <p class="setting-help">
                {win()
                  ? "Needs commands: choose Full access or Desktop control first."
                  : "Needs commands: choose Work in folders, Full access or Desktop control first. Agents' commands then run in Tessera's sandbox, where sudo doesn't work."}
              </p>
            }
          >
            <Segmented value={adminNow()} options={ADMIN_CHOICES.map((a) => ({ value: a.value, label: a.label, warn: a.warn }))} onChange={(v) => void setAdmin(v)} label="Administrator" class="mach-admin" />
            <p class="setting-help">{(ADMIN_CHOICES.find((a) => a.value === adminNow()) ?? ADMIN_CHOICES[0]).help}</p>
            <Show when={adminNow() !== "off"}>
              <div class="mach-admin-pw" data-saved={node().admin_password ? "yes" : "no"}>
                <Icon name="key" size={14} />
                <span class="grow">{node().admin_password ? "sudo's password is saved on this computer." : "No password saved. If sudo asks for one here, save it: it stays on this computer."}</span>
                <button type="button" class="btn btn-sm" disabled={!!busy()} onClick={() => void savePassword()}>
                  <Show when={busy() === "admin-pw"}>
                    <Spinner size={13} />
                  </Show>
                  {node().admin_password ? "Change" : "Save password"}
                </button>
                <Show when={node().admin_password}>
                  <button type="button" class="btn btn-sm btn-ghost" disabled={!!busy()} onClick={() => void forgetPassword()}>
                    Forget
                  </button>
                </Show>
              </div>
            </Show>
          </Show>
        </div>
        </Show>

        {/* Advanced */}
        <div class="mach-block">
          <button type="button" class="disclosure" aria-expanded={showAdvanced()} onClick={() => setAdvanced(!showAdvanced())} disabled={preset() === "custom"}>
            <Icon name="chevronRight" size={14} class="disc-chev" />
            Advanced
          </button>
          <Show when={showAdvanced()}>
            <div class="mach-advanced">
              <ul class="sec-folders">
                <Show when={!scope().folders.length}>
                  <li class="sec-folder fixed">
                    <Icon name="folder" size={15} />
                    <span class="sec-folder-path">No folders yet</span>
                  </li>
                </Show>
                <For each={scope().folders}>
                  {(f) => (
                    <li class="sec-folder">
                      <Icon name="folder" size={15} />
                      <span class="sec-folder-path mono" title={f.path}>
                        {tilde(f.path)}
                      </span>
                      <Segmented
                        value={f.write ? "rw" : "ro"}
                        options={[
                          { value: "ro", label: "Read only" },
                          { value: "rw", label: "Read & write", warn: true },
                        ]}
                        onChange={(v) => void setWrite(f, v === "rw")}
                        label={`Access to ${tilde(f.path)}`}
                        class="sm"
                      />
                      <button type="button" class="icon-btn sm danger" aria-label={`Remove ${tilde(f.path)}`} data-tip="Remove" disabled={!!busy()} onClick={() => void removeFolder(f.path)}>
                        <Icon name="x" size={14} />
                      </button>
                    </li>
                  )}
                </For>
              </ul>
              <div class="sec-add">
                <button type="button" class="btn btn-sm" disabled={!!busy()} onClick={() => void addAdvancedFolder()}>
                  <Icon name="plus" size={14} /> Add a folder
                </button>
              </div>
              <SwitchRow
                title="Run commands"
                help={
                  scope().commands
                    ? win()
                      ? "On: commands run as you with your full rights (no sandbox on Windows), starting in one of your folders."
                      : "On: commands run in Tessera's sandbox. They can also read other files your account can read, except protected passwords and keys. Only read & write folders can change."
                    : "Off: agents can only read and write files in your folders."
                }
                checked={scope().commands}
                saving={busy() === "advanced"}
                disabled={!!busy()}
                onChange={(v) => void setCommands(v)}
              />
              {/* Work in folders has it above; with the other choices and your own rules it is here. */}
              <Show when={preset() !== "work_in_folders" && platform().webviews}>
                <SwitchRow title="Web views here" help={WEBVIEWS_HELP} checked={!!scope().webviews} saving={busy() === "webviews"} disabled={!!busy()} onChange={(v) => void setWebviews(v)} />
              </Show>
              <Show when={platform().desktop}>
              <SwitchRow
                title="Desktop control"
                help={
                  !desktopOk()
                    ? "Not on this computer yet. Update Tessera here to check again."
                    : win()
                      ? "Agents can see your screens and use your mouse and keyboard while you're signed in."
                      : "Startup and window manager settings may change (backed up first), and commands get your desktop session."
                }
                checked={!!scope().desktop}
                disabled={!!busy() || (!desktopOk() && !scope().desktop)}
                onChange={(v) => void setDesktop(v)}
              />
              </Show>
              <div class="setting-row col mach-row">
                <div class="setting-text">
                  <div class="setting-title">Time limit</div>
                  <div class="setting-help">The longest one action may run before it's stopped.</div>
                </div>
                <Segmented value={String(scope().timeout_secs)} options={limits()} onChange={(v) => void setLimit(Number(v))} label="Time limit" />
              </div>
            </div>
          </Show>
        </div>
      </Show>

      {/* Stay on */}
      <SwitchRow
        title="Keep it on in the background"
        help={
          node().service.available
            ? node().service.installed
              ? `Runs as a background service: it keeps going when this app is closed${node().service.linger ? " and starts with this computer" : " and starts when you log in"}.`
              : "Off: agents can use this computer only while this app is open."
            : "This computer can't run it as a background service, so agents can use it only while this app is open."
        }
        checked={node().service.installed}
        saving={busy() === "service"}
        disabled={!!busy() || !node().service.available}
        onChange={(v) => void run("service", () => nodeService(v), v ? "Couldn't keep it on in the background" : "Couldn't stop the background service")}
      />

      <Show when={node().startup_changes > 0 || scope().desktop}>
        <StartupChanges count={node().startup_changes} home={home() ?? node().home} onRestored={() => props.refetch()} />
      </Show>

      <button type="button" class="btn btn-sm mach-activity-link" onClick={() => showActivity(props.machine?.id ?? null)}>
        <Icon name="listChecks" size={14} /> What agents did here
      </button>

      <p class="sec-doc">
        <Icon name="terminal" size={13} />
        <span>
          In a terminal here: <code>tessera node status</code>, <code>tessera node preset</code>.
        </span>
      </p>
    </div>
  );
}

/** Settings → Devices → Activity, on this machine's actions. */
export function showActivity(machine: string | null) {
  setActivityFocus(machine);
  setSettingsTab("activity");
}

/** Startup files agents changed here, each with "Put back". */
function StartupChanges(props: { count: number; home: string | null; onRestored: () => void }) {
  const [open, setOpen] = createSignal(false);
  const [list, { refetch }] = createResource(open, () => nodeStartupChanges(100).then((l) => l ?? []));
  const [busy, setBusy] = createSignal<number | null>(null);
  const restore = async (id: number, path: string) => {
    const ok = await confirmDialog({ title: `Put back ${tildify(path, props.home ?? undefined)}?`, body: "It goes back to how it was before that change. Putting it back is recorded too, so you can undo it the same way.", confirmLabel: "Put back" });
    if (!ok) return;
    setBusy(id);
    try {
      await nodeRestore(id);
      toast({ level: "success", message: `${tildify(path, props.home ?? undefined)} is back as it was.` });
      void refetch();
      props.onRestored();
    } catch (e) {
      toastError(e, "Couldn't put it back");
    } finally {
      setBusy(null);
    }
  };
  return (
    <div class="mach-block mach-startup">
      <button type="button" class="disclosure" aria-expanded={open()} onClick={() => setOpen(!open())}>
        <Icon name="chevronRight" size={14} class="disc-chev" />
        Startup files changed by agents
        <Show when={props.count}>
          <span class="badge">{props.count}</span>
        </Show>
      </button>
      <Show when={open()}>
        <Show when={list()} fallback={<div class="empty-state">{list.error ? <p class="danger-text">{errorMessage(list.error)}</p> : <Spinner size={18} />}</div>}>
          <Show when={list()!.length} fallback={<p class="muted small mach-log-empty">None yet. Before an agent changes a startup file here, a copy is kept, and it shows up in this list.</p>}>
            <ul class="mach-startup-list">
              <For each={list()}>
                {(c) => (
                  <li>
                    <span class="grow mach-startup-what">
                      <span class="mono">{tildify(c.path, props.home ?? undefined)}</span>
                      <span class="muted small">
                        {c.action === "restore" ? "put back" : !c.existed ? "added" : !c.exists ? "deleted" : "changed"} {relTime(c.ts)}
                        {c.by ? ` · ${c.by}` : ""}
                      </span>
                    </span>
                    <button type="button" class="btn btn-sm" disabled={busy() != null || c.lost} title={c.lost ? "It was too large to keep a copy" : undefined} onClick={() => void restore(c.id, c.path)}>
                      <Show when={busy() === c.id} fallback={<Icon name="rewind" size={13} />}>
                        <Spinner size={13} />
                      </Show>
                      Put back
                    </button>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        </Show>
      </Show>
    </div>
  );
}

/** What agents did on this computer, newest first, as this computer recorded it. */
export function ActivityLog() {
  const [open, setOpen] = createSignal(false);
  const [log, { refetch }] = createResource(open, () => nodeLog(200).then((l) => compactLog(l ?? [])));
  return (
    <div class="mach-block mach-log">
      <div class="mach-log-head">
        <button type="button" class="disclosure" aria-expanded={open()} onClick={() => setOpen(!open())}>
          <Icon name="chevronRight" size={14} class="disc-chev" />
          Everything done on this computer
        </button>
        <Show when={open()}>
          <button type="button" class="icon-btn sm" aria-label="Refresh" data-tip="Refresh" disabled={log.loading} onClick={() => void refetch()}>
            <Icon name="refresh" size={14} />
          </button>
        </Show>
      </div>
      <Show when={open()}>
        <Show when={log()} fallback={<div class="empty-state">{log.error ? <p class="danger-text">{errorMessage(log.error)}</p> : <Spinner size={18} />}</div>}>
          <Show when={log()!.length} fallback={<p class="muted small mach-log-empty">Nothing yet. Everything agents do here, or are refused, shows up in this list.</p>}>
            <ul class="mach-log-list">
              <For each={log()!}>{(e) => <LogRow e={e} />}</For>
            </ul>
          </Show>
        </Show>
      </Show>
    </div>
  );
}

function LogRow(props: { e: NodeLogEntry }) {
  const e = () => props.e;
  const l = createMemo(() => logLine(e()));
  const badge = () => ({ ok: "badge-ok", warn: "badge-warn", err: "badge-err", info: "badge-accent", "": "" })[l().tone];
  return (
    <li class="mach-log-row" classList={{ refused: e().event === "refused" || e().event === "failed" }}>
      <span class="mach-log-time tabular">{fmtDateTime(e().ts)}</span>
      <span class="mach-log-what">
        <Show when={l().action} fallback={<span class="mach-log-action">{l().label}</span>}>
          <span class="mach-log-action">{e().action}</span>
          <Show when={e().target}>
            <span class="mono mach-log-target" title={e().target!}>
              {e().target}
            </span>
          </Show>
        </Show>
        <Show when={e().bot}>
          <span class="mach-log-from">by {e().bot}</span>
        </Show>
        <Show when={e().chat}>
          <span class="mach-log-from">in “{e().chat}”</span>
        </Show>
        <Show when={e().detail}>
          <span class="mach-log-reason">{e().detail}</span>
        </Show>
        <Show when={e().duration_ms && e().duration_ms! >= 1000}>
          <span class="mach-log-from">{fmtDuration(e().duration_ms!)}</span>
        </Show>
      </span>
      <Show when={l().action}>
        <span class={`badge ${badge()}`}>{l().label}</span>
      </Show>
    </li>
  );
}
