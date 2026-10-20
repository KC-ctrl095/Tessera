// Settings → Help & tour (at the foot of the list): the short tour, setup
// again, the keyboard shortcuts, the suggestions on new chats, the one-time
// hints, the built-in guides, the terms, privacy and licences, and which
// version this is.
import { For, Show } from "solid-js";
import { reconcile } from "solid-js/store";
import { prefs, setPrefs } from "../../lib/prefs";
import { state } from "../../lib/store";
import { openSetup, setCheatsheetOpen, setSettingsTab } from "../../lib/ui";
import { touchUi } from "../../lib/util";
import { GuideLink, type GuideName } from "../Guide";
import { Icon } from "../icons";
import { Toggle } from "../primitives";
import { startTour } from "../Tour";
import { Card, Row, ThisDevice } from "./kit";
import { AppUpdates } from "./AppUpdates";

const GUIDES: { name: GuideName; title: string }[] = [
  { name: "security", title: "How Tessera keeps you safe" },
  { name: "machines", title: "Letting agents use your computers" },
  { name: "remote-access", title: "Reaching Tessera from anywhere" },
  { name: "notifications", title: "Notifications and files" },
  { name: "claude-code", title: "Your Claude subscription" },
  { name: "windows-start", title: "Getting started on Windows" },
  { name: "windows", title: "Tessera on Windows" },
];

const LEGAL: { name: GuideName; title: string }[] = [
  { name: "terms", title: "Terms of use" },
  { name: "privacy", title: "Privacy" },
  { name: "licences", title: "Open-source licences" },
];

export function HelpTab() {
  const later = (fn: () => void) => {
    setSettingsTab(null);
    setTimeout(fn, 300);
  };
  return (
    <div class="stack">
      <Card>
        <Row title="Take the tour" help="A few pointers to what's where, one at a time.">
          <button type="button" class="btn" onClick={() => later(() => startTour(true))}>
            <Icon name="sparkles" size={16} /> Start the tour
          </button>
        </Row>
        <Row title="Run setup again" help="Your AI, where you'll use Tessera, your phone: step by step.">
          <button type="button" class="btn" onClick={() => later(openSetup)}>
            Run setup
          </button>
        </Row>
        <Show when={!touchUi()}>
          <Row title="Keyboard shortcuts" help="Tiles, mosaics and chats from the keyboard. Alt+/ shows them anywhere.">
            <button type="button" class="btn" onClick={() => later(() => setCheatsheetOpen(true))}>
              <Icon name="keyboard" size={16} /> Show shortcuts
            </button>
          </Row>
        </Show>
        <Row
          title="Suggestions on new chats"
          help={
            <>
              <ThisDevice /> Three things to try, under a new chat's message box.
            </>
          }
        >
          <Toggle checked={prefs.tips} onChange={(v) => setPrefs("tips", v)} label="Suggestions on new chats" />
        </Row>
        <Row title="Hints" help="Bring back the one-time hints you closed.">
          <button
            type="button"
            class="btn"
            disabled={!Object.keys(prefs.hints).length}
            onClick={() => {
              // A plain object would be merged into the store; reconcile replaces it.
              setPrefs("hints", reconcile({}));
              later(() => startTour(true));
            }}
          >
            Show hints again
          </button>
        </Row>
      </Card>
      <Card>
        <div class="setting-title">Guides</div>
        <ul class="help-guides">
          <For each={GUIDES}>
            {(g) => (
              <li>
                <GuideLink name={g.name}>{g.title}</GuideLink>
              </li>
            )}
          </For>
        </ul>
      </Card>
      <Card>
        <div class="setting-title">Legal</div>
        <p class="muted small help-legal-note">Tessera is free, open-source software (Apache License 2.0) and comes with no warranty. Its agents act for you, so what they do is your responsibility.</p>
        <ul class="help-guides help-legal">
          <For each={LEGAL}>
            {(g) => (
              <li>
                <GuideLink name={g.name}>{g.title}</GuideLink>
              </li>
            )}
          </For>
        </ul>
      </Card>
      <AppUpdates />
      <p class="muted small help-version">
        Tessera app {__APP_VERSION__}
        <Show when={state.server?.version}> · server {state.server!.version}</Show>
      </p>
    </div>
  );
}
