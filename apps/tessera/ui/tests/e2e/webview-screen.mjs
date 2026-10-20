// Finishing a site's check yourself: a web view on one of your computers (a machine with "Web views
// here") can be shown on that computer's screen in a normal browser window ("Show on Desktop's screen");
// the tile waits with a plain note and "Back to the tile", and comes back by itself when that window is
// closed there. A web view on the server says plainly why it can't, and offers your own browser instead.
// Desktop and the folded Fold's cover screen, light and dark.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const call = (p, method, params) => p.eval(`__mod('lib/store.ts').then(s => s.call({ method: ${JSON.stringify(method)}, params: ${JSON.stringify(params)} })).then(r => r, e => ({ error: String(e?.message ?? e) }))`);
const A_SID = "ses_7d3a91e5c4b2";
const waitFor = async (fn, ms = 6000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const v = await fn();
    if (v) return v;
    await sleep(100);
  }
  return null;
};
const note = (p, pane) =>
  p.eval(`(() => { const n = document.querySelector('[data-pane-id="${pane}"] .wv-on-screen'); return n && n.getClientRects().length ? { title: n.querySelector('h3')?.textContent ?? '', text: n.querySelector('p')?.textContent ?? '', button: n.querySelector('button')?.textContent.trim() ?? '' } : null; })()`);
const menuItems = (p) => p.eval(`[...document.querySelectorAll('.menu-item')].map((e) => ({ label: e.querySelector('.menu-label')?.textContent ?? e.textContent.trim(), disabled: e.disabled || e.getAttribute('aria-disabled') === 'true' }))`);
const openMenu = async (p, pane) => {
  await p.click(`[data-pane-id="${pane}"] button[aria-label="Web view options"]`);
  await sleep(300);
};

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const probe = await wsClient("probe");

  // ── a web view on the desktop (a machine with web views) ──
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Desktop");
  const list = await call(p, "mock_machines", { me: { name: "Desktop" } });
  const me = (Array.isArray(list) ? list : []).find((m) => m.name === "Desktop");
  await ws(p, `m.openSession('${A_SID}', { pane: m.workspace.focused() })`);
  await sleep(600);
  await p.click(`.topbar .tb-new-more`);
  await sleep(250);
  await p.clickText(".menu-item", "Web view");
  const W = await waitFor(() => ws(p, `return m.workspace.order().find(id => m.workspace.panes[id].content.type === 'draft')`));
  if (!W || !await waitFor(() => p.eval(`!!document.querySelector('[data-pane-id="${W}"] .wv-input')?.getClientRects().length`))) {
    throw new Error("desktop web-view fixture's address input did not become ready");
  }
  await p.click(`[data-pane-id="${W}"] .wv-input`);
  await p.type("example.com/login");
  await p.key("Enter", "Enter");
  await sleep(1200);
  const B = (await probe.rpc("list_boards", { session_id: A_SID })).result.find((b) => b.content.type === "url" && b.host === me?.id);
  check(!!B, "a web view running on the desktop", B?.host);
  await waitFor(() => p.eval(`!!document.querySelector('[data-pane-id="${W}"] .wv-canvas.shown')`));

  await openMenu(p, W);
  const items = await menuItems(p);
  const item = items.find((i) => /Show on Desktop's screen/.test(i.label));
  check(!!item && !item.disabled, "its options offer “Show on Desktop's screen”", items.map((i) => i.label));
  await p.shot(`${SHOTS}/webview-screen-menu.png`);
  await p.clickText(".menu-item", "Show on Desktop's screen");
  let n = await waitFor(() => note(p, W));
  check(!!n && /Open on Desktop's screen/.test(n.title) && /close that window/.test(n.text) && /Back to the tile/.test(n.button), "the tile says where the page is and how to come back", n);
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    await sleep(200);
    await p.shot(`${SHOTS}/webview-screen-desk-${theme}.png`);
  }
  await p.theme("dark");
  await openMenu(p, W);
  check((await menuItems(p)).find((i) => /Show on Desktop's screen/.test(i.label))?.disabled === true, "…and doesn't offer it twice");
  await p.key("Escape");
  await sleep(200);
  await p.clickText(`[data-pane-id="${W}"] .wv-on-screen button`, "Back to the tile");
  await sleep(600);
  check(!(await note(p, W)), "Back to the tile: the page is here again");

  // Closed there by the user: the tile comes back by itself.
  await openMenu(p, W);
  await p.clickText(".menu-item", "Show on Desktop's screen");
  await waitFor(() => note(p, W));
  await sleep(2300);
  await probe.rpc("mock_screen_closed", { board_id: B.id });
  check(!!(await waitFor(async () => !(await note(p, W)))), "closed on the desktop: the tile carries on by itself");

  // ── a page that wants a "prove you're a person" check: the tile says so, with the way out ──
  await probe.rpc("mock_browser_check", { board_id: B.id, check: true });
  const banner = await waitFor(() => p.eval(`(() => { const b = document.querySelector('[data-pane-id="${W}"] .wv-check'); return b && b.getClientRects().length ? { text: b.textContent, buttons: [...b.querySelectorAll('button')].map((x) => x.textContent.trim() || x.getAttribute('aria-label')) } : null; })()`));
  check(!!banner && /check you're a person/.test(banner.text) && banner.buttons.includes("Show on Desktop's screen"), "a check on the page: the tile says so and offers the screen", banner);
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    await sleep(200);
    await p.shot(`${SHOTS}/webview-screen-check-desk-${theme}.png`);
  }
  await p.theme("dark");
  await p.clickText(`[data-pane-id="${W}"] .wv-check button`, "Show on Desktop's screen");
  check(!!(await waitFor(() => note(p, W))), "…and one click shows it there");
  check(!(await p.eval(`!!document.querySelector('[data-pane-id="${W}"] .wv-check')`)), "…the hint gives way to the note");
  await p.clickText(`[data-pane-id="${W}"] .wv-on-screen button`, "Back to the tile");
  await sleep(600);
  await p.click(`[data-pane-id="${W}"] .wv-check button[aria-label="Dismiss"]`);
  await sleep(200);
  check(!(await p.eval(`!!document.querySelector('[data-pane-id="${W}"] .wv-check')`)), "the hint can be dismissed");
  await probe.rpc("mock_browser_check", { board_id: B.id, check: false });

  // ── a web view on the server: no screen there, said plainly ──
  const m = await device();
  await m.viewport(344, 882, true, 2.625);
  await signIn(m, "Phone");
  const made = await call(m, "create_board", { session_id: A_SID, title: "Shop", placed: false, content: { type: "url", url: "https://example.org/" } });
  const P = await ws(m, `return m.openBoard('${made.id}')`);
  await sleep(1200);
  await waitFor(() => m.eval(`!!document.querySelector('[data-pane-id="${P}"] button[aria-label="Web view options"]')`));
  await openMenu(m, P);
  const phoneItems = await menuItems(m);
  check(phoneItems.some((i) => /Finish a check yourself/.test(i.label)) && !phoneItems.some((i) => /screen/.test(i.label)), "on the server's web view: “Finish a check yourself…”", phoneItems.map((i) => i.label));
  await m.clickText(".menu-item", "Finish a check yourself");
  await sleep(500);
  const d = await m.eval(`document.querySelector('.dialog-body') ? { body: document.querySelector('.dialog-text')?.textContent ?? '', ok: document.querySelector('.dialog-actions button[type=submit]')?.textContent.trim() ?? '' } : null`);
  check(!!d && /no screen/.test(d.body) && /your own browser/.test(d.body) && /Web views here/.test(d.body) && /Open in your browser/.test(d.ok), "it says why not, and what works instead", d);
  for (const theme of ["dark", "light"]) {
    await m.theme(theme);
    await sleep(200);
    await m.shot(`${SHOTS}/webview-screen-server-cover-${theme}.png`);
  }
  check(await m.eval(`document.documentElement.scrollWidth <= innerWidth + 1`), "the explanation fits the cover screen");
  await m.click(".dialog-actions .btn-ghost");
  await sleep(300);
  // A check on a page there: the hint offers to finish it yourself, and fits the cover screen.
  await waitFor(() => m.eval(`!!document.querySelector('[data-pane-id="${P}"] .wv-canvas.shown')`));
  await probe.rpc("mock_browser_check", { board_id: made.id, check: true });
  const hint = await waitFor(() => m.eval(`(() => { const b = document.querySelector('[data-pane-id="${P}"] .wv-check'); if (!b || !b.getClientRects().length) return null; const r = b.getBoundingClientRect(); return { fits: r.right <= innerWidth + 1 && r.left >= 0, buttons: [...b.querySelectorAll('button')].map((x) => x.textContent.trim()) }; })()`));
  check(!!hint && hint.fits && hint.buttons.includes("Finish it yourself"), "on the server's web view: “Finish it yourself”, and it fits the cover screen", hint);
  for (const theme of ["dark", "light"]) {
    await m.theme(theme);
    await sleep(200);
    await m.shot(`${SHOTS}/webview-screen-check-cover-${theme}.png`);
  }
  const e = await call(m, "browser_show_on_screen", { board_id: made.id, show: true });
  check(/no screen to show it on/.test(e?.error ?? ""), "and the server refuses it there too", e);
  await m.close();
  await p.close();
  probe.close?.();
  finish();
});
