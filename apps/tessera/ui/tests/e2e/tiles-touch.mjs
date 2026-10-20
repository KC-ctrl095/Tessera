// Tiles, batch 2 (touch): swiping from the first tile (over wide code it scrolls the code,
// elsewhere it turns the page), carrying a tile across pages by holding it at a side of the
// screen (phone and Fold), the overview's visible grips, and the Fold's handle getting out
// of the way while the pages slide.
import { SHOTS, check, device, finish, mock, readEarlier, run, signIn, sleep } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const touchTraces = new WeakMap();
const touch = (p, type, pts) => {
  const params = { type, touchPoints: pts.map(([x, y], id) => ({ x, y, id })) };
  const trace = touchTraces.get(p);
  const entry = trace ? { type, points: params.touchPoints, startMs: performance.now(), wallMs: Date.now() } : null;
  if (entry) trace.push(entry);
  const sent = p.send("Input.dispatchTouchEvent", params);
  return entry ? sent.finally(() => (entry.endMs = performance.now())) : sent;
};

/** Observe the first phone carry without adding round trips while the finger is down. */
const startCarryTrace = async (p) => {
  await p.eval(`Promise.all([__mod('lib/dnd.ts'), __mod('lib/workspace.ts')]).then(([d, w]) => {
    const pager = document.querySelector('.workspace > .ws-dots');
    const r = document.querySelector('.workspace').getBoundingClientRect();
    const data = { initial: { atMs: performance.now(), wallMs: Date.now(), bounds: { left: r.left, right: r.right }, order: [...w.workspace.order()], focused: w.workspace.focused(), pager: pager?.getAttribute('aria-label') }, events: [] };
    const record = (type, detail) => data.events.push({ type, atMs: performance.now(), wallMs: Date.now(), ...detail });
    const snapshot = () => ({ pager: pager?.getAttribute('aria-label'), dropHint: d.dropHint() ? { ...d.dropHint() } : null });
    const pointer = (e) => {
      record(e.type, { pointerId: e.pointerId, pointerType: e.pointerType, x: e.clientX, y: e.clientY, eventTimeStamp: e.timeStamp, ...(e.type === 'pointerup' || e.type === 'pointercancel' ? snapshot() : {}) });
    };
    const types = ['pointerdown', 'pointermove', 'pointerup', 'pointercancel'];
    for (const type of types) window.addEventListener(type, pointer, { capture: true, passive: true });
    // Observation time is not timer firing time; preserve intermediate aria values.
    const observer = new MutationObserver((changes) => {
      for (let i = 0; i < changes.length; i++) {
        const change = changes[i];
        const next = changes.slice(i + 1).find((m) => m.target === change.target);
        record('pager', { from: change.oldValue, to: next ? next.oldValue : change.target.getAttribute('aria-label') });
      }
    });
    if (pager) observer.observe(pager, { attributes: true, attributeFilter: ['aria-label'], attributeOldValue: true });
    window.__touchCarryTrace = {
      beforeEnd: () => record('before-end', snapshot()),
      stop: () => {
        observer.disconnect();
        for (const type of types) window.removeEventListener(type, pointer, true);
        data.after = { atMs: performance.now(), wallMs: Date.now(), ...snapshot(), order: [...w.workspace.order()], focused: w.workspace.focused() };
        return data;
      },
    };
  })`);
  touchTraces.set(p, []);
};
const dumpCarryTrace = async (p) => {
  const node = touchTraces.get(p);
  touchTraces.delete(p);
  const browser = await p.eval(`(() => { const data = window.__touchCarryTrace.stop(); delete window.__touchCarryTrace; return data; })()`);
  console.log("phone carry trace", JSON.stringify({ node, browser }));
};
const swipe = async (p, x0, y, x1, steps = 10) => {
  await touch(p, "touchStart", [[x0, y]]);
  for (let k = 1; k <= steps; k++) {
    await touch(p, "touchMove", [[x0 + ((x1 - x0) * k) / steps, y + k * 0.3]]);
    await sleep(16);
  }
  await touch(p, "touchEnd", []);
  await sleep(600);
};
/** Hold, then move in steps (the finger stays down). */
const holdAndMove = async (p, from, path, holdMs = 480) => {
  await touch(p, "touchStart", [[from.x, from.y]]);
  await sleep(holdMs);
  let cur = from;
  for (const to of path) {
    const steps = 10;
    for (let k = 1; k <= steps; k++) {
      await touch(p, "touchMove", [[cur.x + ((to.x - cur.x) * k) / steps, cur.y + ((to.y - cur.y) * k) / steps]]);
      await sleep(16);
    }
    cur = to;
    if (to.wait) await sleep(to.wait);
    if (to.look) await to.look();
  }
};
const idx = (p) => ws(p, `return m.workspace.order().indexOf(m.workspace.focused())`);
const order = (p) => ws(p, `return m.workspace.order()`);
/** The page shown first in the carousel (from the track's position). */
const firstShown = (p) =>
  p.eval(`(() => { const t = document.querySelector('.ws-track'); const x = new DOMMatrixReadOnly(getComputedStyle(t).transform).m41; const ids = [...document.querySelectorAll('.ws-track > .pane')].filter(e => getComputedStyle(e).display !== 'none'); const a = document.querySelector('.workspace').getBoundingClientRect(); const hit = ids.find(e => { const r = e.getBoundingClientRect(); return r.left >= a.left - 2 && r.left < a.left + 40; }); return hit?.dataset.paneId ?? null; })()`);
const titleOf = (p, id) => p.eval(`(() => { const r = document.querySelector('[data-pane-id="${id}"] .head-title').getBoundingClientRect(); return { x: r.x + Math.min(60, r.width / 2), y: r.y + r.height / 2 }; })()`);
const centerOf = (p, id) => p.eval(`(() => { const r = document.querySelector('[data-pane-id="${id}"]').getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`);
const setup = async (p, first = "ses_e0b6d4a29c1f") => {
  await ws(p, `m.openSession('${first}', { pane: m.workspace.focused() })`);
  await sleep(300);
  await ws(p, `m.openSession('ses_5a3e8f10c2d9', { newPane: true })`);
  await sleep(300);
  await ws(p, `m.openSession('ses_1d9e4b7a0c52', { newPane: true })`);
  await sleep(500);
  await ws(p, `m.focusPane(m.workspace.order()[0])`);
  await sleep(700);
};

run(async () => {
  // ── phone ─────────────────────────────────────────────────────────────
  console.log("— phone: swiping from the first tile");
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const phone = await device();
  await phone.viewport(375, 812, true, 2);
  await signIn(phone, "Phone");
  await setup(phone);
  const [P0, P1, P2] = await order(phone);
  // Scroll up as a reader first; a script-only scroll is not follow intent.
  await readEarlier(phone);
  // Something wide in the first tile: a code block that scrolls sideways.
  const wide = await phone.eval(`(() => { const t = document.querySelector('[data-pane-id="${P0}"] .transcript'); const pre = [...t.querySelectorAll('.code-block pre, .table-wrap')].find(e => e.scrollWidth > e.clientWidth + 20); if (!pre) return null; pre.scrollIntoView({ block: 'center' }); pre.dataset.t = 'wide'; return true; })()`);
  await sleep(1200);
  // Where it is now (no scrolling into view), and that a finger there lands on it.
  const spot = () =>
    phone.eval(`(() => { const e = document.querySelector('[data-t="wide"]'); e.scrollIntoView({ block: 'center' }); const r = e.getBoundingClientRect(); const y = r.top + r.height / 2; return { cy: y, on: !!document.elementFromPoint(300, y)?.closest('[data-t="wide"]') }; })()`);
  await spot();
  await sleep(800);
  if (check(!!wide, "the first chat has a code block wider than the phone")) {
    const b = await spot();
    check(b.on, "…under the finger", b);
    await swipe(phone, 300, b.cy, 80);
    const sl = await phone.eval(`document.querySelector('[data-t="wide"]').scrollLeft`);
    check(sl > 40 && (await idx(phone)) === 0, "swiping over it scrolls the code, not the page", { sl, idx: await idx(phone) });
    // Scrolled to its end, the next swipe turns the page.
    await phone.eval(`(() => { const e = document.querySelector('[data-t="wide"]'); e.scrollLeft = e.scrollWidth; })()`);
    const b2 = await spot();
    await swipe(phone, 300, b2.cy, 80);
    check((await idx(phone)) === 1, "at its end, the swipe goes on to the next tile", await idx(phone));
    await swipe(phone, 80, 400, 320);
    check((await idx(phone)) === 0, "…and back");
  }
  // Over ordinary text: the very first swipe from the first tile works.
  const textY = await phone.eval(`(() => { const p = [...document.querySelectorAll('[data-pane-id="${P0}"] .transcript p')].find(e => { const r = e.getBoundingClientRect(); return r.top > 120 && r.bottom < 700; }); const r = p?.getBoundingClientRect(); return r ? r.top + r.height / 2 : 400; })()`);
  await swipe(phone, 320, textY, 60);
  check((await idx(phone)) === 1 && (await firstShown(phone)) === P1, "right-to-left from the first tile goes to the second", [await idx(phone), await firstShown(phone)]);
  await swipe(phone, 320, 400, 60);
  check((await idx(phone)) === 2, "…and on to the third");
  // A short, slow drag springs back; a quick flick is enough.
  await swipe(phone, 200, 400, 160, 6);
  check((await idx(phone)) === 2, "a short drag springs back");
  await ws(phone, `m.focusPane('${P0}')`);
  await sleep(500);

  console.log("— phone: carrying a tile to another page");
  const from = await titleOf(phone, P0);
  let edgeSeen = false;
  let turned = null;
  await startCarryTrace(phone);
  await holdAndMove(phone, from, [
    { x: 200, y: 360 },
    {
      x: 362,
      y: 380,
      wait: 120,
      look: async () => {
        edgeSeen = await phone.eval(`!!document.querySelector('.page-edge[data-side="right"]')`);
        // A stationary hold, not ten 0.1px CDP moves: their round trips can add
        // enough dwell to trigger the intentional 800ms repeat before edge exit.
        await sleep(250);
      },
    },
    { x: 190, y: 420, wait: 300, look: async () => {
      // Read the completed glide off the edge, before releasing the carried tile.
      turned = await firstShown(phone);
      await phone.shot(`${SHOTS}/carry-over-phone.png`);
    } },
  ]);
  const hint = await phone.eval(`(() => { window.__touchCarryTrace.beforeEnd(); return document.querySelector('.drop-chip')?.textContent ?? null; })()`);
  await touch(phone, "touchEnd", []);
  await sleep(700);
  await dumpCarryTrace(phone);
  check(edgeSeen, "held at the right side, the screen shows the page will turn");
  check(turned === P1, "…and it turns under the tile", turned);
  check(hint === "Swap", "over the next tile's middle it offers a swap", hint);
  let o = await order(phone);
  check(o[0] === P1 && o[1] === P0, "dropped: the tile moved to the second page", { before: [P0, P1, P2], after: o });
  check((await ws(phone, `return m.workspace.focused()`)) === P0 && (await firstShown(phone)) === P0, "…and stays in view, focused", await firstShown(phone));
  check(await phone.eval(`!document.querySelector('.workspace').scrollLeft`), "the pages aren't left askew");
  if (o[0] !== P1 || o[1] !== P0) await phone.shot(`${SHOTS}/carry-drop-failed-phone.png`);
  // A picture of the cue: carried back toward the left side.
  await holdAndMove(phone, await titleOf(phone, P0), [
    { x: 180, y: 380 },
    { x: 12, y: 390, wait: 150, look: async () => phone.shot(`${SHOTS}/carry-edge-phone.png`) },
    { x: 190, y: 420, wait: 200 },
  ]);
  await touch(phone, "touchEnd", []);
  await sleep(700);
  o = await order(phone);

  // Holding at the edge deliberately must still repeat across more than one page.
  // This is separate from the single-page carry above; do not suppress repeat in the app.
  const repeatBefore = [...o];
  await ws(phone, `m.focusPane('${repeatBefore[0]}')`);
  await sleep(700);
  let repeated = null;
  await holdAndMove(phone, await titleOf(phone, repeatBefore[0]), [
    { x: 200, y: 360 },
    { x: 362, y: 380, wait: 1500, look: async () => (repeated = await firstShown(phone)) },
    { x: 190, y: 420, wait: 300 },
  ]);
  const repeatHint = await phone.eval(`document.querySelector('.drop-chip')?.textContent ?? null`);
  await touch(phone, "touchEnd", []);
  await sleep(700);
  o = await order(phone);
  check(repeated === repeatBefore[2], "a deliberate longer edge hold reaches the third page", repeated);
  check(repeatHint === "Swap", "the repeat hold still offers the actual center target", repeatHint);
  check(o[0] === repeatBefore[2] && o[1] === repeatBefore[1] && o[2] === repeatBefore[0], "a deliberate multi-page hold swaps only its source and final target", { before: repeatBefore, after: o });

  console.log("— phone: the overview's grips");
  await phone.eval(`__mod('lib/ui.ts').then(m => m.setOverviewOpen(true))`);
  await sleep(600);
  check((await phone.eval(`document.querySelectorAll('.ov-card .ov-grip').length`)) === 3, "each card shows a grip");
  await phone.shot(`${SHOTS}/overview-grips-phone.png`);
  const g = await phone.box(`.ov-card[data-card="${o[0]}"] .ov-grip`);
  const last = await phone.box(`.ov-card[data-card="${o[2]}"]`);
  // No long-press: the grip picks the card up at once.
  await touch(phone, "touchStart", [[g.cx, g.cy]]);
  for (let k = 1; k <= 12; k++) {
    await touch(phone, "touchMove", [[g.cx + ((last.cx - g.cx) * k) / 12, g.cy + ((last.cy - g.cy) * k) / 12]]);
    await sleep(20);
  }
  await sleep(150);
  await touch(phone, "touchEnd", []);
  await sleep(600);
  const o2 = await order(phone);
  check(o2[2] === o[0], "dragging a grip rearranges right away", { before: o, after: o2 });
  await phone.eval(`history.back()`);
  await sleep(500);
  console.log("phone errors:", phone.errors);
  check(!phone.errors.length, "no page errors (phone)");
  await phone.close();

  // ── the Fold, unfolded ────────────────────────────────────────────────
  console.log("— fold");
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const fold = await device();
  await fold.viewport(673, 841, true, 2.625);
  await signIn(fold, "Fold");
  await setup(fold, "ses_7d3a91e5c4b2");
  const [F0, F1, F2] = await order(fold);
  check((await ws(fold, `return m.workspace.mode()`)) === "dual", "two tiles side by side");
  // The handle gets out of the way while the pages slide.
  const h0 = await fold.eval(`!!document.querySelector('.dual-handle:not(.away)')`);
  await touch(fold, "touchStart", [[500, 450]]);
  for (let k = 1; k <= 6; k++) {
    await touch(fold, "touchMove", [[500 - k * 18, 451]]);
    await sleep(16);
  }
  const away = await fold.eval(`!!document.querySelector('.dual-handle.away')`);
  const op = await fold.eval(`getComputedStyle(document.querySelector('.dual-handle')).opacity`);
  await fold.shot(`${SHOTS}/fold-swipe-handle-away.png`);
  await touch(fold, "touchEnd", []);
  await sleep(900);
  check(h0 && away, "the handle between the tiles hides while swiping", { h0, away, op });
  check(await fold.eval(`!!document.querySelector('.dual-handle:not(.away)')`), "…and is back once the pages settle");
  await fold.shot(`${SHOTS}/fold-673x841.png`);
  // Carry the first tile to the far page.
  await ws(fold, `m.focusPane('${F0}')`);
  await sleep(500);
  let turnedF = null;
  await holdAndMove(fold, await titleOf(fold, F0), [
    { x: 400, y: 420 },
    { x: 660, y: 430, wait: 700, look: async () => (turnedF = await firstShown(fold)) },
    { x: 520, y: 460, wait: 300 },
  ]);
  const hintF = await fold.eval(`document.querySelector('.drop-chip')?.textContent ?? null`);
  await fold.shot(`${SHOTS}/carry-over-fold.png`);
  await touch(fold, "touchEnd", []);
  await sleep(700);
  check(turnedF === F1, "on the Fold too, the pages turn under a tile held at the side", turnedF);
  check(hintF === "Swap", "…and it can be dropped on the tile that came into view", hintF);
  const of = await order(fold);
  check(of[2] === F0 && of[0] === F2, "dropped: it swapped with the far tile", of);
  console.log("fold errors:", fold.errors);
  check(!fold.errors.length, "no page errors (fold)");
  // The Fold held sideways (841×701).
  await fold.viewport(841, 701, true, 2.625);
  await sleep(800);
  await fold.shot(`${SHOTS}/fold-841x701.png`);
  check((await ws(fold, `return m.workspace.mode()`)) === "dual", "sideways it's still two tiles");
  await fold.close();
  finish();
});
