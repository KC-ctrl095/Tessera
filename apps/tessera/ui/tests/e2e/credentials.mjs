// SECRETS: Settings → Passwords & keys. Values are never shown again (not in
// the page, not in a reply), yet everything can be edited: replace a value
// (an empty field keeps it), rename, "Who can use it" (any chat / only these
// bots) and where (this server / machines), delete with a confirm. The bot
// editor shows what a bot can use. Desktop, phone, folded phone (cover
// screen) and unfolded Fold, dark and light. (The saving choice, the groups,
// search and chats' cards: secret-saving.mjs.)
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

// The mock's seeded values (dev/mock-server.mjs seedSecrets) and a new one.
const VALUES = ["mock-password", "octo-user", "JBSWY3DPEHPK3PXP", "owm_mock", "sk_test_mock", "whsec_mock", "mock-switch-pass"];
const NEW_PASS = "Fresh-Pass-7731!";
const FORGE = "bot_forge1";

run(async () => {
  await mock("/__mock/reset");
  const ws = await wsClient("credentials script");
  const list = async () => (await ws.rpc("list_secrets")).result;
  const stored = async (name, field, value) => (await ws.rpc("mock_secret_check", { name, field, value })).result;
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Credentials desktop");
  const open = async () => {
    await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings('secrets'))`);
    await p.waitFor(`document.querySelector('.secret-item')`, 8000);
    await sleep(300);
  };
  /** Nothing on the page holds a stored value: text, attributes or input values. */
  const leaks = () =>
    p.eval(`(() => { const html = document.documentElement.outerHTML + [...document.querySelectorAll('input, textarea')].map(i => i.value).join(' '); return ${JSON.stringify([...VALUES, NEW_PASS])}.filter(v => html.includes(v)); })()`);
  const fill = (sel, text) =>
    p.eval(`(() => { const i = document.querySelector(${JSON.stringify(sel)}); i.focus(); const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; set.call(i, ${JSON.stringify(text)}); i.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
  const row = (name) => `.secret-item[data-name="${name}"]`;
  const edit = async (name) => {
    await p.clickText(`${row(name)} .secret-actions .btn`, "Edit");
    await p.waitFor(`document.querySelector('${row(name)}.editing form')`, 4000);
    await sleep(200);
  };
  const save = async () => {
    await p.clickText(".secret-item.editing .form-actions .btn-primary", "Save changes");
    await sleep(700);
  };

  // ── the list ──
  await open();
  const wire = JSON.stringify(await list());
  check(VALUES.every((v) => !wire.includes(v)), "list_secrets carries no value", wire.slice(0, 300));
  // A page of Privacy & safety (batch 16): its title, with its section marked in the list.
  const tab = await p.eval(`({ title: document.querySelector('.settings-head h3')?.textContent.trim(), crumb: document.querySelector('.settings-crumb')?.textContent })`);
  check(tab.title === "Passwords & keys" && tab.crumb === "Privacy & safety", "the page is called Passwords & keys, in Privacy & safety", tab);
  const rows = await p.eval(`[...document.querySelectorAll('.secret-item')].map(r => ({ name: r.dataset.name, who: r.querySelector('.cred-who')?.textContent, where: r.querySelector('.cred-where')?.textContent }))`);
  const sw = rows.find((r) => r.name === "SWITCH_ADMIN");
  const gh = rows.find((r) => r.name === "GITHUB");
  check(sw?.who === "Only Forge" && sw?.where === "Desktop", "a bot-only key says who and where: Only Forge · Desktop", sw);
  check(gh?.who === "Any chat · asks first" && gh?.where === "This server", "an older key reads as any chat, asking first, on this server", gh);
  check((await leaks()).length === 0, "no value anywhere on the page", await leaks());
  await p.shot(`${SHOTS}/cred-list-desk-dark.png`);

  // ── edit without seeing: saved fields only offer Replace… ──
  await edit("GITHUB");
  const fields = await p.eval(`[...document.querySelectorAll('.secret-item.editing .sfield')].map(f => ({ k: f.dataset.field, saved: !!f.querySelector('.cred-saved'), input: !!f.querySelector('.sv input') }))`);
  const savedOnes = fields.filter((f) => f.saved).map((f) => f.k).sort();
  check(savedOnes.join() === "password,totp,username", "saved fields show “saved” with Replace…, no input", fields);
  check((await leaks()).length === 0, "the editor shows no value, length or hint", await leaks());
  const filled = await p.eval(`[...document.querySelectorAll('.secret-item.editing .sfield input')].filter(i => i.value).length`);
  check(filled === 0, "no value input starts filled", filled);
  await p.shot(`${SHOTS}/cred-edit-desk-dark.png`);

  // Scope: only Forge; description changes; values untouched (blank keeps).
  await fill(".secret-item.editing .grid2 label:first-child input", "GitHub (work)");
  await p.clickText(".secret-item.editing .access-mode", "Only these bots");
  await sleep(200);
  const warn = await p.eval(`!!document.querySelector('.secret-item.editing .access .warn-text')`);
  check(warn, "only-bots with none picked warns that nothing can use it");
  await p.clickText(".secret-item.editing .access-bot", "Forge");
  await sleep(150);
  const askShown = await p.eval(`!!document.querySelector('.secret-item.editing .cred-ask')`);
  check(!askShown, "bots never ask, so the ask switch goes away");
  check(await p.eval(`document.querySelector('.secret-item.editing form').textContent.includes('send the value anywhere')`), "a bot-only grant plainly says a command can send the value anywhere");
  await p.shot(`${SHOTS}/cred-scope-desk-dark.png`);
  await save();
  let g = (await list()).find((s) => s.name === "GITHUB");
  check(g?.description === "GitHub (work)" && JSON.stringify(g?.bots) === JSON.stringify([FORGE]), "saved: description and Only Forge", g);
  check((await stored("GITHUB", "password", "mock-password")) && (await stored("GITHUB", "username", "octo-user")), "values kept when their fields were left alone");

  // Replace one value; an opened-but-empty Replace keeps the other.
  await edit("GITHUB");
  await p.clickText('.secret-item.editing .sfield[data-field="password"] .cred-replace', "Replace");
  await sleep(150);
  await fill('.secret-item.editing .sfield[data-field="password"] input', NEW_PASS);
  await p.clickText('.secret-item.editing .sfield[data-field="username"] .cred-replace', "Replace");
  await sleep(150);
  await save();
  check(await stored("GITHUB", "password", NEW_PASS), "Replace… sets the new password");
  check(await stored("GITHUB", "username", "octo-user"), "an empty replacement keeps the saved username");
  check((await leaks()).length === 0, "the new value isn't on the page after saving", await leaks());

  // Back to any chat.
  await edit("GITHUB");
  await p.clickText(".secret-item.editing .access-mode", "Any chat (asks first)");
  await save();
  g = (await list()).find((s) => s.name === "GITHUB");
  check(g && g.bots === undefined, "Any chat again (no bots list)", g);

  // ── where: a machine-only key ──
  await edit("SWITCH_ADMIN");
  const places = await p.eval(`[...document.querySelectorAll('.secret-item.editing .cred-place')].map(b => ({ t: b.textContent, on: b.getAttribute('aria-pressed') }))`);
  check(places.find((x) => x.t.startsWith("This server"))?.on === "false" && places.find((x) => x.t.startsWith("Desktop"))?.on === "true", "where: Desktop ticked, this server not", places);
  await p.clickText(".secret-item.editing .cred-place", "Desktop");
  await sleep(150);
  check(await p.eval(`!!document.querySelector('.secret-item.editing .cred-sec .warn-text')`), "no place ticked: a warning");
  await p.clickText(".secret-item.editing .cred-place", "nas");
  await save();
  const swi = (await list()).find((s) => s.name === "SWITCH_ADMIN");
  check(JSON.stringify(swi?.on_machines) === JSON.stringify(["dev_nas"]) && swi?.on_server === false && JSON.stringify(swi?.bots) === JSON.stringify([FORGE]), "saved: only on nas, still only Forge", swi);

  // ── rename, with the warning ──
  await edit("OPENWEATHER_KEY");
  await fill('.secret-item.editing input[aria-label="Name"]', "OPENWEATHER_API_KEY");
  await sleep(200);
  const renameWarn = await p.eval(`document.querySelector('.secret-item.editing .cred-rename')?.textContent ?? ''`);
  check(/\$OPENWEATHER_KEY becomes \$OPENWEATHER_API_KEY/.test(renameWarn), "renaming says what changes for agents", renameWarn);
  await p.shot(`${SHOTS}/cred-rename-desk-dark.png`);
  await save();
  const names = (await list()).map((s) => s.name);
  check(names.includes("OPENWEATHER_API_KEY") && !names.includes("OPENWEATHER_KEY"), "renamed", names);
  check(await stored("OPENWEATHER_API_KEY", "value", "owm_mock"), "its value came along");

  // ── delete asks first ──
  await p.click(`${row("STRIPE_TEST")} .icon-btn.danger`);
  await sleep(400);
  const dlg = await p.eval(`document.querySelector('.dialog-body') ? { body: document.querySelector('.dialog-text')?.textContent ?? '', danger: !!document.querySelector('.dialog-actions .btn-danger') } : null`);
  check(!!dlg && dlg.danger && /\$STRIPE_TEST_API_KEY/.test(dlg.body), "delete asks, naming what stops working", dlg);
  await p.click(".dialog-actions .btn-ghost");
  await sleep(300);
  check((await list()).some((s) => s.name === "STRIPE_TEST"), "Cancel keeps it");
  await p.click(`${row("STRIPE_TEST")} .icon-btn.danger`);
  await sleep(400);
  await p.click(".dialog-actions button[type=submit]");
  await sleep(600);
  check(!(await list()).some((s) => s.name === "STRIPE_TEST"), "confirmed: deleted");

  // ── a new key, only for a bot ──
  await p.click(".cred-add");
  await sleep(300);
  await p.clickText(".menu-item", "Key or token");
  await p.waitFor(`document.querySelector('.secret-card form')`);
  await fill(".secret-card .grid2 label:first-child input", "Home Assistant");
  await fill('.secret-card .sfield[data-field="value"] input', "ha-token-0000-1111");
  await p.clickText(".secret-card .access-mode", "Only these bots");
  await p.clickText(".secret-card .access-bot", "Forge");
  await p.clickText(".secret-card .form-actions .btn-primary", "Save");
  await sleep(700);
  const ha = (await list()).find((s) => s.name === "HOME_ASSISTANT_TOKEN");
  check(!!ha && JSON.stringify(ha.bots) === JSON.stringify([FORGE]) && ha.on_server, "a new key, only for Forge, on this server", ha);
  check(!(await p.eval(`document.documentElement.outerHTML.includes('ha-token-0000-1111')`)), "its value is gone from the page once saved");

  // ── the bot editor: every password and key Forge can use, each switched there and saved with the bot ──
  const credRows = () => p.eval(`[...document.querySelectorAll('.be-creds .be-cred')].map(r => ({ name: r.dataset.name, kind: r.dataset.kind, on: r.querySelector('.toggle')?.getAttribute('aria-checked') }))`);
  const flip = (name) => p.eval(`document.querySelector('.be-cred[data-name="${name}"] .toggle').click()`);
  const forge = async () => (await ws.rpc("list_bots")).result.find((x) => x.id === FORGE);
  await p.eval(`__mod('lib/ui.ts').then(m => { m.setSettingsTab(null); m.openBotEditor(${JSON.stringify(FORGE)}); })`);
  await p.waitFor(`document.querySelector('.be-creds .be-cred')`, 8000);
  await sleep(500);
  check(await p.eval(`document.querySelector('.be-cred[data-name="HOME_ASSISTANT_TOKEN"]').textContent.includes('No first-use approval') && document.querySelector('.be-cred[data-name="HOME_ASSISTANT_TOKEN"]').textContent.includes('not just the listed sites')`), "the bot editor shows the no-approval loosening and site limit");
  let cr = await credRows();
  check(cr.find((c) => c.name === "SWITCH_ADMIN")?.on === "true" && cr.find((c) => c.name === "HOME_ASSISTANT_TOKEN")?.on === "true", "Forge's editor: the keys kept for it, switched on", cr);
  check(cr.find((c) => c.name === "GITHUB")?.kind === "open" && cr.find((c) => c.name === "GITHUB")?.on === "true", "…and the ones any chat may use, each with its own switch (made before the setting: all on)", cr);
  check(!!(await p.eval(`document.querySelector('.be-cred-legacy')`)), "…saying it was made before the setting");
  await p.eval(`document.querySelector('.be-creds').scrollIntoView({ block: 'center' })`);
  await p.shot(`${SHOTS}/cred-bot-desk-dark.png`);
  await flip("HOME_ASSISTANT_TOKEN");
  await flip("GITHUB");
  await sleep(400);
  check(JSON.stringify((await list()).find((s) => s.name === "HOME_ASSISTANT_TOKEN")?.bots) === JSON.stringify([FORGE]) && !(await forge()).secrets, "nothing changes until Save");
  await p.clickText(".be-actions .btn-primary", "Save changes");
  await sleep(900);
  const ha2 = (await list()).find((s) => s.name === "HOME_ASSISTANT_TOKEN");
  check(JSON.stringify(ha2?.bots) === "[]" && (await stored("HOME_ASSISTANT_TOKEN", "value", "ha-token-0000-1111")), "Save: Forge is off that key's list, its value kept", ha2);
  const fs = (await forge()).secrets;
  check(fs?.default === true && fs?.each?.GITHUB === false && Object.keys(fs.each).length === 1, "…and Forge's own choice: every open one except GitHub", fs);
  await p.eval(`__mod('lib/ui.ts').then(m => m.openBotEditor(${JSON.stringify(FORGE)}))`);
  await p.waitFor(`document.querySelector('.be-creds .be-cred')`, 8000);
  await sleep(500);
  cr = await credRows();
  check(cr.find((c) => c.name === "GITHUB")?.on === "false" && cr.find((c) => c.name === "HOME_ASSISTANT_TOKEN")?.on === "false" && !(await p.eval(`document.querySelector('.be-cred-legacy')`)), "opened again, it shows what was saved", cr);
  check(/of/.test((await p.eval(`document.querySelector('.be-cred-count')?.textContent`)) ?? ""), "…and how many it can use");
  await p.eval(`__mod('lib/ui.ts').then(m => m.setBotEditor(null))`);
  await sleep(300);

  // ── a new bot: none of them until switched on, chosen before it exists ──
  await p.eval(`__mod('lib/ui.ts').then(m => m.openBotEditor(null))`);
  await p.waitFor(`document.querySelector('.be-name')`, 8000);
  await fill(".be-name", "Keeper");
  await p.eval(`document.querySelector('.be-more')?.click()`);
  await p.waitFor(`document.querySelector('.be-creds .be-cred')`, 8000);
  await sleep(400);
  cr = await credRows();
  check(cr.length > 0 && cr.every((c) => c.on === "false"), "a new bot starts with none", cr);
  check((await p.eval(`document.querySelector('.be-cred-later .toggle')?.getAttribute('aria-checked')`)) === "false", "…nor the ones saved later");
  await flip("GITHUB");
  await flip("HOME_ASSISTANT_TOKEN");
  await sleep(200);
  await p.clickText(".be-actions .btn", "Create and keep editing");
  await sleep(1200);
  const keeper = (await ws.rpc("list_bots")).result.find((x) => x.name === "Keeper");
  check(keeper?.secrets?.default === false && keeper?.secrets?.each?.GITHUB === true, "created with GitHub", keeper?.secrets);
  check((await list()).find((s) => s.name === "HOME_ASSISTANT_TOKEN")?.bots?.includes(keeper?.id), "…and on the Home Assistant key's list");
  await p.eval(`__mod('lib/ui.ts').then(m => m.setBotEditor(null))`);
  await sleep(300);

  // ── light theme, desktop ──
  await p.theme("light");
  await open();
  await p.shot(`${SHOTS}/cred-list-desk-light.png`);
  await edit("SWITCH_ADMIN");
  await p.shot(`${SHOTS}/cred-edit-desk-light.png`);
  check(p.errors.length === 0, "desktop: no errors", p.errors);
  await p.close();

  // ── phone, folded phone (cover screen) and the unfolded Fold ──
  for (const [name, w, h, theme] of [["phone", 412, 915, "dark"], ["phone", 412, 915, "light"], ["folded", 344, 882, "dark"], ["folded", 344, 882, "light"], ["fold", 884, 1104, "dark"], ["fold", 884, 1104, "light"]]) {
    const m = await device();
    await m.viewport(w, h, true, 2);
    await signIn(m, `Credentials ${name} ${theme}`);
    await m.theme(theme);
    await m.eval(`__mod('lib/ui.ts').then(u => { u.setDrawerOpen?.(false); u.openSettings('secrets'); })`);
    await m.waitFor(`document.querySelector('.secret-item')`, 8000);
    await sleep(600);
    await m.shot(`${SHOTS}/cred-list-${name}-${theme}.png`);
    const over = await m.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth - b.clientWidth; })()`);
    check(over <= 1, `${name} ${theme}: the list fits the width`, over);
    // Every row's parts stay inside it: name, actions, variables, details.
    const spill = await m.eval(`[...document.querySelectorAll('.secret-item')].flatMap(r => { const b = r.getBoundingClientRect(); return [...r.querySelectorAll('.secret-title, .secret-actions, .var-chip, .cred-scope > span')].filter(e => { const x = e.getBoundingClientRect(); return x.right > b.right + 1 || x.left < b.left - 1; }).map(e => r.dataset.name + ':' + e.className); })`);
    check(spill.length === 0, `${name} ${theme}: nothing spills out of a row`, spill);
    const actions = await m.eval(`(() => { const r = document.querySelector('.secret-item[data-name="GITHUB"]'); const t = r.querySelector('.secret-title').getBoundingClientRect(); const a = r.querySelector('.secret-actions').getBoundingClientRect(); return { sameLine: Math.abs((t.top + t.bottom) / 2 - (a.top + a.bottom) / 2) < 8, h: a.height }; })()`);
    check(actions.sameLine, `${name} ${theme}: Edit and Delete sit on the name's line`, actions);
    await m.eval(`(() => { const b = [...document.querySelectorAll('.secret-item[data-name="SWITCH_ADMIN"] .secret-actions .btn')].find(x => x.textContent.includes('Edit')); b.click(); })()`);
    await m.waitFor(`document.querySelector('.secret-item.editing form')`, 4000);
    await sleep(400);
    await m.eval(`document.querySelector('.secret-item.editing .access').scrollIntoView({ block: 'center' })`);
    await sleep(200);
    await m.shot(`${SHOTS}/cred-edit-${name}-${theme}.png`);
    const over2 = await m.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth - b.clientWidth; })()`);
    check(over2 <= 1, `${name} ${theme}: the editor fits the width`, over2);
    if (w < 560) {
      const modes = await m.eval(`(() => { const r = [...document.querySelectorAll('.secret-item.editing .access-mode')].map(b => b.getBoundingClientRect()); return { n: r.length, stacked: r.length === 2 && r[1].top >= r[0].bottom - 1, h: Math.min(...r.map(x => x.height)) }; })()`);
      check(modes.n === 2 && modes.stacked && modes.h >= 36, `${name} ${theme}: the two choices stack, big enough to tap`, modes);
    }
    check(m.errors.length === 0, `${name} ${theme}: no errors`, m.errors);
    // The bot editor's list on a narrow screen: each name with its switch, nothing wider than the screen.
    if (name !== "phone") {
      await m.eval(`__mod('lib/ui.ts').then(u => { u.setSettingsTab(null); u.openBotEditor(${JSON.stringify(FORGE)}); })`);
      await m.waitFor(`document.querySelector('.be-creds .be-cred')`, 8000);
      await sleep(500);
      await m.eval(`document.querySelector('.be-creds').scrollIntoView({ block: 'start' })`);
      await sleep(200);
      await m.shot(`${SHOTS}/cred-bot-${name}-${theme}.png`);
      const wide = await m.eval(`[...document.querySelectorAll('.be-cred')].filter(r => { const b = document.querySelector('.be-creds').getBoundingClientRect(); const t = r.querySelector('.toggle').getBoundingClientRect(); return t.right > b.right + 1; }).length`);
      check(wide === 0, `${name} ${theme}: the bot's switches stay on screen`, wide);
    }
    await m.close();
  }
  ws.close();
  finish();
});
