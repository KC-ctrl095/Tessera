// Settings → Privacy & safety → What commands can reach → the terminal: whether the user's own devices may open a terminal on
// this server (off until turned on here), how it is confined, when an
// untouched one closes, and the record of terminals opened lately (who, when,
// where; never what was typed or shown). Machines decide for themselves, on
// that computer (Settings → Devices, this computer's entry). Agents never get a terminal.
import { For, Show, createResource, createSignal } from "solid-js";
import type { TerminalAccess } from "../../proto/TerminalAccess";
import { fullAccessWarning, setTerminalSettings, terminalStatus, type TerminalStatus } from "../../lib/terminal";
import { RpcError } from "../../lib/transport";
import { confirmDialog, errorMessage, toastError } from "../../lib/ui";
import { fmtDateTime, relTime } from "../../lib/util";
import { Icon, Spinner, type IconName } from "../icons";
import { Segmented } from "../primitives";
import "../../styles/terminal.css";

const IDLE: { value: string; label: string }[] = [
  { value: "15", label: "15 min" },
  { value: "30", label: "30 min" },
  { value: "60", label: "1 hour" },
  { value: "120", label: "2 hours" },
];

const oldServer = (e: unknown) => e instanceof RpcError && (e.code === "bad_request" || e.code === "not_found") && /unknown|variant|method/i.test(e.message);

function minutes(ms: number): string {
  const m = Math.max(1, Math.round(ms / 60000));
  return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${m % 60 ? `${m % 60} min` : ""}`.trim();
}

export function TerminalTab() {
  const [info, { mutate }] = createResource(() => terminalStatus());
  const [busy, setBusy] = createSignal(false);

  const save = async (s: TerminalStatus, patch: Partial<TerminalStatus["settings"]>) => {
    setBusy(true);
    try {
      mutate(await setTerminalSettings({ ...s.settings, ...patch }));
    } catch (e) {
      toastError(e, "That didn't change");
    } finally {
      setBusy(false);
    }
  };

  const choose = async (s: TerminalStatus, access: TerminalAccess) => {
    if (access === s.settings.access) return;
    if (access === "full") {
      const ok = await confirmDialog({
        title: "Terminals with full access?",
        body: fullAccessWarning(s.sandbox_ok),
        confirmLabel: "Turn on",
        danger: true,
      });
      if (!ok) return;
    }
    await save(s, { access });
  };

  const choices = (s: TerminalStatus): { value: TerminalAccess; label: string; icon: IconName; desc: string; disabled?: boolean; warn?: boolean }[] => [
    { value: "off", label: "Off", icon: "shieldCheck", desc: "Recommended unless you need it. No terminal can open on this server." },
    {
      value: "sandboxed",
      label: "Sandboxed",
      icon: "shield",
      desc: s.sandbox_ok ? "Your account's command line, with Tessera's own keys and saved passwords out of reach." : `Needs the command sandbox, which is ${s.sandbox.startsWith("off") ? "off" : "not available"} here.`,
      disabled: !s.sandbox_ok,
    },
    {
      value: "full",
      label: "Full access",
      icon: "shieldOff",
      desc: s.strict ? "Not with the strict sandbox (above)." : "Everything your account here can do, Tessera's own files included.",
      disabled: s.strict,
      warn: true,
    },
  ];

  return (
    <Show
      when={info()}
      fallback={
        <div class="empty-state">
          <Show when={info.error} fallback={<Spinner size={20} />}>
            <span class="empty-icon">
              <Icon name="alertCircle" size={20} />
            </span>
            <p>{oldServer(info.error) ? "This server doesn't have terminals yet. Update Tessera on it to use them." : errorMessage(info.error)}</p>
          </Show>
        </div>
      }
    >
      {(s) => (
        <div class="stack sec term-settings">
          <div class="explain">
            <Icon name="terminal" size={18} />
            <p>
              A terminal is a command line on the computer, as your account there: whatever you type runs with that account's rights. Open one from <b>New → Terminal</b>. Only you see it, on the device
              that opened it; agents never get one.
            </p>
          </div>

          <Show
            when={s().supported}
            fallback={
              <div class="card">
                <p class="setting-help">Terminals aren't available on Windows servers yet.</p>
              </div>
            }
          >
            <div class="card">
              <div class="setting-row col sec-mode">
                <div class="setting-text">
                  <div class="setting-title">Terminals on this server</div>
                  <div class="setting-help">
                    <Show when={s().sandbox_ok} fallback={<>The command sandbox is {s().sandbox.startsWith("off") ? "off (above)" : "not available here"}, so a terminal here has your account's full rights.</>}>
                      Sandboxed works like the agent's own commands: Tessera's keys, logins and saved passwords stay out of reach.
                    </Show>
                  </div>
                </div>
                <div class="perm-grid wide" role="radiogroup" aria-label="Terminals on this server">
                  <For each={choices(s())}>
                    {(c) => (
                      <button
                        type="button"
                        role="radio"
                        class="perm-card"
                        data-access={c.value}
                        aria-checked={s().settings.access === c.value}
                        classList={{ on: s().settings.access === c.value, yolo: !!c.warn }}
                        disabled={busy() || (!!c.disabled && s().settings.access !== c.value)}
                        onClick={() => void choose(s(), c.value)}
                      >
                        <span class="perm-top">
                          <Icon name={c.icon} size={15} />
                          <span class="perm-label">{c.label}</span>
                          <Show when={s().settings.access === c.value}>
                            <Icon name="check" size={13} class="perm-check" />
                          </Show>
                        </span>
                        <span class="perm-desc">{c.desc}</span>
                      </button>
                    )}
                  </For>
                </div>
              </div>
              <Show when={s().settings.access !== "off"}>
                <div class="setting-row col">
                  <div class="setting-text">
                    <div class="setting-title">Close a terminal nobody types in after</div>
                    <div class="setting-help">While a program still runs in it, it stays open up to 8 hours. A terminal also closes {Math.round(s().grace_secs / 60)} minutes after its device goes away, and at once when that device is signed out.</div>
                  </div>
                  <Segmented
                    label="Close a terminal nobody types in after"
                    value={String(s().settings.idle_minutes)}
                    options={IDLE.some((o) => o.value === String(s().settings.idle_minutes)) ? IDLE : [...IDLE, { value: String(s().settings.idle_minutes), label: `${s().settings.idle_minutes} min` }]}
                    onChange={(v) => void save(s(), { idle_minutes: Number(v) })}
                  />
                </div>
              </Show>
              <Show when={!s().private}>
                <div class="form-error warn">
                  <Icon name="shieldAlert" size={14} />
                  <span>This connection isn't end-to-end encrypted, so terminals can't open over it. The Tessera app always is.</span>
                </div>
              </Show>
            </div>
          </Show>

          <div class="card">
            <div class="setting-title">Your machines</div>
            <p class="setting-help sec-lede">Each computer decides for itself, on that computer: Settings → Devices → its entry → Terminal from my devices. It is off until turned on there.</p>
          </div>

          <div class="card">
            <div class="setting-title">Lately</div>
            <p class="setting-help sec-lede">Who opened a terminal, where and when. What was typed or shown is never kept.</p>
            <Show when={s().recent.length} fallback={<p class="setting-help">No terminals yet.</p>}>
              <ul class="term-recent">
                <For each={s().recent}>
                  {(r) => (
                    <li>
                      <Icon name="terminal" size={14} />
                      <span class="grow">
                        <b>{r.device}</b> on {r.place}
                        <small>
                          {fmtDateTime(r.opened_at)} · {r.sandboxed ? "sandboxed" : "full access"}
                          {r.closed_at ? ` · ${minutes(r.closed_at - r.opened_at)} · ${r.ended}` : " · open now"}
                        </small>
                      </span>
                      <span class="term-recent-when" title={fmtDateTime(r.opened_at)}>
                        {relTime(r.opened_at)}
                      </span>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </div>
        </div>
      )}
    </Show>
  );
}
