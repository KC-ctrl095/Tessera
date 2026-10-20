// The bot editor's "Passwords & keys": which saved passwords and keys this
// bot can use in commands (never their values), every one of them switched
// here and saved with the rest of the form. Two kinds, one list:
// - kept for certain bots (Passwords & keys → Who can use it → Only these
//   bots): the switch adds this bot to that list or takes it off;
// - open to any chat: the bot's own choice (`Bot.secrets`), which nothing
//   else changes. New bots start with none of these; bots made before the
//   choice existed have all of them until some are switched off here.
// Adding new ones is in Settings → Passwords & keys.
import { For, Show, createResource } from "solid-js";
import type { BotSecrets } from "../../proto/BotSecrets";
import type { SecretInfo } from "../../proto/SecretInfo";
import { api } from "../../lib/store";
import { Icon } from "../icons";
import { Toggle } from "../primitives";
import "../../styles/credentials.css";

const label = (s: SecretInfo) => s.description || s.name;

/** The choice as it stands (a bot made before it: every open one). */
const choiceOf = (v: BotSecrets | undefined): BotSecrets => v ?? { default: true, each: {} };

export function BotCredentials(props: {
  botId: string | null;
  botName: string;
  /** The bot's choice for passwords open to any chat; undefined: made before it. */
  value: BotSecrets | undefined;
  onChange: (v: BotSecrets) => void;
  /** Changes waiting for Save to passwords kept for certain bots (name → this bot can use it). */
  kept: Record<string, boolean>;
  onKept: (name: string, on: boolean) => void;
  /** Bumped after a save, to list them again. */
  reload: number;
}) {
  const [list] = createResource(
    () => [props.botId, props.reload] as const,
    () => api.listSecrets().catch(() => [] as SecretInfo[]),
  );
  // One kept for a single chat belongs to that chat, not to a bot.
  const shown = () => (list() ?? []).filter((s) => s.chat == null).sort((a, b) => label(a).localeCompare(label(b)));
  const isKept = (s: SecretInfo) => s.bots != null;
  const can = (s: SecretInfo): boolean => {
    if (isKept(s)) return s.name in props.kept ? props.kept[s.name] : !!props.botId && s.bots!.includes(props.botId);
    const c = choiceOf(props.value);
    return c.each[s.name] ?? c.default;
  };
  const set = (s: SecretInfo, on: boolean) => {
    if (isKept(s)) return props.onKept(s.name, on);
    const c = choiceOf(props.value);
    const each = { ...c.each, [s.name]: on };
    // An entry equal to the default says nothing.
    if (on === c.default) delete each[s.name];
    props.onChange({ default: c.default, each });
  };
  const setLater = (on: boolean) => {
    const c = choiceOf(props.value);
    // The passwords listed keep what they show now; only later ones follow.
    const each: Record<string, boolean> = {};
    for (const s of shown()) if (!isKept(s) && (c.each[s.name] ?? c.default) !== on) each[s.name] = c.each[s.name] ?? c.default;
    props.onChange({ default: on, each });
  };
  const usable = () => shown().filter(can).length;
  return (
    <section class="be-sec be-creds">
      <h3 class="be-h">
        <Icon name="key" size={16} /> Passwords & keys
      </h3>
      <p class="muted small be-note">
        What {props.botName} can use in commands, without ever seeing the values. Changes count once you save.
      </p>
      <Show when={!props.value && props.botId}>
        <p class="small be-cred-legacy">
          <Icon name="info" size={13} /> Made before this setting: it can use every password any chat may use. Switch off the ones it shouldn't have.
        </p>
      </Show>
      <Show when={list()}>
        <Show when={shown().length} fallback={<p class="muted small">No passwords or keys saved yet. Add them in Settings → Passwords & keys.</p>}>
          <p class="small be-cred-count">
            {usable() === 0 ? "It can use none of them." : usable() === shown().length ? "It can use all of them." : `It can use ${usable()} of ${shown().length}.`}
          </p>
          <div class="be-cred-list" role="group" aria-label={`Passwords and keys ${props.botName} can use`}>
            <For each={shown()}>
              {(s) => (
                <div class="setting-row be-cred" data-name={s.name} data-kind={isKept(s) ? "kept" : "open"}>
                  <div class="setting-text">
                    <div class="setting-title">{label(s)}</div>
                    <div class="setting-help">
                      <code class="var-chip">{s.kind === "token" ? `$${s.name}` : s.name}</code> ·{" "}
                      {isKept(s) ? "kept for certain bots" : s.ask ? "any chat may use it (asks first)" : "any chat may use it"}
                    </div>
                    <Show when={isKept(s) || (can(s) && !s.ask)}>
                      <div class="setting-help">No first-use approval. Its commands can send the value anywhere, not just the listed sites.</div>
                    </Show>
                  </div>
                  <Toggle checked={can(s)} onChange={(v) => set(s, v)} label={`${props.botName} can use ${label(s)}`} />
                </div>
              )}
            </For>
          </div>
          <div class="setting-row be-cred-later">
            <div class="setting-text">
              <div class="setting-title">Ones you save later</div>
              <div class="setting-help">New passwords any chat may use: {choiceOf(props.value).default ? `${props.botName} gets them too.` : `${props.botName} gets them only when you switch them on here.`}</div>
            </div>
            <Toggle checked={choiceOf(props.value).default} onChange={setLater} label={`${props.botName} gets new passwords any chat may use`} />
          </div>
        </Show>
        <p class="muted small">Add new ones, or keep one for certain bots only, in Settings → Passwords & keys.</p>
      </Show>
    </section>
  );
}

/**
 * Save the waiting changes to passwords kept for certain bots, now that the bot exists (`botId`).
 * Read fresh, so another device's change in between isn't undone. Returns the ones that failed.
 */
export async function applyKept(botId: string, changes: Record<string, boolean>): Promise<string[]> {
  const names = Object.keys(changes);
  if (!names.length) return [];
  const list = await api.listSecrets();
  const failed: string[] = [];
  for (const name of names) {
    const s = list.find((x) => x.name === name);
    if (!s || s.bots == null) continue;
    const on = changes[name];
    if (s.bots.includes(botId) === on) continue;
    const bots = on ? [...s.bots, botId] : s.bots.filter((x) => x !== botId);
    try {
      // Only who can use it changes: empty fields keep every value.
      await api.saveSecret({ name: s.name, kind: s.kind, description: s.description, fields: {}, ask: s.ask, hosts: s.hosts, bots });
    } catch {
      failed.push(label(s));
    }
  }
  return failed;
}
