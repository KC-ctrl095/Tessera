/* @refresh reload */
// Dev-only review page for the mosaic avatars: open /avatar-gallery.html under `npm run dev`.
// Query: ?theme=dark|light (default both) · ?only=grid|modes|picker|check · ?sizes=24,32,48,128 · ?value=<initial picker value>
// ?only=check validates every avatar at both size classes and prints PASS/FAIL into #avatar-check.
import { render } from "solid-js/web";
import { createSignal, For, Show } from "solid-js";
import "../styles/fonts.css";
import "../styles/tokens.css";
import "../styles/avatars.css";
import { ALL_AVATARS, AvatarPicker, CATEGORY_LABEL, MosaicAvatar, type AvatarCategory } from "./index";
import { tesserize, svgDataUrl, type Design } from "./engine";
import { designFor, isMosaicAvatar } from "./catalog";
import { FALLBACK_DESIGN, fieldDesign } from "./render";

const q = new URLSearchParams(location.search);
const themes = (q.get("theme") ?? "dark,light").split(",");
const only = q.get("only");
const sizes = (q.get("sizes") ?? "24,32,48,128").split(",").map(Number);
const show = (part: string) => !only || only.split(",").includes(part);

// Cost of cutting every portrait once (both size classes), for the header.
const t0 = performance.now();
let bytes = 0;
for (const a of ALL_AVATARS) bytes += tesserize(designFor(a.id)!, { small: true }).length;
const cutMs = performance.now() - t0;

const css = `
  body { margin: 0; font: 13px var(--font); }
  .g-theme { background: var(--grout); color: var(--text); padding: 18px 20px 28px; }
  .g-theme h2 { font: 400 1.25rem var(--display); margin: 0 0 4px; }
  .g-theme h3 { font-size: .72rem; letter-spacing: .08em; text-transform: uppercase; color: var(--faint); margin: 18px 0 8px; }
  .g-meta { color: var(--muted); font-size: .8rem; }
  .g-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(300px, 1fr)); gap: 8px; }
  .g-card { display: flex; align-items: flex-end; gap: 10px; padding: 10px; background: var(--stone); border-radius: var(--r); }
  .g-card .g-id { font: 10px var(--mono); color: var(--faint); writing-mode: vertical-rl; transform: rotate(180deg); align-self: stretch; text-align: right; }
  .g-row { display: flex; flex-wrap: wrap; align-items: center; gap: 14px; background: var(--stone); padding: 12px; border-radius: var(--r); }
  .g-mode { display: flex; flex-direction: column; align-items: center; gap: 6px; font-size: .72rem; color: var(--muted); }
  .g-ctx { display: grid; gap: 8px; grid-template-columns: repeat(auto-fill, minmax(260px, 1fr)); }
  .g-item { display: flex; align-items: center; gap: 9px; padding: 6px 8px; background: var(--stone); border-radius: var(--r); }
  .g-item b { font-weight: 600; } .g-item span { color: var(--faint); font-size: .76rem; }
  .g-pick { display: grid; grid-template-columns: minmax(280px, 420px) 1fr; gap: 16px; align-items: start; }
  .g-val { font: 12px var(--mono); color: var(--muted); display: flex; align-items: center; gap: 10px; }
`;

function Theme(props: { theme: string }) {
  const [value, setValue] = createSignal(q.get("value") ?? "m:laurel-beard");
  const cats: AvatarCategory[] = ["figure", "creature", "emblem"];
  return (
    <section class="g-theme" data-theme={props.theme}>
      <h2>Mosaic avatars · {props.theme}</h2>
      <div class="g-meta">
        {ALL_AVATARS.length} portraits ({cats.map((c) => `${ALL_AVATARS.filter((a) => a.category === c).length} ${CATEGORY_LABEL[c].toLowerCase()}`).join(", ")}) · cut in {cutMs.toFixed(0)} ms · {(bytes / 1024).toFixed(0)} KB of SVG
      </div>
      <Show when={show("grid")}>
        <For each={cats}>
          {(cat) => (
            <>
              <h3>{CATEGORY_LABEL[cat]}</h3>
              <div class="g-grid">
                <For each={ALL_AVATARS.filter((a) => a.category === cat)}>
                  {(a) => (
                    <div class="g-card">
                      <span class="g-id">{a.id}</span>
                      <For each={sizes}>{(s) => <MosaicAvatar avatar={"m:" + a.id} size={s} />}</For>
                    </div>
                  )}
                </For>
              </div>
            </>
          )}
        </For>
      </Show>
      <Show when={show("modes")}>
        <h3>Emoji, text, unknown, ring</h3>
        <div class="g-row">
          <For
            each={
              [
                ["🦉", "#7d9be0", "emoji"],
                ["✨", "#d4af5f", "emoji"],
                ["🔧", "#e07a56", "emoji"],
                ["N", "#9d8bd6", "letter"],
                ["AB", "#5fa9b3", "2 letters"],
                ["m:not-a-real-one", undefined, "unknown id"],
                ["m:owl", "#6fb08e", "ring"],
              ] as [string, string | undefined, string][]
            }
          >
            {([v, c, l]) => (
              <div class="g-mode">
                <div style={{ display: "flex", gap: "8px", "align-items": "end" }}>
                  <MosaicAvatar avatar={v} color={c} size={24} />
                  <MosaicAvatar avatar={v} color={c} size={32} />
                  <MosaicAvatar avatar={v} color={c} size={72} />
                </div>
                {l}
              </div>
            )}
          </For>
        </div>
        <h3>In context</h3>
        <div class="g-ctx">
          <For each={["m:laurel-beard", "m:owl-helm", "m:winged-cap", "m:trident-crown", "m:sun-crown", "m:crescent-bow", "🦊", "m:guard-dog"]}>
            {(v, i) => (
              <div class="g-item">
                <MosaicAvatar avatar={v} size={i() % 2 ? 26 : 20} label="" />
                <b>{["Nova", "Sage", "Scout", "Forge", "Ember", "Wren", "Fox", "Rex"][i()]}</b>
                <span>@{["nova", "sage", "scout", "forge", "ember", "wren", "fox", "rex"][i()]}</span>
              </div>
            )}
          </For>
        </div>
      </Show>
      <Show when={show("picker")}>
        <h3>Picker</h3>
        <div class="g-pick">
          <AvatarPicker value={value()} onChange={setValue} color="#d4af5f" />
          <div class="g-val">
            <MosaicAvatar avatar={value()} size={72} color="#d4af5f" />
            <span>value = {JSON.stringify(value())}</span>
          </div>
        </div>
      </Show>
    </section>
  );
}

/** Validate every design at both size classes: XML parses, no NaN/Infinity, data URL round-trips and loads. */
async function checkAll(): Promise<string> {
  const fails: string[] = [];
  const parser = new DOMParser();
  const loads = (url: string) =>
    new Promise<boolean>((res) => {
      const img = new Image();
      img.onload = () => res(true);
      img.onerror = () => res(false);
      img.src = url;
    });
  const ids = new Set<string>();
  for (const a of ALL_AVATARS) {
    if (ids.has(a.id)) fails.push(`${a.id}: duplicate id`);
    ids.add(a.id);
    if (!isMosaicAvatar("m:" + a.id)) fails.push(`${a.id}: not a valid m:<id>`);
  }
  const designs: Design[] = [...ALL_AVATARS.map((a) => designFor(a.id)!), FALLBACK_DESIGN, fieldDesign("#d4af5f"), fieldDesign()];
  let n = 0;
  for (const d of designs)
    for (const small of [true, false]) {
      n++;
      const tag = `${d.id} (${small ? "small" : "large"})`;
      const svg = tesserize(d, { small });
      const url = svgDataUrl(svg);
      if (parser.parseFromString(svg, "image/svg+xml").getElementsByTagName("parsererror").length) fails.push(`${tag}: XML parse error`);
      else if (/NaN|Infinity|undefined/.test(svg)) fails.push(`${tag}: NaN/Infinity/undefined in output`);
      else if (decodeURIComponent(url.slice(url.indexOf(",") + 1)) !== svg) fails.push(`${tag}: data URL does not round-trip`);
      else if (!(await loads(url))) fails.push(`${tag}: image failed to load`);
    }
  return fails.length ? `FAIL ${fails.length}/${n}\n${fails.join("\n")}` : `PASS ${n}/${n} SVGs (${ALL_AVATARS.length} avatars × 2 sizes + fallback + fields)`;
}
if (show("check") && only) {
  const pre = document.createElement("pre");
  pre.id = "avatar-check";
  pre.textContent = "checking…";
  document.body.appendChild(pre);
  checkAll().then((r) => (pre.textContent = r));
}

const style = document.createElement("style");
style.textContent = css;
document.head.appendChild(style);
render(() => <For each={themes}>{(t) => <Theme theme={t} />}</For>, document.getElementById("root")!);
