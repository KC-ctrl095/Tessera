// Settings' navigation (batch 16: sections and Help & tour; batch 17: Usage a ninth section). A phone (the folded Fold's cover
// screen, a normal phone): a list of sections like a phone's own settings, a tap opens one full
// screen with a back arrow, and the back gesture goes up a level (a page → its section → the list →
// out), remembering where you were. Wider screens (the unfolded Fold, computers) keep the side list,
// Help & tour at its foot. Search finds a section or a page by what's in it; older names
// (`openSettings("security")`, "voice", "machines") land in their new homes, and the ones whose home
// is outside Settings now (bots, scheduled tasks, archived chats) open there; the dialogs inside
// Settings still work.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const ui = (p, js) => p.eval(`__mod('lib/ui.ts').then((u) => { ${js} })`);
const shown = (p, sel) => p.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); return !!e && e.getClientRects().length > 0 && getComputedStyle(e).visibility !== 'hidden'; })()`);
const state = (p) =>
  p.eval(`(() => { const l = document.querySelector('.settings-layout'); if (!l) return null; const vis = (s) => { const e = document.querySelector(s); return !!e && e.getClientRects().length > 0; }; return { view: l.dataset.view ?? null, list: vis('.settings-nav'), section: vis('.settings-main'), back: vis('.settings-back'), title: document.querySelector('.settings-head h3')?.textContent ?? null, crumb: document.querySelector('.settings-crumb')?.textContent ?? null, active: document.querySelector('.settings-tab.active')?.dataset.tab ?? null }; })()`);
const rowBox = (p, id) =>
  p.eval(`(() => { const e = document.querySelector('.settings-tab[data-tab="${id}"]'); if (!e) return null; e.scrollIntoView({ block: 'nearest' }); const r = e.getBoundingClientRect(); return { cx: r.x + r.width / 2, cy: r.y + r.height / 2, h: r.height, right: r.right }; })()`);
const tapRow = async (p, id) => {
  const b = await rowBox(p, id);
  await p.tap(b.cx, b.cy);
  await sleep(500);
};
/** A line in a section that leads to a page of its own. */
const tapPage = async (p, title) => {
  const b = await p.eval(`(() => { const e = [...document.querySelectorAll('.settings-body .page-row')].find((x) => x.querySelector('.setting-title')?.textContent === ${JSON.stringify(title)}); if (!e) return null; e.scrollIntoView({ block: 'center' }); const r = e.getBoundingClientRect(); return { cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);
  if (!b) throw new Error(`no page row ${title}`);
  await p.tap(b.cx, b.cy);
  await sleep(700);
};
const back = async (p) => {
  await p.eval(`history.back()`);
  await sleep(500);
};
const typeSearch = async (p, text) => {
  await p.eval(`(() => { const i = document.querySelector('.settings-search input'); i.value = ${JSON.stringify(text)}; i.dispatchEvent(new InputEvent('input', { bubbles: true })); })()`);
  await sleep(250);
};
const found = (p) => p.eval(`[...document.querySelectorAll('.settings-tab')].map((e) => e.dataset.tab)`);
/** No sideways scrolling, and its blocks inside the screen (wide tables scroll inside their own card). */
const noSideScroll = (p, sel) => p.eval(`(() => { const b = document.querySelector(${JSON.stringify(sel)}); return b.scrollWidth <= b.clientWidth + 1 && [...b.children].every((e) => e.getBoundingClientRect().right <= innerWidth + 1); })()`);
const SECTIONS = ["appearance", "notifications", "models", "usage", "safety", "memory", "web", "devices", "connection"];

run(async () => {
  await mock("/__mock/reset");

  // ── a phone: the folded Fold's cover screen ──
  for (const theme of ["dark", "light"]) {
    console.log(`— cover screen (${theme})`);
    const p = await device();
    await p.viewport(344, 882, true, 2.625);
    await signIn(p, `Settings cover ${theme}`);
    await p.theme(theme);
    const hist0 = await p.eval(`history.length`);
    await ui(p, `u.openSettings()`);
    await p.waitFor(`document.querySelector('.settings-layout')`, 8000);
    await sleep(500);
    let s = await state(p);
    check(s.view === "list" && s.list && !s.section, "Settings opens on the list of sections", s);
    const rows = await p.eval(`[...document.querySelectorAll('.settings-tab')].map((e) => { const r = e.getBoundingClientRect(); return { id: e.dataset.tab, h: Math.round(r.height), hint: e.querySelector('.settings-tab-hint')?.textContent ?? '', chev: !!e.querySelector('.settings-tab-chev'), right: r.right }; })`);
    check(JSON.stringify(rows.map((r) => r.id)) === JSON.stringify([...SECTIONS, "help"]), "nine sections (Usage its own), then Help & tour", rows.map((r) => r.id));
    check(rows.every((r) => r.h >= 56 && r.hint && r.chev && r.right <= 344), "every section: a row big enough for a thumb, with its icon, name, a line about it and a chevron", rows.filter((r) => !(r.h >= 56 && r.hint && r.chev)));
    check(await noSideScroll(p, ".settings-nav"), "nothing runs off the side");
    check((await p.eval(`document.querySelectorAll('.settings-group').length`)) === 0 && (await p.eval(`!!document.querySelector('.settings-foot .settings-tab[data-tab="help"]')`)), "no group headings; Help & tour sits apart at the foot");
    check(!(await p.eval(`document.activeElement?.matches('input')`)), "no keyboard pops up on opening");
    await p.shot(`${SHOTS}/settings-nav-cover-list-${theme}.png`);

    // into a section and back
    await tapRow(p, "safety");
    await p.waitFor(`document.querySelector('.sec .sec-glance')`, 8000);
    s = await state(p);
    check(s.view === "section" && !s.list && s.section && s.back && s.title === "Privacy & safety", "a tap opens the section full screen, with a back arrow", s);
    check(await noSideScroll(p, ".settings-body"), "the section fits the width");
    await p.shot(`${SHOTS}/settings-nav-cover-safety-${theme}.png`);
    // a page one step deeper, and back to its section
    await tapPage(p, "Passwords & keys");
    s = await state(p);
    check(s.title === "Passwords & keys" && s.crumb === "Privacy & safety" && s.back, "a page opens one step deeper, saying which section it is in", s);
    await p.shot(`${SHOTS}/settings-nav-cover-page-${theme}.png`);
    await back(p);
    s = await state(p);
    check(s?.view === "section" && s.title === "Privacy & safety", "the back gesture from a page returns to its section", s);
    await back(p);
    s = await state(p);
    check(s?.view === "list" && s.list, "…then to the list", s);
    check(s.active === "safety", "…which marks where you were", s.active);
    // the list remembers its scroll; a section remembers its own
    await p.eval(`document.querySelector('.settings-nav').scrollTop = 160`);
    await sleep(150);
    const row = await rowBox(p, "web");
    const listTop = await p.eval(`document.querySelector('.settings-nav').scrollTop`);
    await p.tap(row.cx, row.cy);
    await sleep(800);
    await p.eval(`document.querySelector('.settings-body').scrollTop = 300`);
    await sleep(200);
    const bodyTop = await p.eval(`document.querySelector('.settings-body').scrollTop`);
    await p.tap(...(await p.eval(`(() => { const r = document.querySelector('.settings-back').getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; })()`)));
    await sleep(500);
    s = await state(p);
    const listTop2 = await p.eval(`document.querySelector('.settings-nav').scrollTop`);
    check(s.view === "list" && Math.abs(listTop2 - listTop) <= 2, "the back arrow too; the list is scrolled where it was", { listTop, listTop2 });
    await tapRow(p, "web");
    await sleep(400);
    const bodyTop2 = await p.eval(`document.querySelector('.settings-body').scrollTop`);
    check(bodyTop > 100 && Math.abs(bodyTop2 - bodyTop) <= 2, "a section opens where you left it", { bodyTop, bodyTop2 });
    // back twice: list, then out of Settings (and still in the app)
    await back(p);
    await back(p);
    check(!(await state(p)), "back from the list closes Settings");
    check((await p.eval(`!!document.querySelector('.workspace') && location.href.startsWith('http')`)) && (await p.eval(`history.length`)) >= hist0, "…and stays in the app");

    // search: sections and pages
    await ui(p, `u.openSettings()`);
    await p.waitFor(`document.querySelector('.settings-layout[data-view="list"]')`, 6000);
    await sleep(300);
    await typeSearch(p, "sandbox");
    let f = await found(p);
    check(f.includes("safety") && f.includes("commands") && !f.includes("appearance"), "searching “sandbox” finds Privacy & safety and its page What commands can reach", f);
    check(/In Privacy & safety/.test(await p.eval(`document.querySelector('.settings-tab[data-tab="commands"] .settings-tab-hint')?.textContent ?? ''`)), "…the page says which section it is in");
    await typeSearch(p, "dark");
    f = await found(p);
    check(f.includes("appearance"), "…“dark” finds Appearance", f);
    await typeSearch(p, "pair phone");
    f = await found(p);
    check(f.join() === "devices", "…“pair phone” finds Devices", f);
    await p.shot(`${SHOTS}/settings-nav-cover-search-${theme}.png`);
    await p.eval(`document.querySelector('.settings-search input').dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))`);
    await sleep(500);
    s = await state(p);
    check(s.view === "section" && s.title === "Devices", "Enter opens the first match", s);
    await back(p);
    check((await p.eval(`document.querySelector('.settings-search input').value`)) === "pair phone", "back on the list, the search is still there");
    await typeSearch(p, "archived");
    f = await found(p);
    check(f.join() === "archived" && (await p.eval(`document.querySelector('.settings-tab[data-tab="archived"] .settings-tab-hint')?.textContent`)) === "In the sidebar", "…“archived” finds the archived chats, in the sidebar now", f);
    await p.shot(`${SHOTS}/settings-nav-cover-elsewhere-${theme}.png`);
    await typeSearch(p, "zzzz");
    check(await shown(p, ".settings-none"), "nothing found says so");
    await typeSearch(p, "");
    check((await p.eval(`document.querySelectorAll('.settings-tab').length`)) === rows.length, "clearing it shows every section again");
    await p.click(".settings-x");
    await sleep(400);
    check(!(await state(p)), "✕ closes Settings");

    // older names: straight to their new home; back leaves Settings (back where the link was)
    await ui(p, `u.openSettings('security')`);
    await p.waitFor(`document.querySelector('.sec .sec-glance')`, 8000);
    s = await state(p);
    check(s.view === "section" && s.title === "Privacy & safety" && s.back, "openSettings('security') (an older name) opens Privacy & safety directly", s);
    // a dialog inside a section: back closes only the dialog
    await p.eval(`(() => { const b = [...document.querySelectorAll('.settings-body .link-btn')].find((b) => /Security guide/.test(b.textContent)); b.scrollIntoView({ block: 'center' }); b.click(); })()`);
    await p.waitFor(`document.querySelector('.guide-modal')`, 6000);
    await back(p);
    check(!(await shown(p, ".guide-modal")) && (await state(p))?.view === "section", "back closes a guide opened from a section, and only it");
    await p.eval(`(() => { const b = [...document.querySelectorAll('.settings-body .link-btn')].find((b) => /Security guide/.test(b.textContent)); b.click(); })()`);
    await p.waitFor(`document.querySelector('.guide-modal')`, 6000);
    await p.key("Escape", "Escape", 0, 27);
    await sleep(400);
    check(!(await shown(p, ".guide-modal")) && (await state(p))?.view === "section", "so does Escape");
    await back(p);
    check(!(await state(p)), "back from a linked section leaves Settings");
    // the back arrow on a linked section goes to the list, from where back works by levels
    await ui(p, `u.openSettings('machines')`);
    await p.waitFor(`document.querySelector('.settings-layout[data-view="section"]')`, 6000);
    await sleep(300);
    s = await state(p);
    check(s.title === "Devices", "an older link (machines) lands on Devices", s);
    await p.click(".settings-back");
    await sleep(400);
    check((await state(p)).view === "list", "the back arrow shows the list");
    await tapRow(p, "memory");
    await back(p);
    check((await state(p))?.view === "list", "then a section, and back: the list");
    await back(p);
    check(!(await state(p)), "and back again: out");
    // opened from a link while the list shows: the section, at that card
    await ui(p, `u.openSettings()`);
    await sleep(400);
    await ui(p, `u.setSettingsTab('voice')`);
    await sleep(1200);
    s = await state(p);
    const voiceTop = await p.eval(`(() => { const v = document.getElementById('set-voice'); const b = document.querySelector('.settings-body'); return v ? Math.round(v.getBoundingClientRect().top - b.getBoundingClientRect().top) : null; })()`);
    check(s.view === "section" && s.title === "Web & voice" && voiceTop !== null && voiceTop < 120, "an older link (voice) opens Web & voice at the voice card", { s, voiceTop });
    await back(p);
    check((await state(p))?.view === "list", "…and back goes to the list");
    // unfolding keeps the section: side list beside it; folding again: the section, back → list
    await tapRow(p, "connection");
    await p.viewport(673, 841, true, 2.625);
    await sleep(500);
    s = await state(p);
    check(s.view === null && s.list && s.section && s.active === "connection" && !s.back, "unfolded: the side list, the same section open", s);
    await p.viewport(344, 882, true, 2.625);
    await sleep(500);
    s = await state(p);
    check(s.view === "section" && s.title === "Connection", "folded again: still in that section", s);
    await back(p);
    check((await state(p))?.view === "list", "…and back goes to the list");
    await back(p);
    check(!(await state(p)), "everything closed");
    // names whose home is outside Settings now
    await ui(p, `u.openSettings('archived')`);
    await p.waitFor(`document.querySelector('.archived-view')`, 6000).catch(() => {});
    check((await shown(p, ".archived-view")) && !(await state(p)), "openSettings('archived') opens the archived chats' list (not Settings)");
    await back(p);
    await ui(p, `u.openSettings('bots')`);
    await sleep(700);
    check(!(await state(p)) && (await p.eval(`!!document.querySelector('.drawer-layer.open') && document.querySelector('.drawer .sb-tab[aria-selected="true"]')?.id.endsWith('bots')`)), "openSettings('bots') opens the side menu at Bots");
    await back(p);
    check(p.errors.length === 0, `cover (${theme}): no errors`, p.errors);
    await p.close();
  }

  // ── a normal phone ──
  {
    console.log("— phone");
    const p = await device();
    await p.viewport(412, 915, true, 2.625);
    await signIn(p, "Settings phone");
    await ui(p, `u.openSettings()`);
    await p.waitFor(`document.querySelector('.settings-layout[data-view="list"]')`, 6000);
    await sleep(400);
    await p.shot(`${SHOTS}/settings-nav-phone-list.png`);
    await tapRow(p, "models");
    await sleep(600);
    check((await state(p)).title === "Models" && (await noSideScroll(p, ".settings-body")), "Models opens and fits");
    await p.shot(`${SHOTS}/settings-nav-phone-models.png`);
    for (const id of [...SECTIONS, "secrets", "commands", "activity", "help"]) {
      await ui(p, `u.setSettingsTab('${id}')`);
      await sleep(["usage", "secrets", "devices", "safety", "commands"].includes(id) ? 1200 : 450);
      const ok = (await state(p))?.view === "section" && (await noSideScroll(p, ".settings-body"));
      if (!ok) await p.shot(`${SHOTS}/settings-nav-phone-overflow-${id}.png`);
      check(ok, `phone: ${id} shows full screen and fits the width`);
    }
    check(p.errors.length === 0, "phone: no errors", p.errors);
    await p.close();
  }

  // ── the unfolded Fold: the side list stays ──
  for (const theme of ["dark", "light"]) {
    console.log(`— Fold open (${theme})`);
    const p = await device();
    await p.viewport(673, 841, true, 2.625);
    await signIn(p, `Settings fold ${theme}`);
    await p.theme(theme);
    await ui(p, `u.openSettings()`);
    await p.waitFor(`document.querySelector('.settings-layout')`, 6000);
    await sleep(500);
    const s = await state(p);
    const nav = await p.eval(`document.querySelector('.settings-nav').getBoundingClientRect().width`);
    check(s.view === null && s.list && s.section && s.active === "appearance" && nav >= 180 && nav <= 230, "the side list beside Appearance", { s, nav });
    check(!(await p.eval(`document.activeElement?.matches('input')`)), "no keyboard pops up on opening");
    await p.shot(`${SHOTS}/settings-nav-fold-${theme}.png`);
    await tapRow(p, "devices");
    await sleep(600);
    check((await state(p)).active === "devices" && (await noSideScroll(p, ".settings-body")), "a tap in the side list switches the section");
    await back(p);
    check(!(await state(p)), "back closes Settings");
    check(p.errors.length === 0, `Fold (${theme}): no errors`, p.errors);
    await p.close();
  }

  // ── a computer ──
  for (const theme of ["dark", "light"]) {
    console.log(`— desktop (${theme})`);
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, `Settings desk ${theme}`);
    await p.theme(theme);
    await ui(p, `u.openSettings()`);
    await p.waitFor(`document.querySelector('.settings-layout')`, 6000);
    await sleep(500);
    let s = await state(p);
    const labels = await p.eval(`[...document.querySelectorAll('.settings-tab')].map((t) => t.textContent.trim())`);
    check(s.view === null && s.list && s.section && s.active === "appearance" && !s.back, "the side list with Appearance open", s);
    check(JSON.stringify(labels) === JSON.stringify(["Appearance", "Notifications", "Models", "Usage", "Privacy & safety", "Memory & skills", "Web & voice", "Devices", "Connection", "Help & tour"]), "nine sections and Help & tour, just their names", labels);
    const foot = await p.eval(`(() => { const h = document.querySelector('.settings-tab[data-tab="help"]').getBoundingClientRect(); const n = document.querySelector('.settings-nav').getBoundingClientRect(); return { gap: Math.round(n.bottom - h.bottom) }; })()`);
    check(foot.gap < 40, "Help & tour sits at the foot of the list", foot);
    check(await p.eval(`document.activeElement?.matches('.settings-search input')`), "the search box has the keyboard");
    await p.shot(`${SHOTS}/settings-nav-desk-${theme}.png`);
    await p.type("passw");
    await sleep(300);
    const f = await found(p);
    check(f.includes("secrets") && f.includes("safety"), "typing filters the list (a section and its page)", f);
    await p.key("Enter", "Enter", 0, 13);
    await sleep(600);
    check((await state(p)).active === f[0], "Enter opens the first", { f, s: await state(p) });
    await p.click(".settings-search input");
    await p.key("Escape", "Escape", 0, 27);
    await sleep(300);
    check((await p.eval(`document.querySelector('.settings-search input').value`)) === "" && !!(await state(p)), "Escape clears the search first");
    // what lives in the sidebar now is still found, and a pick goes there
    await p.type("scheduled");
    await sleep(300);
    const away = await p.eval(`(() => { const e = document.querySelector('.settings-tab[data-tab="jobs"]'); return e ? { hint: e.querySelector('.settings-tab-hint')?.textContent ?? '', role: e.getAttribute('role') } : null; })()`);
    check(away?.hint === "In the sidebar" && away.role === null, "searching “scheduled” finds Scheduled tasks, in the sidebar now", away);
    await p.shot(`${SHOTS}/settings-nav-elsewhere-${theme}.png`);
    await p.click('.settings-tab[data-tab="jobs"]');
    await sleep(600);
    check(!(await state(p)) && (await p.eval(`document.querySelector('.sidebar:not(.drawer) .sb-tab[aria-selected="true"]')?.id.endsWith('jobs')`)), "…a click closes Settings at the sidebar's Scheduled tab");
    await ui(p, `u.openSettings()`);
    await p.waitFor(`document.querySelector('.settings-layout')`, 6000);
    await sleep(400);
    // ↑/↓ in the side list
    await p.click('.settings-tab[data-tab="safety"]');
    await p.key("ArrowDown", "ArrowDown", 0, 40);
    await sleep(300);
    s = await state(p);
    check(s.active === "memory" && (await p.eval(`document.activeElement?.dataset.tab`)) === "memory", "↓ moves to the next section", s);
    await p.key("ArrowUp", "ArrowUp", 0, 38);
    await sleep(300);
    check((await state(p)).active === "safety", "↑ goes back");
    // a page: its section stays marked in the list; Escape goes up to the section, then closes
    await ui(p, `u.setSettingsTab('secrets')`);
    await sleep(900);
    s = await state(p);
    check(s.title === "Passwords & keys" && s.active === "safety" && s.back, "a page keeps its section marked, with a way back to it", s);
    await p.key("Escape", "Escape", 0, 27);
    await sleep(400);
    check((await state(p))?.title === "Privacy & safety", "Escape on a page goes up to its section");
    // a guide over Settings: Escape closes the guide only
    await p.waitFor(`document.querySelector('.sec .sec-glance')`, 8000);
    await p.eval(`(() => { const b = [...document.querySelectorAll('.settings-body .link-btn')].find((b) => /Security guide/.test(b.textContent)); b.scrollIntoView({ block: 'center' }); b.click(); })()`);
    await p.waitFor(`document.querySelector('.guide-modal')`, 6000);
    await p.key("Escape", "Escape", 0, 27);
    await sleep(400);
    check(!(await shown(p, ".guide-modal")) && (await state(p))?.active === "safety", "Escape over a guide closes the guide, not Settings");
    await p.key("Escape", "Escape", 0, 27);
    await sleep(400);
    check(!(await state(p)), "Escape again closes Settings");
    // older names
    await ui(p, `u.openSettings('thiscomputer')`);
    await sleep(500);
    check((await state(p))?.active === "devices", "openSettings('thiscomputer') lands on Devices");
    await ui(p, `u.setSettingsTab('terminal')`);
    await sleep(1200);
    s = await state(p);
    check(s?.title === "What commands can reach" && s.active === "safety" && (await shown(p, "#set-terminal")), "…and 'terminal' on What commands can reach, at the terminal", s);
    await ui(p, `u.setSettingsTab(null)`);
    await ui(p, `u.openSettings('jobs')`);
    await sleep(600);
    check(!(await state(p)) && (await p.eval(`document.querySelector('.sidebar:not(.drawer) .sb-tab[aria-selected="true"]')?.id.endsWith('jobs')`)), "openSettings('jobs') shows the sidebar's Scheduled tab");
    check(p.errors.length === 0, `desktop (${theme}): no errors`, p.errors);
    await p.close();
  }
  finish();
});
