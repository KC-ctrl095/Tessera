// Phones and the Fold: the all-mosaics page by touch (drawer, overview, pinch, long-press), keeping a
// mosaic on the device from the overview, and the handle between the two tiles on an unfolded Fold.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const touch = (p, type, pts) => p.send("Input.dispatchTouchEvent", { type, touchPoints: pts.map(([x, y], id) => ({ x, y, id })) });
const hold = async (p, x, y, ms = 650) => {
  await touch(p, "touchStart", [[x, y]]);
  await sleep(ms);
  await touch(p, "touchEnd", []);
  await sleep(250);
};
const drag = async (p, x0, y0, x1, y1, steps = 8) => {
  await touch(p, "touchStart", [[x0, y0]]);
  for (let k = 1; k <= steps; k++) {
    await touch(p, "touchMove", [[x0 + ((x1 - x0) * k) / steps, y0 + ((y1 - y0) * k) / steps]]);
    await sleep(16);
  }
  await touch(p, "touchEnd", []);
  await sleep(250);
};
const pinchIn = async (p, cx, cy) => {
  await touch(p, "touchStart", [[cx - 120, cy - 120], [cx + 120, cy + 120]]);
  for (let k = 1; k <= 8; k++) {
    const d = 120 - k * 12;
    await touch(p, "touchMove", [[cx - d, cy - d], [cx + d, cy + d]]);
    await sleep(16);
  }
  await touch(p, "touchEnd", []);
  await sleep(400);
};
const shown = (p, sel) => p.eval(`!!document.querySelector(${JSON.stringify(sel)})`);
const setup = async (p) => {
  await ws(p, `m.openSession('ses_7d3a91e5c4b2', { pane: m.workspace.focused() })`);
  await sleep(400);
  await ws(p, `m.openSession('ses_5a3e8f10c2d9', { newPane: true })`);
  await sleep(400);
  await ws(p, `m.switchWorkspace(1); m.openSession('ses_1d9e4b7a0c52')`);
  await sleep(400);
  await ws(p, `m.openSession('ses_a6c3f0e82b19', { newPane: true })`);
  await sleep(400);
  await ws(p, `m.switchWorkspace(0); m.focusPane(m.workspace.order()[0])`);
  await sleep(800);
};

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const phone = await device();
  await phone.viewport(375, 812, true, 2);
  await signIn(phone, "Phone");
  await setup(phone);
  const active = () => ws(phone, `return m.workspace.active()`);

  // ── all mosaics, from the drawer ──
  console.log("— phone");
  let b = await phone.box(`[data-pane-id] .head-menu`);
  await phone.tap(b.cx, b.cy);
  await sleep(500);
  b = await phone.box(`.sb-ws-all`);
  check(!!b, "the drawer's Mosaics row has “All”");
  await phone.tap(b.cx, b.cy);
  await sleep(500);
  check(await shown(phone, ".mosaics-page"), "…which opens the all-mosaics page");
  check((await phone.eval(`document.querySelectorAll('.mosaics-page .mz-card[data-ws]').length`)) === 2, "…both mosaics");
  check(await phone.eval(`(() => { const r = document.querySelector('.mosaics-page .mz-card').getBoundingClientRect(); return r.width > 300; })()`), "…as big cards");
  await phone.shot(`${SHOTS}/mosaics-page-phone.png`);
  b = await phone.box(`.mz-card[data-ws="1"] .mz-preview`);
  await phone.tap(b.cx, b.cy);
  await sleep(500);
  check((await active()) === 1 && !(await shown(phone, ".mosaics-page")), "tapping a mosaic goes into it", await active());

  // ── long-press the overview button in a tile header ──
  b = await phone.box(`.pane.focused .head-overview`);
  await hold(phone, b.cx, b.cy);
  await sleep(300);
  check(await shown(phone, ".mosaics-page") && !(await shown(phone, ".overview:not(.mosaics-page)")), "long-pressing the tiles button opens all mosaics (not the tiles)");
  await phone.eval(`history.back()`);
  await sleep(400);
  check(!(await shown(phone, ".mosaics-page")), "the back gesture closes it");
  // A plain tap still opens the tiles of this mosaic.
  b = await phone.box(`.pane.focused .head-overview`);
  await phone.tap(b.cx, b.cy);
  await sleep(500);
  check(await shown(phone, ".overview:not(.mosaics-page)") && !(await shown(phone, ".mosaics-page")), "a tap still opens the tile overview");
  // Its mosaics row leads up to all mosaics, and back down.
  b = await phone.box(`.overview-mosaics .mosaic-chip.all`);
  await phone.tap(b.cx, b.cy);
  await sleep(400);
  check(await shown(phone, ".mosaics-page") && !(await shown(phone, ".overview:not(.mosaics-page)")), "the overview's “All” chip leads to all mosaics");
  await phone.eval(`history.back()`);
  await sleep(400);
  check(!(await shown(phone, ".overview")) && (await phone.eval(`location.href.startsWith('http') && document.querySelectorAll('.pane').length > 0`)), "back from there returns to the tile (still in the app)");
  b = await phone.box(`.pane.focused .head-overview`);
  await phone.tap(b.cx, b.cy);
  await sleep(500);
  // Pinching in on the overview zooms out to the mosaics.
  await pinchIn(phone, 187, 450);
  check(await shown(phone, ".mosaics-page") && !(await shown(phone, ".overview:not(.mosaics-page)")), "pinching in on the overview opens all mosaics");
  // Long-press a card: its options.
  b = await phone.box(`.mz-card[data-ws="1"] .mz-preview`);
  await hold(phone, b.cx, b.cy);
  const opts = await phone.eval(`[...document.querySelectorAll('.menu-item .menu-label')].map(e => e.textContent)`);
  check(["Rename…", "Only on this device", "Delete mosaic…"].every((x) => opts.includes(x)), "long-pressing a card offers rename, this device only, delete", opts);
  await phone.shot(`${SHOTS}/mosaics-page-menu-phone.png`);
  await phone.eval(`history.back()`);
  await sleep(300);
  b = await phone.box(`.mz-card[data-ws="0"] .mz-preview`);
  await phone.tap(b.cx, b.cy);
  await sleep(400);
  check((await active()) === 0, "…and a tap picks one", await active());

  // ── keep a mosaic on this device, from the overview's menu ──
  b = await phone.box(`.pane.focused .head-overview`);
  await phone.tap(b.cx, b.cy);
  await sleep(500);
  b = await phone.box(`.overview:not(.mosaics-page) .overview-head [aria-label^="Options for Mosaic"]`);
  await phone.tap(b.cx, b.cy);
  await sleep(400);
  await phone.clickText(".menu-item", "Only on this device");
  await sleep(500);
  check(await ws(phone, `return m.workspace.localOf(0)`), "“Only on this device” from the overview's menu");
  check(await shown(phone, ".overview-local") && (await shown(phone, `.mosaic-chip .mosaic-chip-lock`)), "the overview says so (title and chip)");
  await phone.shot(`${SHOTS}/overview-local-phone.png`);
  // The toast (with its Undo) sits over the header on phones: dismiss it first.
  await phone.eval(`document.querySelectorAll('.toast-x').forEach((x) => x.click())`);
  await sleep(300);
  await phone.shot(`${SHOTS}/overview-local-phone2.png`);
  b = await phone.box(`.overview-head [aria-label^="Options for Mosaic"]`);
  await phone.tap(b.cx, b.cy);
  await sleep(400);
  await phone.shot(`${SHOTS}/overview-menu-phone.png`);
  await phone.clickText(".menu-item", "Only on this device");
  await sleep(500);
  check(!(await ws(phone, `return m.workspace.localOf(0)`)), "…and off again");
  await phone.eval(`history.back()`);
  await sleep(300);

  // ── spawn a tile on the phone: New at the top right of the tile on screen, the same menu as on computers ──
  const visBox = (p, sel) => p.eval(`(() => { const e = [...document.querySelectorAll(${JSON.stringify(sel)})].find(e => { const r = e.getBoundingClientRect(); return r.width && r.left >= 0 && r.right <= innerWidth; }); if (!e) return null; const r = e.getBoundingClientRect(); return { cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);
  await sleep(600);
  await phone.eval(`document.querySelectorAll('.toast-x').forEach((x) => x.click())`);
  await sleep(300);
  const head = await phone.eval(`(() => { const h = [...document.querySelectorAll('.pane-head')].find((e) => { const r = e.getBoundingClientRect(); return r.width && r.left >= 0 && r.right <= innerWidth; }); if (!h) return null; const n = h.querySelector('.head-new'); const r = n?.getBoundingClientRect(); return { new: !!n, right: r ? innerWidth - r.right : null, close: !!h.querySelector('.head-close') }; })()`);
  check(head?.new && head.right < 16 && head.close, "the tile on screen has New at the top right and its close X alongside", head);
  b = await visBox(phone, `.pane .head-new`);
  await phone.tap(b.cx, b.cy);
  await phone.waitFor(`document.querySelector('.menu-pop .menu-item')`, 3000).catch(() => {});
  await sleep(300);
  const kinds = await phone.eval(`[...document.querySelectorAll('.menu-item .menu-label')].map(e => e.textContent)`);
  await phone.shot(`${SHOTS}/spawn-menu-phone.png`);
  check(JSON.stringify(kinds) === JSON.stringify(["Chat", "Private chat", "Note", "Web view", "More"]), "the phone's New menu is the computer's (no image, no web app)", kinds);
  check(await phone.eval(`!document.querySelector('.menu-pop')?.classList.contains('as-sheet')`), "…a compact menu, not a big sheet");
  b = await phone.eval(`(() => { const e = [...document.querySelectorAll('.menu-item')].find(e => e.textContent.includes('Note')); const r = e.getBoundingClientRect(); return { cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);
  await phone.tap(b.cx, b.cy);
  await sleep(1500);
  const cur = await ws(phone, `return m.workspace.focused()`);
  const kind = await ws(phone, `return m.workspace.panes['${cur}']?.content.type`);
  check(kind === "note", "a note tile is made and brought on screen", kind);
  check(await phone.eval(`(() => { const e = document.querySelector('[data-pane-id="${cur}"]'); const r = e.getBoundingClientRect(); return r.left >= -1 && r.right <= innerWidth + 1; })()`), "…in view");
  await phone.shot(`${SHOTS}/note-tile-phone.png`);
  console.log("phone errors:", phone.errors);
  check(!phone.errors.length, "no page errors (phone)");
  await phone.close();

  // ── the Fold, unfolded: two tiles and the handle between them ──
  console.log("— fold");
  const fold = await device();
  await fold.viewport(673, 841, true, 2.625);
  await signIn(fold, "Fold");
  await sleep(1200);
  await ws(fold, `m.switchWorkspace(0); m.focusPane(m.workspace.order()[0])`);
  await sleep(600);
  check((await ws(fold, `return m.workspace.mode()`)) === "dual", "an unfolded Fold shows two tiles side by side");
  // Its top bar is the desktop's: all mosaics, and no extra button for managing one mosaic's tiles.
  check(await shown(fold, `.topbar [aria-label="All mosaics"]`), "the Fold's top bar has “All mosaics”");
  check(!(await shown(fold, `.topbar [aria-label^="All tiles"]`)), "…and no separate mosaic-managing button next to it");
  const h = await fold.box(`.dual-handle`);
  check(!!h && Math.abs(h.cx - 673 / 2) < 3, "the handle sits between them, in the middle", h);
  const grip = await fold.box(`.dual-grip`);
  await drag(fold, grip.cx, grip.cy, grip.cx + 110, grip.cy + 6);
  const r = await ws(fold, `return m.workspace.dualRatio()`);
  check(Math.abs(r - (0.5 + 110 / 673)) < 0.02, "dragging it gives the left tile more room", r);
  const [L, R] = await ws(fold, `return m.workspace.order().slice(0, 2)`);
  const wl = (await fold.box(`[data-pane-id="${L}"]`)).w;
  const wr = (await fold.box(`[data-pane-id="${R}"]`)).w;
  check(wl > wr + 150, "…and the tiles follow", { wl, wr });
  check((await ws(fold, `return m.workspace.order().indexOf(m.workspace.focused())`)) === 0, "dragging the handle doesn't swipe to another tile");
  await fold.shot(`${SHOTS}/fold-handle.png`);
  await fold.reload(2500);
  await sleep(800);
  check(Math.abs((await ws(fold, `return m.workspace.dualRatio()`)) - r) < 0.001, "the split stays after a reload");
  const g2 = await fold.box(`.dual-grip`);
  // tap() waits 150 ms after each touch for menus to settle. Together with
  // the gap and slow software rendering that can exceed the handle's 320 ms
  // double-tap window on a software-rendering host. Send the actual gesture, not two settled taps.
  for (let i = 0; i < 2; i++) {
    await touch(fold, "touchStart", [[g2.cx, g2.cy]]);
    await sleep(40);
    await touch(fold, "touchEnd", []);
    if (i === 0) await sleep(80);
  }
  await sleep(400);
  check(Math.abs((await ws(fold, `return m.workspace.dualRatio()`)) - 0.5) < 0.001, "a double-tap evens it out");
  // Held ends: it never gets narrower than a usable tile.
  const g3 = await fold.box(`.dual-grip`);
  await drag(fold, g3.cx, g3.cy, 20, g3.cy);
  const rMin = await ws(fold, `return m.workspace.dualRatio()`);
  check(rMin >= 0.29 && rMin < 0.31, "it stops where a tile would get too narrow", rMin);
  await ws(fold, `m.setDualRatio(0.5)`);
  // Swiping from a tile's content still moves between tiles.
  await ws(fold, `m.openSession('ses_91d0c3e7a8b2', { newPane: true })`);
  await sleep(600);
  await ws(fold, `m.focusPane(m.workspace.order()[0])`);
  await sleep(600);
  await drag(fold, 520, 600, 200, 605, 10);
  await sleep(500);
  check((await ws(fold, `return m.workspace.order().indexOf(m.workspace.focused())`)) >= 1, "a swipe on a tile still moves along");
  await fold.eval(`__mod('lib/mosaicsPage.ts').then(m => m.openMosaics())`);
  await sleep(500);
  check(await fold.eval(`(() => { const c = document.querySelector('.mz-new'), b = document.querySelector('.mz-new-body'); return !!c && b.getBoundingClientRect().width > c.getBoundingClientRect().width - 30; })()`), "the new-mosaic card's body fills its card");
  await fold.shot(`${SHOTS}/mosaics-page-fold.png`);
  // Managing a mosaic happens on that page: its ⋯ has everything the old button's page had for it.
  const more = await fold.box(`.mz-card[data-ws="0"] .mz-more`);
  await fold.tap(more.cx, more.cy);
  await fold.waitFor(`document.querySelector('.menu-pop .menu-item')`, 3000).catch(() => {});
  const mzOpts = await fold.eval(`[...document.querySelectorAll('.menu-item')].map(e => e.textContent)`);
  check(["Open", "Tiles", "Rename", "Only on this device", "Delete mosaic"].every((k) => mzOpts.some((x) => x.includes(k))), "the mosaics page's ⋯ opens, shows the tiles, renames, keeps on this device and deletes a mosaic", mzOpts);
  await fold.clickText(".menu-item", "Tiles");
  await sleep(500);
  check((await shown(fold, ".overview:not(.mosaics-page)")) && !(await shown(fold, ".mosaics-page")), "…“Tiles” shows its tiles as cards (rearrange, move, close)");
  await fold.shot(`${SHOTS}/overview-fold.png`);
  console.log("fold errors:", fold.errors);
  check(!fold.errors.length, "no page errors (Fold)");
  finish();
});
