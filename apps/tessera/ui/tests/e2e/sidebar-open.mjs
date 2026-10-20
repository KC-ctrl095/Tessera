// A chat picked in the side menu opens in a new tile (a tile already showing it just gets the
// focus); its ⋯ menu has one other way, "Open in selected tile" (into the selected tile), instead of
// "Open beside" / "Open below". Bots likewise. The folded phone shows one tile at a time: there a
// tap still replaces the tile on screen, and the menu offers "Open in a new tile".
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const A = "ses_7d3a91e5c4b2"; // "Add a /healthz endpoint"
const B = "ses_91d0c3e7a8b2"; // "Summarize RFC 9110 caching"
const C = "ses_2b7e91c04d5a"; // "Fix flaky websocket reconnect test"
const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const tiles = (p) => ws(p, `return m.workspace.order().length`);
const shows = (p, sid) => ws(p, `return m.paneShowing('${sid}')`);
const focusedChat = (p) => ws(p, `const f = m.workspace.focused(); const c = f ? m.workspace.panes[f]?.content : null; return c?.type === 'chat' ? c.sessionId : null`);
const menuLabels = (p) => p.eval(`[...document.querySelectorAll('.menu-pop .menu-label')].map((e) => e.textContent.trim())`);
/** Wait (up to `ms`) until `fn` says yes: the app answers late on a busy machine. */
const until = async (fn, ms = 4000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    if (await fn()) return true;
    await sleep(100);
  }
  return false;
};
/** Open the side menu and wait for it to finish sliding in. */
const openDrawer = async (p) => {
  await p.eval(`__mod('lib/ui.ts').then((u) => u.setDrawerOpen(true))`);
  await p.waitFor(`document.querySelector('.drawer-layer.open') && new DOMMatrixReadOnly(getComputedStyle(document.querySelector('.drawer-panel')).transform).m41 === 0`, 4000).catch(() => {});
  await sleep(150);
};
const closeMenu = async (p) => {
  await p.key("Escape", "Escape", 0, 27);
  await sleep(250);
};
/** The ⋯ of a chat (or bot) row whose text has `text`, in `root`. */
const moreOf = (p, root, text) =>
  p.eval(`(() => { const r = [...document.querySelectorAll('${root} .srow')].find((e) => e.textContent.includes(${JSON.stringify(text)})); const b = r?.querySelector('.srow-more'); if (!b) return null; b.scrollIntoView({ block: 'nearest' }); const x = b.getBoundingClientRect(); return { x: x.x + x.width / 2, y: x.y + x.height / 2 }; })()`);

run(async () => {
  // ── desktop ──
  {
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, "Side menu desktop");
    await ws(p, `return m.openSession('${A}', { pane: m.workspace.focused() })`);
    await sleep(700);
    check((await tiles(p)) === 1, "one tile to start with");

    await p.clickText(".sidebar .srow-main", "Summarize RFC");
    await until(async () => !!(await shows(p, B)));
    await sleep(300);
    const tA = await shows(p, A);
    const tB = await shows(p, B);
    check((await tiles(p)) === 2 && !!tA && !!tB && tA !== tB, "a chat clicked in the side menu opens in a new tile; the one that was there stays", { n: await tiles(p), tA, tB });
    check((await focusedChat(p)) === B, "…and the new tile has the focus");
    await p.shot(`${SHOTS}/sidebar-open-desktop.png`);

    await ws(p, `m.focusPane('${tA}')`);
    await sleep(200);
    await p.clickText(".sidebar .srow-main", "Summarize RFC");
    await until(async () => (await focusedChat(p)) === B);
    await sleep(300);
    check((await tiles(p)) === 2 && (await focusedChat(p)) === B, "a chat already in a tile: that tile gets the focus, no second one");

    // The ⋯ menu: one short "Open in selected tile", no "Open beside" / "Open below".
    let b = await moreOf(p, ".sidebar", "Fix flaky websocket");
    await p.mouse(b.x, b.y);
    await p.waitFor(`document.querySelector('.menu-pop .menu-item')`, 3000).catch(() => {});
    let labels = await menuLabels(p);
    check(labels.includes("Open in selected tile") && !labels.some((l) => /Open beside|Open below/.test(l)), "a chat's ⋯ menu: Open in selected tile, no Open beside / below", labels);
    const hints = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-item')].map((e) => e.textContent).join(' ')`);
    check(!/-click/i.test(hints), "no Ctrl-click hints left", hints);
    await p.shot(`${SHOTS}/sidebar-open-menu-desktop.png`);
    await p.clickText(".menu-pop .menu-item", "Open in selected tile");
    await until(async () => !!(await shows(p, C)));
    await sleep(300);
    check((await tiles(p)) === 2 && (await shows(p, C)) === tB && !(await shows(p, B)) && (await shows(p, A)) === tA, "Open in selected tile puts the chat into the selected tile", { c: await shows(p, C), tB });

    // Bots: a click opens the bot's chat in a new tile; their menu has Open in selected tile too.
    await p.eval(`document.querySelectorAll('.sidebar .sb-tab')[1].click()`);
    await p.waitFor(`document.querySelector('.sidebar .brow')`, 4000).catch(() => {});
    await sleep(300);
    await p.clickText(".sidebar .brow .srow-main", "Nova");
    await until(async () => (await tiles(p)) === 3);
    await sleep(300);
    const nova = "ses_6f2c0a9d3e18"; // "Plan my week", Nova's latest chat
    check((await tiles(p)) === 3 && !!(await shows(p, nova)) && (await focusedChat(p)) === nova, "a bot clicked in the side menu: its chat in a new tile", await tiles(p));
    b = await moreOf(p, ".sidebar", "Forge");
    await p.mouse(b.x, b.y);
    await p.waitFor(`document.querySelector('.menu-pop .menu-item')`, 3000).catch(() => {});
    labels = await menuLabels(p);
    check(labels.includes("Open in selected tile") && !labels.includes("Open beside"), "a bot's ⋯ menu: Open in selected tile, no Open beside", labels);
    await p.clickText(".menu-pop .menu-item", "Open in selected tile");
    const forge = "ses_9e41b7d05c2a"; // "Why is CI slow?"
    await until(async () => !!(await shows(p, forge)));
    await sleep(300);
    check((await tiles(p)) === 3 && (await shows(p, forge)) && !(await shows(p, nova)), "…which puts the bot's chat into the selected tile", { n: await tiles(p) });
    await p.eval(`document.querySelectorAll('.sidebar .sb-tab')[0].click()`);
    check(p.errors.length === 0, "desktop: no script errors", p.errors.slice(0, 3));
    await p.close();
  }

  // ── the folded phone: a tap replaces the tile on screen; the menu offers a new tile ──
  for (const [label, w, h, newTile] of [["folded", 344, 882, false], ["unfolded", 690, 829, true]]) {
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(w, h, true, 2.625);
    await signIn(p, `Side menu ${label}`);
    await ws(p, `return m.openSession('${A}', { pane: m.workspace.focused() })`);
    await sleep(700);
    const n0 = await tiles(p);
    await openDrawer(p);
    const row = await p.eval(`(() => { const r = [...document.querySelectorAll('.drawer-layer.open .srow-main')].find((e) => e.textContent.includes('Summarize RFC')); const x = r.getBoundingClientRect(); return { x: x.x + 40, y: x.y + x.height / 2 }; })()`);
    await p.tap(row.x, row.y);
    await p.waitFor(`!document.querySelector('.drawer-layer.open')`, 4000).catch(() => {});
    await sleep(400);
    check(!(await p.eval(`!!document.querySelector('.drawer-layer.open')`)), `${label}: the side menu closes`);
    check((await tiles(p)) === n0 + (newTile ? 1 : 0) && (await focusedChat(p)) === B && (newTile ? !!(await shows(p, A)) : !(await shows(p, A))), newTile ? `${label}: a tapped chat opens in a new tile` : `${label}: a tapped chat takes the place of the one on screen`, { n0, n: await tiles(p) });
    await openDrawer(p);
    const b = await moreOf(p, ".drawer-layer.open", "Fix flaky websocket");
    await p.tap(b.x, b.y);
    await p.waitFor(`document.querySelector('.menu-pop .menu-item')`, 3000).catch(() => {});
    await sleep(200);
    const labels = await menuLabels(p);
    const want = newTile ? "Open in selected tile" : "Open in a new tile";
    check(labels.includes(want) && !labels.some((l) => /Open beside|Open below/.test(l)), `${label}: a chat's ⋯ menu offers “${want}”`, labels);
    await p.shot(`${SHOTS}/sidebar-open-menu-${label}.png`);
    const before = await tiles(p);
    await p.clickText(".menu-pop .menu-item", want);
    await until(async () => !!(await shows(p, C)));
    await sleep(500);
    check((await tiles(p)) === before + (newTile ? 0 : 1) && (await focusedChat(p)) === C, `${label}: ${want} does that`, { before, n: await tiles(p) });
    check(!(await p.eval(`!!document.querySelector('.drawer-layer.open')`)), `${label}: …and the side menu closes`);
    if (!newTile) await closeMenu(p);
    check(p.errors.length === 0, `${label}: no script errors`, p.errors.slice(0, 3));
    await p.close();
  }
  finish();
});
