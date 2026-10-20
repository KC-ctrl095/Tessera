import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";
run(async () => {
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Bot editor");
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    for (const [w, h, m] of [[673, 841, 1], [717, 512, 1], [841, 701, 1], [884, 1104, 1], [768, 1024, 1], [1440, 900, 0], [375, 812, 1], [360, 780, 1]]) {
      await p.viewport(w, h, !!m, m ? 2 : 1);
      await sleep(250);
      await p.eval(`__mod('lib/ui.ts').then(m => m.openBotEditor('bot_nova01'))`);
      await sleep(1300);
      const g = await p.eval(`(() => {
        const r = (s) => { const e = document.querySelector(s); if (!e) return null; const b = e.getBoundingClientRect(); return { x: Math.round(b.x), y: Math.round(b.y), w: Math.round(b.width), h: Math.round(b.height), b: Math.round(b.bottom) }; };
        return { modal: r('.bot-editor'), form: r('.be-form'), actions: r('.be-actions'), primary: r('.be-actions .btn-primary'), name: r('#be-name'), avatar: r('.be-avatar'), fields: r('.be-id-fields'), vh: innerHeight, vw: innerWidth, overflowX: document.querySelector('.be-form').scrollWidth > document.querySelector('.be-form').clientWidth + 1 };
      })()`);
      if (theme === "dark") {
        check(g.actions && g.primary && g.primary.b <= g.vh && g.primary.y >= 0, `${w}x${h}: actions visible and pinned`, g.primary);
        check(!g.overflowX, `${w}x${h}: no sideways scroll in the form`);
        check(g.modal.w <= 1080 && (w < 701 || g.modal.x >= 12), `${w}x${h}: a centred sheet with margins (w ${g.modal.w}, x ${g.modal.x})`);
        if (w >= 701) check(g.name.w <= 520, `${w}x${h}: name input not stretched (${g.name.w}px)`);
        if (w >= 768 && h > 600) check(g.avatar && g.fields && Math.abs(g.avatar.y - g.fields.y) < 40 && g.avatar.x > g.fields.x, `${w}x${h}: avatar picker beside the identity fields`, { a: g.avatar, f: g.fields });
      }
      await p.shot(`${SHOTS}/boteditor-${w}x${h}-${theme}.png`);
      if (theme === "dark" && (w === 717 || w === 884)) {
        await p.eval(`document.querySelector('.be-form').scrollTop = 99999`);
        await sleep(200);
        await p.shot(`${SHOTS}/boteditor-${w}x${h}-${theme}-scrolled.png`);
      }
      await p.eval(`__mod('lib/ui.ts').then(m => m.setBotEditor(null))`);
      await sleep(250);
    }
  }
  // a new bot too
  await p.theme("dark");
  await p.viewport(841, 701, true, 2);
  await p.eval(`__mod('lib/ui.ts').then(m => m.openBotEditor(null))`);
  await sleep(900);
  // A new bot starts short: a template, its name and what it does; the rest behind "More settings".
  const first = await p.eval(`(() => ({ tpl: !!document.querySelector('.be-templates'), name: !!document.querySelector('#be-name'), more: document.querySelector('.be-more')?.textContent ?? null, duties: !!document.querySelector('.be-duties'), personality: [...document.querySelectorAll('.be-h')].some((h) => h.textContent.includes('Personality')) }))()`);
  check(first.tpl && first.name && /More settings/.test(first.more ?? "") && !first.duties && !first.personality, "a new bot's first view: a template, a name, then “More settings” (no duties or personality yet)", first);
  await p.shot(`${SHOTS}/boteditor-new-841x701-dark.png`);
  await p.click(".be-more");
  await sleep(300);
  check(await p.eval(`!!document.querySelector('.be-duties') && !document.querySelector('.be-more')`), "…“More settings” opens the rest in place");
  await p.shot(`${SHOTS}/boteditor-new-more-841x701-dark.png`);
  // Duties while creating a bot: drafted in the form, created with the bot, nothing before.
  for (const [label, w, h, mobile] of [["desktop", 1440, 900, false], ["phone", 375, 812, true]]) {
    for (const theme of ["dark", "light"]) {
      await mock("/__mock/reset");
      const q = await device();
      await q.viewport(w, h, mobile, mobile ? 2 : 1);
      await signIn(q, `Bot duties ${label}`);
      await q.theme(theme);
      const c = await wsClient("duties");
      const jobs = async () => (await c.rpc("list_jobs")).result;
      const bots = async () => (await c.rpc("list_bots")).result;
      const botNamed = async (name) => (await bots()).find((x) => x.name === name);
      const jobsOf = async (name) => {
        const bot = await botNamed(name);
        return bot ? (await jobs()).filter((j) => j.bot_id === bot.id) : [];
      };
      const sec = ".bot-editor .be-duties";
      const rows = () => q.eval(`[...document.querySelectorAll('${sec} .card.job .strong')].map((e) => e.textContent)`);
      const text = (sel) => q.eval(`document.querySelector(${JSON.stringify(sel)})?.textContent ?? null`);
      const fill = (sel, v) =>
        q.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); e.focus(); Object.getOwnPropertyDescriptor(Object.getPrototypeOf(e), 'value').set.call(e, ${JSON.stringify(v)}); e.dispatchEvent(new Event('input', { bubbles: true })); })()`);
      const openNew = async (tpl) => {
        await q.eval(tpl ? `__mod('components/BotEditor.tsx').then((m) => m.openBotTemplate('${tpl}'))` : `__mod('lib/ui.ts').then((m) => m.openBotEditor(null))`);
        // A new bot's duties are under "More settings".
        await q.waitFor(`document.querySelector('.bot-editor .be-more')`);
        await q.click(".bot-editor .be-more");
        await q.waitFor(`document.querySelector('${sec}')`);
        await sleep(500);
      };
      // "How often" leads in plain words; the cron expressions these cases type are under Custom.
      const customSchedule = () => q.eval(`(() => { const e = document.querySelector('${sec} .form-card .sched > select'); e.value = 'custom'; e.dispatchEvent(new Event('change', { bubbles: true })); })()`);
      const addDuty = async (name, schedule, prompt) => {
        await q.click(`${sec} .be-h .btn`);
        await q.waitFor(`document.querySelector('${sec} .form-card')`);
        await fill(`${sec} .form-card input[placeholder="Morning briefing"]`, name);
        await customSchedule();
        await fill(`${sec} .form-card .sched input.mono`, schedule);
        await fill(`${sec} .form-card textarea`, prompt);
        await q.click(`${sec} .form-card button[type=submit]`);
        await sleep(250);
      };
      const showDuties = () => q.eval(`document.querySelector('${sec}').scrollIntoView({ block: 'start' })`);
      const tag = `${label} ${theme}`;
      const base = (await jobs()).length;
      const nBots = (await bots()).length;

      // before the bot exists
      await openNew();
      const intro = await text(sec);
      check(!/first/i.test(intro ?? "") && (await q.eval(`!!document.querySelector('${sec} .be-h .btn')`)), `${tag}: the Duties section offers "Add duty" for a new bot, no "save first"`, intro);
      await fill("#be-name", "Orion");
      await addDuty("Morning briefing", "0 8 * * 1-5", "Brief me on today.");
      await addDuty("Nightly digest", "0 22 * * *", "Summarise the day.");
      check(JSON.stringify(await rows()) === JSON.stringify(["Morning briefing", "Nightly digest"]), `${tag}: both duties are listed as drafts`, await rows());
      check((await text(`${sec} .be-note`))?.includes("Added when you create Orion"), `${tag}: the list says when they are added`);
      check(await q.eval(`!document.querySelector('${sec} .card.job .btn-sm')`), `${tag}: no "Run now" on a draft`);
      check((await text(".be-preview-badges"))?.includes("2 duties"), `${tag}: the preview counts the drafts`);
      check((await jobs()).length === base && !(await botNamed("Orion")), `${tag}: nothing is created before saving`);
      // remove one, edit the other
      await q.click(`${sec} button[aria-label="Delete Nightly digest"]`);
      await sleep(250);
      check(JSON.stringify(await rows()) === JSON.stringify(["Morning briefing"]) && (await q.eval(`__mod('lib/ui.ts').then((m) => !m.dialog())`)), `${tag}: a draft is removed straight away, no confirm`, await rows());
      await q.click(`${sec} button[aria-label="Edit Morning briefing"]`);
      await q.waitFor(`document.querySelector('${sec} .form-card')`);
      check((await text(`${sec} .form-card h4`)) === "Edit duty" && (await text(`${sec} .form-card button[type=submit]`))?.trim() === "Update duty", `${tag}: editing a draft says so`);
      await fill(`${sec} .form-card input[placeholder="Morning briefing"]`, "Weekday briefing");
      await q.click(`${sec} .form-card button[type=submit]`);
      await sleep(250);
      check(JSON.stringify(await rows()) === JSON.stringify(["Weekday briefing"]), `${tag}: the edit is kept`, await rows());
      // a bad schedule is caught in the draft editor
      await q.click(`${sec} .be-h .btn`);
      await q.waitFor(`document.querySelector('${sec} .form-card')`);
      await fill(`${sec} .form-card input[placeholder="Morning briefing"]`, "Bad one");
      await customSchedule();
      await fill(`${sec} .form-card .sched input.mono`, "nonsense");
      await fill(`${sec} .form-card textarea`, "x");
      await q.click(`${sec} .form-card button[type=submit]`);
      await sleep(250);
      check(/not a valid cron/.test((await text(`${sec} .form-card .form-error`)) ?? ""), `${tag}: an invalid schedule is refused before the bot is saved`);
      // an open, half-filled duty isn't silently dropped by Create
      await q.clickText(".be-actions .btn", "Create and keep editing");
      await sleep(300);
      check(/Finish the duty/.test((await text(".be-side .form-error")) ?? "") && !(await botNamed("Orion")), `${tag}: Create waits for the open duty form`, await text(".be-side .form-error"));
      await q.clickText(`${sec} .form-card .btn`, "Cancel");
      await sleep(250);
      check(JSON.stringify(await rows()) === JSON.stringify(["Weekday briefing"]) && (await text(".be-side .form-error")) === null, `${tag}: cancelling the editor adds nothing and clears that warning`, await rows());
      const head = await q.eval(`(() => { const h = document.querySelector('${sec} .card.job .job-head'); const r = h.getBoundingClientRect(); const a = h.querySelector('.job-actions').getBoundingClientRect(); return { h: Math.round(r.height), aTop: Math.round(a.top - r.top), over: document.querySelector('.be-form').scrollWidth > document.querySelector('.be-form').clientWidth + 1 }; })()`);
      check(head.h <= 70 && head.aTop < 40 && !head.over, `${tag}: a draft row is one compact line, no sideways scroll`, head);
      await showDuties();
      await sleep(250);
      await q.shot(`${SHOTS}/bot-duties-draft-${label}-${theme}.png`);

      // create: the duty arrives with the bot, once
      await q.clickText(".be-actions .btn", "Create and keep editing");
      await q.waitFor(`document.body.innerText.includes('Orion created with 1 duty')`, 6000);
      const cer = await jobsOf("Orion");
      check(cer.length === 1 && cer[0].name === "Weekday briefing" && cer[0].schedule === "0 8 * * 1-5", `${tag}: the new bot has exactly the drafted duty`, cer);
      check((await jobs()).length === base + 1, `${tag}: and nothing else was created (the removed draft isn't)`, (await jobs()).length);
      await sleep(300);
      check(JSON.stringify(await rows()) === JSON.stringify(["Weekday briefing"]) && (await q.eval(`!!document.querySelector('${sec} .card.job .btn-sm')`)), `${tag}: afterwards it is a real duty (Run now)`, await rows());
      await showDuties();
      await q.shot(`${SHOTS}/bot-duties-created-${label}-${theme}.png`);

      // closing without saving creates nothing
      await q.clickText(".be-actions .btn", "Cancel");
      await sleep(300);
      await openNew();
      await fill("#be-name", "Ghost");
      await addDuty("Never created", "0 7 * * *", "Nope.");
      await q.clickText(".be-actions .btn", "Cancel");
      await sleep(400);
      check((await jobs()).length === base + 1 && !(await botNamed("Ghost")), `${tag}: closing the form without saving creates no bot and no duty`);

      // a template's duty arrives once, follows the bot's home folder, and can be dropped
      await openNew("assistant");
      check(await q.eval(`document.querySelector('.tpl-duty input').checked`), `${tag}: the template's duty is offered`);
      check(JSON.stringify(await rows()) === JSON.stringify(["Morning briefing"]), `${tag}: and listed as a draft`, await rows());
      await q.click(".tpl-duty input");
      await sleep(200);
      check((await rows()).length === 0, `${tag}: unticking drops it`, await rows());
      await q.click(".tpl-duty input");
      await sleep(200);
      check(JSON.stringify(await rows()) === JSON.stringify(["Morning briefing"]), `${tag}: ticking brings it back`, await rows());
      await q.eval(`(() => { const sec = [...document.querySelectorAll('.be-sec')].find((s) => s.textContent.includes('Home folder')); sec.querySelector('input.mono').scrollIntoView(); })()`);
      await fill(`.be-sec:has(.setting-row) input.mono.grow`, "/srv/assistant-home");
      await sleep(250);
      check(((await text(`${sec} .job-meta`)) ?? "").includes("assistant-home"), `${tag}: the template's duty shows the bot's home folder`, await text(`${sec} .job-meta`));
      await showDuties();
      await q.shot(`${SHOTS}/bot-duties-template-${label}-${theme}.png`);
      await q.clickText(".be-actions .btn", "Create and keep editing");
      await q.waitFor(`document.body.innerText.includes('created with 1 duty')`, 6000);
      const nova2 = (await bots()).filter((x) => x.name === "Nova" && x.id !== "bot_nova01").pop();
      const nj = nova2 ? (await jobs()).filter((j) => j.bot_id === nova2.id) : [];
      check(nj.length === 1 && nj[0].name === "Morning briefing" && nj[0].cwd === "/srv/assistant-home", `${tag}: the template's duty was created once, in the bot's home`, nj);
      await q.clickText(".be-actions .btn", "Cancel");
      await sleep(300);
      await openNew("assistant");
      await q.click(".tpl-duty input");
      await fill("#be-name", "Plain Nova");
      await q.clickText(".be-actions .btn", "Create and keep editing");
      await q.waitFor(`document.body.innerText.includes('Plain Nova created')`, 6000);
      check((await jobsOf("Plain Nova")).length === 0, `${tag}: a template's duty that was unticked isn't created`);
      await q.clickText(".be-actions .btn", "Cancel");
      await sleep(300);

      // one duty the server refuses: the bot and the other duty are created, the failed one stays to fix
      await openNew();
      await fill("#be-name", "Scylla");
      await addDuty("Good duty", "0 9 * * *", "Fine.");
      await addDuty("Broken duty", "61 9 * * *", "Rejected by the server.");
      await q.clickText(".be-actions .btn", "Create and keep editing");
      await q.waitFor(`document.querySelector('${sec} .job-error')`, 6000);
      await sleep(500);
      const scy = await jobsOf("Scylla");
      check(scy.length === 1 && scy[0].name === "Good duty", `${tag}: the other duty is created`, scy);
      check(JSON.stringify(await rows()) === JSON.stringify(["Broken duty", "Good duty"]), `${tag}: the failed duty stays in the list, above the real one`, await rows());
      check(/Invalid schedule/.test((await text(`${sec} .job-error`)) ?? "") && /saved, but a duty couldn't be added/.test((await text(".be-side .form-error")) ?? ""), `${tag}: the reason is shown on the duty and in the form`, [await text(`${sec} .job-error`), await text(".be-side .form-error")]);
      check(await q.eval(`!!document.querySelector('.bot-editor')`), `${tag}: the form stays open`);
      await showDuties();
      await sleep(250);
      await q.shot(`${SHOTS}/bot-duties-failed-${label}-${theme}.png`);
      await q.click(`${sec} button[aria-label="Edit Broken duty"]`);
      await q.waitFor(`document.querySelector('${sec} .form-card')`);
      check(/Invalid schedule/.test((await text(`${sec} .form-card .form-error`)) ?? ""), `${tag}: editing it shows the reason again`);
      await fill(`${sec} .form-card .sched input.mono`, "@every 90m");
      check((await text(`${sec} .form-card button[type=submit]`))?.trim() === "Save duty", `${tag}: saving it says "Save duty" now that the bot exists`);
      await q.click(`${sec} .form-card button[type=submit]`);
      await sleep(600);
      const scy2 = await jobsOf("Scylla");
      check(scy2.length === 2 && scy2.some((j) => j.name === "Broken duty" && j.schedule === "@every 90m"), `${tag}: the fixed duty is created`, scy2);
      check(JSON.stringify(await rows()) === JSON.stringify(["Good duty", "Broken duty"]) && (await q.eval(`!document.querySelector('${sec} .job-error')`)), `${tag}: and the list is all real duties`, await rows());
      await q.clickText(".be-actions .btn", "Cancel");
      await sleep(300);

      // Create & start chatting also creates them, then closes the form
      await openNew();
      await fill("#be-name", "Hydra");
      await addDuty("Head count", "0 12 * * *", "Count heads.");
      await q.clickText(".be-actions .btn", "Create & start chatting");
      await q.waitFor(`!document.querySelector('.bot-editor')`, 6000);
      const hyd = await jobsOf("Hydra");
      check(hyd.length === 1 && hyd[0].name === "Head count", `${tag}: "Create & start chatting" creates the duty and closes the form`, hyd);
      check((await bots()).length === nBots + 5, `${tag}: one bot per Create (${nBots + 5})`, (await bots()).length);
      console.log(tag, "errors", q.errors);
      c.close();
      await q.close();
    }
  }
  console.log("errors", p.errors);
  finish();
});
