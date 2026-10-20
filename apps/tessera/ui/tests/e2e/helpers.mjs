// Helper limits: Settings → Models holds the defaults as plain number fields (no preset buttons), and
// each chat sets its own in Chat options → Helpers → Limits: any number at once, steps and time for each,
// or "No limit" (with a plain word on cost). Helpers past the limit wait their turn and say so ("Waiting
// for a slot", queued but still busy for older apps), and start as soon as the limit allows. Desktop,
// the folded phone (344 px) and the unfolded Fold; dark and light.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const SID = "ses_7d3a91e5c4b2"; // "Add a /healthz endpoint"
const ui = (p, js) => p.eval(`__mod('lib/ui.ts').then((u) => { ${js} })`);
const api = (p, js) => p.eval(`__mod('lib/store.ts').then((m) => ${js})`);
const until = (p, expr, ms = 6000) => p.waitFor(expr, ms).then(() => true, () => false);
/** Type into a field the way a person does (Solid listens for input events). */
const fill = (p, sel, v) =>
  p.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) return false; e.focus(); e.value = ${JSON.stringify(v)}; e.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
const fits = (p, sel) => p.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) return null; const r = e.getBoundingClientRect(); const kids = [...e.querySelectorAll('*')].every((k) => { const b = k.getBoundingClientRect(); return b.width === 0 || (b.left >= r.left - 1 && b.right <= r.right + 1); }); return kids && r.left >= -1 && r.right <= innerWidth + 1 && e.scrollWidth <= e.clientWidth + 1; })()`);

const targets = async (p, container, min, tag) => {
  const rects = await p.eval(`(() => { const root = document.querySelector(${JSON.stringify(container)}); return [...root.querySelectorAll('.hl-num:not(:disabled), .hl-none, .hl-acts .btn:not(:disabled), .hl-sum')].map((e) => { e.scrollIntoView({block:'center'}); const r = e.getBoundingClientRect(); const hit = document.elementFromPoint(r.x+r.width/2, r.y+r.height/2); return {text:e.textContent, width:r.width, height:r.height, hit:e === hit || e.contains(hit)}; }); })()`);
  check(rects.length > 0 && rects.every((r) => r.width >= 40 && r.height >= min && r.hit), `${tag}: ${min}px helper control targets with working center hit-tests`, rects);
};

async function send(p, mobile, msg) {
  await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
  await p.type(msg);
  if (mobile) await p.click(".pane.focused .send-btn");
  else await p.key("Enter", "Enter", 0, 13);
}

async function openOptions(p) {
  const sel = await p.eval(`(() => { for (const s of ['.pane.focused .opts-btn', '.pane.focused .title-menu']) { const e = document.querySelector(s); if (e && e.getBoundingClientRect().width > 0) return s; } return null; })()`);
  if (sel) await p.click(sel);
  const ready = await until(p, `!!document.querySelector('.tile-opts .hl-sum')`, 6000);
  if (!ready) {
    console.log('options diagnostic', await p.eval(`({ focused: document.querySelector('.pane.focused')?.textContent?.slice(0, 1000), options: document.querySelector('.tile-opts')?.textContent, buttons: [...document.querySelectorAll('.pane.focused .opts-btn, .pane.focused .title-menu')].map((e) => ({text:e.textContent, rect:e.getBoundingClientRect().toJSON(), expanded:e.getAttribute('aria-expanded')})) })`));
    await p.shot(`${SHOTS}/helpers-options-failure.png`);
  }
  return ready;
}

run(async () => {
  for (const [label, w, h, mobile, dpr] of [["desktop", 1440, 900, false, 1], ["phone", 344, 760, true, 2.6], ["fold", 841, 701, true, 2.625]]) {
    for (const theme of ["dark", "light"]) {
      const tag = `${label}-${theme}`;
      console.log(`— ${tag}`);
      await mock("/__mock/reset");
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Helpers ${label}`);
      await p.theme(theme);

      // ── Settings → Models: the defaults, as plain numbers ─────────────────────
      await ui(p, `u.openSettings('models')`);
      check(await until(p, `document.querySelectorAll('.helpers-card .hl-num').length === 3`, 8000), `${tag}: Models shows the helper defaults as three number fields`);
      await sleep(400);
      const card = await p.eval(`(() => { const c = document.querySelector('.helpers-card'); return { title: c.querySelector('.setting-title')?.textContent, help: c.querySelector('.setting-help')?.textContent, labels: [...c.querySelectorAll('.hl-label > span:first-child')].map((e) => e.textContent), values: [...c.querySelectorAll('.hl-num')].map((e) => e.value), presets: c.querySelectorAll('[role=radio], .segmented').length, note: c.querySelector('.hl-note')?.textContent }; })()`);
      check(card.title === "Helpers" && card.presets === 0, `${tag}: …no preset buttons`, card);
      check(JSON.stringify(card.values) === JSON.stringify(["4", "40", "15"]), `${tag}: …4 at once, 40 steps, 15 minutes`, card.values);
      check(/Usual for this server: 4/.test(card.help ?? "") && /chat's options/.test(card.help ?? ""), `${tag}: …explained in plain words, pointing to each chat's own`, card.help);
      check(/up to 16 helpers at once across all chats/.test(card.note ?? ""), `${tag}: …and what the server carries in total`, card.note);
      check(await p.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth <= b.clientWidth + 1; })()`), `${tag}: …without scrolling sideways`);
      check(await fits(p, ".helpers-card"), `${tag}: …every field inside the card`);
      await p.eval(`document.querySelector('.helpers-card').scrollIntoView({ block: 'center' })`);
      await sleep(250);
      await targets(p, '.helpers-card', mobile ? 44 : 40, tag);
      await p.shot(`${SHOTS}/helpers-settings-${tag}.png`);
      // Any number, well past the old presets; then 2 for the chat below.
      await fill(p, ".helpers-card .hl-num", "12");
      await p.eval(`[...document.querySelectorAll('.helpers-card .btn-primary')].find((b) => b.textContent.trim() === 'Save').click()`);
      check(await until(p, `__mod('lib/store.ts').then((m) => m.api.getHelpers()).then((h) => h.at_once === 12)`), `${tag}: 12 at once is saved, and the server uses it`);
      await fill(p, ".helpers-card .hl-num", "0");
      await p.eval(`[...document.querySelectorAll('.helpers-card .btn-primary')].find((b) => b.textContent.trim() === 'Save').click()`);
      await sleep(400);
      check((await api(p, `m.api.getHelpers()`))?.at_once === 12, `${tag}: typed 0 is refused; No limit is an explicit checkbox`);
      // The intentional validation error stays visible until dismissed and
      // covers the phone's title. Close it before exercising chat options.
      if (await p.eval(`!!document.querySelector('.toast-x')`)) await p.click('.toast-x');
      check(await until(p, `!document.querySelector('.toast-error')`), `${tag}: dismissed the expected validation warning`);
      await p.eval(`document.querySelectorAll('.helpers-card .hl-row .hl-none input').forEach((e) => e.click())`);
      check(await until(p, `/plan or credits/.test(document.querySelector('.helpers-card .hl-warn')?.textContent ?? '')`), `${tag}: server No limit has a cost warning`);
      await p.eval(`[...document.querySelectorAll('.helpers-card .btn-primary')].find((b) => b.textContent.trim() === 'Save').click()`);
      check(await until(p, `__mod('lib/store.ts').then((m) => m.api.getHelpers()).then((h) => h.at_once === 0 && h.steps === 0 && h.minutes === 0)`), `${tag}: all three server defaults allow No limit`);
      await p.eval(`document.querySelectorAll('.helpers-card .hl-row .hl-none input').forEach((e) => e.click())`);
      await fill(p, '.helpers-card .hl-row:nth-of-type(2) .hl-num', '40');
      await fill(p, '.helpers-card .hl-row:nth-of-type(3) .hl-num', '15');
      await fill(p, ".helpers-card .hl-num", "2");
      await p.eval(`[...document.querySelectorAll('.helpers-card .btn-primary')].find((b) => b.textContent.trim() === 'Save').click()`);
      check(await until(p, `__mod('lib/store.ts').then((m) => m.api.getHelpers()).then((h) => h.at_once === 2)`), `${tag}: back to 2`);
      await p.key("Escape", "Escape", 0, 27);
      // On a phone Escape goes back to the settings list, not out of Settings.
      if (await p.eval(`!!document.querySelector('.settings')`)) await p.click('.settings-x');
      check(await until(p, `!document.querySelector('.settings')`), `${tag}: closed Settings before opening the chat`);
      await sleep(500);

      // ── Chat options → Helpers → Limits: this chat's own ─────────────────────
      await p.eval(`__mod('lib/workspace.ts').then((m) => m.openSession('${SID}', { pane: m.workspace.focused() }))`);
      await sleep(1200);
      check(await openOptions(p), `${tag}: chat options show the helper limits under Helpers`);
      check(await until(p, `/2 at once · 40 steps · 15 min/.test(document.querySelector('.tile-opts .hl-sum')?.textContent ?? '')`), `${tag}: …the server's numbers while the chat has none`, await p.eval(`document.querySelector('.tile-opts .hl-sum')?.textContent`));
      await p.click(".tile-opts .hl-sum");
      check(await until(p, `document.querySelectorAll('.tile-opts .hl-num').length === 3`), `${tag}: Change opens three fields in place`);
      await targets(p, '.tile-opts', mobile ? 44 : 40, tag);
      const ph = await p.eval(`[...document.querySelectorAll('.tile-opts .hl-num')].map((e) => e.placeholder)`);
      check(JSON.stringify(ph) === JSON.stringify(["2", "40", "15"]), `${tag}: …empty ones show the server's default`, ph);
      check(!(await p.eval(`!!document.querySelector('.tile-opts .hl-warn')`)), `${tag}: …no cost warning before choosing No limit`);
      await fill(p, ".tile-opts .hl-num", "1");
      // No time limit: a plain word on cost appears.
      await p.eval(`document.querySelectorAll('.tile-opts .hl-none input')[2].click()`);
      check(await until(p, `/plan or credits/.test(document.querySelector('.tile-opts .hl-warn')?.textContent ?? '')`), `${tag}: No limit comes with a plain word on cost`);
      check(await fits(p, ".tile-opts"), `${tag}: …the fields fit the options`);
      await p.shot(`${SHOTS}/helpers-chat-limits-${tag}.png`, await p.eval(`(() => { const r = document.querySelector('.tile-opts').getBoundingClientRect(); return { x: Math.max(0, r.x - 6), y: Math.max(0, r.y - 6), width: Math.min(innerWidth, r.width + 12), height: r.height + 12 }; })()`));
      await p.eval(`[...document.querySelectorAll('.tile-opts .hl-acts .btn')].find((b) => b.textContent.trim() === 'Save').click()`);
      check(await until(p, `__mod('lib/store.ts').then((m) => JSON.stringify(m.state.sessions['${SID}']?.info.helper_limits) === JSON.stringify({ at_once: 1, minutes: 0 }))`), `${tag}: saved on the chat: 1 at once, no time limit`);
      check(await until(p, `/1 at once · 40 steps · no time limit/.test(document.querySelector('.tile-opts .hl-sum')?.textContent ?? '')`), `${tag}: …and the summary says so`, await p.eval(`document.querySelector('.tile-opts .hl-sum')?.textContent`));
      await p.key("Escape", "Escape", 0, 27);
      await sleep(500);

      // ── Three helpers, one at a time: two wait their turn and say so ──────────
      await send(p, mobile, "delegate the reconnect research in parallel");
      check(await until(p, `document.querySelectorAll('.pane.focused .subagent').length === 3`, 25000), `${tag}: three helper cards`);
      check(await until(p, `document.querySelectorAll('.pane.focused .subagent[data-status=waiting]').length === 2`, 8000), `${tag}: two of them wait (this chat: one at a time)`);
      check(await until(p, `document.querySelectorAll('.pane.focused .subagent[data-status=running]').length === 1`, 8000), `${tag}: one works`);
      const snap = await api(p, `m.api.getSession('${SID}')`);
      check(snap.children.filter((i) => i.helper_queued && i.status === "running").length === 2, `${tag}: …queued on the server, still busy for older apps`, snap.children.map((i) => [i.status, i.helper_queued]));
      const cards = await p.eval(`[...document.querySelectorAll('.pane.focused .subagent')].map((c) => ({ status: c.dataset.status, nameW: Math.round(c.querySelector('.subagent-name').getBoundingClientRect().width), cut: c.querySelector('.subagent-name').scrollWidth > c.querySelector('.subagent-name').clientWidth + 1, stats: c.querySelector('.subagent-stats')?.textContent.trim(), activity: c.querySelector('.subagent-activity')?.textContent }))`);
      const waiting = cards.filter((c) => c.status === "waiting");
      check(waiting.length === 2 && waiting.every((c) => c.activity === "Waiting for a slot" && !c.stats), `${tag}: …"Waiting for a slot", no steps (not "Finished")`, cards);
      check(cards.every((c) => c.nameW >= 90 && !c.cut), `${tag}: every name keeps its room next to role and numbers (none cut short)`, cards);
      await p.eval(`document.querySelector('.pane.focused .subagent')?.scrollIntoView({ block: 'center' })`);
      await sleep(200);
      await p.shot(`${SHOTS}/helpers-cards-${tag}.png`);
      // No limit at once, from the chat's options: the waiting ones start now.
      check(await openOptions(p), `${tag}: options again`);
      await p.click(".tile-opts .hl-sum");
      await until(p, `document.querySelectorAll('.tile-opts .hl-none input').length === 3`);
      await p.eval(`document.querySelectorAll('.tile-opts .hl-none input')[0].click()`);
      await p.eval(`[...document.querySelectorAll('.tile-opts .hl-acts .btn')].find((b) => b.textContent.trim() === 'Save').click()`);
      check(await until(p, `/any number at once/.test(document.querySelector('.tile-opts .hl-sum')?.textContent ?? '')`), `${tag}: No limit at once is saved`);
      await p.key("Escape", "Escape", 0, 27);
      check(await until(p, `!document.querySelector('.pane.focused .subagent[data-status=waiting]')`, 8000), `${tag}: the waiting ones start at once`);
      // Back to the server's defaults.
      check(await openOptions(p), `${tag}: options once more`);
      await p.click(".tile-opts .hl-sum");
      await until(p, `!!document.querySelector('.tile-opts .hl-acts')`);
      await p.eval(`[...document.querySelectorAll('.tile-opts .hl-acts .btn')].find((b) => b.textContent.trim() === 'Use defaults').click()`);
      check(await until(p, `__mod('lib/store.ts').then((m) => !m.state.sessions['${SID}']?.info.helper_limits)`), `${tag}: Use defaults clears the chat's own`);
      await p.key("Escape", "Escape", 0, 27);
      check(!p.errors.length, `${tag}: no page errors`, p.errors);
      // Distinct server disabled state survives a settings round trip.
      await api(p, `m.api.setHelpers(0, 40, 15, false)`);
      check((await api(p, `m.api.getHelpers()`)).enabled === false, `${tag}: disabled is distinct from No limit`);
      await api(p, `m.api.setHelpers(4, 40, 15, true)`);
      await p.close();
    }
  }
  finish();
});
