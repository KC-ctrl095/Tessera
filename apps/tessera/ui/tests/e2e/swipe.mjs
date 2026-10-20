// Swiping between tiles on a phone (the Fold's cover screen) keeps working whatever happened
// before: strokes that start on things that scroll (a new chat's welcome, tool output), odd and
// interrupted gestures (a release the page never hears, the app going to the background, the
// screen folding mid-stroke, a lifted tile left behind), tiles closed or opened on another
// device, and the phone folding and unfolding.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const touch = (p, type, pts) => p.send("Input.dispatchTouchEvent", { type, touchPoints: pts.map(([x, y], id) => ({ x, y, id })) });
const swipe = async (p, x0, y, x1, steps = 10, settle = 600) => {
  await touch(p, "touchStart", [[x0, y]]);
  for (let k = 1; k <= steps; k++) {
    await touch(p, "touchMove", [[x0 + ((x1 - x0) * k) / steps, y + k * 0.3]]);
    await sleep(16);
  }
  await touch(p, "touchEnd", []);
  await sleep(settle);
};
/** Start a sideways stroke and leave the finger down. */
const strokeStart = async (p, x0, y, dx = -100, steps = 5) => {
  await touch(p, "touchStart", [[x0, y]]);
  for (let k = 1; k <= steps; k++) {
    await touch(p, "touchMove", [[x0 + (dx * k) / steps, y]]);
    await sleep(16);
  }
};
const idx = (p) => ws(p, `return m.workspace.order().indexOf(m.workspace.focused())`);
const order = (p) => ws(p, `return m.workspace.order()`);
const focusAt = async (p, i) => {
  await ws(p, `m.focusPane(m.workspace.order()[${i}])`);
  await sleep(500);
};
/** The page on screen: the tile whose left edge is at the screen's (none when the pages sit askew). */
const shown = (p) =>
  p.eval(`(() => { const a = document.querySelector('.workspace').getBoundingClientRect(); const hit = [...document.querySelectorAll('.ws-track > .pane')].find((e) => e.style.display !== 'none' && Math.abs(e.getBoundingClientRect().left - a.left) < 1.5); return hit?.dataset.paneId ?? null; })()`);
/** The next release never reaches the page (nor does the end of its capture), as when the system takes the touch. */
const loseRelease = (p) =>
  p.eval(`(() => { const kinds = ['pointerup', 'pointercancel', 'lostpointercapture']; let left = 2; const eat = (e) => { e.stopImmediatePropagation(); if (e.type !== 'lostpointercapture' || --left <= 0) setTimeout(() => kinds.forEach((k) => window.removeEventListener(k, eat, true)), 50); }; kinds.forEach((k) => window.addEventListener(k, eat, true)); })()`);
/** Pretend the app went to the background and came back (visibilitychange both ways). */
const hideAndShow = (p) =>
  p.eval(`(async () => { const set = (v) => { Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => v }); Object.defineProperty(document, 'hidden', { configurable: true, get: () => v === 'hidden' }); document.dispatchEvent(new Event('visibilitychange')); }; set('hidden'); await new Promise((r) => setTimeout(r, 150)); set('visible'); delete document.visibilityState; delete document.hidden; })()`);
/** A line of plain transcript text on screen, to swipe over. */
/** The leftmost tile fully on screen (two-up: the pair's left one; tiles sit in the grout's gap). */
const firstShown = (p) =>
  p.eval(`(() => { const a = document.querySelector('.workspace').getBoundingClientRect(); const on = [...document.querySelectorAll('.ws-track > .pane')].filter((e) => { const r = e.getBoundingClientRect(); return e.style.display !== 'none' && r.width > 0 && r.left >= a.left - 1 && r.right <= a.right + 1; }); on.sort((x, y) => x.getBoundingClientRect().left - y.getBoundingClientRect().left); return on[0]?.dataset.paneId ?? null; })()`);
const textY = (p) =>
  p.eval(`(() => { const a = document.querySelector('.workspace').getBoundingClientRect(); const e = [...document.querySelectorAll('.pane.focused .transcript p')].find((e) => { const r = e.getBoundingClientRect(); return r.top > a.top + 80 && r.bottom < a.bottom - 160 && r.left >= a.left && r.right <= a.right + 1; }); const r = e?.getBoundingClientRect(); return r ? r.top + r.height / 2 : a.top + a.height * 0.45; })()`);

/** A normal stroke over plain text turns the page (forward, or back from the last page), and the page sits square. */
async function normalSwipe(p, label) {
  const n = (await order(p)).length;
  const i0 = await idx(p);
  const back = i0 >= n - 1;
  const y = await textY(p);
  if (back) await swipe(p, 60, y, 300);
  else await swipe(p, 300, y, 60);
  const i1 = await idx(p);
  const want = i0 + (back ? -1 : 1);
  const o = await order(p);
  const on = await shown(p);
  return check(i1 === want && on === o[want], `${label}: a normal swipe still turns the page`, { i0, i1, want, shown: on, page: o[want] });
}

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const phone = await device();
  // The Galaxy Fold folded: its cover screen.
  await phone.viewport(344, 882, true, 2.625);
  await signIn(phone, "Fold");
  await ws(phone, `m.openSession('ses_2b7e91c04d5a', { pane: m.workspace.focused() })`);
  await sleep(300);
  for (const id of ["ses_8c1f2a9d4e70", "ses_e0b6d4a29c1f", "ses_1d9e4b7a0c52"]) {
    await ws(phone, `m.openSession('${id}', { newPane: true })`);
    await sleep(300);
  }
  // A new chat beside the third tile: its welcome screen fills the tile.
  await ws(phone, `m.openPane(undefined, m.workspace.order()[2])`);
  await sleep(900);
  check((await ws(phone, `return m.workspace.mode()`)) === "mobile", "the cover screen shows one tile at a time");
  let o = await order(phone);
  check(o.length === 5, "five tiles", o);

  // ── strokes that start on something that scrolls ──────────────────────
  console.log("— strokes over things that scroll");
  const heroIdx = o.indexOf(await phone.eval(`document.querySelector('.pane .hero')?.closest('.pane')?.dataset.paneId ?? ''`));
  check(heroIdx > 0 && heroIdx < o.length - 1, "the new chat sits between other tiles", heroIdx);
  await focusAt(phone, heroIdx);
  const heroY = await phone.eval(`(() => { const r = document.querySelector('.pane.focused .hero').getBoundingClientRect(); return r.top + r.height * 0.3; })()`);
  await swipe(phone, 300, heroY, 60);
  check((await idx(phone)) === heroIdx + 1, "a stroke on a new chat's welcome screen turns to the next tile", await idx(phone));
  await focusAt(phone, heroIdx);
  await swipe(phone, 60, heroY, 300);
  check((await idx(phone)) === heroIdx - 1, "…and back to the one before", await idx(phone));
  // Tool output (it scrolls both ways).
  const toolAt = o.findIndex((id, i) => i < o.length - 1 && i !== heroIdx);
  let toolY = null;
  for (const [i, id] of o.entries()) {
    if (i >= o.length - 1) continue;
    await focusAt(phone, i);
    // Open a few tool cards so their output shows.
    await phone.eval(`[...document.querySelectorAll('[data-pane-id="${id}"] .tool-head[aria-expanded="false"]')].slice(0, 4).forEach((b) => b.click())`);
    await sleep(300);
    const found = await phone.eval(`(() => { const e = [...document.querySelectorAll('[data-pane-id="${id}"] .tool-pre, [data-pane-id="${id}"] .tool-live')].find((e) => e.getClientRects().length && e.getBoundingClientRect().height > 30); if (!e) return false; e.dataset.t = 'tool'; e.scrollIntoView({ block: 'center' }); return true; })()`);
    if (!found) continue;
    await sleep(800);
    toolY = await phone.eval(`(() => { const a = document.querySelector('.workspace').getBoundingClientRect(); const r = document.querySelector('[data-t="tool"]').getBoundingClientRect(); const top = Math.max(r.top, a.top + 60), bot = Math.min(r.bottom, a.bottom - 140); return bot > top + 10 && document.elementFromPoint(250, (top + bot) / 2)?.closest('[data-t="tool"]') ? (top + bot) / 2 : null; })()`);
    if (toolY !== null) {
      await swipe(phone, 250, toolY, 40);
      check((await idx(phone)) === i + 1, "a stroke that starts on tool output turns the page", { from: i, now: await idx(phone) });
      break;
    }
  }
  check(toolY !== null || toolAt < 0, "found tool output to swipe over");
  // Nothing in any tile lets the browser take a sideways stroke (it would never become a swipe).
  const grabbers = await phone.eval(`(() => {
    const out = [];
    for (const el of document.querySelectorAll('.pane *')) {
      const cs = getComputedStyle(el);
      if (!/(auto|scroll)/.test(cs.overflowX + ' ' + cs.overflowY)) continue;
      if (el.matches('input, textarea:not([data-swipe])')) continue;
      if (/auto|pan-x|manipulation/.test(cs.touchAction)) out.push((el.className?.baseVal ?? el.className) + ' {' + cs.touchAction + '}');
    }
    return [...new Set(out)];
  })()`);
  check(!grabbers.length, "no scroller in a tile hands sideways strokes to the browser", grabbers);

  // ── odd and interrupted gestures ─────────────────────────────────────
  console.log("— interrupted gestures");
  await focusAt(phone, 1);
  await normalSwipe(phone, "to begin with");

  await focusAt(phone, 1);
  await strokeStart(phone, 300, 450);
  await touch(phone, "touchCancel", []);
  await sleep(500);
  check((await shown(phone)) === o[1], "a stroke the system calls off springs back", await shown(phone));
  await normalSwipe(phone, "after a cancelled stroke");

  // A move and a cancel in the same frame: no queued frame leaves the pages askew.
  await focusAt(phone, 1);
  await touch(phone, "touchStart", [[300, 450]]);
  for (let k = 1; k <= 4; k++) await touch(phone, "touchMove", [[300 - k * 30, 450]]);
  await touch(phone, "touchCancel", []);
  await sleep(500);
  check((await shown(phone)) === o[1], "a cancel right after a move leaves the page square", await shown(phone));

  // The release never reaches the page (a system gesture took the touch, the app was switched away).
  await focusAt(phone, 1);
  await strokeStart(phone, 300, 450);
  await loseRelease(phone);
  await touch(phone, "touchEnd", []);
  await sleep(500);
  await normalSwipe(phone, "after a stroke whose release never arrived");
  check((await shown(phone)) !== null, "…and the pages sit square");

  await focusAt(phone, 1);
  await loseRelease(phone);
  await phone.tap(170, 450);
  await sleep(200);
  await normalSwipe(phone, "after a tap whose release never arrived");

  await focusAt(phone, 1);
  await touch(phone, "touchStart", [[120, 400], [230, 520]]);
  await touch(phone, "touchMove", [[125, 405], [225, 515]]);
  await loseRelease(phone);
  await touch(phone, "touchEnd", []);
  await sleep(400);
  check(!(await phone.eval(`!!document.querySelector('.overview')`)), "two fingers lifted: no overview opened by mistake");
  await normalSwipe(phone, "after two fingers, one release lost");

  // A tile lifted by a long press whose release never arrives: the next touch puts it back.
  await focusAt(phone, 1);
  const head = await phone.eval(`(() => { const r = document.querySelector('.pane.focused .head-title').getBoundingClientRect(); return [r.x + 40, r.y + r.height / 2]; })()`);
  await touch(phone, "touchStart", [head]);
  await sleep(600);
  await touch(phone, "touchMove", [[head[0] + 10, head[1] + 60]]);
  await sleep(100);
  check(await phone.eval(`!!document.querySelector('.pane.lifted')`), "a long press lifts the tile");
  await loseRelease(phone);
  await touch(phone, "touchEnd", []);
  await sleep(300);
  await phone.tap(170, 600);
  await sleep(500);
  check(!(await phone.eval(`!!document.querySelector('.pane.lifted, .workspace.dragging')`)), "the next touch puts a tile left lifted back");
  await normalSwipe(phone, "after a tile drag whose release never arrived");

  // The app goes to the background mid-stroke (no release at all), then comes back.
  await focusAt(phone, 1);
  await strokeStart(phone, 300, 450, -120);
  await hideAndShow(phone);
  await sleep(500);
  check((await phone.eval(`!document.querySelector('.workspace.swiping')`)) && (await shown(phone)) === o[1], "back from the background, the half-swiped page settles", await shown(phone));
  await touch(phone, "touchEnd", []);
  await sleep(300);
  await normalSwipe(phone, "after the app went to the background mid-stroke");
  // …and with a tile lifted.
  await focusAt(phone, 1);
  await touch(phone, "touchStart", [head]);
  await sleep(600);
  await touch(phone, "touchMove", [[head[0] + 10, head[1] + 60]]);
  await sleep(100);
  await hideAndShow(phone);
  await sleep(400);
  check(!(await phone.eval(`!!document.querySelector('.pane.lifted, .workspace.dragging')`)), "a tile lifted when the app went to the background goes back");
  await touch(phone, "touchEnd", []);
  await sleep(300);

  // Folding mid-stroke: the screen changes size under the finger.
  await focusAt(phone, 1);
  await strokeStart(phone, 300, 450);
  await phone.viewport(673, 841, true, 2.625);
  await sleep(500);
  check((await ws(phone, `return m.workspace.mode()`)) === "dual", "unfolded: two tiles side by side");
  await phone.viewport(344, 882, true, 2.625);
  await sleep(600);
  await touch(phone, "touchEnd", []);
  await sleep(400);
  check((await shown(phone)) !== null, "folded again mid-stroke: the pages sit square", await shown(phone));
  await normalSwipe(phone, "after folding mid-stroke");

  // A long press on text, a vertical scroll.
  await focusAt(phone, 1);
  await touch(phone, "touchStart", [[170, 450]]);
  await sleep(900);
  await touch(phone, "touchEnd", []);
  await sleep(300);
  await phone.eval(`document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))`);
  await normalSwipe(phone, "after a long press on text");
  await focusAt(phone, 1);
  await touch(phone, "touchStart", [[170, 650]]);
  for (let k = 1; k <= 10; k++) {
    await touch(phone, "touchMove", [[170, 650 - k * 30]]);
    await sleep(16);
  }
  await touch(phone, "touchEnd", []);
  await sleep(700);
  await normalSwipe(phone, "after scrolling the chat");
  await phone.shot(`${SHOTS}/swipe-fold-cover.png`);

  // ── tiles closed or opened on another device ─────────────────────────
  console.log("— another device changes the mosaic");
  const other = await wsClient("Desktop");
  const edit = async (fn) => {
    for (let k = 0; k < 4; k++) {
      const L = (await other.rpc("get_mosaics", {})).result;
      const next = fn(structuredClone(L));
      const r = await other.rpc("save_mosaics", { layout: next });
      if (!r.error) return r.result;
      await sleep(300);
    }
    return null;
  };
  await sleep(1500); // the phone's layout reaches the server
  o = await order(phone);
  let shared = (await other.rpc("get_mosaics", {})).result.mosaics.flatMap((m) => m.tiles.map((t) => t.id));
  const sharedOrder = o.filter((id) => shared.includes(id));
  check(sharedOrder.length >= 3, "the phone's tiles are shared", { o, shared });
  // On the last page, the desktop closes that very tile.
  const last = sharedOrder[sharedOrder.length - 1];
  await ws(phone, `m.focusPane('${last}')`);
  await sleep(600);
  await edit((L) => ({ ...L, mosaics: L.mosaics.map((m) => ({ ...m, tiles: m.tiles.filter((t) => t.id !== last) })) }));
  await sleep(1500);
  o = await order(phone);
  check(!o.includes(last), "the tile closed on the desktop goes from the phone", o);
  const on = await shown(phone);
  check(on !== null && o.includes(on) && (await idx(phone)) === o.indexOf(on), "the phone shows a tile that exists (the page is in range)", { on, idx: await idx(phone), o });
  await normalSwipe(phone, "after the tile on screen closed elsewhere");
  // A tile before the one on screen closes elsewhere: the phone stays on its tile.
  await focusAt(phone, 2);
  const stay = (await order(phone))[2];
  const before = (await order(phone)).slice(0, 2).find((id) => shared.includes(id));
  await edit((L) => ({ ...L, mosaics: L.mosaics.map((m) => ({ ...m, tiles: m.tiles.filter((t) => t.id !== before) })) }));
  await sleep(1500);
  check((await shown(phone)) === stay, "a tile closed elsewhere before this one: the phone stays on its tile", { shown: await shown(phone), stay });
  await normalSwipe(phone, "after a tile before it closed elsewhere");
  // The desktop opens a tile.
  await edit((L) => ({ ...L, mosaics: L.mosaics.map((m, i) => (i === 0 ? { ...m, tiles: [...m.tiles, { id: "tile_fromdesk", session_id: "ses_7d3a91e5c4b2" }] } : m)) }));
  await sleep(1500);
  o = await order(phone);
  check(o.includes("tile_fromdesk"), "a tile opened on the desktop arrives", o);
  const here = await shown(phone);
  check(here !== null && (await idx(phone)) === o.indexOf(here), "…and the phone stays where it was, square");
  await ws(phone, `m.focusPane(m.workspace.order()[m.workspace.order().length - 2])`);
  await sleep(500);
  await swipe(phone, 300, await textY(phone), 60);
  check((await shown(phone)) === "tile_fromdesk", "…and a swipe reaches it", await shown(phone));
  other.close();

  // ── folding and unfolding ─────────────────────────────────────────────
  console.log("— fold and unfold");
  await focusAt(phone, 0);
  await phone.viewport(673, 841, true, 2.625);
  await sleep(800);
  check((await ws(phone, `return m.workspace.mode()`)) === "dual", "unfolded: dual");
  const o2 = await order(phone);
  const first = await firstShown(phone);
  // A stroke on the left tile (touching it focuses it).
  await swipe(phone, 300, 450, 40);
  const dualIdx = await ws(phone, `return m.workspace.order().indexOf(m.workspace.focused())`);
  check(dualIdx >= 1, "unfolded, a swipe moves on", dualIdx);
  // With the left tile focused the pages used to turn and spring straight back (the focused
  // tile pulled them): the page itself must have moved on, and stay.
  await sleep(400);
  const firstNow = await firstShown(phone);
  check(o2.indexOf(firstNow) === o2.indexOf(first) + 1, "…a stroke on the left tile: the pages turn one and stay", { first, firstNow });
  // Back, and on again with the right tile focused: both ways, whichever tile has focus.
  await swipe(phone, 200, 450, 500);
  await sleep(400);
  check((await firstShown(phone)) === first, "…and back", await firstShown(phone));
  // A stroke on the right tile.
  await swipe(phone, 600, 450, 340);
  await sleep(400);
  check(o2.indexOf(await firstShown(phone)) === o2.indexOf(first) + 1, "…a stroke on the right tile: on again", await firstShown(phone));
  await phone.shot(`${SHOTS}/swipe-fold-open.png`);
  await phone.viewport(841, 701, true, 2.625);
  await sleep(700);
  await swipe(phone, 600, 400, 250);
  check((await ws(phone, `return m.workspace.mode()`)) === "dual", "held sideways: still dual");
  await phone.viewport(344, 882, true, 2.625);
  await sleep(800);
  const sq = await shown(phone);
  check(sq !== null && (await idx(phone)) === (await order(phone)).indexOf(sq), "folded again: the focused tile is on screen, square", { sq, idx: await idx(phone) });
  await normalSwipe(phone, "folded again");

  console.log("errors:", phone.errors);
  check(!phone.errors.length, "no page errors");
  await phone.close();
  finish();
});
