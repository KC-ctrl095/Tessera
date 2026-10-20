// Regression: the all-mosaics page draws each mosaic's tiles where they really are — no empty cells,
// no overlaps — with boards among them (a board tile once took the app's own `.board` styles and fell
// out of place), after a lock, a board shown by the user, and a reload; and a saved layout whose tree
// names a tile that no longer exists keeps the rest of that mosaic.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
/** Per card: tile count, header count, covered share of the preview, overlapping pairs, positioned right. */
const layoutOf = (p) =>
  p.eval(`[...document.querySelectorAll('.mosaics-page .mz-card[data-ws]')].map((card) => {
    const pv = card.querySelector('.mz-preview').getBoundingClientRect();
    const tiles = [...card.querySelectorAll('.mz-tile')].map((t) => ({ r: t.getBoundingClientRect(), pos: getComputedStyle(t).position, title: t.querySelector('.mz-tile-title').textContent }));
    // Sample the preview: every point (off the grout) must fall on a tile.
    let hit = 0, all = 0;
    for (let i = 1; i < 12; i++) for (let j = 1; j < 12; j++) {
      const x = pv.left + (pv.width * i) / 12, y = pv.top + (pv.height * j) / 12;
      all++;
      if (tiles.some(({ r }) => x >= r.left - 3 && x <= r.right + 3 && y >= r.top - 3 && y <= r.bottom + 3)) hit++;
    }
    let overlaps = 0;
    for (let i = 0; i < tiles.length; i++) for (let j = i + 1; j < tiles.length; j++) {
      const a = tiles[i].r, b = tiles[j].r;
      const w = Math.min(a.right, b.right) - Math.max(a.left, b.left), h = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
      if (w > 1 && h > 1) overlaps++;
    }
    const inside = tiles.every(({ r }) => r.left >= pv.left - 1 && r.right <= pv.right + 1 && r.top >= pv.top - 1 && r.bottom <= pv.bottom + 1);
    return { ws: card.dataset.ws, count: tiles.length, header: parseInt(card.querySelector('.mz-meta').textContent.match(/(\\d+) tile/)?.[1] ?? '0'), covered: +(hit / all).toFixed(2), overlaps, inside, absolute: tiles.every((t) => t.pos === 'absolute'), titles: tiles.map((t) => t.title) };
  })`);

async function scenario(label, w, h, mobile, dpr) {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const p = await device();
  await p.viewport(w, h, mobile, dpr);
  await signIn(p, `Preview ${label}`);
  await ws(p, `m.openSession('ses_wb7c1e5a9f20', { pane: m.workspace.focused() })`);
  await sleep(1500);
  // The agent shows a web page beside the chat.
  await mock("/__mock/board", { session_id: "ses_wb7c1e5a9f20", title: "Web page", content: { type: "url", url: "http://127.0.0.1:7991/" }, ask: false });
  await sleep(2000);
  await ws(p, `m.setMosaicLocal(0, true)`);
  await sleep(1200);
  // The user shows notes beside it (the server can't see the chat's tile any more).
  await p.eval(`(async () => { const b = await __mod('lib/boards.ts'); await b.showBesideChat('ses_wb7c1e5a9f20', 'Notes', { type: 'markdown', markdown: 'one' }); })()`);
  await sleep(2500);
  await p.reload(2500);
  await sleep(1200);
  await p.eval(`__mod('lib/mosaicsPage.ts').then(m => m.openMosaics())`);
  await sleep(800);
  let cards = await layoutOf(p);
  const m1 = cards.find((c) => c.ws === "0");
  check(m1 && m1.count === 3 && m1.header === 3, `${label}: mosaic 1 draws its three tiles`, m1);
  check(m1 && m1.absolute && m1.inside && !m1.overlaps, `${label}: each tile sits in its own place, inside the preview`, m1);
  check(m1 && m1.covered === 1, `${label}: …and together they fill it (no empty cell)`, m1?.covered);
  await p.shot(`${SHOTS}/preview-${label}.png`);
  await p.eval(`__mod('lib/mosaicsPage.ts').then(m => m.closeMosaics())`);
  // A saved tree naming a tile that no longer exists: the rest of the mosaic stays, and draws whole.
  const before = await ws(p, `return m.workspace.orderOf(0)`);
  await p.eval(`(() => {
    const k = Object.keys(localStorage).find((x) => x.startsWith('tessera.workspace.'));
    const d = JSON.parse(localStorage.getItem(k));
    const w = d.workspaces[0];
    w.root = { t: 'split', id: 's_x', dir: 'h', ratio: 0.5, a: w.root, b: { t: 'leaf', pane: 'p_gone' } };
    localStorage.setItem(k, JSON.stringify(d));
    location.reload();
  })()`).catch(() => {});
  await sleep(3000);
  await p.waitFor(`document.querySelector('.workspace')`, 8000).catch(() => {});
  await sleep(800);
  const after = await ws(p, `return m.workspace.orderOf(0)`);
  check(JSON.stringify(after) === JSON.stringify(before), `${label}: a missing tile in a saved layout loses only itself`, { before, after });
  await p.eval(`__mod('lib/mosaicsPage.ts').then(m => m.openMosaics())`);
  await sleep(800);
  cards = await layoutOf(p);
  const again = cards.find((c) => c.ws === "0");
  check(again && again.count === 3 && again.covered === 1 && !again.overlaps, `${label}: …and its preview has no hole`, again);
  console.log(label, "errors", p.errors);
  check(!p.errors.length, `${label}: no page errors`);
  await p.close();
}

run(async () => {
  await scenario("fold", 841, 701, true, 2.625);
  await scenario("phone", 375, 812, true, 2);
  await scenario("desktop", 1440, 900, false, 1);
  finish();
});
