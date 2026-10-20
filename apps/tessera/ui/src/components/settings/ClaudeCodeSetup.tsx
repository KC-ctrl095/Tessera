// Your Claude plan through your own Claude Code: three plain steps (install
// it on the server, sign in once there, Tessera's tools come built in), the
// server's own next step, and "Check again". Tessera never signs in to Claude:
// there is no password or key to type here, by design.
import { For, Show, createResource, createSignal } from "solid-js";
import type { ClaudeCodeInfo } from "../../proto/ClaudeCodeInfo";
import { api, refreshProviders, state } from "../../lib/store";
import { toastError } from "../../lib/ui";
import { openExternal } from "../../lib/util";
import { GuideLink } from "../Guide";
import { Icon, Spinner } from "../icons";
import { CopyButton } from "../primitives";
import "../../styles/claude.css";

const SETUP_URL = "https://code.claude.com/docs/en/setup";
const INSTALL = "curl -fsSL https://claude.ai/install.sh | bash";
/** What a chat on Claude Code gets from Tessera, in the words of the apps. */
const BUILT_IN = ["Notes", "Whiteboards", "Memory", "Notifications", "Browser", "Machines", "Passwords & keys", "Bots and jobs"];

/** A server sentence with `commands` in backticks, shown as code. */
const withCode = (text: string) => text.split("`").map((part, i) => (i % 2 ? <code>{part}</code> : part));

export function ClaudeCodeSetup(props: {
  info?: ClaudeCodeInfo | null;
  onChecked?: (info: ClaudeCodeInfo) => void;
  /** Another Claude account of the user's (its provider id): signed in with its own folder. */
  provider?: string;
}) {
  const [fetched, { mutate }] = createResource(
    () => props.info === undefined || null,
    () => api.claudeCodeStatus(false, props.provider).catch(() => null),
  );
  const info = () => (props.info !== undefined ? props.info : fetched());
  const [checking, setChecking] = createSignal(false);
  const check = async () => {
    setChecking(true);
    try {
      const i = await api.claudeCodeStatus(true, props.provider);
      mutate(i);
      props.onChecked?.(i);
      await refreshProviders(true);
    } catch (e) {
      toastError(e, "Could not check Claude Code");
    } finally {
      setChecking(false);
    }
  };
  const installed = () => !!info()?.installed;
  const signedIn = () => !!info()?.signed_in;
  const ready = () => signedIn() && !info()?.api_key;
  const plan = () => (info()?.plan ? info()!.plan!.replace(/^./, (c) => c.toUpperCase()) : "");
  /** Another account: its name, and the command that signs it in (with its own folder). */
  const accountName = () => (props.provider ? state.providers.find((p) => p.id === props.provider)?.name ?? "This Claude account" : null);
  const signInCmd = () => info()?.sign_in ?? null;
  return (
    <div class="claude-setup" classList={{ ready: ready() }} aria-label="Your Claude plan">
      <div class="claude-setup-head">
        <span class="claude-setup-icon">
          <Icon name={ready() ? "check" : "sparkles"} size={17} />
        </span>
        <div class="grow">
          <div class="claude-setup-title">
            {ready() ? `${accountName() ?? "Claude"} is ready${plan() ? ` · ${plan()}` : ""}` : accountName() ? `Sign in to ${accountName()}` : "Use your Claude Pro or Max plan"}
          </div>
          <div class="claude-setup-sub">
            Tessera runs your own Claude Code on the server, so chats use your plan. You sign in to Claude Code yourself, once; Tessera never sees your
            Claude login.
          </div>
        </div>
      </div>
      <ol class="claude-steps">
        <li data-done={installed()}>
          <span class="claude-step-mark">{installed() ? <Icon name="check" size={12} /> : "1"}</span>
          <div class="grow">
            <b>Install Claude Code</b>
            <Show when={installed()} fallback={<span> on the computer that runs Tessera:</span>}>
              <span class="muted"> · found{info()?.version ? ` ${info()!.version}` : ""}</span>
            </Show>
            <Show when={!installed() && info() !== undefined}>
              <div class="claude-cmd">
                <code>{INSTALL}</code>
                <CopyButton text={() => INSTALL} label="Copy" />
              </div>
              <button type="button" class="link-btn" onClick={() => void openExternal(SETUP_URL)}>
                Other ways to install <Icon name="external" size={11} />
              </button>
            </Show>
          </div>
        </li>
        <li data-done={signedIn()}>
          <span class="claude-step-mark">{signedIn() ? <Icon name="check" size={12} /> : "2"}</span>
          <div class="grow">
            <b>Sign in once</b>
            <Show
              when={signedIn()}
              fallback={
                <Show
                  when={signInCmd()}
                  fallback={
                    <span>
                      : run <code>claude</code> in a terminal there, as the user Tessera runs as, and log in with your Claude account.
                    </span>
                  }
                >
                  <span>: run this in a terminal there, as the user Tessera runs as, and log in with that Claude account. It keeps this account apart from your others.</span>
                  <div class="claude-cmd">
                    <code>{signInCmd()}</code>
                    <CopyButton text={() => signInCmd()!} label="Copy" />
                  </div>
                </Show>
              }
            >
              <span class="muted">
                {" "}
                · {info()?.account ?? "signed in"}
                {plan() ? ` · ${plan()}` : ""}
              </span>
            </Show>
          </div>
        </li>
        <li data-done={ready()}>
          <span class="claude-step-mark">{ready() ? <Icon name="check" size={12} /> : "3"}</span>
          <div class="grow">
            <b>Tessera's tools come built in</b>
            <span class="muted"> · nothing to set up</span>
            <div class="claude-chips">
              <For each={BUILT_IN}>{(t) => <span class="claude-chip">{t}</span>}</For>
            </div>
          </div>
        </li>
      </ol>
      {/* Another account not signed in yet: step 2 already shows what to run. */}
      <Show when={info()?.hint && !ready() && !(signInCmd() && !signedIn() && installed())}>
        <p class="claude-next" role="status">
          <Icon name="arrowRight" size={13} />
          <span>{withCode(info()!.hint)}</span>
        </p>
      </Show>
      <div class="claude-setup-actions">
        <button type="button" class="btn btn-sm" classList={{ "btn-primary": !ready(), "btn-ghost": ready() }} disabled={checking()} onClick={() => void check()}>
          <Show when={checking()} fallback={<Icon name="refresh" size={14} />}>
            <Spinner size={13} />
          </Show>
          Check again
        </button>
        <span class="spacer" />
        <GuideLink name="claude-code" class="link-btn claude-guide">
          What Tessera does with your Claude account
        </GuideLink>
      </div>
    </div>
  );
}
