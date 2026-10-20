// Regression test: a tile deleted on the phone stays deleted (phone + desktop + server).
import { APP, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";
import { freshDeviceCases } from "./sync-fresh.mjs";

const tilesOf = (p) =>
  p.eval(`(() => [...document.querySelectorAll('.workspace [data-pane-id]')].filter(e => e.style.display !== 'none' && (() => { const r = e.getBoundingClientRect(); return r.right > 4 && r.left < innerWidth - 4; })()).map(e => e.dataset.paneId))()`);
const serverTiles = async (c) => {
  const L = (await c.rpc("get_mosaics", {})).result;
  return { version: L.version, ids: L.mosaics.flatMap((m) => m.tiles.map((t) => t.id)), L };
};

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  let probe = await wsClient("probe");
  const desk = await device();
  await desk.viewport(1440, 900);
  await signIn(desk, "Desktop");
  // three tiles: ctrl+click two sidebar chats next to the first one
  await desk.eval(`document.querySelector('.sidebar .srow-main')?.click()`);
  await sleep(500);
  for (let k = 0; k < 3; k++) {
    const pt = await desk.eval(`(() => { const e = [...document.querySelectorAll('.sidebar .srow:not(.shown) .srow-main')][0]; const r = e.getBoundingClientRect(); return [r.x + 30, r.y + r.height/2]; })()`);
    await desk.mouse(pt[0], pt[1], 2);
    await sleep(500);
  }
  await sleep(1500);
  let s = await serverTiles(probe);
  check(s.ids.length === 4, "desktop shared 4 tiles", s.ids);

  const phone = await device();
  await phone.viewport(375, 812, true, 2);
  await signIn(phone, "Phone");
  await sleep(1500);
  let pt = await tilesOf(phone);
  check(pt.length === 1 && s.ids.length === 4, "phone shows one page of the 4-tile mosaic", pt);
  const phoneAll = await phone.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order())`);
  check(phoneAll.length === 4, "phone has the 4 tiles", phoneAll);

  // 1) close the visible tile on the phone through the tile options
  const victim = pt[0];
  await phone.click(`[data-pane-id="${victim}"] .opts-btn, [data-pane-id="${victim}"] .title-menu`);
  await sleep(400);
  await phone.clickText(".act-btn", "Close");
  await sleep(2500);
  s = await serverTiles(probe);
  check(!s.ids.includes(victim), "server dropped the phone's closed tile", s.ids);
  check(!(await desk.eval(`!!document.querySelector('[data-pane-id="${victim}"]')`)), "desktop dropped it");
  check(!(await phone.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().includes(${JSON.stringify(victim)}))`)), "phone still has it closed");

  // 2) desktop changes focus in the meantime (noise) while the phone closes another one via the overview
  const order2 = await phone.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order())`);
  const victim2 = order2[order2.length - 1];
  await desk.click(`[data-pane-id="${order2[0]}"] .transcript, [data-pane-id="${order2[0]}"] .hero`).catch(() => {});
  await phone.eval(`__mod('lib/actions.ts').then(m => m.requestClosePane(${JSON.stringify(victim2)}))`);
  await sleep(120);
  await desk.click(`[data-pane-id="${order2[1]}"] .pane-head`).catch(() => {});
  await sleep(2500);
  s = await serverTiles(probe);
  check(!s.ids.includes(victim2), "server dropped the second closed tile (with desktop noise)", s.ids);
  check(!(await desk.eval(`!!document.querySelector('[data-pane-id="${victim2}"]')`)), "desktop dropped the second one");
  check(!(await phone.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().includes(${JSON.stringify(victim2)}))`)), "phone kept the second one closed");

  // 3) the phone closes a tile and is killed at once (Android kills backgrounded apps): it must still stick
  const order3 = await phone.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order())`);
  const victim3 = order3[0];
  await phone.eval(`__mod('lib/actions.ts').then(m => m.requestClosePane(${JSON.stringify(victim3)}))`);
  await sleep(60);
  await phone.eval(`document.dispatchEvent(new Event('visibilitychange')); window.dispatchEvent(new Event('pagehide'))`);
  await phone.reload(2500);
  s = await serverTiles(probe);
  check(!s.ids.includes(victim3), "server dropped a tile closed right before the app was killed", s.ids);
  check(!(await phone.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().includes(${JSON.stringify(victim3)}))`)), "phone kept it closed after a restart");
  check(!(await desk.eval(`!!document.querySelector('[data-pane-id="${victim3}"]')`)), "desktop dropped it too");

  // more tiles for the next steps (from the desktop)
  for (const sid of ["ses_5a3e8f10c2d9", "ses_1d9e4b7a0c52", "ses_a6c3f0e82b19"]) {
    await desk.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${sid}', { newPane: true }))`);
    await sleep(700);
  }
  await sleep(1500);
  const dn = await desk.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().length)`);
  const pn = await phone.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().length)`);
  check(pn === dn && pn >= 3, `the desktop's new tiles reached the phone (${pn} of ${dn})`);
  // 4) the connection is down when the phone closes a tile (a flaky mobile network), then it comes back
  const order4 = await phone.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order())`);
  check(order4.length > 1, "enough tiles for the offline step", order4);
  if (order4.length > 1) {
    const victim4 = order4[order4.length - 1];
    await mock("/__mock/drop?hard=1");
    await sleep(80);
    await phone.eval(`__mod('lib/actions.ts').then(m => m.requestClosePane(${JSON.stringify(victim4)}))`);
    await sleep(6000); // the phone and the desktop reconnect on their own
    probe = await wsClient("probe2");
    s = await serverTiles(probe);
    check(!s.ids.includes(victim4), "server dropped a tile closed while disconnected", s.ids);
    check(!(await phone.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().includes(${JSON.stringify(victim4)}))`)), "phone kept the offline close");
    check(!(await desk.eval(`!!document.querySelector('[data-pane-id="${victim4}"]')`)), "desktop dropped the offline close");
  }
  // 4b) closed while disconnected, and the app is killed before it reconnects
  await desk.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_91d0c3e7a8b2', { newPane: true }))`);
  await sleep(2000);
  const order4b = await phone.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order())`);
  if (order4b.length > 1) {
    const v = order4b[order4b.length - 1];
    await mock("/__mock/drop?hard=1");
    await sleep(80);
    await phone.eval(`__mod('lib/actions.ts').then(m => m.requestClosePane(${JSON.stringify(v)}))`);
    await sleep(100);
    await phone.reload(4000);
    probe = await wsClient("probe3");
    s = await serverTiles(probe);
    check(!s.ids.includes(v), "server dropped a tile closed offline right before a restart", s.ids);
    check(!(await phone.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().includes(${JSON.stringify(v)}))`)), "phone kept it closed after the restart");
    await sleep(1500);
    check(!(await desk.eval(`!!document.querySelector('[data-pane-id="${v}"]')`)), "desktop dropped it too");
  } else check(false, "desktop's new tile reached the phone", order4b);
  // 5) desktop closes one: phone follows
  const order5 = await desk.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order())`);
  check(order5.length > 1, "enough tiles for the desktop step", order5);
  if (order5.length > 1) {
    const v5 = order5[0];
    await desk.click(`[data-pane-id="${v5}"] .head-close`);
    await sleep(2000);
    check(!(await phone.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().includes(${JSON.stringify(v5)}))`)), "phone follows a desktop close");
    s = await serverTiles(probe);
    check(!s.ids.includes(v5), "server dropped the desktop close", s.ids);
  }
  console.log("phone errors:", phone.errors, "desktop errors:", desk.errors);
  check(!phone.errors.length && !desk.errors.length, "no page errors");
  await phone.close();
  await desk.close();
  // A second device signing in (fresh install / no stored base): its placeholder never joins the
  // shared mosaic, and empty, chat and board tiles closed on either side stay closed (sync-fresh.mjs).
  console.log("\n— second device signing in —");
  await freshDeviceCases();
  finish();
});
