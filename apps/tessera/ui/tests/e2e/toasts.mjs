// Toasts, and the messages agents send with `notify`, inside the app. With the top bar (the
// unfolded phone, the desktop) the newest sits in the bar's free middle as one line, never over a
// tile or the bar's own buttons, with a count that drops the others down as a short list; a tap
// opens an agent's message, a swipe up puts it away. The folded phone keeps them below its header: the
// newest only (with the same count), who sent it above the title, swiped sideways to go, tapped to
// open its chat. Over a dialog or the picture viewer they float as before. A message seen on
// another device (or its chat opened here) takes its toast away; the pointer on a toast keeps them.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const SID = "ses_7d3a91e5c4b2"; // "Add a /healthz endpoint"
const OTHER = "ses_5a3e8f10c2d9"; // "Plan Android foldable layout"
const BOT = "ses_6f2c0a9d3e18"; // "Plan my week", Nova's chat
const LONG = "ses_e0b6d4a29c1f";
const SQL = "ses_8c1f2a9d4e70";
const WSOCK = "ses_2b7e91c04d5a"; // "Fix flaky websocket reconnect test"

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const ui = (p, js) => p.eval(`__mod('lib/ui.ts').then(m => { ${js} })`);
const clear = (p) => ui(p, `m.toasts().forEach((t) => m.dismissToast(t.id))`);
const focusedSession = (p) => ws(p, `return m.paneSession(m.workspace.focused())?.info.id ?? null`);
const waitFor = async (fn, ms = 5000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const v = await fn();
    if (v) return v;
    await sleep(80);
  }
  return null;
};
const touch = (p, type, pts) => p.send("Input.dispatchTouchEvent", { type, touchPoints: pts.map(([x, y]) => ({ x, y })) });
async function drag(p, x0, y0, x1, y1, steps = 10) {
  await touch(p, "touchStart", [[x0, y0]]);
  for (let k = 1; k <= steps; k++) {
    await touch(p, "touchMove", [[x0 + ((x1 - x0) * k) / steps, y0 + ((y1 - y0) * k) / steps]]);
    await sleep(16);
  }
  await touch(p, "touchEnd", []);
  await sleep(400);
}
const overlap = (a, b) => a.l < b.r - 0.5 && b.l < a.r - 0.5 && a.t < b.b - 0.5 && b.t < a.b - 0.5;

/** Where everything is: the bar, its free middle, the toasts on screen, the bar's buttons, the tiles. */
const geom = (p) =>
  p.eval(`(() => {
    // Measured where they end up: headless Chromium may not have played their entrance yet.
    const done = (a) => { try { a.finish(); } catch {} };
    document.querySelector('.toasts').getAnimations({ subtree: true }).forEach(done);
    document.querySelectorAll('[data-pane-id]').forEach((e) => e.getAnimations().forEach(done));
    const r = (e) => { if (!e) return null; const b = e.getBoundingClientRect(); return { l: b.left, t: b.top, r: b.right, b: b.bottom, w: b.width, h: b.height }; };
    const shown = (e) => e.getClientRects().length > 0;
    const host = document.querySelector('.toasts');
    const list = document.querySelector('.toast-list');
    return {
      inBar: host.classList.contains('in-bar'),
      overModal: host.classList.contains('over-modal'),
      vw: innerWidth, vh: innerHeight,
      bar: r(document.querySelector('.topbar')),
      slot: r(document.querySelector('.tb-toasts')),
      toasts: [...document.querySelectorAll('.toasts > .toast')].filter(shown).map((e) => ({ ...r(e), text: e.textContent })),
      list: list ? { ...r(list), n: list.querySelectorAll('.toast').length } : null,
      buttons: [...document.querySelectorAll('.topbar button')].filter(shown).map((b) => ({ ...r(b), label: b.getAttribute('aria-label') })),
      panes: [...document.querySelectorAll('[data-pane-id]')].filter(shown).map(r).filter((x) => x.w > 0 && x.r > 0 && x.l < innerWidth),
      line: (() => { const e = document.querySelector('.toasts > .toast-pill.newest .toast-line'); return e ? { sh: e.scrollHeight, ch: e.clientHeight, ws: getComputedStyle(e).whiteSpace } : null; })(),
      more: document.querySelector('.toasts > .toast.newest .toast-more, .toasts:not(.in-bar) > .toast:last-child .toast-more')?.textContent ?? null,
    };
  })()`);

/** The bar's buttons still take their own clicks (nothing drawn over them). */
const buttonsFree = (p) =>
  p.eval(`[...document.querySelectorAll('.topbar button')].filter((b) => b.getClientRects().length).every((b) => { const r = b.getBoundingClientRect(); const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2); return hit === b || b.contains(hit); })`);

/** One line in the top bar, centred in its free middle, clear of its buttons and of every tile. */
function checkInBar(g, label) {
  const t = g.toasts[0];
  check(g.inBar && g.toasts.length === 1, `${label}: the toast sits in the top bar, the newest only`, { inBar: g.inBar, n: g.toasts.length });
  if (!t) return;
  check(t.t >= g.bar.t - 0.5 && t.b <= g.bar.b + 0.5 && t.h <= 32, `${label}: within the bar's height`, { t, bar: g.bar });
  check(t.l >= g.slot.l - 0.5 && t.r <= g.slot.r + 0.5 && Math.abs((t.l + t.r) / 2 - (g.slot.l + g.slot.r) / 2) < 2, `${label}: centred in its free middle`, { t, slot: g.slot });
  check(!g.buttons.some((b) => overlap(t, b)), `${label}: never over the bar's own buttons`, g.buttons.filter((b) => overlap(t, b)));
  check(g.panes.length > 0 && !g.panes.some((p) => overlap(t, p)), `${label}: never over a tile (its header or its composer)`, { panes: g.panes });
  check(g.line && g.line.ws === "nowrap" && g.line.sh <= g.line.ch + 2, `${label}: one line`, g.line);
}

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const probe = await wsClient("probe");

  // ── desktop: in the top bar ───────────────────────────────────────────
  console.log("— desktop");
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Desktop");
  await ws(p, `m.openSession('${SID}', { pane: m.workspace.focused() })`);
  await sleep(400);
  await ws(p, `m.openSession('${WSOCK}', { newPane: true })`);
  await sleep(900);
  await clear(p);
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    await probe.rpc("mock_notify", { session_id: OTHER, title: `Build finished (${theme})`, body: "All 42 tests pass. The release is in out/ and the changelog is updated." });
    const shown = await waitFor(async () => (await geom(p)).toasts.find((t) => t.text.includes(`Build finished (${theme})`)));
    check(!!shown, `desktop ${theme}: an agent's message shows`);
    await sleep(400);
    const g = await geom(p);
    checkInBar(g, `desktop ${theme}`);
    check(/Plan Android foldable layout/.test(g.toasts[0]?.text) && /All 42 tests pass/.test(g.toasts[0]?.text), `desktop ${theme}: with who sent it, its title and its text`, g.toasts[0]?.text);
    check(await buttonsFree(p), `desktop ${theme}: the bar's buttons stay usable`);
    await p.shot(`${SHOTS}/toasts-bar-desktop-${theme}.png`);
    await p.shot(`${SHOTS}/toasts-bar-desktop-${theme}-top.png`, { x: 0, y: 0, width: 1440, height: 140 });
    await clear(p);
  }

  // Several at once: the newest, a count; the count drops them all down under the bar.
  console.log("— desktop: several");
  await p.theme("dark");
  // Kept while we look (a loaded machine is slow): timing has its own check below.
  await ui(p, `m.holdToasts(true)`);
  await ui(p, `m.toast({ level: 'error', title: 'Could not save', message: 'The disk is full: free some space and try again.', timeout: 30000 })`);
  await probe.rpc("mock_notify", { session_id: BOT, title: "Your week is planned", body: "Monday: dentist at 9. Wednesday: the release. Friday: nothing, on purpose." });
  await waitFor(async () => (await geom(p)).toasts.some((t) => /Your week/.test(t.text)));
  await ui(p, `m.toast({ level: 'info', message: 'Moved to Mosaic 2', timeout: 30000 })`);
  await sleep(350);
  let g = await geom(p);
  check(g.toasts.length === 1 && /Moved to Mosaic 2/.test(g.toasts[0].text), "the newest shows, alone", g.toasts);
  check(g.more === "+2", "with a count of the others", g.more);
  check((await p.eval(`document.querySelectorAll('.toasts > .toast').length`)) === 3, "the others are still there (behind it)");
  await p.click(".toasts > .toast.newest .toast-more");
  await sleep(350);
  g = await geom(p);
  check(g.list && g.list.n === 3 && g.list.t >= g.bar.b - 0.5 && g.list.l >= 0 && g.list.r <= g.vw, "the count drops them all down under the bar, inside the window", g.list);
  check(/Nova/.test(await p.eval(`[...document.querySelectorAll('.toast-list .toast')].map((t) => t.querySelector('.toast-from')?.textContent ?? '').join(',')`)), "the bot's message says it is from the bot");
  await p.shot(`${SHOTS}/toasts-bar-list-desktop.png`);
  await p.shot(`${SHOTS}/toasts-bar-list-desktop-top.png`, { x: 300, y: 0, width: 1140, height: 360 });
  // A click elsewhere puts the list away.
  const pane = g.panes[0];
  await p.mouse(pane.l + pane.w / 2, pane.t + pane.h / 2);
  check(!(await p.eval(`!!document.querySelector('.toast-list')`)), "a click elsewhere puts the list away");
  await p.click(".toasts > .toast.newest .toast-more");
  await sleep(250);
  await p.key("Escape");
  check(!(await p.eval(`!!document.querySelector('.toast-list')`)), "and so does Escape");
  // Dismissing the newest brings the one before it back into the bar.
  await p.click(".toasts > .toast.newest .toast-x");
  await sleep(300);
  g = await geom(p);
  check(g.toasts.length === 1 && /Your week/.test(g.toasts[0].text) && g.more === "+1", "the ✕ takes the newest away; the one before it shows", { t: g.toasts, more: g.more });
  // Tapping an agent's message opens its chat.
  await p.click(".toasts > .toast.newest .toast-line");
  check(!!(await waitFor(async () => (await focusedSession(p)) === BOT)), "clicking an agent's message opens its chat");
  check(!(await p.eval(`[...document.querySelectorAll('.toast')].some((t) => /Your week/.test(t.textContent))`)), "and it goes");
  await ui(p, `m.holdToasts(false)`);
  await clear(p);

  // Actions stay: Undo in the bar.
  console.log("— desktop: actions, hold, repeats");
  await ui(p, `m.toast({ level: 'info', message: 'Chat archived', action: { label: 'Undo', run: () => (window.__undo = 1) }, timeout: 30000 })`);
  await sleep(300);
  g = await geom(p);
  checkInBar(g, "desktop with an action");
  await p.clickText(".toasts > .toast.newest .btn", "Undo");
  check((await p.eval(`window.__undo`)) === 1 && !(await geom(p)).toasts.length, "Undo works from the bar, and the toast goes");
  // The pointer on a toast keeps it; it goes a moment after the pointer leaves.
  await ui(p, `m.toast({ level: 'info', message: 'Hold me', timeout: 2500 })`);
  const pill = (await geom(p)).toasts[0];
  await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: pill.l + 30, y: (pill.t + pill.b) / 2 });
  await sleep(3200);
  check((await geom(p)).toasts.some((t) => /Hold me/.test(t.text)), "the pointer on a toast keeps it");
  await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: 700, y: 600 });
  check(!!(await waitFor(async () => !(await geom(p)).toasts.length, 6000)), "it goes after the pointer leaves");
  // The same message twice is one toast.
  await ui(p, `m.toast({ level: 'info', message: 'Layout: Columns' }); m.toast({ level: 'info', message: 'Layout: Columns' })`);
  check((await p.eval(`document.querySelectorAll('.toasts > .toast').length`)) === 1, "the same message twice is one toast");
  await clear(p);

  // Seen on another device, or its chat opened here: the toast goes.
  console.log("— desktop: seen");
  const n1 = (await probe.rpc("mock_notify", { session_id: LONG, title: "Indexing done", body: "3,200 files" })).result;
  await waitFor(async () => (await geom(p)).toasts.some((t) => /Indexing done/.test(t.text)));
  await probe.rpc("seen_notifications", { ids: [n1.id] });
  check(!!(await waitFor(async () => !(await geom(p)).toasts.some((t) => /Indexing done/.test(t.text)))), "seen on another device: its toast goes here too");
  await probe.rpc("mock_notify", { session_id: SQL, title: "Migration ready", body: "Review the plan" });
  await waitFor(async () => (await geom(p)).toasts.some((t) => /Migration ready/.test(t.text)));
  await ws(p, `m.openSession('${SQL}', { pane: m.workspace.focused() })`);
  check(!!(await waitFor(async () => !(await geom(p)).toasts.some((t) => /Migration ready/.test(t.text)))), "its chat opened here: its toast goes");

  // Over a dialog and over the viewer they float, as before.
  console.log("— desktop: over a dialog, the viewer");
  await ui(p, `m.setSettingsTab('notifications')`);
  await p.waitFor(`document.querySelector('.toasts.over-modal')`, 8000).catch(() => {});
  await sleep(300);
  await ui(p, `m.toast({ level: 'success', message: 'Saved', timeout: 30000 })`);
  await sleep(350);
  g = await geom(p);
  check(!g.inBar && g.overModal && g.toasts.length === 1 && g.toasts[0].t < 80 && g.toasts[0].h > 30, "over a dialog: floating at the top, in full", g);
  await p.shot(`${SHOTS}/toasts-over-dialog-desktop.png`);
  await ui(p, `m.setSettingsTab(null)`);
  await p.waitFor(`!document.querySelector('.toasts.over-modal')`, 8000).catch(() => {});
  await sleep(300);
  g = await geom(p);
  check(g.inBar && g.toasts.length === 1, "the dialog closed: back in the bar", { inBar: g.inBar, n: g.toasts.length });
  await clear(p);
  const img = await p.eval(`(() => { const c = document.createElement('canvas'); c.width = 64; c.height = 40; const x = c.getContext('2d'); x.fillStyle = '#2a7'; x.fillRect(0, 0, 64, 40); return c.toDataURL('image/png'); })()`);
  await p.eval(`__mod('lib/viewer.ts').then((m) => m.openViewer([{ src: ${JSON.stringify(img)}, alt: 'green' }]))`);
  await sleep(600);
  await ui(p, `m.toast({ level: 'info', message: 'Copied', timeout: 30000 })`);
  await sleep(300);
  g = await geom(p);
  check(!g.inBar && g.toasts.length === 1, "over the viewer: floating", g.toasts);
  await p.eval(`__mod('lib/viewer.ts').then((m) => m.closeViewer())`);
  await sleep(400);
  await clear(p);
  console.log("errors", p.errors);
  check(!p.errors.length, "desktop: no errors", p.errors);
  await p.close();

  // ── the unfolded phone: in the top bar too ────────────────────────────
  console.log("— unfolded phone");
  const f = await device();
  await f.viewport(673, 841, true, 2.625);
  await signIn(f, "Fold");
  await ws(f, `m.openSession('${SID}', { pane: m.workspace.focused() })`);
  await sleep(400);
  await ws(f, `m.openSession('${BOT}', { newPane: true })`);
  await sleep(900);
  await clear(f);
  check((await ws(f, `return m.workspace.mode()`)) === "dual", "the unfolded phone shows two tiles (dual)");
  for (const theme of ["dark", "light"]) {
    await f.theme(theme);
    await probe.rpc("mock_notify", { session_id: OTHER, title: `Photos sorted (${theme})`, body: "1,204 photos in 38 albums; 12 blurry ones are in the Review album." });
    await waitFor(async () => (await geom(f)).toasts.some((t) => t.text.includes(`Photos sorted (${theme})`)));
    await sleep(400);
    g = await geom(f);
    checkInBar(g, `unfolded ${theme}`);
    check(await buttonsFree(f), `unfolded ${theme}: the bar's buttons stay usable`);
    await f.shot(`${SHOTS}/toasts-bar-fold-${theme}.png`);
    await f.shot(`${SHOTS}/toasts-bar-fold-${theme}-top.png`, { x: 0, y: 0, width: 673, height: 140 });
    // A swipe up puts it away.
    const t = g.toasts[0];
    await drag(f, t.l + 60, (t.t + t.b) / 2, t.l + 64, (t.t + t.b) / 2 - 60);
    check(!(await waitFor(async () => (await geom(f)).toasts.length, 600)), `unfolded ${theme}: a swipe up puts it away`);
    await clear(f);
  }
  await f.theme("dark");
  await ui(f, `m.holdToasts(true)`);
  await ui(f, `m.toast({ level: 'warning', message: 'Low battery on Desktop', timeout: 30000 })`);
  await probe.rpc("mock_notify", { session_id: OTHER, title: "Backup done", body: "38 GB" });
  await waitFor(async () => (await geom(f)).toasts.some((t) => /Backup done/.test(t.text)));
  await sleep(300);
  g = await geom(f);
  check(g.more === "+1", "unfolded: a count of the others", g.more);
  let b = await f.box(".toasts > .toast.newest .toast-more");
  await f.tap(b.cx, b.cy);
  await sleep(350);
  g = await geom(f);
  check(g.list && g.list.n === 2 && g.list.t >= g.bar.b - 0.5 && g.list.l >= 0 && g.list.r <= g.vw, "unfolded: a tap on the count drops them down", g.list);
  await f.shot(`${SHOTS}/toasts-bar-fold-list.png`);
  // Tap the agent's message in the bar: its chat opens.
  b = await f.box(".toasts > .toast.newest .toast-line");
  await f.tap(b.cx, b.cy);
  check(!!(await waitFor(async () => (await focusedSession(f)) === OTHER)), "unfolded: a tap on an agent's message opens its chat");
  await ui(f, `m.holdToasts(false)`);
  await clear(f);
  console.log("errors", f.errors);
  check(!f.errors.length, "unfolded: no errors", f.errors);
  await f.close();

  // ── the folded phone: below the header, clear of its controls ──────────
  console.log("— folded phone");
  for (const theme of ["dark", "light"]) {
    const ph = await device();
    await ph.viewport(344, 882, true, 3);
    await signIn(ph, `Phone ${theme}`);
    await ph.theme(theme);
    await ws(ph, `m.openSession('${SID}', { pane: m.workspace.focused() })`);
    await sleep(900);
    await clear(ph);
    await ui(ph, `m.holdToasts(true)`);
    await probe.rpc("mock_notify", { session_id: BOT, title: "Your week is planned", body: "Monday: dentist at 9. Wednesday: the release. Friday: nothing, on purpose." });
    await waitFor(async () => (await geom(ph)).toasts.some((t) => /Your week/.test(t.text)));
    await sleep(400);
    g = await geom(ph);
    const t = g.toasts[0];
    const header = await ph.box('.pane.focused .pane-head');
    check(!g.inBar && g.toasts.length === 1 && t.t >= header.y + header.h - 1 && t.t < header.y + header.h + 20 && Math.abs((t.l + t.r) / 2 - g.vw / 2) < 2 && t.l >= 0 && t.r <= g.vw, `folded ${theme}: below the header, centred, on screen`, { toast: t, header });
    check(await ph.eval(`[...document.querySelectorAll('.pane.focused .pane-head button')].filter(e => e.getClientRects().length).every(e => { const r = e.getBoundingClientRect(); return e.contains(document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2)); })`), `folded ${theme}: a toast leaves the sidebar, title, close and New buttons usable`);
    check((await ph.eval(`document.querySelector('.toasts > .toast:last-child .toast-from')?.textContent`)) === "Nova", `folded ${theme}: says who sent it (the bot)`);
    await ph.shot(`${SHOTS}/toasts-folded-${theme}.png`);
    // Several: the newest, with a count that shows the others.
    await ui(ph, `m.toast({ level: 'success', message: 'Saved to Downloads', timeout: 30000 })`);
    await ui(ph, `m.toast({ level: 'error', title: 'Could not send', message: 'The server is offline.', timeout: 30000 })`);
    await sleep(350);
    g = await geom(ph);
    check(g.toasts.length === 1 && /Could not send/.test(g.toasts[0].text) && g.more === "+2", `folded ${theme}: the newest only, with a count`, { t: g.toasts, more: g.more });
    await ph.shot(`${SHOTS}/toasts-folded-stack-${theme}.png`);
    b = await ph.box(".toasts > .toast:last-child .toast-more");
    await ph.tap(b.cx, b.cy);
    await sleep(300);
    g = await geom(ph);
    check(g.toasts.length === 3 && g.toasts.every((x) => x.l >= 0 && x.r <= g.vw), `folded ${theme}: the count shows them all`, g.toasts.length);
    await ph.shot(`${SHOTS}/toasts-folded-open-${theme}.png`);
    await ph.tap(b.cx, b.cy);
    await sleep(300);
    check((await geom(ph)).toasts.length === 1, `folded ${theme}: and a second tap the newest only again`);
    // A swipe sideways puts the newest away; the one before it shows.
    let top = (await geom(ph)).toasts[0];
    await drag(ph, top.l + 40, (top.t + top.b) / 2, top.l + 220, (top.t + top.b) / 2 + 4);
    g = await geom(ph);
    check(g.toasts.length === 1 && /Saved to Downloads/.test(g.toasts[0].text) && !(await ph.eval(`[...document.querySelectorAll('.toast')].some((t) => /Could not send/.test(t.textContent))`)), `folded ${theme}: a swipe sideways puts it away`, g.toasts);
    // A passing notice lets taps through to what's under it (only its buttons take them).
    top = g.toasts[0];
    check(await ph.eval(`!document.elementFromPoint(${top.l + 60}, ${(top.t + top.b) / 2})?.closest('.toast')`), `folded ${theme}: a passing notice lets taps through`);
    // So does one with an Undo: only its button takes taps.
    await ui(ph, `m.toast({ level: 'info', message: 'Chat archived', action: { label: 'Undo', run: () => {} }, timeout: 30000 })`);
    await sleep(350);
    top = (await geom(ph)).toasts[0];
    check(/Chat archived/.test(top.text) && (await ph.eval(`!document.elementFromPoint(${top.l + 30}, ${(top.t + top.b) / 2})?.closest('.toast')`)), `folded ${theme}: an Undo toast lets taps through too, but its button`, top);
    await ui(ph, `m.toasts().filter((t) => !t.from).forEach((t) => m.dismissToast(t.id))`);
    await sleep(200);
    // A short drag springs back.
    top = (await geom(ph)).toasts[0];
    await drag(ph, top.l + 40, (top.t + top.b) / 2, top.l + 70, (top.t + top.b) / 2, 4);
    check((await geom(ph)).toasts.some((x) => /Your week/.test(x.text)), `folded ${theme}: a short drag leaves it`);
    // A tap on the agent's message opens its chat.
    top = (await geom(ph)).toasts[0];
    await ph.tap(top.l + 60, top.t + 20);
    check(!!(await waitFor(async () => (await focusedSession(ph)) === BOT)), `folded ${theme}: a tap on it opens its chat`);
    check(!(await geom(ph)).toasts.length, `folded ${theme}: and it goes`);
    await ui(ph, `m.holdToasts(false)`);
    // With the viewer open they go to the bottom, clear of its controls.
    await ph.eval(`__mod('lib/viewer.ts').then((m) => m.openViewer([{ src: ${JSON.stringify(img)}, alt: 'green' }]))`);
    await sleep(600);
    await ui(ph, `m.toast({ level: 'info', message: 'Copied', timeout: 30000 })`);
    await sleep(350);
    g = await geom(ph);
    check(g.toasts.length === 1 && g.toasts[0].b > g.vh - 60, `folded ${theme}: over the viewer, at the bottom`, g.toasts[0]);
    await ph.eval(`__mod('lib/viewer.ts').then((m) => m.closeViewer())`);
    console.log("errors", ph.errors);
    check(!ph.errors.length, `folded ${theme}: no errors`, ph.errors);
    await ph.close();
  }
  probe.close();
  finish();
});
