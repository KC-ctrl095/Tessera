// Web views on a machine ("Web views here"): a web view made on a device
// that is also a machine runs there ("on Desktop" in its bar), a phone shows
// it the same (relayed), a web view made on the phone stays on the server,
// and when the machine goes offline the tile says so and offers to open the
// page on the server instead. Desktop and phone, light and dark.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const call = (p, method, params) => p.eval(`__mod('lib/store.ts').then(s => s.call({ method: ${JSON.stringify(method)}, params: ${JSON.stringify(params)} })).then(r => r, e => ({ error: String(e?.message ?? e) }))`);
const A_SID = "ses_7d3a91e5c4b2";
const waitFor = async (fn, ms = 5000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const v = await fn();
    if (v) return v;
    await sleep(100);
  }
  return null;
};
const tile = (p, pane) =>
  p.eval(`(() => { const t = document.querySelector('[data-pane-id="${pane}"]'); if (!t) return null; const h = t.querySelector('.wv-host');
    const down = t.querySelector('.wv-host-down'); return { host: h?.textContent.trim() ?? null, hostTip: h?.getAttribute('data-tip') ?? null, hostDown: h?.classList.contains('down') ?? false,
    card: down ? { title: down.querySelector('h3')?.textContent, text: down.querySelector('p')?.textContent, button: down.querySelector('button')?.textContent.trim() } : null,
    shown: !!t.querySelector('.wv-canvas.shown') }; })()`);

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const probe = await wsClient("probe");

  // ── the desktop is a machine with web views ───────────────────────────
  console.log("— made on a machine");
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Desktop");
  const list = await call(p, "mock_machines", { me: { name: "Desktop" } });
  const me = (Array.isArray(list) ? list : []).find((m) => m.name === "Desktop");
  check(!!me && me.scope.webviews === true, "this device is the machine Desktop, with web views on", me?.scope);
  await ws(p, `m.openSession('${A_SID}', { pane: m.workspace.focused() })`);
  await sleep(600);
  await p.click(`.topbar .tb-new-more`);
  await sleep(250);
  await p.clickText(".menu-item", "Web view");
  await sleep(500);
  const W = await ws(p, `return m.workspace.order().find(id => m.workspace.panes[id].content.type === 'draft')`);
  await p.type("example.com");
  await p.key("Enter", "Enter");
  await sleep(1200);
  const boards = (await probe.rpc("list_boards", { session_id: A_SID })).result.filter((b) => b.content.type === "url");
  const B = boards[0];
  check(B && B.host === me?.id, "the web view runs on the machine it was made on (its host)", B && { host: B.host, me: me?.id });
  let t = await waitFor(async () => {
    const x = await tile(p, W);
    return x?.host && x.shown ? x : null;
  });
  check(t?.host === "on Desktop", "its bar says where it runs: “on Desktop”", t);
  check(/runs in the browser on Desktop/.test(t?.hostTip ?? "") && /stay on that computer/.test(t?.hostTip ?? ""), "…and, on hover, that sign-ins stay on that computer", t?.hostTip);
  for (const theme of ["light", "dark"]) {
    await p.theme(theme);
    await sleep(250);
    const box = await p.eval(`(() => { const r = document.querySelector('[data-pane-id="${W}"] .wv-bar').getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height + 120 }; })()`);
    await p.shot(`${SHOTS}/webview-host-desktop-${theme}.png`, box);
  }

  // ── a phone shows it the same, relayed ────────────────────────────────
  console.log("— shown on a phone");
  const m = await device();
  await m.viewport(412, 915, true, 2.625);
  await signIn(m, "Phone");
  const P = await ws(m, `return m.openBoard('${B.id}')`);
  await ws(m, `m.focusPane('${P}')`);
  await sleep(800);
  const pt = await waitFor(async () => {
    const x = await tile(m, P);
    return x?.shown ? x : null;
  });
  check(pt?.host === "on Desktop" && pt.shown, "the phone shows the desktop's web view, saying where it runs", pt);
  await m.theme("dark");
  await sleep(1000);
  await m.shot(`${SHOTS}/webview-host-phone-dark.png`);

  // ── made on the phone: the server's ───────────────────────────────────
  console.log("— made on a phone");
  const made = await call(m, "create_board", { session_id: A_SID, title: "Docs", content: { type: "url", url: "https://example.com/" }, placed: true });
  check(made && !made.error && made.host === undefined, "a web view made on the phone (not a machine) runs on the server", made);
  const P2 = await ws(m, `return m.openBoard('${made.id}')`);
  await sleep(800);
  const t2 = await tile(m, P2);
  check(t2 && t2.host === null, "…and its bar shows no host", t2);
  await ws(m, `m.closePane('${P2}')`);

  // ── the machine goes offline ──────────────────────────────────────────
  console.log("— offline");
  await probe.rpc("mock_machines", { online: { id: me.id, online: false } });
  t = await waitFor(async () => {
    const x = await tile(p, W);
    return x?.card ? x : null;
  });
  check(t?.card?.title === "Desktop is offline", "the tile says plainly that Desktop is offline", t?.card);
  check(/comes back when Desktop can show it again/.test(t?.card?.text ?? "") && /aren't signed in there/.test(t?.card?.text ?? ""), "…what that means, and that sign-ins don't follow", t?.card?.text);
  check(/^Open on /.test(t?.card?.button ?? ""), "…and offers to open it on the server", t?.card?.button);
  check(t?.hostDown === true, "the host hint turns to a warning colour");
  const attach = await call(p, "browser_attach", { board_id: B.id, width: 800, height: 600, dpr: 1, mobile: false, pipelined: true, formats: [] });
  check(/Desktop is offline/.test(attach?.error ?? ""), "attaching while it's offline fails with the reason", attach);
  for (const theme of ["light", "dark"]) {
    await p.theme(theme);
    await sleep(250);
    const box = await p.eval(`(() => { const r = document.querySelector('[data-pane-id="${W}"]').getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; })()`);
    await p.shot(`${SHOTS}/webview-host-offline-${theme}.png`, box);
  }
  await ws(m, `m.focusPane('${P}')`);
  await sleep(1000);
  const pc = await waitFor(async () => (await tile(m, P))?.card);
  check(pc?.title === "Desktop is offline", "the phone's tile says so too", pc);
  await m.shot(`${SHOTS}/webview-host-offline-phone.png`);

  // It comes back by itself when the machine does.
  await probe.rpc("mock_machines", { online: { id: me.id, online: true } });
  t = await waitFor(async () => {
    const x = await tile(p, W);
    return !x?.card && x?.shown ? x : null;
  }, 6000);
  check(t && !t.card && t.shown && t.host === "on Desktop", "back online: the page shows again, on Desktop", t);

  // Offline again, and opened on the server instead.
  await probe.rpc("mock_machines", { online: { id: me.id, online: false } });
  await waitFor(async () => (await tile(p, W))?.card);
  await p.clickText(`[data-pane-id="${W}"] .wv-host-down button`, "Open on");
  t = await waitFor(async () => {
    const x = await tile(p, W);
    return !x?.card && x?.shown && x.host === null ? x : null;
  }, 6000);
  check(t && t.host === null && t.shown, "“Open on …” moves it to the server: the page shows, with no host", t);
  const after = (await probe.rpc("get_board", { id: B.id })).result;
  check(after && after.host === undefined && after.version === B.version + 1, "the board is the server's now", after && { host: after.host, v: after.version });
  const pt2 = await waitFor(async () => {
    const x = await tile(m, P);
    return x && !x.card && x.host === null ? x : null;
  }, 6000);
  check(!!pt2, "the phone follows: no offline card, no host", pt2);

  // A machine with web views off: new web views made there stay on the server.
  await call(p, "mock_machines", { me: { name: "Desktop", webviews: false } });
  const off = await call(p, "create_board", { session_id: A_SID, title: "Off", content: { type: "url", url: "https://example.org/" }, placed: true });
  check(off && !off.error && off.host === undefined, "with web views off on it, the desktop's new web views run on the server", off);

  await m.close();
  await p.close();
  probe.close?.();
  finish();
});
