// Settings → Privacy & safety → What commands can reach: the sandbox around the
// agent's commands (how complete it is here, and the choice), the folders it may
// change or only read besides each chat's own, and the terminal on this server
// (the user's own command line; agents never get one).
import { For, Show, onMount } from "solid-js";
import type { FolderAccess } from "../../proto/FolderAccess";
import type { SandboxMode } from "../../proto/SandboxMode";
import type { SecurityInfo } from "../../proto/SecurityInfo";
import { state } from "../../lib/store";
import { browseDir, confirmDialog, errorMessage } from "../../lib/ui";
import { tildify } from "../../lib/util";
import { Icon, Spinner, type IconName } from "../icons";
import { CopyButton, Segmented } from "../primitives";
import { loadSecurity, oldSecurityServer, saveSecurity, security, securityBusy, securityError } from "./securityState";
import { TerminalTab } from "./TerminalTab";

export const MODES: { value: SandboxMode; label: string; icon: IconName; desc: string }[] = [
  { value: "auto", label: "Automatic", icon: "shieldCheck", desc: "Recommended. Commands use the safe box where available. If it can't work here, every command asks first. Choose Off below only if you want to run without it." },
  { value: "strict", label: "Strict", icon: "lock", desc: "Commands never run without the full sandbox. They fail where it isn't available." },
  { value: "off", label: "Off", icon: "shieldOff", desc: "Commands run with all your access and could read Tessera's keys and chats." },
];

/** What the status line says, from the server's plain description. */
export function sandboxState(i: SecurityInfo): { icon: IconName; tone: "ok" | "warn" | "bad"; title: string; detail: string } {
  const raw = /\(([\s\S]*)\)\s*$/.exec(i.sandbox_status)?.[1] ?? "";
  const detail = raw.charAt(0).toUpperCase() + raw.slice(1);
  if (i.sandbox === "off") return { icon: "shieldOff", tone: "bad", title: "The sandbox is off", detail: "Commands run with all your access." };
  if (/^on\b/i.test(i.sandbox_status) && i.sandbox_complete) return { icon: "shieldCheck", tone: "ok", title: "Commands run in a sandbox", detail };
  if (/^(on|partial)\b/i.test(i.sandbox_status)) return { icon: "shield", tone: "warn", title: "The full sandbox isn't available", detail: i.sandbox === "auto" ? `${detail}. Every command asks first. Choose Off below only to allow commands without the safe box.` : `${detail}. Strict mode refuses commands.` };
  return {
    icon: "shieldAlert",
    tone: "bad",
    title: i.sandbox === "strict" ? "Commands can't run: no sandbox on this system" : "No sandbox on this system",
    detail: i.sandbox === "auto" ? "Every command asks first. To allow commands without the safe box, make a one-time choice of Off below. Commands could then reach your account's files, including Tessera's keys and chats." : detail || "Commands cannot run.",
  };
}

/** A hint with `commands` in backticks: the commands become copyable code. */
function Hint(props: { text: string }) {
  const parts = () => props.text.split(/(`[^`]+`)/g).filter(Boolean);
  return (
    <span>
      <For each={parts()}>
        {(p) =>
          p.startsWith("`") ? (
            <span class="sec-cmd">
              <code>{p.slice(1, -1)}</code>
              <CopyButton text={() => p.slice(1, -1)} label="Copy command" size={13} class="sm" />
            </span>
          ) : (
            p
          )
        }
      </For>
    </span>
  );
}

export function CommandsTab() {
  onMount(() => void loadSecurity());
  const busy = securityBusy;
  const home = () => state.server?.home;
  const tilde = (p: string) => tildify(p, home());

  const setMode = async (mode: SandboxMode) => {
    const i = security()!;
    if (mode === i.sandbox || busy()) return;
    if (
      mode === "off" &&
      !(await confirmDialog({
        title: "Turn the sandbox off?",
        body:
          "The agent's commands would run with all your access: they could read Tessera's keys and logins, your other chats and its settings. " +
          "Only turn it off if a tool you need can't work inside it, and turn it back on afterwards.",
        confirmLabel: "Turn off the sandbox",
        danger: true,
      }))
    )
      return;
    if (
      mode === "strict" &&
      !/^on\b/i.test(i.sandbox_status) &&
      !(await confirmDialog({
        title: "Use strict mode?",
        body: /^partial\b/i.test(i.sandbox_status)
          ? "This system only supports part of the sandbox, so the agent won't be able to run commands until the rest is available."
          : "This system can't sandbox commands, so the agent won't be able to run any.",
        confirmLabel: "Use strict mode",
      }))
    )
      return;
    await saveSecurity("sandbox", { sandbox: mode });
  };

  const setFolders = async (folders: FolderAccess[]) => {
    if (!busy()) await saveSecurity("folders", { folders });
  };
  const addFolder = async () => {
    const path = await browseDir(home() ?? "/", "A folder the agent may work in");
    if (!path) return;
    const i = security()!;
    const clean = path.replace(/\/+$/, "") || "/";
    if (i.folders.some((f) => f.path === clean)) return;
    // All of the user's files at once deserves a second look (the server refuses "/" itself, and says why).
    const broad = clean !== "/" && !!home() && (clean === home() || home()!.startsWith(clean + "/"));
    if (
      broad &&
      !(await confirmDialog({
        title: `Let the agent change everything in ${tilde(clean)}?`,
        body: "It could edit or delete any of your files there without asking. Tessera's own files stay out of reach. A project folder is usually enough.",
        confirmLabel: "Allow",
        danger: true,
      }))
    )
      return;
    await setFolders([...i.folders, { path: clean, write: true }]);
  };

  return (
    <div class="stack sec">
      <Show
        when={security()}
        fallback={
          <div class="empty-state">
            <Show when={securityError()} fallback={<Spinner size={20} />}>
              <span class="empty-icon">
                <Icon name={oldSecurityServer() ? "server" : "alertCircle"} size={20} />
              </span>
              <p>{oldSecurityServer() ? "This server doesn't have these settings yet. Update Tessera on it to manage them here." : errorMessage(securityError())}</p>
            </Show>
          </div>
        }
      >
        {(i) => {
          const st = () => sandboxState(i());
          return (
            <>
              {/* The sandbox */}
              <div class="card" id="sec-sandbox">
                <div class="sec-status" data-tone={st().tone} role="status">
                  <span class="sec-status-icon">
                    <Icon name={st().icon} size={18} />
                  </span>
                  <div class="grow">
                    <div class="setting-title">{st().title}</div>
                    <Show when={st().detail}>
                      <div class="setting-help">{st().detail}</div>
                    </Show>
                  </div>
                </div>
                <Show when={i().sandbox !== "off" && !i().sandbox_complete && i().sandbox_hint}>
                  <div class="form-error warn sec-hint">
                    <Icon name="info" size={14} />
                    <Hint text={i().sandbox_hint!} />
                  </div>
                </Show>
                <div class="setting-row col sec-mode">
                  <div class="setting-text">
                    <div class="setting-title">Sandbox</div>
                    <div class="setting-help">Fences off Tessera's keys, logins and chats, and your desktop session, from the commands the agent runs. Your projects work as usual.</div>
                  </div>
                  <div class="perm-grid wide" role="radiogroup" aria-label="Sandbox">
                    <For each={MODES}>
                      {(m) => (
                        <button
                          type="button"
                          role="radio"
                          class="perm-card"
                          aria-checked={i().sandbox === m.value}
                          classList={{ on: i().sandbox === m.value, yolo: m.value === "off" }}
                          disabled={!!busy()}
                          onClick={() => void setMode(m.value)}
                        >
                          <span class="perm-top">
                            <Icon name={m.icon} size={15} />
                            <span class="perm-label">{m.label}</span>
                            <Show when={i().sandbox === m.value}>
                              <Icon name="check" size={13} class="perm-check" />
                            </Show>
                          </span>
                          <span class="perm-desc">{m.desc}</span>
                        </button>
                      )}
                    </For>
                  </div>
                </div>
              </div>

              {/* Folders */}
              <div class="card">
                <div class="setting-title">Folders</div>
                <p class="setting-help sec-lede">Each chat can always change files in its own folder. Add other folders the agent may change, or only read.</p>
                <ul class="sec-folders">
                  <li class="sec-folder fixed">
                    <Icon name="folder" size={15} />
                    <span class="sec-folder-path">The chat's own folder</span>
                    <span class="sec-folder-note">Read &amp; write</span>
                  </li>
                  <For each={i().folders}>
                    {(f, n) => (
                      <li class="sec-folder">
                        <Icon name="folder" size={15} />
                        <span class="sec-folder-path mono" title={f.path}>
                          {tilde(f.path)}
                        </span>
                        <Segmented
                          value={f.write ? "rw" : "ro"}
                          options={[
                            { value: "ro", label: "Read only" },
                            { value: "rw", label: "Read & write" },
                          ]}
                          onChange={(v) => void setFolders(i().folders.map((x, k) => (k === n() ? { ...x, write: v === "rw" } : x)))}
                          label={`Access to ${tilde(f.path)}`}
                          class="sm"
                        />
                        <button
                          type="button"
                          class="icon-btn sm danger"
                          aria-label={`Remove ${tilde(f.path)}`}
                          data-tip="Remove"
                          disabled={!!busy()}
                          onClick={() => void setFolders(i().folders.filter((_, k) => k !== n()))}
                        >
                          <Icon name="x" size={14} />
                        </button>
                      </li>
                    )}
                  </For>
                </ul>
                <div class="sec-add">
                  <button type="button" class="btn btn-sm" disabled={!!busy()} onClick={() => void addFolder()}>
                    <Show when={busy() === "folders"} fallback={<Icon name="plus" size={14} />}>
                      <Spinner size={13} />
                    </Show>
                    Add a folder
                  </button>
                </div>
              </div>
            </>
          );
        }}
      </Show>

      {/* The terminal on this server (the user's own) */}
      <section class="settings-section" id="set-terminal">
        <h4 class="section-title">The terminal on this server</h4>
        <TerminalTab />
      </section>
    </div>
  );
}
