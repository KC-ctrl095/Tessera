// Batch 16, content lives where content is: scheduled tasks are made with New → More → Scheduled
// task and edited from the sidebar's Scheduled tab (a view of their own, full screen on phones),
// with Run now, Pause and Delete in each row's ⋯ menu; archived chats are a list of their own,
// opened from the line under the chats; bot templates are only in New bot, whose first view is
// short (a template, a name, then "More settings"). Desktop and the folded phone, light and dark.
import { SHOTS, check, device, finish, menuPick, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const store = (p, js) => p.eval(`__mod('lib/store.ts').then(m => { ${js} })`);
const ui = (p, js) => p.eval(`__mod('lib/ui.ts').then(m => { ${js} })`);
const fill = (p, sel, v) =>
  p.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); e.focus(); Object.getOwnPropertyDescriptor(Object.getPrototypeOf(e), 'value').set.call(e, ${JSON.stringify(v)}); e.dispatchEvent(new Event('input', { bubbles: true })); })()`);
const tabs = (p, side = ".sidebar") => p.eval(`[...document.querySelectorAll('${side} .sb-tab')].map((t) => t.getAttribute('aria-label').split(' ·')[0])`);
const vis = (p, sel) =>
  p.eval(`(() => { const e = [...document.querySelectorAll(${JSON.stringify(sel)})].find((e) => { const r = e.getBoundingClientRect(); return r.width && r.left >= 0 && r.right <= innerWidth + 1; }); if (!e) return null; const r = e.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height, r: r.right, b: r.bottom, cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);

run(async () => {
  for (const theme of ["dark", "light"]) {
    console.log(`— desktop (${theme})`);
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const c = await wsClient(`homes ${theme}`);
    const jobs = async () => (await c.rpc("list_jobs")).result;
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, `Homes desktop ${theme}`);
    await p.theme(theme);
    await sleep(600);
    // No scheduled tasks yet: no Scheduled tab.
    for (const j of await jobs()) await c.rpc("delete_job", { id: j.id });
    await sleep(600);
    check(!(await tabs(p)).includes("Scheduled"), "without scheduled tasks there is no Scheduled tab", await tabs(p));

    // New → More → Scheduled task: the editor, a view of its own.
    await p.click(".topbar .tb-new-more");
    await sleep(250);
    await menuPick(p, "Scheduled task");
    await p.waitFor(`document.querySelector('.job-editor-view h4')`, 8000);
    check((await p.eval(`document.querySelector('.job-editor-view h4').textContent`)) === "New scheduled task", "New → More → Scheduled task opens its editor");
    const labels = await p.eval(`[...document.querySelectorAll('.job-editor-view .field-label')].map((e) => e.textContent.trim())`);
    check(labels.includes("What should it do?") && labels.includes("Folder") && labels.includes("How much freedom") && !labels.includes("Prompt") && !labels.includes("Working directory"), "…in plain words", labels);
    await p.shot(`${SHOTS}/homes-job-new-desktop-${theme}.png`);
    await fill(p, ".job-editor-view input.input:not(.mono)", "Morning news");
    await fill(p, ".job-editor-view textarea", "Tell me the three biggest stories this morning.");
    await p.click(".job-editor-view button[type=submit]");
    await p.waitFor(`!document.querySelector('.job-editor-view')`, 6000).catch(() => {});
    await sleep(600);
    const made = (await jobs()).find((j) => j.name === "Morning news");
    check(!!made, "saving makes it on the server", made?.name);
    check((await tabs(p)).includes("Scheduled"), "…and the Scheduled tab appears with it", await tabs(p));

    // The Scheduled tab: a tap edits; ⋯ has the rest.
    await p.click(`.sidebar:not(.drawer) .sb-tab[id$="-jobs"]`);
    await sleep(300);
    await p.clickText(".sidebar .srow-main", "Morning news");
    await p.waitFor(`document.querySelector('.job-editor-view h4')`, 8000);
    check((await p.eval(`document.querySelector('.job-editor-view h4').textContent`)) === "Edit scheduled task", "a task in the Scheduled tab opens its editor");
    await p.key("Escape");
    await sleep(400);
    check(!(await p.eval(`!!document.querySelector('.job-editor-view')`)), "…Escape closes it");
    const row = await p.eval(`(() => { const r = [...document.querySelectorAll('.sidebar .srow')].find((x) => x.textContent.includes('Morning news')); const b = r.querySelector('.srow-more').getBoundingClientRect(); return { cx: b.x + b.width / 2, cy: b.y + b.height / 2 }; })()`);
    await p.mouse(row.cx, row.cy);
    await sleep(300);
    const items = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-item .menu-label')].map((e) => e.textContent)`);
    check(["Edit…", "Run now", "Pause", "Delete…"].every((x) => items.includes(x)), "its ⋯ menu: edit, run now, pause, delete", items);
    await p.clickText(".menu-pop .menu-item", "Pause");
    await sleep(600);
    check((await jobs()).find((j) => j.name === "Morning news")?.enabled === false, "Pause pauses it on the server");
    await p.shot(`${SHOTS}/homes-jobs-tab-desktop-${theme}.png`, { x: 0, y: 0, width: 270, height: 400 });

    // Archived chats: the line under the chats opens a list of their own.
    await p.click(`.sidebar:not(.drawer) .sb-tab[id$="-chats"]`);
    await p.waitFor(`document.querySelector('.sidebar .sb-archived')`, 8000).catch(() => {});
    await p.click(".sidebar .sb-archived");
    await p.waitFor(`document.querySelector('.archived-view .archived-row')`, 8000).catch(() => {});
    const arch = await p.eval(`(() => { const v = document.querySelector('.archived-view'); return v && { title: v.querySelector('.modal-title, h2')?.textContent ?? '', rows: v.querySelectorAll('.archived-row').length, settings: !!document.querySelector('.settings-layout') }; })()`);
    check(!!arch && arch.rows > 0 && !arch.settings && /Archived chats/.test(arch.title), "the Archived chats line opens their list (not Settings)", arch);
    await p.shot(`${SHOTS}/homes-archived-desktop-${theme}.png`);
    await p.clickText(".archived-view .archived-row .btn", "Restore");
    await sleep(600);
    check((await p.eval(`document.querySelectorAll('.archived-view .archived-row').length`)) === arch.rows - 1, "Restore puts one back");
    await p.key("Escape");
    await sleep(300);
    // The palette goes there too.
    await p.key("k", "KeyK", 2, 75);
    await p.waitFor(`document.querySelector('.palette input')`, 6000);
    await p.type("Archived chats");
    await sleep(300);
    await p.key("Enter", "Enter", 0, 13);
    await p.waitFor(`document.querySelector('.archived-view')`, 6000).catch(() => {});
    check(await p.eval(`!!document.querySelector('.archived-view') && !document.querySelector('.settings-layout')`), "the palette's “Archived chats” opens the same list");
    await p.key("Escape");
    await sleep(300);

    // Bot templates only in New bot.
    await p.click(`.sidebar:not(.drawer) .sb-tab[id$="-bots"]`);
    await sleep(300);
    check(!(await p.eval(`!!document.querySelector('.sidebar .sb-tpls, .sidebar .sb-tpl')`)), "the Bots tab lists bots, no templates");
    await p.click(".topbar .tb-new-more");
    await sleep(250);
    await menuPick(p, "Bot");
    await p.waitFor(`document.querySelector('.bot-editor .be-templates')`, 8000);
    check(await p.eval(`document.querySelectorAll('.bot-editor .tpl-card').length >= 3`), "New → More → Bot starts from a template (or from scratch)");
    await ui(p, `m.setBotEditor(null)`);
    await sleep(300);
    check(!p.errors.length, "no page errors (desktop)", p.errors);
    await p.close();
    c.close?.();

    // ── the folded phone ────────────────────────────────────────────────
    console.log(`— phone (${theme})`);
    const q = await device();
    await q.viewport(344, 882, true, 2.625);
    await signIn(q, `Homes phone ${theme}`);
    await q.theme(theme);
    await sleep(600);
    const nb = await vis(q, ".pane-head .head-new");
    await q.tap(nb.cx, nb.cy);
    await sleep(300);
    await menuPick(q, "Scheduled task", true);
    await q.waitFor(`document.querySelector('.job-editor-view h4')`, 8000);
    // Measured once its opening animation has finished (mid-animation it is a little smaller).
    await q.waitFor(`(() => { const v = document.querySelector('.job-editor-view'); return v && v.getAnimations({ subtree: true }).every((a) => a.playState !== 'running'); })()`, 3000);
    const box = await vis(q, ".job-editor-view");
    check(!!box && box.w >= 340 && (await q.eval(`document.querySelector('.job-editor-view').scrollWidth <= innerWidth + 1`)), "on a phone the editor fills the screen, nothing runs off the side", box);
    await q.shot(`${SHOTS}/homes-job-new-phone-${theme}.png`);
    await q.eval(`history.back()`);
    await sleep(500);
    await ui(q, `m.setArchivedOpen(true)`);
    await q.waitFor(`document.querySelector('.archived-view .archived-row')`, 8000).catch(() => {});
    check(await q.eval(`document.querySelector('.archived-view').getBoundingClientRect().width >= 340`), "archived chats: full screen on a phone");
    await q.shot(`${SHOTS}/homes-archived-phone-${theme}.png`);
    await q.eval(`history.back()`);
    await sleep(500);
    await ui(q, `m.openBotEditor(null)`);
    await q.waitFor(`document.querySelector('.bot-editor .be-more')`, 8000);
    check(!(await q.eval(`!!document.querySelector('.bot-editor .be-duties')`)), "a new bot on a phone starts short too");
    await q.shot(`${SHOTS}/homes-new-bot-phone-${theme}.png`);
    check(!q.errors.length, "no page errors (phone)", q.errors);
    await q.close();
  }
  finish();
});
