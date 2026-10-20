// MACHINES-APP: Settings → Devices, one list of everything paired with the
// server, with machines marked on their entries (what each allows as a
// preset, who can use it, recent activity, pause/revoke and nothing that
// widens), "Add…" to pair a device or add a machine (the command to run on
// the other computer, then it shows up live), and in the desktop app This
// computer pinned first with its local controls (a stand-in shell here):
// one click on, presets, folders, "Ask first", who can use it, the desktop
// control warning, putting startup files back. Machine calls in chats name
// the machine. Desktop and phone widths, dark and light.
import { APP, MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";
import { shellScript } from "./machines-shell.mjs";
const REAL = !!process.env.REAL; // a real throwaway server: no mock-only controls

// A refresh must update the panel in place, not remove it and put a new one back.
async function keepPlace(p, control, change, label) {
  const before = await p.eval(`(() => {
    const e = ${control}, b = document.querySelector('.settings-body');
    b.scrollTop += e.getBoundingClientRect().top - b.getBoundingClientRect().top - b.clientHeight / 2;
    e.focus({ preventScroll: true });
    window.__place = { e, b, panel: e.closest('.dev-item'), top: b.scrollTop };
    return { top: b.scrollTop, focused: document.activeElement === e };
  })()`);
  check(before.top > 40 && before.focused, `${label}: starts scrolled down with the control focused`, before);
  await change();
  await sleep(1000);
  const after = await p.eval(`(() => {
    const { e, b, panel, top } = window.__place;
    return { samePanel: panel.isConnected, sameControl: e.isConnected, focused: document.activeElement === e, top: b.scrollTop, delta: b.scrollTop - top };
  })()`);
  check(after.samePanel && after.sameControl, `${label}: the same panel and control stay mounted`, after);
  check(Math.abs(after.delta) <= 2, `${label}: the scroll position stays put`, after);
  check(after.focused, `${label}: the control keeps focus`, after);
}

run(async () => {
  if (!REAL) await mock("/__mock/reset");
  const ws = await wsClient("machines script");
  if (REAL) await ws.rpc("complete_setup");
  const machines = async () => (await ws.rpc("list_machines")).result;

  const helpers = (p) => {
    const open = async (tab) => {
      await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings(${JSON.stringify(tab)}))`);
      await sleep(700);
    };
    const dialog = () => p.eval(`document.querySelector('.dialog-body') ? { title: [...document.querySelectorAll('.modal h2')].pop()?.textContent ?? '', body: document.querySelector('.dialog-text')?.textContent ?? '', danger: !!document.querySelector('.dialog-actions .btn-danger') } : null`);
    const answer = async (ok) => {
      await p.click(ok ? ".dialog-actions button[type=submit]" : ".dialog-actions .btn-ghost");
      await sleep(500);
    };
    const tabs = () => p.eval(`[...document.querySelectorAll('.settings-tab')].map(t => t.textContent.trim())`);
    const items = () =>
      p.eval(`[...document.querySelectorAll('.dev-item')].map(li => ({ name: li.querySelector('.dev-name').textContent, meta: li.querySelector('.dev-meta').textContent, state: li.dataset.state, badge: li.querySelector('.dev-mbadge')?.textContent.trim() ?? null, here: li.classList.contains('here'), online: !!li.querySelector('.dev-dot.on'), open: li.classList.contains('open') }))`);
    const entry = (name) => `[...document.querySelectorAll('.dev-item')].find(li => li.querySelector('.dev-name').textContent.startsWith(${JSON.stringify(name)}))`;
    const toggle = async (name) => {
      await p.eval(`${entry(name)}.querySelector('.dev-row').click()`);
      await sleep(400);
    };
    const noSideScroll = () => p.eval(`(() => { const b = document.querySelector('.settings-body') || document.scrollingElement; return [...document.querySelectorAll('.settings-body *')].every(e => e.getBoundingClientRect().right <= innerWidth + 1) && b.scrollWidth <= b.clientWidth + 1; })()`);
    return { open, dialog, answer, tabs, items, entry, toggle, noSideScroll };
  };

  // ── Settings → Devices in the web UI (desktop width) ──
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Machines desktop");
  if (REAL) {
    await p.eval(`__mod('lib/ui.ts').then(m => m.setSetupOpen(false))`);
    await sleep(400);
  }
  const h = helpers(p);
  await h.open("machines"); // an old link: lands in Devices
  await p.waitFor(`document.querySelector('.dev-list')`, 8000);
  const nav = await h.tabs();
  check(nav.includes("Devices") && !nav.includes("Machines") && !nav.includes("This computer"), "one section, Devices, instead of Devices, Machines and This computer", nav);
  check(/(computers)/.test(await p.eval(`document.querySelector('.settings-blurb').textContent`)), "its blurb says computers live here too");

  if (!REAL) {
    let list = await h.items();
    const by = (n) => list.find((x) => x.name.startsWith(n));
    check(list[0]?.here && list[0].name === "Machines desktop" && /^This device · Browser/.test(list[0].meta), "this device is pinned first, marked “This device”, saying what it is", list[0]);
    check(by("Desktop")?.badge === "Machine · Work in folders" && by("Desktop").online && /Machine for agents · Linux/.test(by("Desktop").meta), "Desktop: a device that is also a machine, its preset on the entry", by("Desktop"));
    check(by("MacBook")?.badge === "Machine · Paused" && by("MacBook").state === "paused", "the MacBook: a paused machine says so", by("MacBook"));
    check(by("nas")?.badge === "Machine · Advanced" && !by("nas").online && /seen 2\s?h/.test(by("nas").meta), "nas: a machine with no device entry is still listed, offline, with when it was seen", by("nas"));
    check(by("Pixel Fold") && !by("Pixel Fold").badge && /Phone/.test(by("Pixel Fold").meta), "plain devices are listed with their kind", by("Pixel Fold"));
    check(!list.some((x) => x.open), "entries start closed (compact)");
    await p.shot(`${SHOTS}/devices-desk-dark.png`);

    // A machine's entry: what it allows, who can use it, a way to what it did.
    await h.toggle("Desktop");
    const hg = await p.eval(`(() => { const li = ${h.entry("Desktop")}; return { preset: li.querySelector('.dev-preset')?.textContent, scope: [...li.querySelectorAll('.mach-scope li')].map(x => x.textContent), fine: li.querySelector('.dev-machine .mach-fine')?.textContent, buttons: [...li.querySelectorAll('.dev-details button')].map(b => b.textContent.trim()) }; })()`);
    check(/Work in folders\./.test(hg.preset) && /read and change the folders you choose/.test(hg.preset) && /Commands can also read other files your account can read/.test(hg.preset) && /change only the chosen folders/.test(hg.preset), "the preset explains the write boundary and wider command reads", hg.preset);
    check(hg.scope.includes("Read & change ~/proj") && hg.scope.some(x => /Commands can also read other files your account can read/.test(x)) && hg.scope.includes("Asks first: every action") && hg.scope.includes("Who can use it: Any chat"), "what it allows and who can use it, in plain words", hg.scope);
    check(hg.scope.includes("Web views can run here (they reach its local network)"), "…and that web views can run there", hg.scope);
    if (!REAL) {
      // Administrator on there: other devices see it plainly (and can pause or stop it, never widen it).
      const all = (await ws.rpc("list_machines")).result;
      await ws.rpc("mock_machines", { list: all.map((m) => (m.name === "Desktop" ? { ...m, scope: { ...m.scope, admin: "ask" } } : m)) });
      await sleep(400);
      const withAdmin = await p.eval(`[...${h.entry("Desktop")}.querySelectorAll('.mach-scope li')].map(x => x.textContent)`);
      check(withAdmin.includes("Administrator commands (root): each one asks first"), "a machine with Administrator on says so on its entry", withAdmin);
      // Stopped on that computer by its user (its Stop bar or key): said plainly; only that computer resumes it.
      await ws.rpc("mock_machines", { list: all.map((m) => (m.name === "Desktop" ? { ...m, scope: { ...m.scope, stopped: true, stops: 1 } } : m)) });
      await sleep(400);
      const stopped = await p.eval(`(() => { const li = ${h.entry("Desktop")}; return { badge: li.querySelector('.dev-mbadge')?.textContent.trim(), note: [...li.querySelectorAll('.mach-note')].map(n => n.textContent).join(' ') }; })()`);
      check(stopped.badge === "Machine · Stopped there" && /Stopped on Desktop/.test(stopped.note) && /resumes it on that computer/.test(stopped.note), "a machine its user stopped says so, and that only that computer resumes it", stopped);
      await p.shot(`${SHOTS}/devices-machine-stopped-desk-dark.png`);
      await ws.rpc("mock_machines", { list: all });
      await sleep(300);
    }
    check(/Change what it allows, and its name, on that computer/.test(hg.fine), "it says loosening (and renaming it) happens on that computer", hg.fine);
    check(hg.buttons.includes("Activity") && hg.buttons.includes("Pause") && hg.buttons.includes("Stop agents using it") && hg.buttons.includes("Sign out"), "activity, pause, stop agents using it, sign out", hg.buttons);
    const widen = await p.eval(`(() => { const t = ${h.entry("Desktop")}.querySelector('.dev-details'); return { switches: t.querySelectorAll('[role=switch], input[type=checkbox]').length, radios: t.querySelectorAll('[role=radio]').length, add: [...t.querySelectorAll('button')].filter(b => /add|enable|turn on|allow|full|desktop/i.test(b.textContent)).map(b => b.textContent) }; })()`);
    check(widen.switches === 0 && widen.radios === 0 && widen.add.length === 0, "nothing in another computer's entry can turn it on or widen it", widen);
    await p.shot(`${SHOTS}/devices-machine-desk-dark.png`);
    await p.theme("light");
    await sleep(300);
    await p.shot(`${SHOTS}/devices-machine-desk-light.png`);
    await p.theme("dark");

    // What it did: Settings → Activity, on that machine.
    await p.eval(`[...${h.entry("Desktop")}.querySelectorAll('.dev-details button')].find(b => b.textContent.trim() === 'Activity').click()`);
    await p.waitFor(`document.querySelector('.act-row')`, 4000).catch(() => {});
    await sleep(300);
    const act = await p.eval(`({ tab: document.querySelector('.settings-tab[aria-current], .settings-tab.active, .settings-tab[aria-selected=true]')?.textContent.trim() ?? null, rows: [...document.querySelectorAll('.act-row')].map(x => x.textContent), on: document.querySelector('.act-chip.on')?.textContent ?? null })`);
    check(act.rows.length === 3 && /Ran cargo build --release/.test(act.rows[0]) && /Done/.test(act.rows[0]) && /Build the site/.test(act.rows[0]) && /Refused/.test(act.rows[2]), "Activity: what ran, where it came from, how it ended", act);
    check(await h.noSideScroll(), "…nothing runs off the side");
    await p.shot(`${SHOTS}/activity-desk-dark.png`);
    await h.open("devices");

    // who can use it, for a machine only for a bot
    await h.toggle("nas");
    const nas = await p.eval(`[...${h.entry("nas")}.querySelectorAll('.mach-scope li')].map(x => x.textContent)`);
    check(nas.includes("Who can use it: Only Forge") && nas.includes("Asks first: only risky ones") && nas.includes("Commands on, but they can't run there (no sandbox)"), "nas: only Forge may use it; asks before risky actions", nas);
    check(!(await p.eval(`!!${h.entry("nas")}.querySelector('.dev-device')`)), "…and it has no device controls (it isn't paired as one)");

    // pause / resume
    await h.toggle("Desktop");
    await p.eval(`${h.entry("Desktop")}.querySelector('.mach-actions .btn:not(.btn-ghost):not([aria-label^=\"What agents did\"])').click()`);
    await sleep(600);
    await keepPlace(p, `${h.entry("Desktop")}.querySelector('[aria-label="Resume Desktop"]')`, async () => {
      await ws.rpc("mock_machines", { online: { id: "dev_desktop", online: true } });
    }, "Desktop server refresh");
    await sleep(600);
    check((await machines()).find((m) => m.id === "dev_desktop")?.paused === true, "Pause: the server has Desktop paused");
    list = await h.items();
    check(by("Desktop") && list.find((x) => x.name.startsWith("Desktop")).badge === "Machine · Paused", "…and its entry reads Machine · Paused", list.find((x) => x.name.startsWith("Desktop")));
    check(/never gives more access/.test(await p.eval(`${h.entry("Desktop")}.querySelector('.mach-note')?.textContent ?? ''`)), "a paused machine says resuming never widens it");
    const scopeBefore = JSON.stringify((await machines()).find((m) => m.id === "dev_desktop").scope);
    await p.eval(`${h.entry("Desktop")}.querySelector('.mach-actions .btn:not(.btn-ghost):not([aria-label^=\"What agents did\"])').click()`);
    await sleep(600);
    const hgNow = (await machines()).find((m) => m.id === "dev_desktop");
    check(hgNow.paused === false && JSON.stringify(hgNow.scope) === scopeBefore, "Resume: running again, with exactly what it allowed before");

    // revoke asks first
    await h.toggle("nas");
    await p.eval(`${h.entry("nas")}.querySelector('.mach-actions .btn-ghost').click()`);
    await sleep(400);
    let d = await h.dialog();
    check(!!d && d.title === "Stop agents using nas?" && d.danger && /turn it on again on that computer/.test(d.body), "Stopping agents asks first, plainly", d);
    await p.shot(`${SHOTS}/devices-revoke-confirm.png`);
    await h.answer(false);
    check((await machines()).some((m) => m.id === "dev_nas"), "Cancel: nas stays");
    await p.eval(`${h.entry("nas")}.querySelector('.mach-actions .btn-ghost').click()`);
    await sleep(400);
    await h.answer(true);
    check(!(await machines()).some((m) => m.id === "dev_nas") && !(await h.items()).some((m) => m.name.startsWith("nas")), "confirmed: nas is gone, here and on the server");

    // Add… : two choices in a small menu
    await p.click(".dev-add-btn");
    await sleep(300);
    const menu = await p.eval(`[...document.querySelectorAll('.menu-item')].map(b => b.querySelector('.menu-label').textContent)`);
    check(JSON.stringify(menu) === JSON.stringify(["Pair a device", "Add a machine"]), "Add… offers the two choices, in one small menu", menu);
    check(!(await p.eval(`!!document.querySelector('.popover.sheet, .sheet')`)), "…not a bottom sheet");
    await p.clickText(".menu-item", "Add a machine");
    await p.waitFor(`document.querySelector('.dev-enable-cmd code')`, 6000);
    const add = await p.eval(`({ cmd: document.querySelector('.dev-enable-cmd code').textContent, install: document.querySelector('.dev-steps .mach-cmd code').textContent, os: [...document.querySelectorAll('.dev-addm .segmented button')].map(b => b.textContent), local: !!document.querySelector('.dev-addm-local'), wait: document.querySelector('.dev-wait')?.textContent ?? '' })`);
    check(add.cmd === "tessera node enable 'tessera://pair…'", "the command to run on the other computer (its one-time code kept off the screen)", add.cmd);
    check(/install\.sh/.test(add.install) && add.os.join() === "Linux,macOS,Windows", "how to install Tessera there, and which system it's for", add);
    // Windows: the PowerShell line, and what a Windows machine can't do yet.
    await p.clickText(".dev-addm .segmented button", "Windows");
    await sleep(200);
    const win = await p.eval(`({ install: document.querySelector('.dev-steps .mach-cmd code').textContent, step: document.querySelector('.dev-steps li span').textContent, note: [...document.querySelectorAll('.dev-steps .muted.small')].map(e => e.textContent).join(' ') })`);
    check(/^irm .*\/install\.ps1 \| iex$/.test(win.install) && /Open PowerShell/.test(win.step) && /without a sandbox/.test(win.note), "Windows: the PowerShell install line, and that commands there are unsandboxed", win);
    await p.shot(`${SHOTS}/devices-add-machine-windows.png`);
    await p.clickText(".dev-addm .segmented button", "Linux");
    await sleep(200);
    check(!add.local, "a browser can't offer This computer");
    check(/Waiting for it to connect/.test(add.wait), "it waits for the computer to show up", add.wait);
    await p.eval(`window.__copied = null; navigator.clipboard.writeText = async (t) => { window.__copied = t; }`);
    await p.click(".dev-enable-cmd .icon-btn");
    await sleep(300);
    const copied = await p.eval(`window.__copied`);
    check(/^tessera node enable 'tessera:\/\/pair\?/.test(copied ?? ""), "Copy gives the whole command, link and all", copied?.slice(0, 40));
    await p.shot(`${SHOTS}/devices-add-machine-desk-dark.png`);
    await ws.rpc("mock_machines", { join: { name: "Studio PC", preset: "work_in_folders" } });
    await p.waitFor(`document.querySelector('.dev-joined')`, 6000);
    const joined = await p.eval(`document.querySelector('.dev-joined').textContent`);
    check(/Studio PC is connected as a machine \(Work in folders\)/.test(joined), "the new machine shows up by itself", joined);
    check((await h.items()).some((x) => x.name.startsWith("Studio PC") && x.badge === "Machine · Work in folders"), "…and in the list, as a machine");
    await p.shot(`${SHOTS}/devices-add-machine-joined.png`);
    await p.click(".dev-add .dev-add-head .icon-btn");
    await sleep(300);
    // Pair a device: the code, in the same place.
    await p.click(".dev-add-btn");
    await sleep(300);
    await p.clickText(".menu-item", "Pair a device");
    await p.waitFor(`document.querySelector('.dev-add .code-big')`, 6000);
    check(/\d{4} \d{4}/.test(await p.eval(`document.querySelector('.dev-add .code-big').textContent`)), "Pair a device: a one-time code right there");
    await p.click(".dev-add .dev-add-head .icon-btn");
    await sleep(300);

    // another device's change shows up live
    await ws.rpc("mock_machines", { online: { id: "dev_desktop", online: false } });
    await sleep(800);
    check((await h.items()).find((m) => m.name.startsWith("Desktop"))?.online === false, "a machine going offline shows up without reloading");
    await ws.rpc("mock_machines", { online: { id: "dev_desktop", online: true } });
    await sleep(500);
    await p.theme("light");
    await sleep(300);
    await p.shot(`${SHOTS}/devices-desk-light.png`);
    await p.theme("dark");
  } else {
    check(!!(await p.eval(`document.querySelector('.dev-list .dev-item')`)), "the list shows (at least this device)");
    await p.shot(`${SHOTS}/devices-real-desk.png`);
  }
  console.log("desktop errors:", p.errors);
  check(!p.errors.length, "no page errors (desktop)");

  // ── machine calls in a chat: the approval card and the tool card name the machine ──
  if (!REAL) {
    const sid = (await ws.rpc("create_session", { title: "Build on Desktop" })).result.id;
    await p.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab(null))`);
    await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession(${JSON.stringify(sid)}))`);
    await sleep(800);
    await ws.rpc("send", { session_id: sid, content: [{ type: "text", text: "build it on Desktop" }] });
    await p.waitFor(`document.querySelector('.approval')`, 15000);
    await sleep(300);
    const card = await p.eval(`(() => { const a = document.querySelector('.approval'); return { label: a.getAttribute('aria-label'), chip: a.querySelector('.approval-machine')?.textContent.trim(), summary: a.querySelector('.approval-summary').textContent }; })()`);
    check(card.chip === "On Desktop" && card.label === "Approval needed on Desktop", "the approval card names the machine first", card);
    check(card.summary === "On Desktop: run cargo build in ~/proj", "…and says what it would do there", card);
    check(
      !(await p.eval(`[...document.querySelectorAll('.approval-actions button')].some(b => /Allow for this session/.test(b.textContent))`)),
      "…and, since Desktop asks before every action, no “Allow for this session”: it asks each time",
    );
    await p.clickText(".approval-actions .btn-primary", "Allow");
    await p.waitFor(`[...document.querySelectorAll('.tool')].pop()?.dataset.status === 'ok'`, 15000);
    const done = await p.eval(`[...document.querySelectorAll('.tool')].pop().querySelector('.tool-name').textContent.trim()`);
    check(done === "Desktop", "after it runs, the tool card still names Desktop", done);
    await p.shot(`${SHOTS}/machines-toolcard-desk-dark.png`);

    // ── as administrator (batch 17): off there, the agent says how to allow it, never "may I?" ──
    const lastText = () => p.eval(`[...document.querySelectorAll('.pane.focused .msg-assistant')].pop()?.textContent ?? ''`);
    const idle = async (id) => {
      for (let i = 0; i < 100; i++) {
        const s = (await ws.rpc("get_session", { session_id: id })).result;
        if (s.info.status === "idle") return s;
        await sleep(150);
      }
      return null;
    };
    await idle(sid);
    await ws.rpc("send", { session_id: sid, content: [{ type: "text", text: "restart bluetooth as administrator on Desktop" }] });
    await idle(sid);
    await sleep(500);
    const offText = await lastText();
    check(/Administrator commands are off on Desktop/.test(offText) && /sudo systemctl restart bluetooth/.test(offText) && !/may i|can i|shall i/i.test(offText), "administrator off there: it says how to allow it or which command to run, and doesn't ask for permission in prose", offText.slice(0, 200));
    const all = (await ws.rpc("list_machines")).result;
    await ws.rpc("mock_machines", { list: all.map((m) => (m.name === "Desktop" ? { ...m, scope: { ...m.scope, admin: "ask" } } : m)) });
    await sleep(300);
    await ws.rpc("send", { session_id: sid, content: [{ type: "text", text: "restart bluetooth as administrator on Desktop" }] });
    await p.waitFor(`document.querySelector('.approval')`, 15000);
    await sleep(300);
    const adminCard = await p.eval(`(() => { const a = document.querySelector('.approval'); return { summary: a.querySelector('.approval-summary').textContent, text: a.textContent }; })()`);
    check(/^On Desktop: run as administrator sudo systemctl restart bluetooth/.test(adminCard.summary) && /root, with sudo/.test(adminCard.text), "on there: the approval card says it runs as administrator, with the exact command", adminCard);
    await p.shot(`${SHOTS}/machines-admin-approval-desk-dark.png`);
    await p.clickText(".approval-actions .btn-primary", "Allow");
    await idle(sid);
    await sleep(400);
    check(/restarted/.test(await lastText()), "…and once allowed it runs", (await lastText()).slice(0, 120));
    await ws.rpc("mock_machines", { list: all });
    console.log("chat errors:", p.errors);
    check(!p.errors.length, "no page errors (chat)");
  }
  await p.close();

  // ── phone (web): the same list, readable at 375 px ──
  for (const theme of ["dark", "light"]) {
    const ph = await device();
    await ph.viewport(375, 812, true, 2);
    await signIn(ph, `Machines phone ${theme}`);
    if (REAL) {
      await ph.eval(`__mod('lib/ui.ts').then(m => m.setSetupOpen(false))`);
      await sleep(400);
    }
    await ph.theme(theme);
    const hp = helpers(ph);
    await hp.open("devices");
    await ph.waitFor(`document.querySelector('.dev-list .dev-item')`, 8000);
    await sleep(300);
    check(await hp.noSideScroll(), `phone (${theme}): nothing runs off the side`);
    if (!REAL) {
      const names = await ph.eval(`[...document.querySelectorAll('.dev-name')].map(n => ({ t: n.textContent, full: n.scrollWidth <= n.clientWidth + 1 && n.clientWidth > 20 }))`);
      check(names.length >= 4 && names.every((n) => n.full), `phone (${theme}): names show in full`, names);
      const badge = await ph.eval(`(() => { const b = ${hp.entry("Desktop")}.querySelector('.dev-mbadge').getBoundingClientRect(); return { right: b.right, w: b.width }; })()`);
      check(badge.right <= 375 && badge.w > 60, `phone (${theme}): the machine badge fits`, badge);
      await ph.shot(`${SHOTS}/devices-phone-${theme}.png`);
      await hp.toggle("Desktop");
      await sleep(200);
      check(await hp.noSideScroll(), `phone (${theme}): an open machine entry fits`);
      const btns = await ph.eval(`[...${hp.entry("Desktop")}.querySelectorAll('.dev-details button')].map(b => { const r = b.getBoundingClientRect(); return { t: b.textContent.trim(), h: r.height, right: r.right }; })`);
      check(btns.length >= 3 && btns.every((b) => b.h >= 28 && b.right <= 375), `phone (${theme}): its buttons fit and are tappable`, btns);
      await ph.shot(`${SHOTS}/devices-machine-phone-${theme}.png`);
      await ph.click(".dev-add-btn");
      await sleep(300);
      check(!(await ph.eval(`!!document.querySelector('.sheet')`)) && (await ph.eval(`document.querySelectorAll('.menu-item').length`)) === 2, `phone (${theme}): Add… is a small menu, not a bottom sheet`);
      await ph.clickText(".menu-item", "Add a machine");
      await ph.waitFor(`document.querySelector('.dev-enable-cmd')`, 6000);
      await sleep(200);
      check(await hp.noSideScroll(), `phone (${theme}): Add a machine fits`);
      await ph.shot(`${SHOTS}/devices-add-machine-phone-${theme}.png`);
    }
    console.log(`phone ${theme} errors:`, ph.errors);
    check(!ph.errors.length, `no page errors (phone ${theme})`);
    await ph.close();
  }

  // ── the desktop app: This computer, pinned first (a stand-in shell) ──
  const pr = await fetch(MOCK + "/api/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "123456", device_name: "Desktop app on devbox" }) });
  const { token } = await pr.json();
  if (!REAL) {
    const a = await device();
    await a.viewport(1280, 820);
    await a.send("Page.addScriptToEvaluateOnNewDocument", { source: shellScript(token) });
    await a.goto(APP, 1800);
    await a.waitFor(`document.querySelector('.workspace')`, 12000);
    const ha = helpers(a);
    const calls = (cmd) => a.eval(`window.__calls.filter(c => c.cmd === ${JSON.stringify(cmd)}).map(c => c.args)`);
    const node = () => a.eval(`JSON.parse(JSON.stringify(window.__node))`);
    const presetNow = () => a.eval(`document.querySelector('.mach-preset[aria-checked=true] .mach-preset-name')?.textContent`);
    const pick = async (label) => {
      await a.clickText(".mach-preset", label);
      await sleep(400);
    };
    await ha.open("thiscomputer"); // an old link: Devices, This computer open
    await a.waitFor(`document.querySelector('.dev-this')`, 8000);
    check(!(await ha.tabs()).includes("This computer"), "desktop app: no separate This computer section either");
    let list = await ha.items();
    check(list[0]?.name === "devbox" && /^This device · Tessera app/.test(list[0].meta) && list[0].open, "This computer is pinned first, “This device”, and opens from an old link", list[0]);
    check(!(await a.eval(`!!document.querySelector('.dev-this .dev-mbadge')`)), "…no machine badge while off");
    check((await presetNow()) === "Off", "its choice reads Off");
    const presets = await a.eval(`[...document.querySelectorAll('.mach-preset')].map(b => ({ name: b.querySelector('.mach-preset-name').textContent, desc: b.querySelector('.mach-preset-desc').textContent }))`);
    check(presets.map((x) => x.name).join() === "Off,Look only,Work in folders,Full access,Desktop control" && presets.every((x) => x.desc.length > 20), "five plain choices, each with one sentence", presets.map((x) => x.name));
    await a.shot(`${SHOTS}/thiscomputer-off-desk-dark.png`);
    await ha.toggle("devbox");
    // One click from Add a machine: This computer.
    await a.click(".dev-add-btn");
    await sleep(300);
    await a.clickText(".menu-item", "Add a machine");
    await a.waitFor(`document.querySelector('.dev-addm-local')`, 6000);
    check(/This computer/.test(await a.eval(`document.querySelector('.dev-addm-local').textContent`)), "the desktop app offers This computer first");
    await a.click(".dev-addm-local .btn-primary");
    await sleep(600);
    check(JSON.stringify(await calls("this_computer_enable")) === JSON.stringify([{ on: true }]) && (await node()).enabled, "one click turns this computer on (a local command)");
    check(!(await a.eval(`!!document.querySelector('.dev-add')`)) && (await ha.items())[0].open, "…the panel closes and This computer opens");
    let st = await a.eval(`({ tone: document.querySelector('.mach-status').dataset.tone, text: document.querySelector('.mach-status').textContent })`);
    check(st.tone === "ok" && /devbox/.test(st.text), "it reads On, naming this computer", st);
    check((await presetNow()) === "Look only" && /No folders yet/.test(await a.eval(`document.querySelector('.mach-folders').textContent`)), "it starts as Look only, with no folders yet (it allows nothing)");

    // Look only: add a folder from THIS computer's picker
    await a.click(".mach-folders .sec-add .btn");
    await a.waitFor(`document.querySelector('.dirb-list .dirb-item')`, 6000);
    const shown = await a.eval(`[...document.querySelectorAll('.dirb-item')].map(b => b.textContent.trim())`);
    check(shown.join(",") === "code,notes,.ssh" && (await calls("this_computer_list_dir")).length > 0, "the folder picker lists this computer's folders (not the server's)", shown);
    await a.clickText(".dirb-item", "code");
    await sleep(500);
    await a.clickText(".dirb-foot .btn-primary", "Use this folder");
    await sleep(400);
    let d = await ha.dialog();
    check(!!d && d.title === "Let agents read ~/code?" && !d.danger, "adding a folder asks first (reading only)", d);
    await ha.answer(true);
    let sc = (await node()).scope;
    check(sc.folders.length === 1 && sc.folders[0].path === "/home/me/code" && !sc.folders[0].write && !sc.commands, "added read-only", sc);

    // Work in folders: asks, then read & change + commands
    await pick("Work in folders");
    d = await ha.dialog();
    check(!!d && d.title === "Switch to Work in folders?" && /run commands there/.test(d.body), "a choice that allows more asks first, in plain words", d);
    await ha.answer(false);
    check((await node()).preset === "look_only", "Cancel: still Look only");
    await pick("Work in folders");
    await ha.answer(true);
    sc = (await node()).scope;
    check((await node()).preset === "work_in_folders" && sc.folders[0].write && sc.commands, "Work in folders: ~/code read & change, commands on", sc);
    const lastPreset = (await calls("this_computer_preset")).pop();
    check(lastPreset.preset === "work_in_folders" && lastPreset.folders === null, "…through the local preset command (the chosen folders kept)", lastPreset);

    // Web views here: an extra with Work in folders, asked first, plainly
    const wvRow = `[...document.querySelectorAll('.dev-this .setting-row')].find(r => r.querySelector('.setting-title')?.textContent === 'Web views here')`;
    check(/local network/.test(await a.eval(`${wvRow}.textContent`)) && (await a.eval(`${wvRow}.querySelector('[role=switch]').getAttribute('aria-checked')`)) === "false", "Work in folders offers “Web views here”, off, saying pages reach the local network");
    await a.eval(`${wvRow}.querySelector('[role=switch]').click()`);
    await sleep(400);
    d = await ha.dialog();
    check(!!d && /web views/i.test(d.title) && /localhost/.test(d.body) && d.danger, "turning web views on asks first (local network and localhost)", d);
    await ha.answer(true);
    check((await node()).scope.webviews === true && (await node()).preset === "work_in_folders", "web views on, still Work in folders");
    await a.eval(`${wvRow}.querySelector('[role=switch]').click()`);
    await sleep(400);
    check(!(await ha.dialog()) && !(await node()).scope.webviews, "off again at once");
    // Each control drawn once (batch 16): with Advanced open, one folder list and one "Web views here".
    await a.eval(`[...document.querySelectorAll('.dev-this .disclosure')].find(d => d.textContent.trim() === 'Advanced')?.click()`);
    await sleep(400);
    const once = await a.eval(`({ lists: document.querySelectorAll('.dev-this .sec-folders').length, wv: [...document.querySelectorAll('.dev-this .setting-row .setting-title')].filter(t => t.textContent === 'Web views here').length, simple: !!document.querySelector('.dev-this .mach-folders') })`);
    check(once.lists === 1 && once.wv === 1 && !once.simple, "with Advanced open: one folder list (Advanced's) and one “Web views here”", once);
    await a.shot(`${SHOTS}/machines-advanced-once.png`);
    await a.eval(`[...document.querySelectorAll('.dev-this .disclosure')].find(d => d.textContent.trim() === 'Advanced')?.click()`);
    await sleep(300);

    // Ask first
    check((await a.eval(`document.querySelector('.mach-ask [aria-checked=true]').textContent`)) === "Every action", "Ask first starts at Every action");
    await a.clickText(".mach-ask button", "Only risky ones");
    await sleep(400);
    d = await ha.dialog();
    check(!!d && /risky/.test(d.title) && /still ask/.test(d.body), "asking less asks first", d);
    await ha.answer(true);
    check((await node()).scope.ask === "risky", "Ask first: only risky ones");
    await a.clickText(".mach-ask button", "Every action");
    await sleep(400);
    check(!(await ha.dialog()) && (await node()).scope.ask === "always", "back to every action at once");

    // Who can use it (the shared picker)
    check(!!(await a.eval(`document.querySelector('.dev-this .access')`)), "Who can use it: the same picker as Passwords & keys");
    await a.clickText(".access-mode", "Only these bots");
    await sleep(300);
    check(!(await calls("this_computer_access")).length, "choosing “Only these bots” saves nothing until a bot is picked");
    await a.clickText(".access-bot", "Forge");
    await sleep(400);
    check(!(await ha.dialog()), "limiting it to a bot narrows: no question");
    const forge = (await calls("this_computer_access")).pop();
    check(forge?.bots?.length === 1 && (await node()).scope.bots?.length === 1, "only Forge may use it now", forge);
    await a.clickText(".access-mode", "Any chat");
    await sleep(400);
    d = await ha.dialog();
    check(!!d && /any chat/i.test(d.title), "letting any chat use it again asks first", d);
    await ha.answer(true);
    check((await node()).scope.bots === undefined, "any chat may use it again");

    // Terminal from my devices: the user's own, decided here, off at first
    const termChoice = () => a.eval(`document.querySelector('.mach-term [aria-checked=true]')?.textContent`);
    check((await termChoice()) === "Off" && /never agents/.test(await a.eval(`document.querySelector('[data-mach-terminal]').textContent`)), "“Terminal from my devices” starts off, saying agents never get one", await termChoice());
    const presetBefore = (await node()).preset;
    await a.clickText(".mach-term button", "Sandboxed");
    await sleep(400);
    d = await ha.dialog();
    check(!!d && /terminal/i.test(d.title) && /out of reach/.test(d.body), "turning terminals on here asks first, saying what it means", d);
    await ha.answer(true);
    check((await node()).scope.terminal === "sandboxed" && (await node()).preset === presetBefore, "terminals on (sandboxed), what agents may do unchanged", (await node()).scope.terminal);
    await a.clickText(".mach-term button", "Off");
    await sleep(400);
    check(!(await ha.dialog()) && ((await node()).scope.terminal ?? "off") === "off", "off again at once, without a question");

    // Full access
    await pick("Full access");
    d = await ha.dialog();
    check(!!d && d.title === "Switch to Full access?" && d.danger && /home folder/.test(d.body), "Full access asks first, as a risky choice", d);
    await ha.answer(true);
    sc = (await node()).scope;
    check((await node()).preset === "full_access" && sc.folders.length === 1 && sc.folders[0].path === "/home/me" && sc.folders[0].write && sc.webviews, "Full access: the home folder, read & change, web views included", sc);
    check(/startup files stay out of reach/.test(await a.eval(`document.querySelector('.mach-homeline').textContent`)), "…and it says startup files stay protected");

    // Administrator: root with sudo, outside the sandbox; set only here, off at first
    const adminChoice = () => a.eval(`document.querySelector('.mach-admin [aria-checked=true]')?.textContent`);
    const adminText = () => a.eval(`document.querySelector('[data-mach-admin]').textContent`);
    check((await adminChoice()) === "Off" && /sudo doesn't work/.test(await adminText()), "“Administrator” starts off, saying sudo doesn't work in the sandbox", await adminChoice());
    await a.clickText(".mach-admin button", "Ask each time");
    await sleep(400);
    d = await ha.dialog();
    check(!!d && /administrator/i.test(d.title) && /as root/.test(d.body) && /outside Tessera's sandbox/.test(d.body) && /exact command/.test(d.body) && d.danger, "turning it on warns plainly: root, outside the sandbox, each one asks with the exact command", d);
    await a.shot(`${SHOTS}/thiscomputer-admin-warning.png`);
    await ha.answer(false);
    check(((await node()).scope.admin ?? "off") === "off", "Cancel: still off");
    await a.clickText(".mach-admin button", "Ask each time");
    await sleep(400);
    await ha.answer(true);
    check((await node()).scope.admin === "ask" && (await node()).preset === "full_access", "on: each administrator command asks first; the preset unchanged");
    check(/No password saved/.test(await adminText()), "it says no password is saved yet");
    await a.clickText(".mach-admin-pw button", "Save password");
    await sleep(300);
    const field = await a.eval(`(() => { const i = document.querySelector('.dialog-body input'); return i && { type: i.type, auto: i.getAttribute('autocomplete') }; })()`);
    check(field?.type === "password" && field.auto === "new-password", "the password goes into a hidden field the browser doesn't fill or keep", field);
    check(/never sent anywhere/.test((await a.eval(`document.querySelector('.dialog-body')?.textContent`)) ?? ""), "…saying it stays on this computer");
    const typeIn = (v) => a.eval(`(() => { const i = document.querySelector('.dialog-body input'); i.value = ${JSON.stringify(v)}; i.dispatchEvent(new InputEvent('input', { bubbles: true })); })()`);
    await typeIn("wrong");
    await a.clickText(".dialog-body button", "Save");
    await sleep(500);
    check(!(await node()).admin_password && /didn't accept/.test((await a.eval(`document.querySelector('.toast')?.textContent`)) ?? ""), "a password sudo refuses isn't saved, and it says so");
    await a.clickText(".mach-admin-pw button", "Save password");
    await sleep(300);
    await typeIn("correct horse");
    await a.clickText(".dialog-body button", "Save");
    await sleep(500);
    const pw = await calls("this_computer_admin_password");
    check(pw.length === 2 && pw[1].password === "correct horse", "…a good one goes to this computer's own command only", pw.map((x) => (x.password ? "•••" : x.password)));
    check(!JSON.stringify(await calls("this_computer_scope")).includes("correct horse") && !(await a.eval(`document.body.textContent.includes('correct horse')`)), "…never into the scope or onto the page");
    check(/saved on this computer/.test(await adminText()), "it says the password is saved");
    await a.eval(`document.querySelector('[data-mach-admin]').scrollIntoView({ block: 'center' })`);
    await a.shot(`${SHOTS}/thiscomputer-admin-desk-dark.png`);
    await a.clickText(".mach-admin button", "As the chat says");
    await sleep(400);
    d = await ha.dialog();
    check(!!d && /without asking/.test(d.title) && /YOLO/.test(d.body) && d.danger, "“As the chat says” warns harder (YOLO chats wouldn't ask)", d);
    await ha.answer(false);
    check((await node()).scope.admin === "ask", "Cancel: it still asks each time");
    await a.clickText(".mach-admin-pw button", "Forget");
    await sleep(400);
    check(!(await node()).admin_password && (await calls("this_computer_admin_password")).pop().password === null, "Forget removes the saved password");
    await a.clickText(".mach-admin button", "Off");
    await sleep(400);
    check(!(await ha.dialog()) && ((await node()).scope.admin ?? "off") === "off", "off again at once, without a question");

    // Desktop control: the strong warning
    await pick("Desktop control");
    d = await ha.dialog();
    check(!!d && d.title === "Turn on desktop control?" && d.danger && /close to giving the agent your account/.test(d.body) && /backed up first/.test(d.body), "desktop control warns plainly (close to giving the agent your account)", d);
    await a.shot(`${SHOTS}/thiscomputer-desktop-warning.png`);
    await ha.answer(false);
    check((await node()).preset === "full_access" && !(await node()).scope.desktop, "Cancel: desktop control stays off");
    await pick("Desktop control");
    await ha.answer(true);
    sc = (await node()).scope;
    check((await node()).preset === "desktop_control" && sc.desktop && sc.ask === "always", "on: desktop control, and every action asks", sc);
    check(/Hyprland on Wayland/.test(await a.eval(`document.querySelector('.mach-desk').textContent`)), "it says which desktop session commands get");
    // asking less with desktop control on warns harder
    await a.clickText(".mach-ask button", "Never");
    await sleep(400);
    d = await ha.dialog();
    check(!!d && d.danger && /desktop session/.test(d.body), "never asking with desktop control on is a danger choice", d);
    await ha.answer(false);

    // Startup files agents changed: put one back
    await a.clickText(".mach-startup .disclosure", "Startup files changed by agents");
    await a.waitFor(`document.querySelector('.mach-startup-list li')`, 6000);
    const ch = await a.eval(`[...document.querySelectorAll('.mach-startup-list li')].map(li => li.textContent)`);
    check(ch.length === 2 && /~\/.config\/hypr\/hyprland.conf/.test(ch[0]) && /changed/.test(ch[0]) && /added/.test(ch[1]), "the startup files agents changed, with what happened", ch);
    await a.click(".mach-startup-list li .btn");
    await sleep(300);
    d = await ha.dialog();
    check(!!d && /Put back ~\/.config\/hypr\/hyprland.conf/.test(d.title), "putting one back asks first", d);
    await ha.answer(true);
    check(JSON.stringify(await calls("this_computer_restore")) === JSON.stringify([{ id: 2 }]), "…and the local command puts it back");
    await a.eval(`document.querySelector('.dev-this .mach-presets').scrollIntoView()`);
    await a.shot(`${SHOTS}/thiscomputer-desk-dark.png`);
    await a.theme("light");
    await sleep(300);
    await a.shot(`${SHOTS}/thiscomputer-desk-light.png`);
    await a.theme("dark");

    // Back down: no questions
    await pick("Look only");
    check(!(await ha.dialog()) && (await node()).preset === "look_only" && (await node()).scope.folders[0]?.path === "/home/me/code" && !(await node()).scope.desktop && !(await node()).scope.webviews, "back to Look only at once, with the folder chosen before (no web views, no desktop control)");
    // Look only: "Web views here" is still one step away, once (in Advanced).
    await a.eval(`[...document.querySelectorAll('.dev-this .disclosure')].find(d => d.textContent.trim() === 'Advanced')?.click()`);
    await sleep(400);
    const wvLook = await a.eval(`[...document.querySelectorAll('.dev-this .setting-row')].filter(r => r.querySelector('.setting-title')?.textContent === 'Web views here').map(r => ({ inAdvanced: !!r.closest('.mach-advanced'), on: r.querySelector('[role=switch]')?.getAttribute('aria-checked') }))`);
    check(wvLook.length === 1 && wvLook[0].inAdvanced && wvLook[0].on === "false", "Look only: “Web views here” once, under Advanced", wvLook);
    await a.eval(`[...document.querySelectorAll('.dev-this .disclosure')].find(d => d.textContent.trim() === 'Advanced')?.click()`);
    // The local controls get fresh status objects, plus the server's real machines_changed event.
    for (const [layout, width, height, mobile] of [["desktop", 1280, 820, false], ["phone", 344, 882, true], ["fold", 884, 1104, true]]) {
      await a.viewport(width, height, mobile);
      for (const theme of ["dark", "light"]) {
        await a.theme(theme);
        await a.eval(`Object.assign(window.__node, { enabled: true, preset: 'work_in_folders', scope: { folders: [{ path: '/home/me/code', write: true }], commands: true, ask: 'risky', webviews: true, terminal: 'sandboxed', timeout_secs: 300 } })`);
        await ws.rpc("mock_machines", { online: { id: "dev_desktop", online: true } });
        await sleep(500);
        const liveChange = async () => {
          await a.eval(`window.__place.e.click()`);
          await ws.rpc("mock_machines", { online: { id: "dev_desktop", online: true } });
        };
        await keepPlace(a, `document.querySelector('.dev-this [role=switch][aria-label="Web views here"]')`, liveChange, `${layout} ${theme}: local switch`);
        check(!(await node()).scope.webviews, `${layout} ${theme}: the switch change was saved`);
        await keepPlace(a, `[...document.querySelectorAll('.dev-this .mach-ask button')].find(b => b.textContent === 'Every action')`, liveChange, `${layout} ${theme}: local choice`);
        check((await node()).scope.ask === "always", `${layout} ${theme}: the choice was saved`);
        await keepPlace(a, `document.querySelector('.dev-this .mach-term button')`, liveChange, `${layout} ${theme}: terminal choice`);
        check((await node()).scope.terminal === "off", `${layout} ${theme}: the terminal choice was saved`);
        await a.eval(`Object.assign(window.__node, { preset: 'custom', scope: { ...window.__node.scope, folders: [{ path: '/home/me/code', write: true }, { path: '/home/me/notes', write: false }] } })`);
        await ws.rpc("mock_machines", { online: { id: "dev_desktop", online: true } });
        await sleep(500);
        await keepPlace(a, `document.querySelector('.dev-this .mach-advanced .sec-folder .segmented button')`, liveChange, `${layout} ${theme}: folder choice`);
        check(!(await node()).scope.folders[0].write, `${layout} ${theme}: the folder choice was saved`);
        await a.shot(`${SHOTS}/devices-scroll-${layout}-${theme}.png`);
        check(await ha.noSideScroll(), `${layout} ${theme}: the scrolled panel fits`);
      }
    }
    await sleep(300);
    // the activity log, in Settings → Activity
    await ha.open("activity");
    await a.clickText(".mach-log .disclosure", "Everything done on this computer");
    await a.waitFor(`document.querySelector('.mach-log-row')`, 6000);
    const log = await a.eval(`[...document.querySelectorAll('.mach-log-row')].map(r => ({ refused: r.classList.contains('refused'), text: r.textContent }))`);
    check(log.length === 3 && /cargo build/.test(log[0].text) && /Done/.test(log[0].text) && log[1].refused && /by Forge/.test(log[1].text), "the activity log: one line per action, refusals marked, the bot named", log);
    check(!(await a.eval(`window.__calls.some(c => c.cmd === 'rpc' && /machine|node/.test(c.args.request.method) && c.args.request.method !== 'list_machines')`)), "everything here went through local commands, none through the server");
    // Off: at once (back in Devices, This computer open)
    await ha.open("thiscomputer");
    await a.waitFor(`document.querySelector('.dev-this .mach-preset')`, 8000);
    await pick("Off");
    check(!(await ha.dialog()) && (await node()).enabled === false, "Off turns it off at once");

    // narrow desktop window
    await a.viewport(420, 820);
    await sleep(400);
    check(await ha.noSideScroll(), "a narrow window: nothing runs off the side");
    await a.shot(`${SHOTS}/thiscomputer-narrow-dark.png`);
    await a.theme("light");
    await sleep(300);
    await a.shot(`${SHOTS}/thiscomputer-narrow-light.png`);
    console.log("desktop app errors:", a.errors);
    check(!a.errors.length, "no page errors (desktop app)");
    await a.close();

    // the phone app (same shell, a phone): no This computer
    const m = await device();
    await m.viewport(375, 812, true, 2);
    await m.send("Page.addScriptToEvaluateOnNewDocument", { source: shellScript(token) });
    await m.goto(APP, 1800);
    await m.waitFor(`document.querySelector('.workspace')`, 12000);
    const hm = helpers(m);
    await hm.open("thiscomputer");
    await m.waitFor(`document.querySelector('.dev-list')`, 8000);
    check(!(await m.eval(`!!document.querySelector('.dev-this, .mach-local-tab')`)) && !(await m.eval(`window.__calls.some(c => c.cmd.startsWith('this_computer'))`)), "the phone app: Devices only, never This computer's controls");
    console.log("phone app errors:", m.errors);
    check(!m.errors.length, "no page errors (phone app)");
    await m.close();

    // The desktop app on Windows: desktop control is the screen, the mouse and the keyboard.
    for (const [width, height, theme] of [
      [1280, 820, "light"],
      [400, 820, "dark"],
    ]) {
      const w = await device();
      await w.viewport(width, height);
      const onWindows = `Object.defineProperty(Navigator.prototype, "platform", { get: () => "Win32" });`;
      const fullAccess = `Object.assign(window.__node, { enabled: true, paired: true, running: true, connected: true, server: "Mock server", preset: "full_access",
        scope: { folders: [{ path: "/home/me", write: true }], commands: true, ask: "risky", webviews: true, timeout_secs: 300 },
        os: "windows x86_64", features: ["desktop"], desktop_session: "Windows desktop, 2 screens", bus_filter: false });`;
      await w.send("Page.addScriptToEvaluateOnNewDocument", { source: `${onWindows}\n${shellScript(token)}\n${fullAccess}` });
      await w.goto(APP, 1800);
      await w.waitFor(`document.querySelector('.workspace')`, 12000);
      await w.theme(theme);
      const hw = helpers(w);
      await hw.open("thiscomputer");
      await w.waitFor(`document.querySelector('.mach-preset')`, 8000);
      const desc = await w.eval(`[...document.querySelectorAll('.mach-preset')].find(b => /Desktop control/.test(b.textContent))?.querySelector('.mach-preset-desc')?.textContent ?? ''`);
      check(/seeing your screens and using your mouse and keyboard/.test(desc) && !/window manager/.test(desc), `Windows (${width}px): desktop control is the screen, mouse and keyboard`, desc);
      const full = await w.eval(`[...document.querySelectorAll('.mach-preset')].find(b => b.querySelector('.mach-preset-name')?.textContent === 'Full access')?.querySelector('.mach-preset-desc')?.textContent ?? ''`);
      check(/without a sandbox/.test(full) && /including passwords and keys/.test(full) && !/stay protected/.test(full), "…and Full access says commands there reach passwords and keys (no sandbox)", full);
      // Capability, never the server OS or navigator guess, chooses the preset.
      for (const [os, features, expected, label] of [
        ["windows x86_64", [], false, "old Windows node with no native capability"],
        ["unknown", undefined, false, "unknown older node"],
        ["linux x86_64", undefined, true, "older Linux command desktop"],
        ["windows x86_64", ["desktop"], true, "new Windows native desktop"],
      ]) {
        await w.eval(`Object.assign(window.__node, { os: ${JSON.stringify(os)}, features: ${features === undefined ? "undefined" : JSON.stringify(features)} })`);
        await hw.open("activity");
        await hw.open("thiscomputer");
        await w.waitFor(`document.querySelector('.mach-preset')`, 8000);
        check((await w.eval(`[...document.querySelectorAll('.mach-preset')].some(b => /Desktop control/.test(b.textContent))`)) === expected, `Windows client (${width}px): ${label} ${expected ? "offers" : "hides"} Desktop control`);
      }
      await w.clickText(".mach-preset", "Desktop control");
      await sleep(400);
      const d = await hw.dialog();
      check(
        !!d && d.danger && /close to giving the agent your account/.test(d.body) && /lock screen/.test(d.body) && /administrator/.test(d.body) && /stay in the chat/.test(d.body) && /say only what kind of action/.test(d.body) && !/backed up first/.test(d.body),
        "…its warning says what it reaches and what Windows keeps out of reach",
        d,
      );
      await w.shot(`${SHOTS}/thiscomputer-windows-desktop-warning-${width}-${theme}.png`);
      await hw.answer(true);
      await sleep(500);
      const n = await w.eval(`JSON.parse(JSON.stringify(window.__node))`);
      check(n.preset === "desktop_control" && n.scope.desktop, "…turned on", n.preset);
      const desk = await w.eval(`document.querySelector('.mach-desk')?.textContent ?? ''`);
      check(
        /Agents can see and use your screen while you're signed in: Windows desktop, 2 screens/.test(desk) && /administrator/.test(desk) && !/xdg-dbus-proxy/.test(desk) && !(await w.eval(`!!document.querySelector('.mach-desk.form-error')`)),
        "…it says which screens agents can use, with nothing to install",
        desk,
      );
      const home = await w.eval(`document.querySelector('.mach-homeline')?.textContent ?? ''`);
      check(/never read or change Tessera's own files or your passwords and keys, nor change your startup files/.test(home) && /commands run as you without a sandbox and can/.test(home), "…on Windows it says file actions are kept out of those, and commands aren't", home);
      check(!(await w.eval(`document.documentElement.scrollWidth > document.documentElement.clientWidth + 1`)), `…nothing sticks out sideways (${width}px)`);
      await w.shot(`${SHOTS}/thiscomputer-windows-desktop-${width}-${theme}.png`);
      // Stopped with the Stop bar (or its key): said first, with Resume here.
      await w.eval(`(() => { window.__node.scope = { ...window.__node.scope, stopped: true, stops: 1 }; window.__node.stop_bar = "Stop bar on; Stop key Ctrl+Alt+Esc"; })()`);
      await hw.open("activity");
      await hw.open("thiscomputer");
      await w.waitFor(`document.querySelector('.mach-status')`, 8000);
      const stopped = await w.eval(`({ state: document.querySelector('.mach-state')?.textContent ?? '', tone: document.querySelector('.mach-status')?.dataset.tone, resume: !!document.querySelector('.mach-resume'), desk: document.querySelector('.mach-desk')?.textContent ?? '' })`);
      check(stopped.state === "Stopped here" && stopped.tone === "bad" && stopped.resume, `Windows (${width}px): stopped here says so first, with Resume`, stopped);
      check(/Stop bar at the top of your screen/.test(stopped.desk) && /Ctrl\+Alt\+Esc/.test(stopped.desk), "…and where the Stop bar is, with its key", stopped.desk);
      check(!(await w.eval(`document.documentElement.scrollWidth > document.documentElement.clientWidth + 1`)), `…nothing sticks out sideways (${width}px, stopped)`);
      await w.shot(`${SHOTS}/thiscomputer-windows-stopped-${width}-${theme}.png`);
      await w.click(".mach-resume");
      await sleep(600);
      const sent = (await w.eval(`window.__calls.filter(c => c.cmd === "this_computer_stop").map(c => c.args)`)).pop();
      const after = await w.eval(`({ state: document.querySelector('.mach-state')?.textContent ?? '', resume: !!document.querySelector('.mach-resume'), stopped: !!window.__node.scope.stopped })`);
      check(sent?.stopped === false && !after.stopped && after.state !== "Stopped here" && !after.resume, "…Resume resumes it here (`tessera node resume`)", { sent, after });
      // Tessera Guard: missing, it says what it is for and installs (Windows asks an administrator).
      const NEEDED = "commands and desktop control on Windows need Tessera Guard";
      await w.eval(`window.__node.guard = { installed: false, running: false, stopped: false, stops: 0, blocked: ${JSON.stringify(NEEDED)} }`);
      await hw.open("activity");
      await hw.open("thiscomputer");
      await w.waitFor(`document.querySelector('.mach-guard')`, 8000).catch(() => {});
      const missing = await w.eval(`document.querySelector('.mach-guard')?.textContent ?? ''`);
      check(/Tessera Guard/.test(missing) && /can't undo/.test(missing) && /Windows asks an administrator once/.test(missing) && !!(await w.eval(`!!document.querySelector('.mach-guard-install')`)), `Windows (${width}px): Tessera Guard, when missing, says what it does and offers to install it`, missing);
      await w.shot(`${SHOTS}/thiscomputer-windows-guard-missing-${width}-${theme}.png`);
      await w.click(".mach-guard-install");
      await w.waitFor(`/On: your Stop/.test(document.querySelector('.mach-guard')?.textContent ?? '')`, 4000).catch(() => {});
      check((await w.eval(`window.__calls.some(c => c.cmd === "this_computer_guard_install")`)) && /On: your Stop is kept/.test(await w.eval(`document.querySelector('.mach-guard')?.textContent ?? ''`)), "…installing it shows it on, with Resume through Windows' approval");
      // Installed but User Account Control too weak: said plainly.
      await w.eval(`window.__node.guard = { installed: true, running: true, stopped: false, stops: 0, blocked: "your account is an administrator and User Account Control isn't at \\"Always notify\\"" }`);
      await hw.open("activity");
      await hw.open("thiscomputer");
      await w.waitFor(`document.querySelector('.mach-guard .form-error')`, 8000).catch(() => {});
      check(/Always notify/.test(await w.eval(`document.querySelector('.mach-guard .form-error')?.textContent ?? ''`)), "…and a reason commands and the screen are refused shows under it");
      check(!(await w.eval(`document.documentElement.scrollWidth > document.documentElement.clientWidth + 1`)), `…nothing sticks out sideways (${width}px, guard)`);
      await w.shot(`${SHOTS}/thiscomputer-windows-guard-${width}-${theme}.png`);
      console.log("windows app errors:", w.errors);
      check(!w.errors.length, `no page errors (Windows desktop app, ${width}px)`);
      await w.close();
    }
  }
  ws.close();
  finish();
});
