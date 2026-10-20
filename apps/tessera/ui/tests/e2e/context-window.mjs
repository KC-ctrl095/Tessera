// Models on the user's own server whose context window is too small for Tessera: Settings → Models
// says so in plain words with how to raise it. The model for new chats says it on the provider's card
// (before opening the list), every affected model says it under its row, the window figure turns amber,
// and nothing is said for a window that is big enough, for a model without a warning, or once the
// server runs it with more (no reload: providers_changed). Desktop, the folded Fold's cover screen
// (344 px), the unfolded Fold; dark and light. ONLY=cover-dark runs one.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const LOCAL = "llama.cpp (desk)";
const CLAUDE = "Anthropic";
const until = (p, expr, ms = 4000) => p.waitFor(expr, ms).then(() => true, () => false);
const card = (name) =>
  `[...document.querySelectorAll('.settings-body .card.provider')].find((c) => c.querySelector('.provider-name')?.firstChild?.textContent.trim() === ${JSON.stringify(name)})`;
const toggle = (p, name) =>
  p.eval(`(() => { const b = ${card(name)}?.querySelector('.provider-actions button[aria-expanded]'); if (!b) return false; b.click(); return true; })()`);
const isOpen = (p, name) => p.eval(`${card(name)}?.querySelector('.provider-actions button[aria-expanded]')?.getAttribute('aria-expanded') === 'true'`);
/** The card's own line (closed list) and every model's row. */
const state = (p, name) =>
  p.eval(`(() => {
    const c = ${card(name)};
    if (!c) return null;
    const hint = c.querySelector('.provider-hint.ctx-warn');
    return {
      hint: hint ? { text: hint.textContent.trim(), color: getComputedStyle(hint).color } : null,
      rows: [...c.querySelectorAll('.model-list li')].map((li) => {
        const w = li.querySelector('.model-ctx-warn');
        const b = li.querySelector('.ctx-badge');
        return {
          name: li.querySelector('.strong')?.textContent ?? null,
          warn: w ? w.textContent.trim() : null,
          badge: b?.textContent.trim() ?? null,
          badgeWarn: b?.classList.contains('warn') ?? false,
          badgeColor: b ? getComputedStyle(b).color : null,
          below: w ? w.getBoundingClientRect().top >= li.querySelector('.model-li-main').getBoundingClientRect().bottom - 1 : null,
          fits: w ? w.getBoundingClientRect().right <= li.getBoundingClientRect().right + 0.5 && w.scrollWidth <= w.clientWidth + 1 : null,
          color: w ? getComputedStyle(w).color : null,
        };
      }),
    };
  })()`);
const fits = (p) =>
  p.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth <= b.clientWidth + 1 && [...b.querySelectorAll('.model-list li, .provider-hint')].every((e) => e.getBoundingClientRect().right <= innerWidth + 0.5); })()`);
const clipOf = (p, name) =>
  p.eval(`(() => { const c = ${card(name)}; c.scrollIntoView({ block: 'start' }); const r = c.getBoundingClientRect(); return { x: Math.max(0, r.x - 4), y: Math.max(0, r.y - 4), width: Math.min(innerWidth, r.width + 8), height: Math.min(innerHeight - Math.max(0, r.y - 4), r.height + 8) }; })()`);

async function openModels(p) {
  await p.eval(`__mod('lib/ui.ts').then((u) => u.openSettings('providers'))`);
  await p.waitFor(`${card(LOCAL)} && ${card(CLAUDE)}`, 8000);
  await sleep(500);
}

run(async () => {
  for (const [label, w, h, mobile, dpr] of [["desktop", 1440, 900, false, 1], ["cover", 344, 882, true, 2.625], ["fold", 673, 841, true, 2.625]]) {
    for (const theme of ["dark", "light"]) {
      const tag = `${label}-${theme}`;
      if (process.env.ONLY && !tag.includes(process.env.ONLY)) continue;
      console.log(`— ${label} ${theme}`);
      await mock("/__mock/reset");
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Window ${tag}`);
      await p.theme(theme);
      const other = await wsClient(`Other ${tag}`);
      // The model for new chats is Gemma on the desk PC (as picking it in Settings does).
      await p.eval(`__mod('lib/store.ts').then(async (m) => { const g = { provider: 'local', model: 'gemma-4-12b' }; await m.api.setDefaultModel(g); m.setState('server', 'default_model', g); })`);

      // ── nothing to say while the server says nothing ──
      await openModels(p);
      let s = await state(p, LOCAL);
      check(s && !s.hint, `${tag}: no line on the card while nothing is wrong`, s?.hint);
      await toggle(p, LOCAL);
      await until(p, `${card(LOCAL)}.querySelector('.model-list li')`);
      s = await state(p, LOCAL);
      check(s.rows.length === 2 && s.rows.every((r) => !r.warn && !r.badgeWarn), `${tag}: no model has a warning`, s.rows);
      await toggle(p, LOCAL);

      // ── the server runs Gemma with 8,192 tokens (llama-server -c 8192) ──
      await mock("/__mock/model-window?id=local&model=gemma-4-12b&tokens=8192&server=llama");
      check(await until(p, `${card(LOCAL)}.querySelector('.provider-hint.ctx-warn')`, 6000), `${tag}: the card says it without a reload`);
      s = await state(p, LOCAL);
      check(s.hint.text.startsWith("Gemma 4 12B: llama.cpp runs this model with a window of 8,192 tokens."), `${tag}: it names the model and what the server runs`, s.hint.text);
      check(s.hint.text.includes("too small for Tessera") && s.hint.text.includes("-c 32768"), `${tag}: it says why it matters and how to raise it`, s.hint.text);
      check(s.hint.color !== (await p.eval(`getComputedStyle(document.querySelector('.settings-body .setting-title, .settings-body')).color`)), `${tag}: it is in the warning colour`, s.hint.color);
      check(await fits(p), `${tag}: the card's line fits the screen`);
      await p.shot(`${SHOTS}/context-window-${tag}-card.png`, await clipOf(p, LOCAL));
      // The list (open) has it under the row instead, once.
      await toggle(p, LOCAL);
      await until(p, `${card(LOCAL)}.querySelector('.model-ctx-warn')`);
      check(await isOpen(p, LOCAL), `${tag}: the list opens`);
      s = await state(p, LOCAL);
      const gemma = s.rows.find((r) => r.name === "Gemma 4 12B");
      const qwen = s.rows.find((r) => r.name === "Qwen3.8 27B");
      check(!s.hint, `${tag}: the card's line goes while the list is open (it is under the row)`, s.hint);
      check(gemma?.warn?.startsWith("llama.cpp runs this model with a window of 8,192 tokens."), `${tag}: the row says it`, gemma?.warn);
      check(gemma.below && gemma.fits, `${tag}: …on a line of its own that fits the row`, gemma);
      check(gemma.badge === "8k" || gemma.badge === "8.2k" || /^8/.test(gemma.badge), `${tag}: the window figure shows what the server runs`, gemma.badge);
      check(gemma.badgeWarn && gemma.badgeColor !== qwen.badgeColor, `${tag}: …in the warning colour`, [gemma.badgeColor, qwen.badgeColor]);
      check(!qwen.warn && !qwen.badgeWarn, `${tag}: the other model is unchanged`, qwen);
      check(await fits(p), `${tag}: no sideways scrolling, nothing past the edge`);
      await p.shot(`${SHOTS}/context-window-${tag}-list.png`, await clipOf(p, LOCAL));

      // ── Qwen runs on Ollama with 4,096: a second model gets its line, in Ollama's words ──
      await mock("/__mock/model-window?id=local&model=qwen3.8-27b&tokens=4096&server=ollama");
      check(await until(p, `${card(LOCAL)}.querySelectorAll('.model-ctx-warn').length === 2`, 6000), `${tag}: a second model gets its line`);
      s = await state(p, LOCAL);
      const q2 = s.rows.find((r) => r.name === "Qwen3.8 27B");
      check(q2.warn.startsWith("Ollama runs this model with a window of 4,096 tokens.") && q2.warn.includes("OLLAMA_CONTEXT_LENGTH"), `${tag}: …saying what Ollama runs it with, and how to raise it`, q2.warn);
      check(/^4\.?\d*k?$/.test(q2.badge) && q2.badgeWarn, `${tag}: …and its figure is what the server runs, in the warning colour`, q2.badge);
      // Other providers are left alone.
      const claude = await state(p, CLAUDE);
      check(claude && !claude.hint, `${tag}: another provider has no line`, claude?.hint);

      // ── the server runs it with 32,768 now: everything goes, without a reload ──
      await mock("/__mock/model-window?id=local&model=gemma-4-12b&tokens=32768&server=llama");
      await mock("/__mock/model-window?id=local&model=qwen3.8-27b&tokens=0");
      check(await until(p, `!${card(LOCAL)}.querySelector('.model-ctx-warn')`, 6000), `${tag}: the lines go when the window is big enough`);
      s = await state(p, LOCAL);
      check(s.rows.every((r) => !r.warn && !r.badgeWarn), `${tag}: no figure is amber any more`, s.rows);
      check(/^3[23]/.test(s.rows.find((r) => r.name === "Gemma 4 12B").badge), `${tag}: Gemma shows its new window`, s.rows);
      await toggle(p, LOCAL);
      check(await until(p, `!${card(LOCAL)}.querySelector('.provider-hint.ctx-warn')`), `${tag}: nothing on the closed card either`);

      // ── after a reload the warning is still there (the server keeps saying it) ──
      await mock("/__mock/model-window?id=local&model=gemma-4-12b&tokens=4096&server=lmstudio");
      await p.reload(1500);
      await p.waitFor(`document.querySelector('.workspace')`, 10000);
      await openModels(p);
      s = await state(p, LOCAL);
      check(s?.hint?.text.startsWith("Gemma 4 12B: LM Studio runs this model with a window of 4,096 tokens.") && s.hint.text.includes("Context Length"), `${tag}: after a reload the card still says it, in LM Studio's words`, s?.hint);
      check(p.errors.length === 0, `${tag}: no page errors`, p.errors);
      other.close();
      await p.close();
    }
  }
  finish();
});
