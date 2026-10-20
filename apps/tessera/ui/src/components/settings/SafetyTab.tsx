// Settings → Privacy & safety: what protects you at a glance, how much freedom
// new chats get, the two pages one step deeper (Passwords & keys; What commands
// can reach), whether chats may ask your bots for help, and under More the
// rest: hiding passwords in what commands return, asking before writing
// elsewhere, whiteboard pictures, encrypted connections, what is always out of
// reach. Only the user changes these, from a paired device: agents have no
// request for them, not even in YOLO mode.
import { newChatFreedom, setNewChatFreedom } from "../../lib/freedom";
import { For, Show, createResource, onMount } from "solid-js";
import type { BotHandoff } from "../../proto/BotHandoff";
import type { PermissionMode } from "../../proto/PermissionMode";
import { PERMISSIONS, confirmYolo } from "../../lib/actions";
import { listMachines, type MachineInfo } from "../../lib/machines";
import { agentLooks, canCaptureHere, setAgentLooks } from "../../lib/snapshot";
import { api, state } from "../../lib/store";
import { confirmDialog, errorMessage, setSettingsTab } from "../../lib/ui";
import { plural, tildify } from "../../lib/util";
import { GuideLink } from "../Guide";
import { Icon, Spinner, type IconName } from "../icons";
import { Toggle } from "../primitives";
import { sandboxState } from "./CommandsTab";
import { Advanced, Card, PageRow, Row, SwitchRow, ThisDevice } from "./kit";
import { machinesNow } from "./SecretMachines";
import { loadSecurity, oldSecurityServer, saveSecurity, security, securityBusy, securityError } from "./securityState";

/** Whether chats may hand work to bots (`bot_handoff`), in plain words. */
const HANDOFF: { value: BotHandoff; label: string; hint: string }[] = [
  { value: "on", label: "Yes", hint: "Chats ask your bots by themselves. When a bot can do more than the chat, you're asked first, unless the chat runs without asking." },
  { value: "ask", label: "Ask me", hint: "You approve every time, in every chat, even in chats that run without asking." },
  { value: "off", label: "No", hint: "Chats never ask your bots. Bots can still ask the bots you allowed in their own settings." },
];
const HANDOFF_RANK: Record<BotHandoff, number> = { off: 0, ask: 1, on: 2 };

/** One line of the summary at the top, and where its setting is. */
interface Glance {
  id: string;
  icon: IconName;
  tone: "ok" | "warn" | "bad" | "info";
  text: string;
  go: () => void;
}

export function SafetyTab() {
  onMount(() => void loadSecurity());
  const [machines] = createResource(() => listMachines().catch(() => [] as MachineInfo[]), { initialValue: [] });
  const [secrets] = createResource(() => api.listSecrets().catch(() => null));
  const busy = securityBusy;
  const to = (id: string) => () => document.getElementById(id)?.scrollIntoView({ block: "start", behavior: "smooth" });
  /** A row under More: open it first, then show the row. */
  const toMore = (id: string) => () => {
    document.querySelector<HTMLButtonElement>('#sec-more .disclosure[aria-expanded="false"]')?.click();
    requestAnimationFrame(() => document.getElementById(id)?.scrollIntoView({ block: "center", behavior: "smooth" }));
  };

  const glance = (): Glance[] => {
    const i = security()!;
    const st = sandboxState(i);
    const out: Glance[] = [{ id: "sandbox", icon: st.icon, tone: st.tone, text: st.title, go: () => setSettingsTab("commands") }];
    out.push(
      i.save_secrets_from_chat
        ? { id: "chats", icon: "key", tone: "ok", text: "Passwords you type are taken out of chats", go: () => setSettingsTab("secrets") }
        : { id: "chats", icon: "key", tone: "bad", text: "Passwords you type stay in chats and reach the AI", go: () => setSettingsTab("secrets") },
    );
    if (i.secret_machines !== undefined && (machines() ?? []).length) {
      const where = machinesNow(i.secret_machines, i.secret_machines_picked ?? [], machines() ?? []);
      out.push(
        i.secret_machines === "off" || where === "No machines yet"
          ? { id: "machines", icon: "server", tone: "ok", text: "Saved passwords are used only on this server", go: () => setSettingsTab("secrets") }
          : { id: "machines", icon: "laptop", tone: "warn", text: `Saved passwords may be used on ${where === "All your machines" ? "all your computers" : where}`, go: () => setSettingsTab("secrets") },
      );
    }
    if (i.bot_handoff !== undefined) {
      const t = { on: "Chats can ask your bots for help", ask: "Chats ask you before asking a bot for help", off: "Chats can't ask your bots for help" }[i.bot_handoff];
      out.push({ id: "bots", icon: "bot", tone: i.bot_handoff === "on" ? "info" : "ok", text: t, go: to("sec-bots") });
    }
    out.push(
      i.require_e2ee
        ? { id: "e2ee", icon: "lock", tone: "ok", text: "Your devices connect encrypted", go: toMore("sec-e2ee") }
        : { id: "e2ee", icon: "lockOpen", tone: "warn", text: "Devices may connect without encryption", go: toMore("sec-e2ee") },
    );
    return out;
  };

  const setHandoff = async (v: BotHandoff) => {
    const i = security()!;
    if (v === i.bot_handoff || busy()) return;
    if (
      HANDOFF_RANK[v] > HANDOFF_RANK[i.bot_handoff ?? "on"] &&
      v === "on" &&
      !(await confirmDialog({
        title: "Let chats ask your bots without asking you?",
        body: "A bot works with its own access, which can be more than the chat's: its saved passwords, computers and freedom. You're still asked when a bot can do more than the chat, except in chats that run without asking.",
        confirmLabel: "Allow",
      }))
    )
      return;
    await saveSecurity("bot_handoff", { bot_handoff: v });
  };

  /** Switches that protect something ask before they go off. */
  const setSwitch = async (key: "ask_outside" | "redact_patterns", on: boolean) => {
    const warn = {
      ask_outside: {
        title: "Let the agent change files anywhere?",
        body: "It would write to any folder your account can, without asking first. Tessera's own files stay out of reach.",
        confirmLabel: "Don't ask",
      },
      redact_patterns: {
        title: "Stop hiding passwords in what commands return?",
        body: "Keys, tokens and passwords in what a command or tool returns would reach the AI model as they are (your saved ones are still removed).",
        confirmLabel: "Stop hiding them",
      },
    }[key];
    if (!on && !(await confirmDialog({ ...warn, danger: true }))) return;
    await saveSecurity(key, { [key]: on });
  };

  const commandsLine = () => {
    const i = security()!;
    const mode = { auto: "automatic", strict: "strict", off: "off" }[i.sandbox];
    return `Sandbox: ${mode}${i.folders.length ? ` · ${plural(i.folders.length, "extra folder", "extra folders")}` : ""}`;
  };
  const secretsLine = () => {
    const i = security()!;
    const n = secrets()?.filter((s) => !s.chat).length;
    const saving = !i.save_secrets_from_chat
      ? "typed ones stay in chats"
      : i.secret_saving
        ? { auto: "typed ones are saved automatically", ask: "asks before saving typed ones", chat_only: "typed ones stay with their chat" }[i.secret_saving]
        : "";
    return [n === undefined ? "" : `${n} saved`, saving].filter(Boolean).join(" · ");
  };

  return (
    <div class="stack sec">
      <div class="explain">
        <Icon name="shieldCheck" size={18} />
        <p>The defaults are the safe choice. Only you can change these, from a paired device — agents can't, not even in YOLO mode.</p>
      </div>

      {/* How much freedom new chats get: one choice for all devices (lib/freedom.ts). */}
      <Card>
        <Row title="How much freedom new chats get" help="On all your devices. Each chat can be changed from its ⋯ menu. Bots have their own." col>
          <div class="perm-grid wide" role="radiogroup" aria-label="How much freedom new chats get">
            <For each={PERMISSIONS}>
              {(p) => (
                <button
                  type="button"
                  role="radio"
                  class="perm-card"
                  aria-checked={newChatFreedom() === p.value}
                  classList={{ on: newChatFreedom() === p.value, yolo: p.value === "auto" }}
                  onClick={async () => {
                    if (await confirmYolo(p.value)) await setNewChatFreedom(p.value as PermissionMode);
                  }}
                >
                  <span class="perm-top">
                    <Icon name={p.icon} size={15} />
                    <span class="perm-label">{p.label}</span>
                    <Show when={newChatFreedom() === p.value}>
                      <Icon name="check" size={13} class="perm-check" />
                    </Show>
                  </span>
                  <span class="perm-desc">{p.short}</span>
                </button>
              )}
            </For>
          </div>
        </Row>
      </Card>

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
        {(i) => (
          <>
            {/* At a glance: one line per protection, each leading to its setting */}
            <div class="card sec-glance" aria-label="At a glance">
              <ul>
                <For each={glance()}>
                  {(g) => (
                    <li data-id={g.id} data-tone={g.tone}>
                      <button type="button" class="sec-glance-row" onClick={() => g.go()}>
                        <span class="sec-glance-icon">
                          <Icon name={g.icon} size={15} />
                        </span>
                        <span class="sec-glance-text">{g.text}</span>
                        <Icon name="chevronRight" size={14} class="sec-glance-go" />
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </div>

            <Card class="sec-pages">
              <PageRow icon="key" title="Passwords & keys" help={secretsLine()} onOpen={() => setSettingsTab("secrets")} />
              <PageRow icon="terminal" title="What commands can reach" help={commandsLine()} onOpen={() => setSettingsTab("commands")} />
              <Show when={i().bot_handoff}>
                {(h) => (
                  <div class="setting-row col sec-handoff" id="sec-bots">
                    <div class="setting-text">
                      <div class="setting-title">Chats asking your bots for help</div>
                      <div class="setting-help">A bot then works with its own access, which can be more than the chat's: its saved passwords, computers and freedom. A bot can also refuse: in the bot's settings, “Other chats may ask it for help”.</div>
                    </div>
                    <div class="sec-handoff-choice">
                      <div class="segmented sec-handoff-seg" role="radiogroup" aria-label="Chats asking your bots for help">
                        <For each={HANDOFF}>
                          {(o) => (
                            <button type="button" role="radio" aria-checked={h() === o.value} classList={{ active: h() === o.value }} disabled={!!busy()} onClick={() => void setHandoff(o.value)}>
                              <span>{o.label}</span>
                            </button>
                          )}
                        </For>
                      </div>
                      <p class="setting-help sec-handoff-hint">{HANDOFF.find((o) => o.value === h())?.hint}</p>
                    </div>
                  </div>
                )}
              </Show>
            </Card>

            <Card class="sec-more">
              <div id="sec-more">
                <Advanced label="More: hide passwords in what commands return, ask before writing elsewhere, whiteboard pictures, encryption, always out of reach">
                  <SwitchRow
                    title="Hide passwords in what commands return"
                    help="Anything that looks like a key, token or password in what a command or tool returns is blanked out before the AI reads it. Your saved ones are always removed."
                    checked={i().redact_patterns}
                    disabled={!!busy()}
                    onChange={(v) => void setSwitch("redact_patterns", v)}
                  />
                  <SwitchRow
                    title="Ask before writing elsewhere"
                    help="Writing a file outside the chat's folder and the folders you added asks you first. Off: it doesn't (system and startup files still ask; Tessera's own stay out of reach)."
                    checked={i().ask_outside}
                    disabled={!!busy()}
                    onChange={(v) => void setSwitch("ask_outside", v)}
                  />
                  <Show when={canCaptureHere}>
                    <Row
                      title="Agents can look at whiteboards"
                      help={
                        <>
                          <ThisDevice /> When an agent asks, this device sends it a picture of that whiteboard's tile, only while the tile is on screen. Nothing else on the screen, and nothing is kept on this device.
                        </>
                      }
                    >
                      <Toggle checked={agentLooks()} onChange={setAgentLooks} label="Agents can look at whiteboards on this screen" />
                    </Row>
                  </Show>
                  <div class="setting-row" id="sec-e2ee">
                    <div class="setting-text">
                      <div class="setting-title">Encrypted connections only</div>
                      <div class="setting-help">Devices on your network or through a tunnel must connect encrypted. Changed only in the server's config.toml.</div>
                    </div>
                    <span class={`badge ${i().require_e2ee ? "badge-ok" : "badge-warn"}`}>{i().require_e2ee ? "On" : "Off"}</span>
                  </div>
                  <Show when={i().protected.length}>
                    <details class="sec-protected">
                      <summary>Always out of reach of commands</summary>
                      <ul>
                        <For each={i().protected}>{(p) => <li class="mono">{tildify(p, state.server?.home)}</li>}</For>
                      </ul>
                    </details>
                  </Show>
                </Advanced>
              </div>
            </Card>
          </>
        )}
      </Show>

      <p class="sec-doc">
        <Icon name="book" size={13} />
        <span>
          How the sandbox and passwords work: <GuideLink name="security">Security guide</GuideLink>
        </span>
      </p>
    </div>
  );
}
