// Batch 16: Settings in sections and Help & tour, every setting in exactly one of them (batch 17: Usage became a ninth).
// What each section holds on its surface, what waits under its More, the pages one step deeper,
// plain words, the "This device" tag, and (walking every section and page) that each setting's
// control appears once. Desktop, the folded phone (344 px) and the unfolded Fold; dark and light.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const ui = (p, js) => p.eval(`__mod('lib/ui.ts').then((u) => { ${js} })`);
const open = async (p, tab, ready = ".settings-body > *") => {
  await ui(p, `u.openSettings('${tab}')`);
  await p.waitFor(`document.querySelector(${JSON.stringify(ready)})`, 8000).catch(() => {});
  await sleep(700);
};
// A title's own words (without its "This device" tag).
const titles = (p) =>
  p.eval(`[...document.querySelectorAll('.settings-body .setting-title')].filter((e) => e.getClientRects().length).map((e) => { const c = e.cloneNode(true); c.querySelectorAll('.this-device').forEach((t) => t.remove()); return c.textContent.trim(); })`);
const disclosures = (p) => p.eval(`[...document.querySelectorAll('.settings-body .disclosure')].map((d) => d.textContent.trim())`);
const openMore = (p, re) => p.eval(`(() => { const d = [...document.querySelectorAll('.settings-body .disclosure')].find((x) => ${re}.test(x.textContent)); if (d && d.getAttribute('aria-expanded') !== 'true') d.click(); return !!d; })()`);
const noSideScroll = (p) => p.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth <= b.clientWidth + 1; })()`);

run(async () => {
  await mock("/__mock/reset");

  // ── a computer: what each section holds ──
  {
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, "Settings homes");
    await p.theme("dark");

    // Appearance: theme, text size, accent colour; the rest under More.
    await open(p, "appearance", ".theme-cards");
    let t = await titles(p);
    check(t.some((x) => /^Theme/.test(x)) && t.includes("Text size") && t.includes("Accent colour") && !t.includes("Spacing") && !t.includes("Space between tiles"), "Appearance: theme, text size and accent colour on top", t);
    check((await p.eval(`document.querySelector('.settings-body .this-device')?.textContent`)) === "This device", "…marked as this device's");
    const accents = await p.eval(`[...document.querySelectorAll('.accent-swatch')].map((b) => b.getAttribute('aria-label'))`);
    check(JSON.stringify(accents) === JSON.stringify(["Gold", "Blue", "Green", "Clay", "Plum", "Stone"]), "accent colours in plain words", accents);
    await p.click('.accent-swatch[aria-label="Blue"]');
    await sleep(200);
    check((await p.eval(`JSON.parse(localStorage.getItem('tessera.prefs')).accent`)) === "lapis", "picking one keeps it on this device");
    await p.click('.accent-swatch[aria-label="Gold"]');
    check(await openMore(p, /spacing/), "More: spacing and the rest");
    await sleep(250);
    t = await titles(p);
    check(["Spacing", "Space between tiles", "Dim other tiles", "Focus follows the mouse", "Where files open"].every((x) => t.includes(x)), "…spacing, space between tiles, dim other tiles, focus follows the mouse, where files open", t);
    await p.shot(`${SHOTS}/settings-homes-appearance-desk.png`);

    // Notifications (a browser): agents' messages, scheduled reminders, one test.
    await open(p, "notifications", ".notify-tests");
    check((await p.eval(`document.querySelectorAll('.notify-tests button').length`)) === 1, "Notifications: one Send a test");

    // Models: one control for the model new chats use; services one line each; roles under More (usage is a section of its own).
    await open(p, "models", ".default-model");
    t = await titles(p);
    check(t[0] === "Model for new chats", "Models: the model for new chats first", t);
    check(!(await p.eval(`[...document.querySelectorAll('.settings-body button')].some((b) => /Make default/.test(b.textContent))`)), "…its one control (no Make default on models)");
    check(!(await p.eval(`!!document.querySelector('.settings-body .connect-grid')`)) && (await p.eval(`!!document.querySelector('.models-connected .section-head .btn')`)), "connecting another service is one button away (Connect)");
    await p.click(".models-connected .section-head .btn");
    await sleep(300);
    const connect = await p.eval(`[...document.querySelectorAll('.connect-card-btn .cc-title')].map((e) => e.textContent)`);
    check(connect.includes("Another service") && !connect.includes("Another provider"), "…offering services in plain words", connect);
    check(!(await p.eval(`[...document.querySelectorAll('.settings-body .page-row .setting-title')].map((e) => e.textContent)`)).some((t) => /Usage/.test(t)), "Models has no Usage row (Usage is its own section)");
    check((await disclosures(p)).some((d) => /models for titles, summaries and memory/.test(d)), "More: the models for titles, summaries and memory", await disclosures(p));
    await p.shot(`${SHOTS}/settings-homes-models-desk.png`);
    // Usage: a section of its own, right after Models (batch 17).
    await open(p, "usage", ".usage .kpi");
    check((await p.eval(`document.querySelector('.settings-head h3')?.textContent`)) === "Usage" && !(await p.eval(`!!document.querySelector('.settings-crumb')`)), "Usage opens as a section of its own (no crumb)");
    check((await p.eval(`document.querySelector('.settings-tab.active')?.dataset.tab`)) === "usage", "…marked in the side list");

    // Privacy & safety: freedom, two pages, bots helping chats; the rest under More.
    await open(p, "safety", ".sec-glance");
    t = await titles(p);
    check(t.includes("How much freedom new chats get") && t.includes("Passwords & keys") && t.includes("What commands can reach") && t.includes("Chats asking your bots for help"), "Privacy & safety: freedom, Passwords & keys, What commands can reach, your bots helping", t);
    check(!t.includes("Sandbox") && !t.includes("Hide passwords in what commands return"), "…the sandbox on its page, the finer switches under More", t);
    check(await openMore(p, /hide passwords/), "More: hide passwords in what commands return…");
    await sleep(250);
    t = await titles(p);
    check(["Hide passwords in what commands return", "Ask before writing elsewhere", "Encrypted connections only"].every((x) => t.includes(x)), "…ask before writing elsewhere, encrypted connections only", t);
    await p.shot(`${SHOTS}/settings-homes-safety-desk.png`);

    // Memory & skills; Web & voice; Devices (Activity a page); Connection; Help & tour.
    await open(p, "memory", "#set-skills");
    check(await p.eval(`!!document.querySelector('.mem-text') && !!document.querySelector('#set-skills')`), "Memory & skills: the two notes and the skills");
    await open(p, "web", "#set-voice");
    check(await p.eval(`!!document.querySelector('.search-choice') && !!document.querySelector('#set-voice')`), "Web & voice: web search, the browser and voice typing");
    check((await disclosures(p)).some((d) => /backup search keys/.test(d)), "…backup search keys under More");
    await open(p, "devices", ".dev-list");
    check((await p.eval(`[...document.querySelectorAll('.settings-body .page-row .setting-title')].map((e) => e.textContent)`)).includes("Activity"), "Devices: Activity is a page of it");
    await open(p, "connection", ".server-card");
    t = await p.eval(`[...document.querySelectorAll('.settings-body .section-title')].map((e) => e.textContent)`);
    check(t.includes("Away from home") && !t.includes("Remote access"), "Connection: “Away from home”", t);
    await open(p, "help", ".help-guides");
    const help = await p.eval(`({ rows: [...document.querySelectorAll('.settings-body .setting-title')].map((e) => e.textContent.trim()), guides: document.querySelectorAll('.help-guides li').length, version: document.querySelector('.help-version')?.textContent ?? '' })`);
    check(["Take the tour", "Run setup again", "Keyboard shortcuts", "Suggestions on new chats", "Hints", "Guides"].every((x) => help.rows.includes(x)) && help.guides >= 5 && /Tessera app/.test(help.version), "Help & tour: the tour, setup, shortcuts, suggestions, hints, guides and the version", help);
    await p.shot(`${SHOTS}/settings-homes-help-desk.png`);

    // Every setting in exactly one place: walk every section and page with each More open, and count.
    const seen = {};
    for (const tab of ["appearance", "notifications", "models", "usage", "safety", "secrets", "commands", "memory", "web", "devices", "activity", "connection", "help"]) {
      await open(p, tab);
      await sleep(400);
      await p.eval(`document.querySelectorAll('.settings-body .disclosure[aria-expanded="false"]').forEach((d) => d.click())`);
      await sleep(400);
      for (const x of await titles(p)) (seen[x] ??= []).push(tab);
    }
    const twice = Object.entries(seen).filter(([k, v]) => v.length > 1 && !["Folders"].includes(k));
    check(twice.length === 0, "no setting is shown in two places", twice);
    for (const k of ["How much freedom new chats get", "Keep passwords and keys out of chats", "Model for new chats", "Theme", "Accent colour"]) check((seen[k] ?? []).length === 1, `“${k}” has one home (${(seen[k] ?? []).join()})`, seen[k]);
    check(!seen["Agents can look at whiteboards"] || seen["Agents can look at whiteboards"].join() === "safety", "whiteboard pictures (where offered) live in Privacy & safety");
    check(p.errors.length === 0, "desktop: no errors", p.errors);
    await p.close();
  }

  // ── every section on the folded phone and the unfolded Fold, dark and light ──
  for (const [name, w, h] of [["cover", 344, 882], ["fold", 673, 841]]) {
    for (const theme of ["dark", "light"]) {
      const p = await device();
      await p.viewport(w, h, true, 2.625);
      await signIn(p, `Settings homes ${name} ${theme}`);
      await p.theme(theme);
      for (const tab of ["appearance", "notifications", "models", "safety", "secrets", "commands", "memory", "web", "devices", "connection", "help"]) {
        await open(p, tab);
        await sleep(300);
        const fits = await noSideScroll(p);
        if (!fits || ["appearance", "models", "safety", "help"].includes(tab)) await p.shot(`${SHOTS}/settings-homes-${tab}-${name}-${theme}.png`);
        check(fits, `${name} ${theme}: ${tab} fits the width`);
      }
      // Opened again at no particular section (here from Help & tour): the folded phone shows its list.
      await ui(p, `u.openSettings()`);
      await sleep(600);
      await p.shot(`${SHOTS}/settings-homes-list-${name}-${theme}.png`);
      const view = await p.eval(`document.querySelector('.settings-layout')?.dataset.view ?? null`);
      check(name === "cover" ? view === "list" : view === null, `${name} ${theme}: opened at no section, ${name === "cover" ? "the list of sections" : "the side list beside Appearance"}`, view);
      check(p.errors.length === 0, `${name} ${theme}: no errors`, p.errors);
      await p.close();
    }
  }

  // ── the desktop list, light ──
  {
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, "Settings homes light");
    await p.theme("light");
    await open(p, "appearance", ".theme-cards");
    await p.shot(`${SHOTS}/settings-homes-appearance-desk-light.png`);
    await open(p, "safety", ".sec-glance");
    await p.shot(`${SHOTS}/settings-homes-safety-desk-light.png`);
    await open(p, "models", ".default-model");
    await p.shot(`${SHOTS}/settings-homes-models-desk-light.png`);
    check(p.errors.length === 0, "desktop light: no errors", p.errors);
    await p.close();
  }
  finish();
});
