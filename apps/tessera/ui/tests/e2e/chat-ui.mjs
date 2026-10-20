// A calmer chat: the model menu (thinking level inside, providers that fold),
// the goal above the composer, no "Getting ready" line, and slim subagent
// reports. (The subagent bar is in chat-agents.mjs.)
// Desktop, phone and Fold, dark and light. ONLY=phone-dark runs one of them.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const SID = "ses_7d3a91e5c4b2"; // "Add a /healthz endpoint" (Claude Sonnet 5.5, six thinking levels)
const PLAN = "ses_8c1f2a9d4e70"; // has a goal, a plan and done-when criteria
const AUDIT = "ses_7a1c5e93b2d0"; // two finished background subagents and their reports
const SWARM = "ses_3c8d1f5a7e62"; // a finished swarm: 24 reports in one group
const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const open = (p, sid) => ws(p, `return m.openSession('${sid}', { pane: m.workspace.focused() })`);
const rect = (p, sel) =>
  p.eval(`(() => { const e = document.querySelector('.pane.focused ${sel}'); if (!e) return null; const r = e.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height, b: r.bottom }; })()`);
const count = (p, sel) => p.eval(`document.querySelectorAll(${JSON.stringify(sel)}).length`);
const text = (p, sel) => p.eval(`document.querySelector(${JSON.stringify(sel)})?.textContent ?? null`);
/** Wait (the machine may be busy) until `expr` holds; false if it never does. */
const until = (p, expr, ms = 4000) => p.waitFor(expr, ms).then(() => true, () => false);
// Use a real reader scroll to reveal a control, not an unowned scrollIntoView that follow corrects.
async function reveal(p, sel) {
  await p.eval(`new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))`);
  const move = await p.eval(`(() => {
    const el = document.querySelector(${JSON.stringify(sel)}), t = el?.closest('.transcript');
    if (!t) return null;
    const r = t.getBoundingClientRect(), e = el.getBoundingClientRect();
    if (e.top >= r.top + 5 && e.bottom <= r.bottom - 5) return null;
    return { x: r.left + r.width / 2, y: r.top + r.height / 2,
      deltaX: 0, deltaY: (e.top + e.bottom - r.top - r.bottom) / 2, type: 'mouseWheel' };
  })()`);
  if (move) { await p.send('Input.dispatchMouseEvent', move); await sleep(400); }
}
/** Click `sel` until `expr` holds (a click can land while the page is still busy laying out). */
async function clickUntil(p, sel, expr) {
  for (let k = 0; k < 3; k++) {
    await reveal(p, sel);
    await p.click(sel);
    await p.eval(`new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))`);
    if (await until(p, expr, 1500)) return true;
  }
  return false;
}

async function send(p, mobile, msg) {
  await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
  await p.type(msg);
  // Enter sends on a desktop; on a phone it is a new line and the button sends.
  if (mobile) await p.click(".pane.focused .send-btn");
  else await p.key("Enter", "Enter", 0, 13);
}

/** Open the model menu from whatever shows it (the chip, the line under the title on narrow tiles, or on phones the Model line in the title's sheet). */
async function openModelMenu(p) {
  const direct = await p.eval(`(() => { const b = [...document.querySelectorAll('.pane.focused .model-chip, .pane.focused .model-sub')].find((e) => e.offsetParent); b?.click(); return !!b; })()`);
  if (!direct) {
    await p.click(".pane.focused .title-menu");
    await p.waitFor(`document.querySelector('.tile-opts .opt-model')`, 3000);
    await p.click(".tile-opts .opt-model");
  }
  await p.waitFor(`document.querySelector('.pop-model .model-menu')`, 3000);
  await sleep(350);
}
/** The thinking level the chat shows: next to the model in the header, or (phones) on the Model line of the title's sheet. */
async function levelShown(p) {
  const head = await p.eval(`document.querySelector('.pane.focused .model-chip-think, .pane.focused .model-sub-think')?.textContent ?? null`);
  if (head !== null || !(await p.eval(`!!document.querySelector('.pane.focused .title-menu')`))) return head;
  await p.click(".pane.focused .title-menu");
  await p.waitFor(`document.querySelector('.tile-opts .opt-model')`, 3000);
  const v = await p.eval(`document.querySelector('.tile-opts .opt-model-think')?.textContent ?? null`);
  await p.key("Escape", "Escape", 0, 27);
  await sleep(250);
  return v;
}
async function closeMenu(p) {
  await p.key("Escape", "Escape", 0, 27);
  await sleep(250);
}

run(async () => {
  for (const [label, w, h, mobile, dpr] of [["desktop", 1440, 900, false, 1], ["phone", 375, 812, true, 2], ["fold", 841, 701, true, 2.625]]) {
    for (const theme of ["dark", "light"]) {
      const tag = `${label}-${theme}`;
      if (process.env.ONLY && !tag.includes(process.env.ONLY)) continue; // a quick run: ONLY=phone-dark
      console.log(`— ${label} ${theme}`);
      const first = theme === "dark";
      await mock("/__mock/reset");
      await mock("/__mock/mosaics/reset");
      const probe = await wsClient("chat-ui probe");
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Chat UI ${label}`);
      await p.theme(theme);
      await open(p, SID);
      await sleep(1300);

      // ── the model menu: thinking inside, nothing to look for elsewhere ──
      const shown0 = await levelShown(p);
      check(shown0 === "Medium", `${tag}: the thinking level shows next to the model (${shown0})`);
      await openModelMenu(p);
      check((await count(p, ".pop-model .menu-think .think-chip")) === 6, `${tag}: the model menu offers the model's six thinking levels`);
      check((await text(p, ".pop-model .think-chip.on")) === "Medium", `${tag}: …with the chat's level on`);
      check(!(await p.eval(`!!document.querySelector('.pop-model .menu-think')?.closest('.model-groups')`)), `${tag}: …above the list, not scrolling with it`);
      await p.shot(`${SHOTS}/chatui-model-menu-${tag}.png`);
      await p.clickText(".pop-model .think-chip", "High");
      await until(p, `document.querySelector('.pop-model .think-chip.on')?.textContent === 'High'`);
      check((await text(p, ".pop-model .think-chip.on")) === "High", `${tag}: picking a level takes effect at once`);
      check(!!(await p.eval(`!!document.querySelector('.pop-model .model-menu')`)), `${tag}: …and the menu stays for the model`);
      check(/Thorough/.test((await text(p, ".pop-model .opt-head-hint")) ?? ""), `${tag}: the line above explains the level`);
      const patched = await p.eval(`__mod('lib/actions.ts').then((m) => m.paneSettings(document.querySelector('.pane.focused').dataset.paneId).thinking)`);
      check(patched === "high", `${tag}: the chat's thinking is high`, patched);
      await closeMenu(p);
      await sleep(300);
      const follow = await levelShown(p);
      check(follow === "High", `${tag}: the level shown follows`, follow);

      // ── the level picked last for a model is where its next chat starts ──
      await openModelMenu(p);
      await p.clickText(".pop-model .think-chip", "Extra high");
      await until(p, `document.querySelector('.pop-model .think-chip.on')?.textContent === 'Extra high'`);
      await closeMenu(p);
      const kept = await until(p, `__mod('lib/store.ts').then((m) => m.state.providers.some((x) => x.models.some((y) => y.id === 'claude-sonnet-5-5' && y.last_thinking === 'xhigh')))`);
      check(kept, `${tag}: the server keeps it for the model`);
      await ws(p, `return m.newChat({ pane: m.workspace.focused() })`);
      await until(p, `__mod('lib/actions.ts').then((m) => m.paneSessionId(document.querySelector('.pane.focused').dataset.paneId) === null)`);
      await sleep(400);
      const fresh = await p.eval(`__mod('lib/actions.ts').then((m) => { const id = document.querySelector('.pane.focused').dataset.paneId; return { s: m.paneSessionId(id), t: m.paneSettings(id).thinking }; })`);
      check(fresh?.s === null && fresh?.t === "xhigh", `${tag}: a new chat on that model starts at it`, fresh);
      await openModelMenu(p);
      check((await text(p, ".pop-model .think-chip.on")) === "Extra high", `${tag}: …and its model menu shows it on at once`);
      if (first) await p.shot(`${SHOTS}/chatui-new-chat-level-${tag}.png`);
      await closeMenu(p);
      await open(p, SID);
      await sleep(900);

      // The options menu no longer has a second place for it.
      await p.click(".pane.focused .opts-btn, .pane.focused .title-menu");
      await until(p, `document.querySelector('.tile-opts')`);
      check((await count(p, ".tile-opts")) === 1 && (await count(p, ".tile-opts .think-chip")) === 0, `${tag}: the options menu has no thinking row`);
      await closeMenu(p);

      // /thinking and the palette go to the model menu, on the level.
      await send(p, mobile, "/thinking");
      await p.waitFor(`document.querySelector('.pop-model .think-chip')`, 3000);
      await sleep(300);
      check(await p.eval(`document.activeElement?.classList.contains('think-chip') && document.activeElement.classList.contains('on')`), `${tag}: /thinking opens the model menu on the level`);
      await closeMenu(p);

      // A model with no thinking modes: no row.
      await openModelMenu(p);
      await p.clickText(".pop-model .model-item", "Gemma 4 12B");
      await until(p, `!document.querySelector('.pane.focused .model-chip-think, .pane.focused .model-sub-think') && !document.querySelector('.pop-model')`);
      await sleep(200);
      await openModelMenu(p);
      check((await count(p, ".pop-model .menu-think")) === 0, `${tag}: a model without thinking modes shows no row`);
      await closeMenu(p);
      check((await levelShown(p)) === null, `${tag}: …and no level next to the model`);

      // ── providers fold, and stay folded ──
      await openModelMenu(p);
      const heads = await p.eval(`[...document.querySelectorAll('.pop-model .model-group-toggle')].map((e) => e.querySelector('.model-group-name').textContent)`);
      check(heads.includes("Anthropic") && heads.includes("OpenRouter"), `${tag}: every provider has a head you can press`, heads);
      const before = await count(p, ".pop-model .model-item");
      await p.clickText(".pop-model .model-group-toggle", "Anthropic");
      await until(p, `document.querySelector('.pop-model .model-group-toggle[aria-expanded="false"]')`);
      const after = await count(p, ".pop-model .model-item");
      check(after === before - 3, `${tag}: folding Anthropic hides its three models (${before} → ${after})`);
      check(await p.eval(`document.querySelector('.pop-model .model-group-toggle[aria-expanded="false"]')?.textContent.includes('Anthropic')`), `${tag}: …and says so (aria-expanded)`);
      if (first) await p.shot(`${SHOTS}/chatui-model-folded-${tag}.png`);
      await closeMenu(p);
      await p.reload(1800);
      await open(p, SID);
      await sleep(900);
      await openModelMenu(p);
      check((await count(p, ".pop-model .model-item")) === after, `${tag}: it is still folded after a reload`);
      await p.clickText(".pop-model .model-group-toggle", "Anthropic");
      await until(p, `document.querySelectorAll('.pop-model .model-item').length === ${before}`);
      check((await count(p, ".pop-model .model-item")) === before, `${tag}: and opens again`);
      await closeMenu(p);

      // A long list (OpenRouter) starts folded; a search opens what it finds.
      const models = Array.from({ length: 60 }, (_, i) => ({ id: `vendor/model-${String(i + 1).padStart(2, "0")}`, name: `Vendor model ${i + 1}`, context_window: 131072 }));
      await probe.rpc("save_provider", { spec: { id: "openrouter", name: "OpenRouter", kind: "openai_chat", base_url: "https://openrouter.ai/api/v1", api_key: "sk-test", models } });
      await until(p, `__mod('lib/store.ts').then((m) => m.state.providers.find((x) => x.id === 'openrouter')?.models.length === 60)`);
      await openModelMenu(p);
      const longFolded = await p.eval(`document.querySelector('.pop-model .model-group-toggle[aria-expanded="false"] .model-group-name')?.textContent`);
      check(longFolded === "OpenRouter", `${tag}: a 60-model provider starts folded`, longFolded);
      check((await text(p, ".pop-model .model-group-toggle[aria-expanded='false'] .model-group-count")) === "60", `${tag}: …with its count on the head`);
      check((await count(p, ".pop-model .model-item")) < 12, `${tag}: …so the list stays short (${await count(p, ".pop-model .model-item")} rows)`);
      await p.eval(`(() => { const i = document.querySelector('.pop-model .menu-search input'); i.focus(); })()`);
      await p.type("model-07");
      await until(p, `document.querySelectorAll('.pop-model .model-item').length === 1`);
      check((await count(p, ".pop-model .model-item")) === 1 && /model-07/.test((await text(p, ".pop-model .model-item .menu-desc")) ?? ""), `${tag}: a search opens the folded provider and finds the model`);
      if (first) await p.shot(`${SHOTS}/chatui-model-search-${tag}.png`);
      await closeMenu(p);
      // Put the chat back on its own model for what follows.
      await openModelMenu(p);
      await p.clickText(".pop-model .model-item", "Claude Sonnet 5.5");
      await until(p, `!document.querySelector('.pop-model')`);

      // ── the goal sits above the composer ──
      await send(p, mobile, "/goal Ship the healthz endpoint");
      await until(p, `document.querySelector('.pane.focused .composer-dock .goal')`);
      const goal = await rect(p, ".goal");
      const comp = await rect(p, ".composer");
      const head = await rect(p, ".pane-head");
      check(!!goal && !!comp && goal.b <= comp.y + 1 && comp.y - goal.b <= 6, `${tag}: the goal sits right above the composer (${Math.round(comp.y - goal.b)}px gap)`, { goal, comp });
      check(!!goal && !!head && goal.y > head.b + 100, `${tag}: …not at the top`, { goal, head });
      check(goal.h <= 28, `${tag}: …one slim line (${Math.round(goal.h)}px)`);
      check((await text(p, ".pane.focused .goal .goal-text")) === "Ship the healthz endpoint", `${tag}: it reads the goal`);
      check((await p.eval(`!!document.querySelector('.pane.focused .composer-dock .goal')`)), `${tag}: it belongs to the dock`);
      if (first) await p.shot(`${SHOTS}/chatui-goal-${tag}.png`);

      // The plan opens upward over the chat; the composer stays where it is.
      await open(p, PLAN);
      await sleep(1400);
      const c0 = await rect(p, ".composer");
      await p.click(".pane.focused .goal-chev");
      await until(p, `document.querySelector('.pane.focused .goal .task-body')`);
      await sleep(250);
      const c1 = await rect(p, ".composer");
      const body = await rect(p, ".goal .task-body");
      check(!!body && body.b <= (await rect(p, ".goal")).y + 1, `${tag}: the plan opens above the goal`, body);
      check(Math.abs(c0.y - c1.y) < 1, `${tag}: …without moving the composer`);
      check((await count(p, ".pane.focused .crit")) === 3, `${tag}: the done-when criteria are in it`);
      await p.shot(`${SHOTS}/chatui-plan-${tag}.png`);
      await p.mouse(w / 2, 200); // a press outside puts it away
      await until(p, `!document.querySelector('.pane.focused .goal .task-body')`);
      check((await count(p, ".pane.focused .goal .task-body")) === 0, `${tag}: a press outside closes it`);
      await p.click(".pane.focused .goal-chev");
      await until(p, `document.querySelector('.pane.focused .goal .task-body')`);
      await p.eval(`document.querySelector('.pane.focused .goal-chev').focus()`);
      await p.key("Escape", "Escape", 0, 27);
      await until(p, `!document.querySelector('.pane.focused .goal .task-body')`);
      check((await count(p, ".pane.focused .goal .task-body")) === 0, `${tag}: so does Escape`);

      // ── subagent reports: two quiet lines ──
      await open(p, AUDIT);
      await until(p, `document.querySelectorAll('.pane.focused .report .report-gist').length === 2`, 8000);
      await sleep(300);
      const rep = await rect(p, ".report");
      check(!!rep && rep.h <= 60, `${tag}: a collapsed report is two lines (${Math.round(rep?.h ?? 0)}px)`, rep);
      check((await count(p, ".pane.focused .report .report-gist")) === 2, `${tag}: …with the first words of what it found`);
      check(/Reconnect path/.test((await text(p, ".pane.focused .report .report-gist")) ?? "") && !/[#*`]/.test((await text(p, ".pane.focused .report .report-gist")) ?? ""), `${tag}: …as plain text`);
      await reveal(p, '.pane.focused .report .report-head');
      await sleep(200);
      if (first) await p.shot(`${SHOTS}/chatui-report-${tag}.png`);
      await clickUntil(p, ".pane.focused .report .report-head", `document.querySelector('.pane.focused .report.open .report-body')`);
      await sleep(300);
      const open1 = await rect(p, ".report.open");
      check(!!open1 && open1.h > 150 && (await count(p, ".pane.focused .report.open .report-body")) === 1, `${tag}: a tap opens the whole report (${Math.round(open1?.h ?? 0)}px)`);
      check((await text(p, ".pane.focused .report.open .report-open"))?.includes("Open task"), `${tag}: …with a way into the task`);
      await p.shot(`${SHOTS}/chatui-report-open-${tag}.png`);
      await p.click(".pane.focused .report .report-head");
      await until(p, `!document.querySelector('.pane.focused .report.open')`);
      check((await count(p, ".pane.focused .report.open")) === 0, `${tag}: and closes again`);

      // A swarm finishing: one group; a row opens its report.
      await open(p, SWARM);
      await until(p, `document.querySelector('.pane.focused .report-group')`, 8000);
      check((await count(p, ".pane.focused .report-group")) === 1, `${tag}: many reports are one group`);
      await reveal(p, '.pane.focused .report-group-head');
      await clickUntil(p, ".pane.focused .report-group-head", `document.querySelector('.pane.focused .report-row')`);
      await clickUntil(p, ".pane.focused .report-row", `document.querySelector('.pane.focused .report-rows .report-body')`);
      check((await count(p, ".pane.focused .report-rows .report-body")) === 1 && (await count(p, ".pane.focused .report-rows .report")) === 0, `${tag}: a row opens its report without a second card`);
      if (first) await p.shot(`${SHOTS}/chatui-report-group-${tag}.png`);

      // ── no "Getting ready" ──
      await open(p, SID);
      await sleep(900);
      await send(p, mobile, "Run the tests");
      let ready = false;
      let busySeen = false;
      for (let k = 0; k < 12; k++) {
        await sleep(150);
        ready ||= await p.eval(`/Getting ready|is getting ready/i.test(document.querySelector('.pane.focused').textContent)`);
        busySeen ||= await p.eval(`!!document.querySelector('.pane.focused .send-btn.stop')`);
      }
      check(!ready, `${tag}: "Getting ready" never shows`);
      check(busySeen, `${tag}: …the chat still shows it is working`);
      await p.eval(`document.querySelector('.pane.focused .send-btn.stop')?.click()`);
      await sleep(900);

      console.log("errors", p.errors);
      check(!p.errors.length, `${tag}: no page errors`, p.errors);
      probe.close();
      await p.close();
    }
  }
  finish();
});
