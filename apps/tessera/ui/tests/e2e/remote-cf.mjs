// Batch 17 (CF): remote access without traps for someone who doesn't know Cloudflare.
// Settings → Connection: the domain link goes where Cloudflare's own docs say; the token recipe starts
// with turning Zero Trust on; an account without Zero Trust is named before anything is made (with a
// link and Check again), and a setup that still fails at Access says the same; after setup, devices
// paired before need nothing (they pick up the address themselves). The devices' Cloudflare key (a
// year): nothing shown while it's fine, a plain line from 30 days before it runs out, after it ran out
// and when its date is unknown, each with Renew (a one-permission token, or "I renewed it" after doing
// it in Cloudflare). Browsers never get the server's addresses and Access key (`get_server_route`).
// The desktop app (a stand-in shell): a server whose addresses the shell updated shows them at once,
// and Settings → Devices → this computer has "Keep Tessera running" on and off.
// Desktop, the folded Fold's cover screen (344 px), the unfolded Fold; dark and light.
import { APP, MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const ui = (p, js) => p.eval(`__mod('lib/ui.ts').then((u) => { ${js} })`);
const panel = ".remote-panel";
const text = (p, sel) => p.eval(`document.querySelector(${JSON.stringify(sel)})?.textContent ?? ''`);
const has = (p, sel) => p.eval(`!!document.querySelector(${JSON.stringify(sel)})`);
const now = () => Math.floor(Date.now() / 1000);
const DAY = 86_400;

/** Settings → Connection, scrolled to "Away from home". */
const open = async (p) => {
  await ui(p, `u.setSettingsTab(null)`);
  await sleep(300);
  await ui(p, `u.openSettings('servers')`);
  await p.waitFor(`!!document.querySelector('${panel} .remote-status')`, 8000);
  await p.eval(`document.querySelector('${panel}').scrollIntoView({ block: 'start' })`);
  await sleep(400);
};
/** Links opened in the system browser (window.open in a browser), recorded instead. */
const watchLinks = (p) => p.eval(`(() => { window.__opened = []; window.open = (u) => { window.__opened.push(u); return null; }; })()`);
const opened = (p) => p.eval(`window.__opened ?? []`);
const clickBtn = async (p, scope, label) => {
  await p.eval(`(() => { const b = [...document.querySelectorAll(${JSON.stringify(scope + " button")})].find((b) => b.textContent.trim().startsWith(${JSON.stringify(label)})); if (!b) throw new Error('no button ' + ${JSON.stringify(label)}); b.scrollIntoView({ block: 'center' }); b.click(); })()`);
  await sleep(350);
};
const typeInto = async (p, sel, value) => {
  await p.click(sel);
  await p.eval(`document.querySelector(${JSON.stringify(sel)}).select?.()`);
  await p.type(value);
};
// Nothing sticks out sideways.
const fits = (p) =>
  p.eval(`(() => { const b = document.querySelector('.settings-body') || document.scrollingElement; return [...document.querySelectorAll('${panel} *')].every((e) => { const r = e.getBoundingClientRect(); return !r.width || r.right <= innerWidth + 1; }) && b.scrollWidth <= b.clientWidth + 1; })()`);
const status = async (c) => (await c.rpc("remote_status")).result;

run(async () => {
  const c = await wsClient("remote-cf-suite");
  await mock("/__mock/reset");
  // A server nobody set up remote access for yet (the mock starts with an address of its own).
  await c.rpc("set_remote_access", { urls: [], headers: {} });

  // ── browsers never get the server's addresses and Access key ──
  const route = await c.rpc("get_server_route");
  check(route.error?.code === "unauthorized" && /apps' own connections/.test(route.error?.message), "get_server_route: refused to a browser's connection", route);

  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "CF desk");
  await p.theme("dark");
  await open(p);
  await watchLinks(p);
  check(/Only at home for now/.test(await text(p, `${panel} .remote-status`)), "not set up yet: only at home", await text(p, `${panel} .remote-status`));

  // ── no domain: the link is the one in Cloudflare's docs ──
  await clickBtn(p, `${panel} .nodomain`, "No domain?");
  await clickBtn(p, `${panel} .nodomain-body`, "Register a domain");
  let links = await opened(p);
  check(links.at(-1) === "https://dash.cloudflare.com/?to=/:account/registrar/register", "Register a domain opens Cloudflare's own Register domains page", links);

  // ── the token recipe starts with Zero Trust ──
  await clickBtn(p, panel, "Set it up with Cloudflare");
  await p.waitFor(`document.querySelector('${panel} .cf-recipe')`, 4000);
  const steps = await p.eval(`[...document.querySelectorAll('${panel} .cf-steps > li')].map((l) => l.textContent.trim())`);
  check(/^Turn on Zero Trust/.test(steps[0]) && /free/.test(steps[0]) && /Free plan/.test(steps[0]), "step 1 of the recipe: turn Zero Trust on (free, once)", steps[0]);
  await clickBtn(p, `${panel} .cf-steps > li:first-child`, "open Zero Trust");
  links = await opened(p);
  check(links.at(-1) === "https://one.dash.cloudflare.com/", "…its link opens Cloudflare's Zero Trust dashboard", links.at(-1));

  // ── an account without Zero Trust: named before anything is made ──
  await typeInto(p, `${panel} form.cf-card input[type=password]`, "nozt-token");
  await clickBtn(p, `${panel} form.cf-card .form-actions`, "Check token");
  await p.waitFor(`document.querySelector('${panel} .cf-zones')`, 5000).catch(() => {});
  check(await has(p, `${panel} .zt-note`), "a token whose account has no Zero Trust: a note right at the domain step");
  const zt = await text(p, `${panel} .zt-note`);
  check(/Turn on Zero Trust first/.test(zt) && /free/i.test(zt) && /Free plan/.test(zt) && /Access: Apps and Policies · Edit/.test(zt), "…in plain words: what, why, that it's free, and what else it could be", zt);
  await clickBtn(p, `${panel} .zt-note`, "Open Zero Trust");
  check((await opened(p)).at(-1) === "https://one.dash.cloudflare.com/", "…with a link to turn it on");
  await clickBtn(p, `${panel} .zt-note`, "Check again");
  await sleep(900);
  check(await has(p, `${panel} .zt-note`), "Check again asks Cloudflare again (still off: the note stays)");
  check(await fits(p), "nothing sticks out (desktop)");
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    await sleep(200);
    await p.shot(`${SHOTS}/remote-cf-zt-desk-${theme}.png`);
  }
  await p.theme("dark");
  // Set up anyway: it stops at Access with the same words, and Cloudflare's own after them.
  await clickBtn(p, `${panel} form.cf-card .form-actions`, "Set up remote access");
  await p.waitFor(`document.querySelector('${panel} .cf-zones') && !document.querySelector('${panel} .cf-progress')`, 9000).catch(() => {});
  await sleep(300);
  const failed = await text(p, `${panel} .zt-note`);
  check(/Turn on Zero Trust first/.test(failed) && /Cloudflare said: Access is not enabled on this account/.test(failed), "a setup refused at Access: the same plain note, Cloudflare's words after it", failed);
  check(!(await status(c)).tunnel, "…and nothing was kept");

  // ── with Zero Trust on: set up; devices paired before need nothing ──
  await clickBtn(p, `${panel} form.cf-card .form-actions`, "Back");
  await typeInto(p, `${panel} form.cf-card input[type=password]`, "good-token");
  await clickBtn(p, `${panel} form.cf-card .form-actions`, "Check token");
  await p.waitFor(`document.querySelector('${panel} .cf-zones')`, 5000).catch(() => {});
  check(!(await has(p, `${panel} .zt-note`)), "Zero Trust on: no note");
  const seen = c.events.length;
  await clickBtn(p, `${panel} form.cf-card .form-actions`, "Set up remote access");
  await p.waitFor(`document.querySelector('${panel} .cf-done')`, 15000).catch(() => {});
  const done = await text(p, `${panel} .cf-done`);
  check(/Remote access is ready/.test(done), "set up", done.slice(0, 80));
  check(/paired before get the new address by themselves/.test(done) && /No need to pair them again/.test(done), "…and it says devices paired before pick it up themselves", await text(p, `${panel} .cf-devices-follow`));
  check(c.events.slice(seen).some((e) => e.type === "server_route_changed"), "the server tells connected apps their way in changed (server_route_changed)");
  const st = await status(c);
  check(Math.abs((st.tunnel?.key_expires_at ?? 0) - (now() + 365 * DAY)) < 120, "the devices' key: good for a year", st.tunnel);
  await p.shot(`${SHOTS}/remote-cf-done-desk-dark.png`);

  // ── the devices' key: nothing to say while it's fine ──
  await open(p);
  check(!(await has(p, `${panel} .key-line`)), "a key good for months: nothing shown");

  // ── 30 days before: a plain line with Renew ──
  await c.rpc("mock_tunnel", { key_expires_at: now() + 12 * DAY + 600 });
  await open(p);
  const soon = await text(p, `${panel} .key-line`);
  check((await p.eval(`document.querySelector('${panel} .key-line')?.dataset.state`)) === "soon" && /runs out in 13 days|runs out in 12 days/.test(soon) && /keep working away from home/.test(soon), "12 days left: says when, and why it matters", soon);
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    await sleep(200);
    await p.shot(`${SHOTS}/remote-cf-soon-desk-${theme}.png`);
  }
  await p.theme("dark");
  await clickBtn(p, `${panel} .key-line`, "Renew");
  check(await has(p, `${panel} form.cf-renew`), "Renew opens the renewal form");
  const form = await text(p, `${panel} form.cf-renew`);
  check(/Access: Service Tokens/.test(form) && /never stored/.test(form) && /nothing to do on them/.test(form), "…one permission, never stored, nothing to do on the devices", form.slice(0, 260));
  check(!(await has(p, `${panel} .key-line`)), "…in place of the line");
  await p.shot(`${SHOTS}/remote-cf-renew-desk-dark.png`);
  await clickBtn(p, `${panel} form.cf-renew .form-actions`, "Renew");
  check(/Paste the token/.test(await text(p, `${panel} form.cf-renew .form-error`)), "no token: says to paste one");
  await typeInto(p, `${panel} form.cf-renew input[type=password]`, "bad-token");
  await clickBtn(p, `${panel} form.cf-renew .form-actions`, "Renew");
  await sleep(500);
  check(/Cloudflare rejected/.test(await text(p, `${panel} form.cf-renew .form-error`)), "a token Cloudflare refuses: its reason");
  check((await p.eval(`document.querySelector('${panel} form.cf-renew input').value`)) === "", "…and the field is emptied");
  await typeInto(p, `${panel} form.cf-renew input[type=password]`, "gone-token");
  await clickBtn(p, `${panel} form.cf-renew .form-actions`, "Renew");
  await sleep(900);
  check(/no longer has the key/.test(await text(p, `${panel} form.cf-renew .form-error`)), "a key deleted in Cloudflare: said plainly, with what to do");
  await typeInto(p, `${panel} form.cf-renew input[type=password]`, "good-token");
  await clickBtn(p, `${panel} form.cf-renew .form-actions`, "Renew");
  await p.waitFor(`!document.querySelector('${panel} form.cf-renew')`, 4000).catch(() => {});
  const toast = await p.eval(`[...document.querySelectorAll('.toast')].map((t) => t.textContent).join(' | ')`);
  check(/Renewed: your devices' key is good until/.test(toast), "renewed: a toast says until when", toast);
  check(!(await has(p, `${panel} .key-line`)) && Math.abs((await status(c)).tunnel.key_expires_at - (now() + 365 * DAY)) < 120, "…the line is gone and the key is good for another year");

  // ── run out: renew by hand in Cloudflare, then "I renewed it" ──
  await c.rpc("mock_tunnel", { key_expires_at: now() - 2 * DAY });
  await open(p);
  const gone = await text(p, `${panel} .key-line`);
  check((await p.eval(`document.querySelector('${panel} .key-line')?.dataset.state`)) === "expired" && /ran out on/.test(gone) && /can't connect/.test(gone) && /work again/.test(gone), "run out: says so, what it means and what fixes it", gone);
  await p.shot(`${SHOTS}/remote-cf-expired-desk-dark.png`);
  await clickBtn(p, `${panel} .key-line`, "Renew");
  await clickBtn(p, `${panel} form.cf-renew`, "Or renew it in Cloudflare yourself");
  const hand = await p.eval(`[...document.querySelectorAll('${panel} .cf-byhand li')].map((l) => l.textContent.trim())`);
  check(hand.length === 3 && /Zero Trust → Service Tokens/.test(hand[0]) && /Tessera apps \(tessera\.example\.com\)/.test(hand[1]) && /Refresh/.test(hand[1]), "by hand: where, which one (by its name), and Refresh", hand);
  await clickBtn(p, `${panel} .cf-byhand li:first-child`, "Zero Trust → Service Tokens");
  check((await opened(p)).at(-1) === "https://one.dash.cloudflare.com/?to=/:account/access/service-auth/service-tokens", "…its link opens the Service Tokens page");
  await p.shot(`${SHOTS}/remote-cf-byhand-desk-dark.png`);
  await clickBtn(p, `${panel} .cf-byhand`, "I renewed it");
  await p.waitFor(`!document.querySelector('${panel} form.cf-renew')`, 4000).catch(() => {});
  check(!(await has(p, `${panel} .key-line`)) && (await status(c)).tunnel.key_expires_at > now() + 360 * DAY, "\"I renewed it\": Tessera counts a year from now and stops reminding");

  // ── a tunnel set up before Tessera kept the date ──
  await c.rpc("mock_tunnel", { key_expires_at: null });
  await open(p);
  const unknown = await text(p, `${panel} .key-line`);
  check((await p.eval(`document.querySelector('${panel} .key-line')?.dataset.state`)) === "unknown" && /lasts a year/.test(unknown) && /Renew it once/.test(unknown), "date unknown: a gentle line to renew once", unknown);

  // ── the open-source licences: the full texts travel inside the app ──
  await ui(p, `u.openSettings('help')`);
  await p.waitFor(`document.querySelector('.help-legal')`, 8000).catch(() => {});
  await p.eval(`(() => { const b = [...document.querySelectorAll('.help-legal .link-btn')].find((b) => b.textContent.trim() === 'Open-source licences'); b.scrollIntoView({ block: 'center' }); b.click(); })()`);
  await p.waitFor(`document.querySelector('.guide-modal .licence-texts')`, 8000).catch(() => {});
  await clickBtn(p, ".guide-modal .licence-texts", "The full licence texts in this app");
  await p.waitFor(`document.querySelectorAll('.guide-modal .licence-list pre').length === 3`, 8000).catch(() => {});
  const lic = await p.eval(`[...document.querySelectorAll('.guide-modal .licence-list section')].map((s) => ({ title: s.querySelector('h3').textContent, size: s.querySelector('pre').textContent.length, head: s.querySelector('pre').textContent.slice(0, 400) }))`);
  check(
    lic.length === 3 && /Apache License/.test(lic[0].head) && /The Tessera Authors/.test(lic[1].head) && lic[2].size > 100_000 && /third-party software/.test(lic[2].head),
    "Open-source licences: the app carries Tessera's licence, its NOTICE and every third-party licence text",
    lic.map((l) => ({ title: l.title, size: l.size })),
  );
  await p.eval(`document.querySelector('.guide-modal .licence-texts').scrollIntoView({ block: 'start' })`);
  await sleep(300);
  await p.shot(`${SHOTS}/remote-cf-licences-desk-dark.png`);
  await p.key("Escape", "Escape", 0, 27);
  await sleep(300);
  check(p.errors.length === 0, "desktop: no errors", p.errors);
  await p.close();

  // ── the folded phone and the unfolded Fold, dark and light ──
  await c.rpc("mock_tunnel", { key_expires_at: now() + 5 * DAY });
  for (const [name, w, h] of [["cover", 344, 882], ["fold", 673, 841]]) {
    for (const theme of ["dark", "light"]) {
      const q = await device();
      await q.viewport(w, h, true, 2.625);
      await signIn(q, `CF ${name} ${theme}`);
      await q.theme(theme);
      await open(q);
      check(/runs out in 5 days|runs out in 6 days/.test(await text(q, `${panel} .key-line`)) && (await fits(q)), `${name} ${theme}: the key line fits`);
      await q.shot(`${SHOTS}/remote-cf-soon-${name}-${theme}.png`);
      await clickBtn(q, `${panel} .key-line`, "Renew");
      await clickBtn(q, `${panel} form.cf-renew`, "Or renew it in Cloudflare yourself");
      await q.eval(`document.querySelector('${panel} form.cf-renew').scrollIntoView({ block: 'start' })`);
      await sleep(300);
      check(await fits(q), `${name} ${theme}: the renewal form fits`);
      await q.shot(`${SHOTS}/remote-cf-renew-${name}-${theme}.png`);
      check(q.errors.length === 0, `${name} ${theme}: no errors`, q.errors);
      await ui(q, `u.openSettings('help')`);
      await q.waitFor(`document.querySelector('.help-legal')`, 8000);
      await clickBtn(q, '.help-legal', 'Open-source licences');
      await q.waitFor(`document.querySelector('.guide-modal .licence-texts')`, 8000);
      await clickBtn(q, '.guide-modal .licence-texts', 'The full licence texts in this app');
      await q.waitFor(`document.querySelectorAll('.guide-modal .licence-list pre').length === 3`, 8000);
      check(await q.eval(`(() => { const b = document.querySelector('.guide-modal'); return b.scrollWidth <= b.clientWidth + 1 && b.getBoundingClientRect().right <= innerWidth + 1; })()`), `${name} ${theme}: full licence texts fit, without widening the page`);
      await q.eval(`document.querySelector('.licence-list').scrollIntoView({ block: 'start' })`);
      await q.shot(`${SHOTS}/remote-cf-licences-${name}-${theme}.png`);
      await q.eval(`history.back()`);
      await sleep(300);
      check(!(await has(q, '.guide-modal')) && await has(q, '.workspace'), `${name} ${theme}: back closes licences, not the app`);
      await q.close();
    }
  }
  // The Zero Trust note on the cover screen.
  await c.rpc("mock_tunnel", { remove: true });
  {
    const q = await device();
    await q.viewport(344, 882, true, 2.625);
    await signIn(q, "CF cover zt");
    await q.theme("dark");
    await open(q);
    await clickBtn(q, panel, "Set it up with Cloudflare");
    await typeInto(q, `${panel} form.cf-card input[type=password]`, "nozt-token");
    await clickBtn(q, `${panel} form.cf-card .form-actions`, "Check token");
    await q.waitFor(`document.querySelector('${panel} .zt-note')`, 5000).catch(() => {});
    await q.eval(`document.querySelector('${panel} .zt-note').scrollIntoView({ block: 'center' })`);
    await sleep(300);
    check((await has(q, `${panel} .zt-note`)) && (await fits(q)), "cover: the Zero Trust note fits");
    await q.shot(`${SHOTS}/remote-cf-zt-cover-dark.png`);
    await q.close();
  }

  // ── the desktop app: the shell updated a saved server's addresses; Keep Tessera running ──
  const token = (await (await fetch(MOCK + "/api/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "123456", device_name: "Desk app" }) })).json()).token;
  // Phone shell: repeated saved-list notifications never reload the page,
  // cut a good connection, replace the composer or lose an unsent message.
  for (const [name, w, h] of [['cover', 344, 882], ['fold', 673, 841]]) {
    for (const theme of ['dark', 'light']) {
      const q = await device();
      await q.viewport(w, h, true, 2.625);
      await q.send('Emulation.setUserAgentOverride', { userAgent: 'Mozilla/5.0 (Linux; Android 16; SM-F966B; wv) AppleWebKit/537.36 Chrome/139.0.0.0 Mobile Safari/537.36' });
      await q.send('Page.addScriptToEvaluateOnNewDocument', { source: shellScript(token, true) });
      await q.goto(APP, 1800);
      await q.waitFor(`document.querySelector('.workspace') && document.querySelector('.composer textarea')`, 12000);
      await q.theme(theme);
      await q.eval(`(() => { const e = document.querySelector('.composer textarea'); e.value = 'Keep my unsent phone message'; e.dispatchEvent(new Event('input', { bubbles: true })); window.__kept = { workspace: document.querySelector('.workspace'), composer: e, start: performance.timeOrigin, connect: window.__calls.filter(c => c.cmd === 'server_connect' && !c.args.keep).length }; })()`);
      check(await q.eval(`window.__calls.filter(c => c.cmd === 'server_connect').every(c => c.args.keep === true)`), `${name} ${theme}: WebView startup adopts the shell connection rather than replacing it`);
      await q.eval(`__mod('lib/backend.ts').then(m => m.backend.connect())`);
      await q.eval(`window.__shell.learn(); for (let i = 0; i < 12; i++) { window.__shell.learn(); window.__shell.notify(); }`);
      await sleep(800);
      const kept = await q.eval(`(() => { const k = window.__kept; return { workspace: k.workspace.isConnected, composer: k.composer.isConnected, draft: k.composer.value, samePage: k.start === performance.timeOrigin, connects: window.__calls.filter(c => c.cmd === 'server_connect' && !c.args.keep).length - k.connect, sockets: window.__socketCount }; })()`);
      check(kept.workspace && kept.composer && kept.samePage && kept.draft === 'Keep my unsent phone message' && kept.connects === 0 && kept.sockets === 1, `${name} ${theme}: repeated route notifications keep the page, draft and live connection`, kept);
      await ui(q, `u.openSettings('servers')`);
      await q.waitFor(`document.querySelectorAll('.server-card .endpoint-list li').length === 2`, 6000);
      await q.eval(`document.querySelector('.server-card').scrollIntoView({ block: 'start' })`);
      await q.shot(`${SHOTS}/remote-cf-app-route-${name}-${theme}.png`);
      const cfg = await q.eval(`__mod('lib/backend.ts').then(m => m.backend.getConfig())`);
      const mobileShell = await q.eval(`__mod('lib/util.ts').then(async m => m.isTauriMobile && !(await window.__TAURI_INTERNALS__.invoke('local_server_detect')).available)`);
      check(mobileShell, `${name} ${theme}: Android WebView is identified as mobile, with no local server`);
      check(cfg.servers[0].routeRev === 1 && cfg.servers[0].headers['CF-Access-Client-Secret'] === 's', `${name} ${theme}: repeated route learns are idempotent and keep Access headers`, cfg.servers[0].routeRev);
      check(!q.errors.length, `${name} ${theme}: phone shell has no page errors`, q.errors);
      await q.close();
    }
  }
  for (const theme of ["dark", "light"]) {
    const d = await device();
    await d.viewport(1280, 860);
    await d.send("Page.addScriptToEvaluateOnNewDocument", { source: shellScript(token) });
    await d.goto(APP, 1800);
    await d.waitFor(`document.querySelector('.workspace')`, 12000);
    await d.theme(theme);
    await ui(d, `u.openSettings('servers')`);
    await d.waitFor(`document.querySelector('.server-card .endpoint-list')`, 6000).catch(() => {});
    const eps = () => d.eval(`[...document.querySelectorAll('.server-card .endpoint-list li')].map((l) => l.textContent.trim())`);
    check((await eps()).length === 1, `app ${theme}: one saved address to begin with`, await eps());
    // The shell learned the server's remote address (follow_route) and says so.
    await d.eval(`window.__shell.learn()`);
    await sleep(600);
    const after = await eps();
    check(after.length === 2 && after.some((e) => /tessera\.example\.com/.test(e) && /Away/.test(e)), `app ${theme}: the new away-from-home address shows at once, without pairing again`, after);
    await d.eval(`document.querySelector('.server-card').scrollIntoView({ block: 'start' })`);
    await d.shot(`${SHOTS}/remote-cf-app-route-${theme}.png`);
    // A save from the page after that (its name here) round-trips the shell's count.
    await d.eval(`__mod('lib/app.ts').then((m) => m.rememberDeviceName('Desk app (renamed)'))`);
    await sleep(400);
    const saved = await d.eval(`window.__saved`);
    check(saved?.servers?.[0]?.routeRev === 1, `app ${theme}: saves from the page carry the shell's count (the new addresses can't be undone by an old copy)`, saved?.servers?.[0]);

    // Settings → Devices → this computer: Keep Tessera running.
    await ui(d, `u.openSettings('thiscomputer')`);
    await d.waitFor(`document.querySelector('.keep-running')`, 8000).catch(() => {});
    const k = () => d.eval(`(() => { const s = document.querySelector('.keep-running'); return s && { title: s.querySelector('.setting-title')?.textContent, help: s.querySelector('.setting-help')?.textContent, on: s.querySelector('.toggle')?.getAttribute('aria-checked') === 'true', sec: s.querySelector('.dev-sec-title')?.textContent }; })()`);
    let kr = await k();
    check(kr?.sec === "Tessera on this computer" && kr.title === "Keep Tessera running: start it when I sign in to Windows" && kr.on, `app ${theme}: this computer has “Keep Tessera running”, on (as setup left it)`, kr);
    check(/stays awake while it's plugged in/.test(kr?.help ?? ""), "…explained as in setup", kr?.help);
    await d.eval(`document.querySelector('.keep-running').scrollIntoView({ block: 'center' })`);
    await sleep(200);
    await d.shot(`${SHOTS}/remote-cf-keep-on-${theme}.png`);
    await d.eval(`document.querySelector('.keep-running .toggle').click()`);
    await sleep(150);
    check(/restarts once/.test(await text(d, ".keep-running .keep-busy")), "turning it off: says Tessera restarts once and chats carry on");
    await d.waitFor(`document.querySelector('.keep-running .toggle')?.getAttribute('aria-checked') === 'false'`, 4000).catch(() => {});
    kr = await k();
    const calls = await d.eval(`window.__calls.filter((c) => c.cmd === 'local_server_set_keep_running').map((c) => c.args.on)`);
    check(!kr?.on && JSON.stringify(calls) === "[false]" && /until you restart or shut down this PC/.test(kr?.help ?? ""), "off: one call to the shell, and it says what that means", { kr, calls });
    await d.shot(`${SHOTS}/remote-cf-keep-off-${theme}.png`);
    await d.eval(`document.querySelector('.keep-running .toggle').click()`);
    await d.waitFor(`document.querySelector('.keep-running .toggle')?.getAttribute('aria-checked') === 'true'`, 4000).catch(() => {});
    check((await k())?.on, "…and back on");
    check(!d.errors.length, `app ${theme}: no page errors`, d.errors);
    await d.close();
  }

  c.close();
  finish();
});

/**
 * A stand-in for the desktop app's shell (Tauri): the app's own commands go to the mock over a
 * WebSocket; the saved server is on this computer, so the server here can be kept running.
 * `window.__shell.learn()` plays `follow_route`: the server's remote address and Access key arrive,
 * the saved server changes, and the shell says so (`tessera://servers`).
 */
function shellScript(token, phone = false) {
  return `(() => {
    const TOKEN = ${JSON.stringify(token)};
    let n = 0, ws = null, rid = 0;
    const cbs = {}, listeners = {}, pending = new Map();
    Object.defineProperty(Navigator.prototype, "platform", { get: () => ${JSON.stringify(phone ? "Linux armv8l" : "Win32")} });
    window.__socketCount = 0;
    window.__calls = [];
    const HOME = ${JSON.stringify(phone ? 'http://192.168.1.10:7878' : new URL(APP).origin)};
    let server = { id: "srv_here", name: ${JSON.stringify(phone ? 'Tessera' : 'This PC')}, endpoints: [HOME], headers: {}, token: "", serverKey: "", fingerprint: "1111 2222", deviceName: ${JSON.stringify(phone ? 'Galaxy Fold' : 'Desk app')}, routeRev: 0 };
    let keep = { supported: true, installed: true, running: true, system: false };
    const emit = (event, payload) => (listeners[event] || []).forEach((id) => cbs[id] && cbs[id]({ event, id: 1, payload }));
    const status = (state) => emit("tessera://status", { state, serverId: "srv_here", endpoint: HOME, error: null, encrypted: true });
    window.__shell = {
      learn() {
        if (server.endpoints.includes('https://tessera.example.com')) return;
        server = { ...server, endpoints: [...server.endpoints, "https://tessera.example.com"], headers: { "CF-Access-Client-Id": "id.access", "CF-Access-Client-Secret": "s" }, routeRev: server.routeRev + 1 };
        emit("tessera://servers", null);
      },
      notify() { emit('tessera://servers', null); },
    };
    function connect() {
      window.__socketCount++;
      ws = new WebSocket(location.origin.replace(/^http/, "ws") + "/api/ws?token=" + encodeURIComponent(TOKEN));
      ws.onopen = () => status("connected");
      ws.onclose = () => status("disconnected");
      ws.onmessage = (m) => {
        const msg = JSON.parse(m.data);
        if (msg.event) emit("tessera://event", msg.event);
        else if (pending.has(msg.id)) { const p = pending.get(msg.id); pending.delete(msg.id); msg.error ? p.reject(msg.error) : p.resolve(msg.result ?? null); }
      };
    }
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
      transformCallback(cb, once) { const id = ++n; cbs[id] = (x) => { if (once) delete cbs[id]; cb(x); }; return id; },
      async invoke(cmd, args = {}) {
        window.__calls.push({ cmd, args: JSON.parse(JSON.stringify(args)) });
        switch (cmd) {
          case "plugin:event|listen": (listeners[args.event] ||= []).push(args.handler); return args.handler;
          case "plugin:event|unlisten": return null;
          case "servers_get": return { servers: [JSON.parse(JSON.stringify(server))], active: "srv_here" };
          case "servers_save": window.__saved = args.config; { const s = args.config.servers.find((x) => x.id === "srv_here"); if (s && s.routeRev === server.routeRev) server = { ...server, deviceName: s.deviceName, name: s.name }; } return null;
          case "server_connect": if (!ws) connect(); else status("connected"); return null;
          case "server_disconnect": return null;
          case "app_resumed": return null;
          case "device_report": return ${JSON.stringify(phone ? { computer: null, host: null, manufacturer: "samsung", model: "SM-F966B" } : { computer: "k-desk-cf", host: "DESKTOP-CF", manufacturer: null, model: null })};
          case "rpc": return new Promise((resolve, reject) => { const id = ++rid; pending.set(id, { resolve, reject }); const r = args.request; ws.send(JSON.stringify({ id, method: r.method, ...(r.params !== undefined ? { params: r.params } : {}) })); });
          case "local_server_detect": return { available: ${!phone}, running: ${!phone}, url: HOME, saved: true };
          case "local_server_keep_running": return { ...keep };
          case "local_server_set_keep_running": await new Promise((r) => setTimeout(r, 600)); keep = { ...keep, installed: !!args.on }; return { ...keep };
          default: throw "command " + cmd + " not found";
        }
      },
    };
  })()`;
}
