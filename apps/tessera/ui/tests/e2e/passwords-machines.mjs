// SEC (batch 15): saved passwords on the user's machines, clearer titles, and
// whether chats may hand work to bots.
// - Passwords & keys: "Let saved passwords be used on my machines" (Off by
//   default / Machines I pick / All my machines, each loosening asks first);
//   every password follows it unless it chooses for itself ("Like my other
//   passwords" / "Choose for this one"); an edit that doesn't touch the
//   machines never clears them; the list says where each one works.
// - Titles that say little ("saved from a chat") get suggestions to apply,
//   each editable or left out.
// - Privacy & safety: the summary at the top, and "Chats asking your bots for help"
//   (Yes / Ask me first / No; Yes asks first); a bot's own "Other chats may
//   ask it for help" switch.
// Desktop, phone, folded phone (cover screen) and unfolded Fold, dark and light.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const FORGE = "bot_forge1";

run(async () => {
  await mock("/__mock/reset");
  const ws = await wsClient("passwords-machines script");
  const rpc = async (method, params) => (await ws.rpc(method, params)).result;
  const list = async () => rpc("list_secrets");
  const secret = async (name) => (await list()).find((s) => s.name === name);
  const sec = async () => rpc("get_security");
  // Two entries saved the old way: titles that say nothing.
  await rpc("save_secret", { name: "DESKTOP_SUDO", kind: "login", description: "saved from a chat", fields: { username: "alex", password: "sudo-pass-1234" } });
  await rpc("save_secret", { name: "GITHUB_TOKEN", kind: "token", description: "GitHub token, saved from a chat", fields: { value: "ghp_mockmockmockmockmockmockmockmock00" }, hosts: ["github.com"] });

  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Passwords desktop");
  const open = async (tab, ready) => {
    await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings(${JSON.stringify(tab)}))`);
    await p.waitFor(ready, 8000);
    await sleep(400);
  };
  const dialog = () => p.eval(`document.querySelector('.dialog-body') ? { title: [...document.querySelectorAll('.modal h2')].pop()?.textContent ?? '', body: document.querySelector('.dialog-text')?.textContent ?? '' } : null`);
  const answer = async (ok) => {
    await p.click(ok ? ".dialog-actions button[type=submit]" : ".dialog-actions .btn-ghost");
    await sleep(600);
  };
  const where = (name) => p.eval(`document.querySelector('.secret-item[data-name="${name}"] .cred-where')?.textContent ?? null`);
  const fill = (sel, text) =>
    p.eval(`(() => { const i = document.querySelector(${JSON.stringify(sel)}); i.focus(); const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; set.call(i, ${JSON.stringify(text)}); i.dispatchEvent(new Event('input', { bubbles: true })); return true; })()`);
  const edit = async (name) => {
    await p.clickText(`.secret-item[data-name="${name}"] .secret-actions .btn`, "Edit");
    await p.waitFor(`document.querySelector('.secret-item[data-name="${name}"].editing form')`, 4000);
    await sleep(200);
  };
  const saveEdit = async () => {
    await p.clickText(".secret-item.editing .form-actions .btn-primary", "Save changes");
    await sleep(700);
  };

  // ── the setting: off by default ──
  await open("secrets", `document.querySelector('.cred-machines')`);
  check((await sec()).secret_machines === "off", "off by default on the server");
  const card = await p.eval(`(() => { const c = document.querySelector('.cred-machines'); return { title: c.querySelector('.setting-title').textContent, on: c.querySelector('[role=radio][aria-checked=true]')?.textContent, hint: c.querySelector('.cred-machines-hint').textContent, opts: [...c.querySelectorAll('[role=radio]')].map(b => b.textContent) }; })()`);
  check(card.title === "Let saved passwords be used on my machines" && card.on === "Off", "the card says what it is, Off chosen", card);
  check(card.opts.join("|") === "Off|Machines I pick|All my machines", "three plain choices", card.opts);
  check(/only on this server/.test(card.hint), "Off says where they are used", card.hint);
  check((await where("GITHUB")) === "This server" && (await where("SWITCH_ADMIN")) === "Desktop", "rows: this server; a password's own machine", [await where("GITHUB"), await where("SWITCH_ADMIN")]);
  await p.shot(`${SHOTS}/pwm-off-desk-dark.png`);

  // All my machines: asks first; Cancel changes nothing.
  await p.clickText(".cred-machines [role=radio]", "All my machines");
  await sleep(400);
  let d = await dialog();
  check(!!d && /all your machines/i.test(d.title) && /only to the command that names it/.test(d.body), "widening asks first, saying what it means", d);
  await answer(false);
  check((await sec()).secret_machines === "off", "Cancel: still off");
  await p.clickText(".cred-machines [role=radio]", "All my machines");
  await sleep(400);
  await answer(true);
  check((await sec()).secret_machines === "all", "Allow: all machines");
  check((await where("GITHUB")) === "This server and all your machines" && (await where("SWITCH_ADMIN")) === "Desktop", "rows follow it; one with its own machines keeps them", [await where("GITHUB"), await where("SWITCH_ADMIN")]);
  check(await p.eval(`!!document.querySelector('.cred-machines .cred-machines-icon.loose')`), "a loosened setting is marked");
  // New ones stay here whatever the setting; the ones other computers can use are flagged.
  check((await secret("DESKTOP_SUDO")).machines_default === false && (await where("DESKTOP_SUDO")) === "This server", "a new one starts on this server only", await secret("DESKTOP_SUDO"));
  const roam = await p.eval(`(() => { const b = document.querySelector('.cred-roaming'); return b ? { text: b.textContent, loose: !!document.querySelector('.secret-item[data-name="GITHUB"] .cred-where.loose'), here: !!document.querySelector('.secret-item[data-name="DESKTOP_SUDO"] .cred-where.loose') } : null; })()`);
  check(!!roam && /can be used on other computers/.test(roam.text) && /Desktop/.test(roam.text) && roam.loose && !roam.here, "the list flags passwords other computers can use, naming them", roam);
  await p.shot(`${SHOTS}/pwm-roaming-desk-dark.png`);

  // Machines I pick: nothing picked is the same as Off, then Desktop (asks first).
  await p.clickText(".cred-machines [role=radio]", "Machines I pick");
  await sleep(500);
  check(!(await dialog()), "narrowing doesn't ask");
  let s = await sec();
  check(s.secret_machines === "picked" && s.secret_machines_picked.length === 0, "picked, none yet", s);
  check(await p.eval(`!!document.querySelector('.cred-machines .warn-text')`), "none picked: it says that's the same as Off");
  await p.clickText(".cred-machines .cred-place", "Desktop");
  await sleep(400);
  d = await dialog();
  check(!!d && /Desktop/.test(d.title), "picking a machine asks first", d);
  await answer(true);
  s = await sec();
  check(JSON.stringify(s.secret_machines_picked) === '["dev_desktop"]', "Desktop picked", s);
  check((await where("GITHUB")) === "This server and Desktop", "rows say Desktop", await where("GITHUB"));
  // Off, then back to the machines picked before: that widens again, so it asks.
  await p.clickText(".cred-machines [role=radio]", "Off");
  await sleep(500);
  check(!(await dialog()) && (await sec()).secret_machines === "off", "Off without asking");
  await p.clickText(".cred-machines [role=radio]", "Machines I pick");
  await sleep(400);
  d = await dialog();
  check(!!d && /Desktop/.test(d.title), "going back to the picked machines asks, naming them", d);
  await answer(true);
  check((await sec()).secret_machines === "picked", "picked again");
  await p.shot(`${SHOTS}/pwm-picked-desk-dark.png`);

  // ── one password: like the others, or its own ──
  await edit("GITHUB");
  const mode = await p.eval(`(() => { const e = document.querySelector('.secret-item.editing'); return { on: e.querySelector('.cred-where-mode [aria-checked=true]')?.textContent, follow: e.querySelector('.cred-follow')?.textContent ?? '', labels: [...e.querySelectorAll('.field-label')].map(l => l.textContent.trim()) }; })()`);
  check(mode.on === "Like my other passwords" && /On Desktop/.test(mode.follow), "it follows the setting, saying where that is", mode);
  check(mode.labels.includes("Title") && mode.labels.includes("Name agents use"), "the editor asks for a title and shows the name agents use", mode.labels);
  await p.shot(`${SHOTS}/pwm-edit-follow-desk-dark.png`);
  // A change that doesn't touch the machines keeps them (no machines sent).
  await fill(".secret-item.editing .grid2 label:first-child input", "GitHub login");
  await saveEdit();
  let g = await secret("GITHUB");
  check(g.description === "GitHub login" && g.machines_default === true, "title saved, still following the setting", g);
  await edit("GITHUB");
  await p.clickText(".secret-item.editing .cred-where-mode button", "Choose for this one");
  await sleep(200);
  await p.clickText(".secret-item.editing .cred-machines-mode .cred-place", "nas");
  await saveEdit();
  g = await secret("GITHUB");
  check(g.machines_default === false && JSON.stringify(g.on_machines) === '["dev_nas"]', "its own machine now", g);
  check((await where("GITHUB")) === "This server and nas", "the row says so", await where("GITHUB"));
  // An edit of the title alone keeps a password's own machines too.
  await edit("SWITCH_ADMIN");
  await fill(".secret-item.editing .grid2 label:first-child input", "Homelab switches (admin)");
  await saveEdit();
  const sw = await secret("SWITCH_ADMIN");
  check(JSON.stringify(sw.on_machines) === '["dev_desktop"]' && sw.on_server === false && sw.description === "Homelab switches (admin)", "an unrelated edit never clears its machines", sw);

  // ── titles that say little ──
  const sugg = await rpc("suggest_secret_titles");
  check(
    sugg.length === 2 && sugg.find((t) => t.name === "DESKTOP_SUDO")?.suggested === "Desktop sudo login" && sugg.find((t) => t.name === "GITHUB_TOKEN")?.suggested === "GitHub token",
    "suggestions from the name, the machine and the known format",
    sugg,
  );
  check(!JSON.stringify(sugg).includes("alex") && !JSON.stringify(sugg).includes("sudo-pass"), "no value in a suggestion");
  const titlesHead = await p.eval(`document.querySelector('.cred-titles-head')?.textContent ?? ''`);
  check(/2 titles don't say what they're for/.test(titlesHead), "a card says two titles are unclear", titlesHead);
  await p.click(".cred-titles-open");
  await sleep(300);
  const rows = await p.eval(`[...document.querySelectorAll('.cred-title-row')].map(r => ({ name: r.dataset.name, value: r.querySelector('.input').value, was: r.querySelector('.cred-title-was').textContent }))`);
  check(rows.length === 2 && rows.find((r) => r.name === "DESKTOP_SUDO")?.value === "Desktop sudo login" && /saved from a chat/.test(rows[0].was + rows[1].was), "each with its suggestion, editable, and what it was", rows);
  await p.shot(`${SHOTS}/pwm-titles-desk-dark.png`);
  // Edit one, leave the other out.
  await fill('.cred-title-row[data-name="DESKTOP_SUDO"] .input', "Desktop admin password");
  await p.eval(`document.querySelector('.cred-title-row[data-name="GITHUB_TOKEN"] input[type=checkbox]').click()`);
  await sleep(150);
  check((await p.eval(`document.querySelector('.cred-titles-apply').textContent.trim()`)) === "Rename 1", "the button counts what it renames");
  await p.click(".cred-titles-apply");
  await sleep(900);
  check((await secret("DESKTOP_SUDO")).description === "Desktop admin password" && (await secret("GITHUB_TOKEN")).description === "GitHub token, saved from a chat", "only the ticked one was renamed, as edited");
  check(await p.eval(`/1 title doesn't say/.test(document.querySelector('.cred-titles-head')?.textContent ?? '')`), "the card counts what's left");
  const leak = await p.eval(`document.documentElement.outerHTML.includes('sudo-pass-1234') || document.documentElement.outerHTML.includes('ghp_mockmock')`);
  check(!leak, "no value on the page");

  // ── Privacy & safety: the summary and chats asking your bots ──
  await open("security", `document.querySelector('.sec-glance')`);
  const glance = await p.eval(`[...document.querySelectorAll('.sec-glance li')].map(l => ({ id: l.dataset.id, tone: l.dataset.tone, text: l.textContent.trim() }))`);
  check(glance.map((g) => g.id).join() === "sandbox,chats,machines,bots,e2ee", "the summary has one line per protection", glance);
  check(glance.find((g) => g.id === "machines")?.tone === "warn" && /Desktop/.test(glance.find((g) => g.id === "machines").text), "passwords on a machine are marked", glance);
  check(/Chats can ask your bots for help/.test(glance.find((g) => g.id === "bots")?.text ?? ""), "bots: the default", glance);
  const handoff = await p.eval(`(() => { const c = document.getElementById('sec-bots'); return { title: c.querySelector('.setting-title').textContent, opts: [...c.querySelectorAll('[role=radio]')].map(b => b.textContent), on: c.querySelector('[role=radio][aria-checked=true]')?.textContent }; })()`);
  check(handoff.title === "Chats asking your bots for help" && handoff.opts.join("|") === "Yes|Ask me|No" && handoff.on === "Yes", "Chats asking your bots for help: Yes by default", handoff);
  await p.shot(`${SHOTS}/pwm-security-desk-dark.png`);
  await p.clickText("#sec-bots [role=radio]", "No");
  await sleep(500);
  check(!(await dialog()) && (await sec()).bot_handoff === "off", "No: off, without asking");
  check(await p.eval(`/can't ask your bots/.test(document.querySelector('.sec-glance li[data-id=bots]').textContent)`), "the summary follows");
  await p.clickText("#sec-bots [role=radio]", "Ask me");
  await sleep(500);
  check((await sec()).bot_handoff === "ask", "Ask me");
  await p.clickText("#sec-bots [role=radio]", "Yes");
  await sleep(400);
  d = await dialog();
  check(!!d && /without asking/.test(d.title), "back to Yes asks first", d);
  await answer(true);
  check((await sec()).bot_handoff === "on", "Yes again");
  // The machines line goes to Passwords & keys.
  await p.click('.sec-glance li[data-id="machines"] button');
  await sleep(600);
  check((await p.eval(`document.querySelector('.settings-head h3')?.textContent.trim()`)) === "Passwords & keys", "the machines line opens Passwords & keys");

  // ── a bot's own switch ──
  await p.eval(`__mod('lib/ui.ts').then(m => { m.setSettingsTab(null); m.openBotEditor(${JSON.stringify(FORGE)}); })`);
  await p.waitFor(`document.querySelector('.be-askable')`, 8000);
  await sleep(300);
  const sw0 = await p.eval(`document.querySelector('.be-askable .toggle').getAttribute('aria-checked')`);
  check(sw0 === "true", "a bot takes work from other chats by default");
  await p.eval(`document.querySelector('.be-askable').scrollIntoView({ block: 'center' }); document.querySelector('.be-askable .toggle').click()`);
  await sleep(200);
  check(/Only you talk to it/.test(await p.eval(`document.querySelector('.be-askable').textContent`)), "off: it says only you talk to it");
  await p.shot(`${SHOTS}/pwm-bot-askable-desk-dark.png`);
  await p.eval(`[...document.querySelectorAll('button.btn-primary')].find(b => /Save changes/.test(b.textContent))?.click()`);
  await sleep(900);
  const forge = (await rpc("list_bots")).find((b) => b.id === FORGE);
  check(forge?.askable === false, "saved: Forge takes work only from the user", forge?.askable);
  await p.eval(`__mod('lib/ui.ts').then(m => m.setBotEditor(null))`);
  await sleep(300);
  check(p.errors.length === 0, "desktop: no errors", p.errors);

  // One tap keeps them all on this server again (asks first).
  await open("secrets", `document.querySelector('.cred-roaming-fix')`);
  await p.click(".cred-roaming-fix");
  await sleep(400);
  d = await dialog();
  check(!!d && /on this server only/.test(d.title) && /only for a machine stays/.test(d.body), "keeping them here asks first, and says the machine-only one stays", d);
  await answer(true);
  await p.waitFor(`(document.querySelector('.cred-roaming')?.textContent ?? '').startsWith('1 password')`, 6000);
  const after = await list();
  const onServer = after.filter((x) => !x.chat && x.on_server !== false);
  check(onServer.every((x) => !x.machines_default && !(x.on_machines ?? []).length) && (await sec()).secret_machines === "off", "every saved one on this server is here only, the setting off", after.map((x) => [x.name, x.machines_default, x.on_machines]));
  check(JSON.stringify((await secret("SWITCH_ADMIN")).on_machines) === '["dev_desktop"]', "the one only for a machine stays as set");

  // ── light, and the phone sizes ──
  await p.theme("light");
  await open("secrets", `document.querySelector('.cred-machines')`);
  await p.shot(`${SHOTS}/pwm-picked-desk-light.png`);
  await open("security", `document.querySelector('.sec-glance')`);
  await p.shot(`${SHOTS}/pwm-security-desk-light.png`);
  await p.close();

  for (const [name, w, h, theme] of [["phone", 412, 915, "dark"], ["folded", 344, 882, "dark"], ["folded", 344, 882, "light"], ["fold", 884, 1104, "light"], ["fold", 884, 1104, "dark"]]) {
    const m = await device();
    await m.viewport(w, h, true, 2);
    await signIn(m, `Passwords ${name} ${theme}`);
    await m.theme(theme);
    await m.eval(`__mod('lib/ui.ts').then(u => { u.setDrawerOpen?.(false); u.openSettings('secrets'); })`);
    await m.waitFor(`document.querySelector('.cred-machines')`, 8000);
    await sleep(600);
    const over = await m.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth - b.clientWidth; })()`);
    check(over <= 1, `${name} ${theme}: Passwords & keys fits the width`, over);
    const seg = await m.eval(`(() => { const c = document.querySelector('.cred-machines'); const r = c.getBoundingClientRect(); const bs = [...c.querySelectorAll('[role=radio]')].map(b => b.getBoundingClientRect()); return { inside: bs.every(b => b.left >= r.left - 1 && b.right <= r.right + 1), h: Math.min(...bs.map(b => b.height)) }; })()`);
    check(seg.inside && seg.h >= 32, `${name} ${theme}: the three choices fit and are big enough to tap`, seg);
    await m.eval(`document.querySelector('.cred-machines').scrollIntoView({ block: 'start' })`);
    await sleep(200);
    await m.shot(`${SHOTS}/pwm-list-${name}-${theme}.png`);
    await m.eval(`document.querySelector('.cred-titles-open')?.click()`);
    await sleep(300);
    const tOver = await m.eval(`(() => { const c = document.querySelector('.cred-titles'); if (!c) return 0; const r = c.getBoundingClientRect(); return Math.max(0, ...[...c.querySelectorAll('input, button, .cred-title-was')].map(e => e.getBoundingClientRect().right - r.right)); })()`);
    check(tOver <= 1, `${name} ${theme}: the titles card fits`, tOver);
    await m.eval(`document.querySelector('.cred-titles')?.scrollIntoView({ block: 'center' })`);
    await sleep(200);
    await m.shot(`${SHOTS}/pwm-titles-${name}-${theme}.png`);
    // The editor's "where".
    await m.eval(`(() => { const b = [...document.querySelectorAll('.secret-item[data-name="GITHUB"] .secret-actions .btn')].find(x => x.textContent.includes('Edit')); b.click(); })()`);
    await m.waitFor(`document.querySelector('.secret-item.editing .cred-where-sec')`, 4000);
    await m.eval(`document.querySelector('.secret-item.editing .cred-where-sec').scrollIntoView({ block: 'center' })`);
    await sleep(300);
    const eOver = await m.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth - b.clientWidth; })()`);
    check(eOver <= 1, `${name} ${theme}: the editor fits`, eOver);
    await m.shot(`${SHOTS}/pwm-where-${name}-${theme}.png`);
    await m.eval(`__mod('lib/ui.ts').then(u => u.openSettings('security'))`);
    await m.waitFor(`document.querySelector('.sec-glance')`, 8000);
    await sleep(500);
    const gOver = await m.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth - b.clientWidth; })()`);
    check(gOver <= 1, `${name} ${theme}: Privacy & safety fits`, gOver);
    await m.shot(`${SHOTS}/pwm-security-${name}-${theme}.png`);
    await m.eval(`document.getElementById('sec-bots').scrollIntoView({ block: 'center' })`);
    await sleep(200);
    await m.shot(`${SHOTS}/pwm-bots-${name}-${theme}.png`);
    check(m.errors.length === 0, `${name} ${theme}: no errors`, m.errors);
    await m.close();
  }
  ws.close();
  finish();
});
