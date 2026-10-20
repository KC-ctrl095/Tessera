// Desktop tile drag: a lifted half-size card, a quiet socket, calm drop preview; no drag with one tile.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

run(async () => {
  for (const theme of ["dark", "light"]) {
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, `Drag ${theme}`);
    await p.theme(theme);
    await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_2b7e91c04d5a', { pane: m.workspace.focused() }))`);
    await sleep(900);
    // one tile: pressing and moving the header must not lift it
    const h0 = await p.box(".pane .pane-head");
    await p.down(h0.x + 200, h0.cy);
    for (let i = 1; i <= 6; i++) await p.move(h0.x + 200 + i * 20, h0.cy + i * 12);
    await sleep(120);
    const liftedSolo = await p.eval(`!!document.querySelector('.pane.lifted') || document.documentElement.classList.contains('dragging-now')`);
    await p.up(h0.x + 320, h0.cy + 72);
    if (theme === "dark") check(!liftedSolo, "a lone tile doesn't start a drag");
    // three tiles
    for (const sid of ["ses_91d0c3e7a8b2", "ses_wb7c1e5a9f20"]) {
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${sid}', { newPane: true }))`);
      await sleep(700);
    }
    await sleep(600);
    const ids = await p.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order())`);
    const src = ids[0];
    const dst = ids[ids.length - 1];
    const hb = await p.box(`[data-pane-id="${src}"] .pane-head`);
    const db = await p.box(`[data-pane-id="${dst}"]`);
    const sx = hb.x + 120;
    const sy = hb.cy;
    await p.down(sx, sy);
    // along a path into the right edge zone of the last tile
    const tx = db.x + db.w * 0.86;
    const ty = db.y + db.h * 0.5;
    for (let i = 1; i <= 24; i++) {
      await p.move(sx + ((tx - sx) * i) / 24, sy + ((ty - sy) * i) / 24);
      await sleep(16);
    }
    await sleep(350);
    const info = await p.eval(`(() => {
      const el = document.querySelector('.pane.lifted');
      if (!el) return null;
      const t = getComputedStyle(el).transform;
      const m = new DOMMatrixReadOnly(t);
      const scale = Math.hypot(m.a, m.b);
      const rot = Math.atan2(m.b, m.a) * 180 / Math.PI;
      const r = el.getBoundingClientRect();
      return { t, is2D: m.is2D, scale, rot, socket: !!document.querySelector('.drag-socket'), preview: !!document.querySelector('.drop-preview'), chip: document.querySelector('.drop-chip')?.textContent, w: r.width };
    })()`);
    if (theme === "dark") {
      check(!!info, "the tile lifts once dragged", info);
      if (info) {
        check(info.is2D && !/matrix3d/.test(info.t), "flat 2D transform (no perspective)", info.t);
        check(info.scale >= 0.49 && info.scale <= 0.61, `lifted at about half size (${info.scale.toFixed(2)})`);
        check(Math.abs(info.rot) <= 1.51, `leans at most 1.5° (${info.rot.toFixed(2)}°)`);
        check(info.socket && info.preview, "socket outline and drop preview are shown");
        check(/right/i.test(info.chip ?? ""), `snaps to the right edge (${info.chip})`);
      }
    }
    await p.shot(`${SHOTS}/drag-lifted-desktop-${theme}.png`);
    // grab point: the header point under the pointer stays under it (scaled around it)
    await p.up(tx, ty);
    await sleep(500);
    const after = await p.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order())`);
    if (theme === "dark") check(after[after.length - 1] === src, "dropped on the right edge: the tile moved there", after);
    await p.shot(`${SHOTS}/drag-dropped-desktop-${theme}.png`);
    // Esc cancels and glides back
    const hb2 = await p.box(`[data-pane-id="${after[0]}"] .pane-head`);
    await p.down(hb2.x + 100, hb2.cy);
    for (let i = 1; i <= 8; i++) await p.move(hb2.x + 100 + i * 25, hb2.cy + i * 20);
    await sleep(200);
    await p.key("Escape", "Escape", 0, 27);
    await sleep(80);
    await p.up(hb2.x + 300, hb2.cy + 160);
    await sleep(400);
    const afterEsc = await p.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order())`);
    if (theme === "dark") check(JSON.stringify(afterEsc) === JSON.stringify(after) && !(await p.eval(`!!document.querySelector('.pane.lifted')`)), "Esc cancels the drag");
    // a click on the title still renames (no drag below 5 px)
    console.log(theme, "errors", p.errors);
    await p.close();
  }
  finish();
});
