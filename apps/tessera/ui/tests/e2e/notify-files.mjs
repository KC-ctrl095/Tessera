// Agents' messages and downloadable files: a file an agent shares shows as
// its own card in the chat (never folded into steps) and downloads in one
// tap; an upload can be downloaded again; a web view's downloads appear in
// its tile; an agent's message shows as a toast that opens its chat and is
// marked seen everywhere; Settings → Notifications lists the devices that
// get messages (and turning one off stops them), what is scheduled (with
// Cancel) and a test from the server. Desktop and phone.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const SID = "ses_7d3a91e5c4b2";
const OTHER = "ses_5a3e8f10c2d9";
const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const waitFor = async (fn, ms = 5000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const v = await fn();
    if (v) return v;
    await sleep(80);
  }
  return null;
};
/** Browser downloads: what the page asked to save, as { name, size }. */
const captureDownloads = (p) =>
  p.eval(`(() => {
    window.__dl = [];
    const sizes = {};
    const create = URL.createObjectURL;
    URL.createObjectURL = (b) => {
      const u = create(b);
      sizes[u] = b.size;
      return u;
    };
    const orig = HTMLAnchorElement.prototype.click;
    HTMLAnchorElement.prototype.click = function () {
      if (this.download) return void window.__dl.push({ name: this.download, size: sizes[this.href] });
      return orig.call(this);
    };
  })()`);
const downloaded = (p) => p.eval(`JSON.stringify(window.__dl)`).then((s) => JSON.parse(s));
const toasts = (p) => p.eval(`JSON.stringify([...document.querySelectorAll('.toast')].map(t => t.textContent))`).then((s) => JSON.parse(s));

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const probe = await wsClient("probe");
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Desktop");
  await ws(p, `m.openSession('${SID}', { pane: m.workspace.focused() })`);
  await sleep(800);
  await captureDownloads(p);

  // ── an agent shares a file ────────────────────────────────────────────
  console.log("— share_file");
  const f = (await probe.rpc("mock_share", { session_id: SID, name: "Q3 report.pdf", size: 3 * 1048576 + 5 })).result;
  const card = `.shared-file[data-file="${f.id}"]`;
  const shown = await waitFor(() => p.eval(`(() => { const c = document.querySelector('${card}'); return !!c && c.getBoundingClientRect().height > 30; })()`));
  check(shown, "the shared file shows as its own card in the chat");
  check(!(await p.eval(`!!document.querySelector('.steps ${card}, details ${card}')`)), "not folded into a steps group");
  const meta = await p.eval(`document.querySelector('${card} .shared-file-meta').textContent`);
  check(/3\.0 MB · PDF/.test(meta), "with its size and type", meta);
  await p.eval(`document.querySelector('${card}').scrollIntoView({ block: 'center' })`);
  await sleep(200);
  await p.shot(`${SHOTS}/files-card-desktop.png`);
  await p.click(`${card} .shared-file-get`);
  const got = await waitFor(async () => (await downloaded(p)).find((d) => d.name === "Q3 report.pdf"), 15000);
  check(got && got.size === f.size, "one tap downloads it, whole (in pieces over the connection)", got);
  const after = await waitFor(() => p.eval(`document.querySelector('${card} .shared-file-meta')?.textContent === 'Downloaded'`));
  check(after, "the card then says Downloaded");

  // ── an upload, downloadable again ─────────────────────────────────────
  console.log("— upload");
  await probe.rpc("mock_share", { session_id: SID, name: "plan.pdf", source: "upload" });
  const get = await waitFor(() => p.eval(`(() => { const b = [...document.querySelectorAll('.file-att')].find(a => a.textContent.includes('plan.pdf'))?.querySelector('.file-get'); if (b) { b.scrollIntoView({ block: 'center' }); b.click(); } return !!b; })()`));
  check(get, "the attached file's chip has a download button");
  const up = await waitFor(async () => (await downloaded(p)).find((d) => d.name === "plan.pdf"));
  check(up && up.size === 2048, "and it downloads", up);

  // ── an agent's message ────────────────────────────────────────────────
  console.log("— notify");
  const n1 = (await probe.rpc("mock_notify", { session_id: OTHER, title: "Build finished", body: "All 42 tests pass" })).result;
  const t1 = await waitFor(async () => (await toasts(p)).find((t) => t.includes("Build finished")));
  check(t1 && t1.includes("All 42 tests pass"), "a message from another chat shows as a toast", t1);
  await p.shot(`${SHOTS}/notify-toast-desktop.png`);
  await p.eval(`[...document.querySelectorAll('.toast')].find(t => t.textContent.includes('Build finished')).querySelector('button.btn, .toast-action, button:not([aria-label])')?.click()`);
  const opened = await waitFor(() => ws(p, `const f = m.workspace.focused(); return m.paneSession(f)?.info.id === '${OTHER}'`));
  check(opened, "Open takes you to its chat");
  const seen = await waitFor(async () => (await probe.rpc("list_notifications")).result.find((n) => n.id === n1.id)?.seen);
  check(seen, "and marks it seen on the server (the other devices take theirs away)");
  check(probe.events.some((e) => e.type === "notifications_seen" && e.ids.includes(n1.id)), "every device hears it was seen");

  // ── Settings → Notifications ──────────────────────────────────────────
  console.log("— settings");
  await p.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab('notifications'))`);
  const devices = await waitFor(() => p.eval(`document.querySelectorAll('.notify-devices li').length`));
  check(devices >= 2, "the devices that get agents' messages are listed", devices);
  check(await p.eval(`[...document.querySelectorAll('.notify-devices li')][0].textContent.includes('this device')`), "this device first");
  await p.shot(`${SHOTS}/notify-settings-desktop.png`);
  // Off for this device: no more messages here.
  await p.eval(`document.querySelector('.notify-devices li .toggle').click()`);
  const off = await waitFor(async () => (await probe.rpc("list_devices")).result.find((d) => d.name === "Desktop")?.notify === false);
  check(off, "turning this device off is saved on the server");
  await probe.rpc("mock_notify", { session_id: OTHER, title: "Quiet please", body: "x" });
  await sleep(900);
  check(!(await toasts(p)).some((t) => t.includes("Quiet please")), "and its messages stop");
  await p.eval(`document.querySelector('.notify-devices li .toggle').click()`);
  await waitFor(async () => (await probe.rpc("list_devices")).result.find((d) => d.name === "Desktop")?.notify === true);
  // A test in a minute: scheduled, listed, cancelled.
  // One "Send a test": now, or in a minute.
  check((await p.eval(`document.querySelectorAll('.notify-tests button').length`)) === 1, "one Send a test button");
  await p.eval(`[...document.querySelectorAll('.notify-tests button')].find(b => b.textContent.includes('Send a test')).click()`);
  await waitFor(() => p.eval(`!!document.querySelector('.menu-pop .menu-item')`));
  await p.eval(`[...document.querySelectorAll('.menu-pop .menu-item')].find(b => b.textContent.includes('In a minute')).click()`);
  const sched = await waitFor(async () => (await probe.rpc("list_notifications")).result.find((n) => !n.sent));
  check(sched, "Send a test → In a minute schedules one on the server");
  await p.eval(`__mod('lib/ui.ts').then(m => { m.setSettingsTab(null); setTimeout(() => m.setSettingsTab('notifications'), 50); })`);
  const listed = await waitFor(() => p.eval(`document.querySelectorAll('.notify-scheduled li').length`));
  check(listed === 1, "it is listed under Scheduled", listed);
  await p.eval(`document.querySelector('.notify-scheduled li button').click()`);
  const gone = await waitFor(async () => !(await probe.rpc("list_notifications")).result.some((n) => !n.sent));
  check(gone, "Cancel removes it");
  await p.eval(`[...document.querySelectorAll('.notify-tests button')].find(b => b.textContent.includes('Send a test')).click()`);
  await waitFor(() => p.eval(`!!document.querySelector('.menu-pop .menu-item')`));
  await p.eval(`[...document.querySelectorAll('.menu-pop .menu-item')].find(b => b.textContent.includes('Now')).click()`);
  const test = await waitFor(async () => (await toasts(p)).find((t) => t.includes("Hello from your server")));
  check(test, "Send a test → Now: a message from the server arrives at once");
  await p.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab(null))`);

  // ── a web view download ───────────────────────────────────────────────
  console.log("— web view download");
  const board = (await probe.rpc("create_board", { session_id: SID, content: { type: "url", url: "https://example.com/" } })).result;
  await ws(p, `m.openBoard('${board.id}')`);
  await sleep(800);
  const dl = (await probe.rpc("mock_share", { session_id: SID, board_id: board.id, name: "export.csv", size: 4096, source: "web_view" })).result;
  const bar = await waitFor(() => p.eval(`!!document.querySelector('.wv-downloads .shared-file[data-file="${dl.id}"]')`));
  check(bar, "the tile shows what its page downloaded");
  check(!!(await waitFor(async () => (await toasts(p)).find((t) => t.includes("export.csv")))), "and a toast offers to save it");
  await p.shot(`${SHOTS}/webview-download-desktop.png`);
  await p.click(`.wv-downloads .shared-file[data-file="${dl.id}"] .shared-file-get`);
  const csv = await waitFor(async () => (await downloaded(p)).find((d) => d.name === "export.csv"));
  check(csv && csv.size === 4096, "one tap downloads it here", csv);
  await p.click(`.wv-downloads .shared-file[data-file="${dl.id}"] .shared-file-x`);
  check(await waitFor(() => p.eval(`!document.querySelector('.wv-downloads')`)), "and it can be dismissed");
  await p.close();

  // ── on a phone ────────────────────────────────────────────────────────
  console.log("— phone");
  const ph = await device();
  await ph.viewport(375, 812, true, 3);
  await signIn(ph, "Phone");
  await ws(ph, `m.openSession('${SID}', { pane: m.workspace.focused() })`);
  await sleep(900);
  const fit = await ph.eval(`(() => { const c = document.querySelector('${card}'); if (!c) return null; c.scrollIntoView({ block: 'center' }); const r = c.getBoundingClientRect(); return { left: r.left, right: r.right, w: innerWidth, overflow: document.documentElement.scrollWidth > innerWidth }; })()`);
  check(fit && fit.left >= 0 && fit.right <= fit.w && !fit.overflow, "the file card fits the phone screen", fit);
  await sleep(200);
  await ph.shot(`${SHOTS}/files-card-phone.png`);
  await probe.rpc("mock_notify", { session_id: OTHER, title: "Reminder", body: "Call the dentist" });
  check(!!(await waitFor(async () => (await toasts(ph)).find((t) => t.includes("Reminder")))), "messages show on the phone too");
  await ph.shot(`${SHOTS}/notify-toast-phone.png`);
  await ph.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab('notifications'))`);
  await waitFor(() => ph.eval(`document.querySelectorAll('.notify-devices li').length`));
  await sleep(300);
  await ph.shot(`${SHOTS}/notify-settings-phone.png`);
  const wide = await ph.eval(`document.documentElement.scrollWidth > innerWidth`);
  check(!wide, "the settings fit the phone screen");
  await ph.close();
  probe.close();
  finish();
});
