// Settings → Appearance (this device): the theme, the text size and the accent
// colour; under More the spacing, the space between tiles, dimming the other
// tiles, focus following the mouse, and where files from chats open.
import { For, Show } from "solid-js";
import { MATERIALS, materialOf, prefs, setPrefs, type Density, type ThemePref } from "../../lib/prefs";
import { isDesktopApp } from "../../lib/native";
import { isTauri, isTauriMobile, touchUi } from "../../lib/util";
import { Icon, type IconName } from "../icons";
import { Segmented, Toggle } from "../primitives";
import { Advanced, Card, Row, ThisDevice } from "./kit";

function ThemeCard(props: { value: ThemePref; label: string; icon: IconName }) {
  return (
    <button type="button" role="radio" class="theme-card" aria-checked={prefs.theme === props.value} classList={{ on: prefs.theme === props.value }} onClick={() => setPrefs("theme", props.value)}>
      <span class="theme-preview" data-preview={props.value}>
        <span class="tp-side" />
        <span class="tp-tile a" />
        <span class="tp-tile b" />
      </span>
      <span class="theme-label">
        <Icon name={props.icon} size={14} /> {props.label}
      </span>
    </button>
  );
}

/** Where files can open on this device: a tile always; a window on computers; this device's app in the apps. */
const openFilesChoices = (): { value: "tile" | "window" | "app"; label: string; icon: IconName }[] => [
  { value: "tile", label: "A tile", icon: "mosaic" },
  ...(isDesktopApp || (!isTauri && !touchUi()) ? [{ value: "window" as const, label: "A new window", icon: "appWindow" as IconName }] : []),
  ...(isTauri ? [{ value: "app" as const, label: isTauriMobile ? "An app on this phone" : "This computer's app", icon: "external" as IconName }] : []),
];

export function AppearanceTab() {
  return (
    <div class="stack">
      <Card>
        <div class="setting-title">
          Theme <ThisDevice />
        </div>
        <div class="theme-cards" role="radiogroup" aria-label="Theme">
          <ThemeCard value="dark" label="Dark" icon="moon" />
          <ThemeCard value="light" label="Light" icon="sun" />
          <ThemeCard value="system" label="Match this device" icon="monitor" />
        </div>
      </Card>
      <Card>
        <Row title="Text size" help={`${prefs.fontSize} px`}>
          <input type="range" class="range" min="12" max="17" step="0.5" value={prefs.fontSize} aria-label="Text size" onInput={(e) => setPrefs("fontSize", Number(e.currentTarget.value))} />
        </Row>
        <Row title="Accent colour" help={materialOf(prefs.accent).name}>
          <div class="accents" role="radiogroup" aria-label="Accent colour">
            <For each={MATERIALS}>
              {(m) => (
                <button
                  type="button"
                  role="radio"
                  class="accent-swatch"
                  aria-checked={prefs.accent === m.id}
                  aria-label={m.name}
                  data-tip={m.name}
                  style={{ "--m-dark": m.dark, "--m-light": m.light }}
                  onClick={() => setPrefs("accent", m.id)}
                />
              )}
            </For>
          </div>
        </Row>
        <Advanced label="More: spacing, space between tiles, dim other tiles, focus follows the mouse, where files open">
          <Row title="Spacing" help="Of lists and messages.">
            <Segmented<Density>
              label="Spacing"
              value={prefs.density}
              onChange={(v) => setPrefs("density", v)}
              options={[
                { value: "comfortable", label: "Roomy" },
                { value: "compact", label: "Compact" },
              ]}
            />
          </Row>
          <Row title="Space between tiles" help={prefs.gaps ? `${prefs.gaps} px` : "Tiles touch"}>
            <input type="range" class="range" min="0" max="12" step="1" value={prefs.gaps} aria-label="Space between tiles" onInput={(e) => setPrefs("gaps", Number(e.currentTarget.value))} />
          </Row>
          <Row title="Dim other tiles" help="The tile you're in stands out.">
            <Toggle checked={prefs.dimInactive} onChange={(v) => setPrefs("dimInactive", v)} label="Dim other tiles" />
          </Row>
          <Show when={!touchUi()}>
            <Row title="Focus follows the mouse" help="Moving the mouse onto a tile puts you in it, so you can type right away. Off: click a tile first (typing over a tile still goes into it when nothing else has the keyboard).">
              <Toggle checked={prefs.focusFollowsMouse} onChange={(v) => setPrefs("focusFollowsMouse", v)} label="Focus follows the mouse" />
            </Row>
          </Show>
          {/* A phone's browser has one way (a tile): nothing to choose there. */}
          <Show when={openFilesChoices().length > 1}>
            <Row
              title="Where files open"
              help={
                <>
                  Where a tap on a text file in a chat (a .txt, a log, code…) opens it. Every file's <b>⋯</b> menu offers the other ways.
                </>
              }
              col
            >
              <Segmented<"tile" | "window" | "app">
                label="Where files open"
                class="open-files-choice"
                value={openFilesChoices().some((o) => o.value === prefs.openFiles) ? prefs.openFiles : "tile"}
                onChange={(v) => setPrefs("openFiles", v)}
                options={openFilesChoices()}
              />
            </Row>
          </Show>
        </Advanced>
      </Card>
    </div>
  );
}
