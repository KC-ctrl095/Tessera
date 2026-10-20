// Small model mode ("Simpler instructions") in Settings → Models: every model of a provider other than Claude Code has a choice
// of Auto / On / Off, with one line about it above the list (not on every row). Auto says what it
// means for the model now: "Auto (on)" for the desk PC's llama.cpp models and OpenRouter's Mistral
// Small, "Auto (off)" for Claude and DeepSeek. A change goes to the server (set_model_small), every
// device hears providers_changed and shows it, it survives a reload, and Auto again shows Tessera's
// own choice. Desktop, the folded Fold's cover screen (344 px), the unfolded Fold; dark and light.
// ONLY=cover-dark runs one.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const HELP = "Simpler instructions: fewer tools and shorter instructions, for small models running on your own computer.";
const LOCAL = "llama.cpp (desk)";
const CLAUDE = "Anthropic";
const ROUTER = "OpenRouter";
const CC = "Claude (your subscription)";

const until = (p, expr, ms = 4000) => p.waitFor(expr, ms).then(() => true, () => false);
const card = (name) =>
  `[...document.querySelectorAll('.settings-body .card.provider')].find((c) => c.querySelector('.provider-name')?.firstChild?.textContent.trim() === ${JSON.stringify(name)})`;
/** Opens a provider's model list (the "N models" button). */
const expand = (p, name) =>
  p.eval(`(() => { const c = ${card(name)}; const b = c?.querySelector('.provider-actions button[aria-expanded]'); if (!b) return false; if (b.getAttribute('aria-expanded') !== 'true') b.click(); return true; })()`);
/** Each model row of a provider's open list: its name and its small model mode choice, as shown. */
const rows = (p, name) =>
  p.eval(`(() => {
    const c = ${card(name)};
    if (!c) return null;
    return [...c.querySelectorAll('.model-list li')].map((li) => {
      const s = li.querySelector('.model-small select');
      return {
        name: li.querySelector('.strong')?.textContent ?? null,
        value: s?.value ?? null,
        shown: s ? s.selectedOptions[0]?.textContent : null,
        options: s ? [...s.options].map((o) => o.textContent) : null,
        aria: s?.getAttribute('aria-label') ?? null,
      };
    });
  })()`);
const row = async (p, name, model) => (await rows(p, name))?.find((r) => r.name === model) ?? null;
/** A model's choice (a JS expression). */
const sel = (name, model) =>
  `[...${card(name)}.querySelectorAll('.model-small select')].find((s) => s.getAttribute('aria-label') === ${JSON.stringify(`Simpler instructions for ${model}`)})`;
/** Picks a choice the way the native picker does (it sets the value and fires change). */
const choose = (p, name, model, mode) =>
  p.eval(`(() => { const s = ${sel(name, model)}; if (!s) return false; s.scrollIntoView({ block: 'center' }); s.value = ${JSON.stringify(mode)}; s.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
/** What the server keeps for a model (through another device's connection). */
async function served(w, provider, model) {
  const r = await w.rpc("list_providers", { refresh: false });
  const m = r.result?.find((x) => x.id === provider)?.models.find((x) => x.id === model);
  return m ? { small_mode: m.small_mode ?? null, small: m.small ?? null } : null;
}
/**
 * Where a provider's rows and their parts sit: the choice fully inside the card and the screen, not
 * covered, its label on one line, nothing in a row overlapping, and whether the choice shares the
 * model's line or has its own.
 */
const layout = (p, name) =>
  p.eval(`(() => {
    const c = ${card(name)};
    const cr = c.getBoundingClientRect();
    const over = (a, b) => a.left < b.right - 0.5 && b.left < a.right - 0.5 && a.top < b.bottom - 0.5 && b.top < a.bottom - 0.5;
    return [...c.querySelectorAll('.model-list li')].map((li) => {
      li.scrollIntoView({ block: 'center' });
      const s = li.querySelector('.model-small select');
      const lab = li.querySelector('.model-small-name');
      const parts = [li.querySelector('.model-li-main'), li.querySelector('.model-caps'), li.querySelector('.model-small'), li.querySelector('.btn, .badge-accent')].filter(Boolean);
      const r = s.getBoundingClientRect(), m = parts[0].getBoundingClientRect(), lr = lab.getBoundingClientRect();
      const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
      const boxes = parts.map((e) => e.getBoundingClientRect());
      let overlap = false;
      for (let i = 0; i < boxes.length; i++) for (let j = i + 1; j < boxes.length; j++) if (over(boxes[i], boxes[j])) overlap = true;
      return {
        inCard: r.left >= cr.left - 0.5 && r.right <= cr.right + 0.5,
        onScreen: r.left >= 0 && r.right <= innerWidth + 0.5,
        reachable: hit === s,
        h: Math.round(r.height),
        w: Math.round(r.width),
        oneLine: lr.height < parseFloat(getComputedStyle(lab).lineHeight || '16') * 1.6 || lr.height < 22,
        overlap,
        ownLine: r.top >= m.bottom - 1,
        textFits: s.scrollWidth <= s.clientWidth + 1,
        left: Math.round(r.left),
      };
    });
  })()`);
/** No sideways scrolling in the section, and nothing of the model lists past the screen's edge. */
const fits = (p) =>
  p.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth <= b.clientWidth + 1 && [...b.querySelectorAll('.model-list, .model-list li, .model-small-help')].every((e) => e.getBoundingClientRect().right <= innerWidth + 0.5); })()`);
const clipOf = (p, name) =>
  p.eval(`(() => { const c = ${card(name)}; c.scrollIntoView({ block: 'start' }); const r = c.getBoundingClientRect(); return { x: Math.max(0, r.x - 4), y: Math.max(0, r.y - 4), width: Math.min(innerWidth, r.width + 8), height: Math.min(innerHeight - Math.max(0, r.y - 4), r.height + 8) }; })()`);

async function openModels(p) {
  await p.eval(`__mod('lib/ui.ts').then((u) => u.openSettings('providers'))`);
  await p.waitFor(`${card(LOCAL)} && ${card(CC)}`, 8000);
  await sleep(500);
  for (const n of [LOCAL, CLAUDE, ROUTER, CC]) await expand(p, n);
  await until(p, `${card(LOCAL)}.querySelector('.model-small select') && ${card(CC)}.querySelector('.model-list')`);
  await sleep(250);
}

run(async () => {
  let once = true;
  for (const [label, w, h, mobile, dpr] of [["desktop", 1440, 900, false, 1], ["cover", 344, 882, true, 2.625], ["fold", 673, 841, true, 2.625]]) {
    for (const theme of ["dark", "light"]) {
      const tag = `${label}-${theme}`;
      if (process.env.ONLY && !tag.includes(process.env.ONLY)) continue;
      console.log(`— ${label} ${theme}`);
      await mock("/__mock/reset");
      await mock("/__mock/claude-code?state=ready");
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Small ${tag}`);
      await p.theme(theme);
      await openModels(p);

      // ── what each model shows ──
      const local = await rows(p, LOCAL);
      check(local?.length === 2 && local.every((r) => r.value === "auto" && r.shown === "Auto (on)"), `${tag}: the desk PC's models say Auto (on)`, local);
      check(local?.[0]?.options?.join() === "Auto (on),On,Off", `${tag}: the choices are Auto, On and Off`, local?.[0]?.options);
      check(local?.[0]?.aria === "Simpler instructions for Qwen3.8 27B", `${tag}: each choice is named for its model`, local?.[0]?.aria);
      const claude = await rows(p, CLAUDE);
      check(claude?.length === 3 && claude.every((r) => r.value === "auto" && r.shown === "Auto (off)"), `${tag}: Claude's models say Auto (off)`, claude);
      const router = await rows(p, ROUTER);
      check(router?.find((r) => r.name === "DeepSeek V3.2")?.shown === "Auto (off)" && router?.find((r) => r.name === "Mistral Small 3.2")?.shown === "Auto (on)", `${tag}: hosted, a big model is off and a known small one on`, router);
      const cc = await rows(p, CC);
      check(cc?.length === 3 && cc.every((r) => r.value === null), `${tag}: Claude Code's models have no such setting`, cc);
      const help = await p.eval(`[${card(LOCAL)}, ${card(CLAUDE)}, ${card(CC)}].map((c) => ({ notes: c.querySelectorAll('.model-small-help').length, text: c.querySelector('.model-small-help')?.textContent ?? null, times: c.textContent.split('shorter instructions').length - 1 }))`);
      check(help[0].notes === 1 && help[0].text === HELP && help[0].times === 1 && help[1].notes === 1 && help[1].times === 1, `${tag}: one line about it above each list, not on the rows`, help);
      check(help[2].notes === 0 && help[2].times === 0, `${tag}: …and none for Claude Code`, help[2]);

      // ── it fits ──
      check(await fits(p), `${tag}: no sideways scrolling, nothing past the edge`);
      for (const n of [LOCAL, CLAUDE]) {
        const l = await layout(p, n);
        check(l.every((r) => r.inCard && r.onScreen && r.reachable), `${tag}: ${n}: every choice fully visible and tappable`, l);
        check(l.every((r) => r.oneLine && !r.overlap && r.textFits && r.w >= 70), `${tag}: ${n}: label on one line, nothing overlapping, the choice readable`, l);
        check(l.every((r) => r.h >= (mobile ? 36 : 28)), `${tag}: ${n}: big enough to ${mobile ? "tap" : "click"}`, l.map((r) => r.h));
        // Wide cards keep it on the model's line; narrow ones (phones, the open Fold) give it its own.
        check(l.every((r) => r.ownLine === (label !== "desktop")), `${tag}: ${n}: ${label === "desktop" ? "after Make default, on the model's line" : "on a line of its own"}`, l.map((r) => r.ownLine));
        // Lined up, whether a row says "Make default" or "Default" (the Anthropic list has the default model).
        check(l.every((r) => Math.abs(r.left - l[0].left) <= 1), `${tag}: ${n}: the choices line up`, l.map((r) => r.left));
      }
      await p.shot(`${SHOTS}/small-mode-${tag}.png`);
      await p.shot(`${SHOTS}/small-mode-${tag}-local.png`, await clipOf(p, LOCAL));

      // ── a change goes to the server; every device hears it ──
      const other = await wsClient(`Other ${tag}`);
      const heard = () => other.events.filter((e) => e.type === "providers_changed").length;
      const before = heard();
      check(await choose(p, LOCAL, "Qwen3.8 27B", "off"), `${tag}: Qwen3.8 27B → Off`);
      // The list shows the server's answer: Off, and Auto no longer says what it would be.
      check(await until(p, `${sel(LOCAL, "Qwen3.8 27B")}.value === 'off' && ${sel(LOCAL, "Qwen3.8 27B")}.options[0].textContent === 'Auto'`, 6000), `${tag}: the list shows Off after providers_changed`, await row(p, LOCAL, "Qwen3.8 27B"));
      let s = await served(other, "local", "qwen3.8-27b");
      check(s?.small_mode === "off" && s.small === null, `${tag}: the server keeps Off (small mode no longer on for it)`, s);
      for (let i = 0; i < 20 && heard() <= before; i++) await sleep(100);
      check(heard() > before, `${tag}: another device heard providers_changed`, heard());
      let q = await row(p, LOCAL, "Qwen3.8 27B");
      check(q?.options?.join() === "Auto,On,Off", `${tag}: while Off, Auto doesn't guess what it would be`, q?.options);
      check((await row(p, LOCAL, "Gemma 4 12B"))?.shown === "Auto (on)", `${tag}: the other model is unchanged`);
      check(await choose(p, CLAUDE, "Claude Opus 5.5", "on"), `${tag}: Claude Opus 5.5 → On`);
      check(await until(p, `${sel(CLAUDE, "Claude Opus 5.5")}.value === 'on' && ${sel(CLAUDE, "Claude Opus 5.5")}.options[0].textContent === 'Auto'`, 6000), `${tag}: the list shows On after providers_changed`, await row(p, CLAUDE, "Claude Opus 5.5"));
      s = await served(other, "anthropic", "claude-opus-5-5");
      check(s?.small_mode === "on" && s.small === true, `${tag}: the server keeps On`, s);
      const inStore = await p.eval(`__mod('lib/store.ts').then((m) => { const f = (p, id) => m.state.providers.find((x) => x.id === p).models.find((x) => x.id === id); return [f('local', 'qwen3.8-27b').small_mode, !!f('local', 'qwen3.8-27b').small, f('anthropic', 'claude-opus-5-5').small_mode, !!f('anthropic', 'claude-opus-5-5').small]; })`);
      check(inStore.join() === "off,false,on,true", `${tag}: the app's model list has both`, inStore);
      await p.shot(`${SHOTS}/small-mode-${tag}-changed.png`, await clipOf(p, LOCAL));

      // ── after a reload ──
      await p.reload(1500);
      await p.waitFor(`document.querySelector('.workspace')`, 10000);
      await openModels(p);
      q = await row(p, LOCAL, "Qwen3.8 27B");
      const o = await row(p, CLAUDE, "Claude Opus 5.5");
      check(q?.value === "off" && q.shown === "Off" && o?.value === "on" && o.shown === "On", `${tag}: both choices are still there after a reload`, { q, o });

      // ── back to Auto: Tessera's own choice again ──
      await choose(p, LOCAL, "Qwen3.8 27B", "auto");
      check(await until(p, `${sel(LOCAL, "Qwen3.8 27B")}.selectedOptions[0].textContent === 'Auto (on)'`, 6000), `${tag}: Qwen3.8 27B back to Auto (on)`, await row(p, LOCAL, "Qwen3.8 27B"));
      await choose(p, CLAUDE, "Claude Opus 5.5", "auto");
      check(await until(p, `${sel(CLAUDE, "Claude Opus 5.5")}.selectedOptions[0].textContent === 'Auto (off)'`, 6000), `${tag}: Claude Opus 5.5 back to Auto (off)`, await row(p, CLAUDE, "Claude Opus 5.5"));
      const back = [await served(other, "local", "qwen3.8-27b"), await served(other, "anthropic", "claude-opus-5-5")];
      check(back[0]?.small_mode === null && back[0].small === true && back[1]?.small_mode === null && back[1].small === null, `${tag}: the server forgets the choice (Auto)`, back);

      if (once) {
        once = false;
        // The server's checks, mirrored by the mock.
        const errs = [];
        for (const params of [{ provider: "a/b", model: "x", mode: "on" }, { provider: "local", model: " ", mode: "on" }, { provider: "", model: "x", mode: "off" }, { provider: "local", model: "x", mode: "maybe" }]) {
          errs.push((await other.rpc("set_model_small", params)).error?.code ?? "accepted");
        }
        check(errs.every((e) => e === "bad_request"), "a provider with a slash, empty names or an unknown choice are refused", errs);
        // A model the list doesn't have (yet) is kept anyway, like the server's config.
        const ok = await other.rpc("set_model_small", { provider: "local", model: "not-listed", mode: "on" });
        check(ok.result === null && !ok.error, "a model not listed yet can still be set", ok);
        // A refusal: the choice goes back to what it was, and a toast says why.
        await p.eval(`__mod('lib/store.ts').then((m) => { window.__realSmall = m.api.setModelSmall; m.api.setModelSmall = () => Promise.reject(new Error('The server said no')); })`);
        await choose(p, LOCAL, "Gemma 4 12B", "off");
        check(await until(p, `${sel(LOCAL, "Gemma 4 12B")}.value === 'auto' && [...document.querySelectorAll('.toast-error')].some((t) => t.textContent.includes('The server said no'))`), "a refused change goes back to what it was, with a toast saying why", await row(p, LOCAL, "Gemma 4 12B"));
        await p.eval(`__mod('lib/store.ts').then((m) => { m.api.setModelSmall = window.__realSmall; })`);
        // Settings search finds it.
        await p.eval(`__mod('lib/ui.ts').then((u) => u.openSettings())`);
        await sleep(300);
        const found = await p.eval(`(() => { const i = document.querySelector('.settings-search input'); if (!i) return null; i.value = 'small model'; i.dispatchEvent(new InputEvent('input', { bubbles: true })); return new Promise((r) => setTimeout(() => r([...document.querySelectorAll('.settings-tab')].map((e) => e.dataset.tab)), 250)); })()`);
        check(found?.includes("models"), "searching Settings for “small model” finds Models", found);
      }
      other.close();
      check(p.errors.length === 0, `${tag}: no page errors`, p.errors);
      await p.close();
    }
  }
  finish();
});
