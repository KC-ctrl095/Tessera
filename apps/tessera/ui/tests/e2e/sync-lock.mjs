// A mosaic kept on one device ("Only on this device"): it leaves the shared layout, other devices drop it,
// it survives reloads on its own device, never comes back by itself, and shares again when unlocked.
// Also: locked while offline, a concurrent edit elsewhere, and shared mosaics that don't fit (overflow).
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const orderOf = (p, i) => ws(p, `return m.workspace.orderOf(${i})`);
const localOf = (p, i) => ws(p, `return m.workspace.localOf(${i})`);
const used = (p) => ws(p, `return m.usedWorkspaces()`);
const allTiles = (p) => ws(p, `return Array.from({ length: 9 }, (_, i) => m.workspace.orderOf(i)).flat()`);
const server = async (c) => {
  const L = (await c.rpc("get_mosaics", {})).result;
  return { version: L.version, ids: L.mosaics.flatMap((m) => m.tiles.map((t) => t.id)), mosaics: L.mosaics, L };
};
const dupes = (list) => list.filter((x, i) => list.indexOf(x) !== i);
const offline = async (p, on) => {
  await p.send("Network.enable");
  await p.send("Network.emulateNetworkConditions", { offline: on, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
};

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  let probe = await wsClient("probe");
  const desk = await device();
  await desk.viewport(1440, 900);
  await signIn(desk, "Desktop");
  // Mosaic 1: two chats. Mosaic 2: two chats.
  await ws(desk, `m.openSession('ses_7d3a91e5c4b2', { pane: m.workspace.focused() })`);
  await sleep(400);
  await ws(desk, `m.openSession('ses_5a3e8f10c2d9', { newPane: true })`);
  await sleep(400);
  await ws(desk, `m.switchWorkspace(1); m.openSession('ses_1d9e4b7a0c52')`);
  await sleep(400);
  await ws(desk, `m.openSession('ses_a6c3f0e82b19', { newPane: true })`);
  await sleep(400);
  await ws(desk, `m.switchWorkspace(0)`);
  await sleep(1500);
  let s = await server(probe);
  check(s.mosaics.length === 2 && s.ids.length === 4, "desktop shared two mosaics, four tiles", s.mosaics.map((m) => [m.id, m.tiles.length]));
  const shared2 = await orderOf(desk, 1);

  const phone = await device();
  await phone.viewport(375, 812, true, 2);
  await signIn(phone, "Phone");
  await sleep(1500);
  check((await orderOf(phone, 1)).length === 2, "the phone has mosaic 2", await orderOf(phone, 1));

  // 1) Lock mosaic 2 on the desktop from the top bar (right-click the pill, "Only on this device").
  const pill = await desk.box(`.ws-pill[data-ws="1"]`);
  await desk.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: pill.cx, y: pill.cy });
  await desk.send("Input.dispatchMouseEvent", { type: "mousePressed", x: pill.cx, y: pill.cy, button: "right", clickCount: 1 });
  await desk.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: pill.cx, y: pill.cy, button: "right", clickCount: 1 });
  await sleep(300);
  await desk.clickText(".menu-item", "Only on this device");
  await sleep(1800);
  s = await server(probe);
  check(s.mosaics.length === 1 && s.ids.length === 2, "the server no longer has the locked mosaic", s.mosaics.map((m) => [m.id, m.tiles.length]));
  check(await localOf(desk, 1), "the desktop marks mosaic 2 as this device's");
  const local2 = await orderOf(desk, 1);
  check(local2.length === 2 && !local2.some((id) => shared2.includes(id) || s.ids.includes(id)), "the desktop keeps both tiles, under ids of its own", { local2, shared2 });
  check(await desk.eval(`!!document.querySelector('.ws-pill[data-ws="1"] .ws-lock')`), "the pill shows a lock");
  check((await orderOf(phone, 1)).length === 0 && !(await used(phone)).includes(1), "the phone dropped mosaic 2", await used(phone));
  await desk.shot(`${SHOTS}/lock-desktop-strip.png`, { x: 250, y: 0, width: 520, height: 44 });

  // 2) Reloads: nothing comes back, nothing is lost.
  await desk.reload(2500);
  await desk.waitFor(`document.querySelector('.workspace')`);
  await sleep(1200);
  check(await localOf(desk, 1), "still this device's after a reload");
  check(JSON.stringify(await orderOf(desk, 1)) === JSON.stringify(local2), "the locked tiles are all still there after a reload", await orderOf(desk, 1));
  s = await server(probe);
  check(s.mosaics.length === 1, "a reload did not share it again", s.mosaics.length);
  await phone.reload(2500);
  await sleep(1200);
  check((await orderOf(phone, 1)).length === 0, "the phone still doesn't show it after a reload");

  // 3) Edits elsewhere and here don't cross the line.
  await ws(phone, `m.openSession('ses_91d0c3e7a8b2', { newPane: true })`);
  await sleep(2000);
  check((await orderOf(desk, 0)).length === 3, "a phone edit to a shared mosaic reaches the desktop", await orderOf(desk, 0));
  check(JSON.stringify(await orderOf(desk, 1)) === JSON.stringify(local2), "…and leaves the locked one alone");
  await ws(desk, `m.switchWorkspace(1); m.openSession('ses_c47a2e9b1f03', { newPane: true })`);
  await sleep(1800);
  const local3 = await orderOf(desk, 1);
  s = await server(probe);
  check(local3.length === 3 && s.mosaics.length === 1 && !local3.some((id) => s.ids.includes(id)), "a tile added to the locked mosaic stays on the desktop", { local3, server: s.ids });
  check((await used(phone)).length === 1, "the phone still has one mosaic", await used(phone));

  // 4) The phone makes a new mosaic where the desktop keeps its own: it lands in another slot there.
  await ws(phone, `m.switchWorkspace(1); m.openSession('ses_1d9e4b7a0c52')`);
  await sleep(2200);
  s = await server(probe);
  check(s.mosaics.length === 2, "the phone's new mosaic is shared", s.mosaics.map((m) => m.id));
  const phoneNew = s.mosaics.find((m) => m.tiles.length === 1 && m.tiles[0].session_id === "ses_1d9e4b7a0c52");
  const deskSlots = await ws(desk, `return Array.from({ length: 9 }, (_, i) => m.workspace.orderOf(i))`);
  const where = deskSlots.findIndex((o) => phoneNew && o.includes(phoneNew.tiles[0].id));
  check(where >= 0 && where !== 1, "the desktop put the phone's mosaic in a free slot", { where, deskSlots });
  check(JSON.stringify(deskSlots[1]) === JSON.stringify(local3), "the desktop's own mosaic 2 is untouched");
  check(!dupes(await allTiles(desk)).length, "no tile is in two places on the desktop");

  // 5) Unlock: shared again, as a mosaic of its own.
  await ws(desk, `m.setMosaicLocal(1, false)`);
  await sleep(2000);
  s = await server(probe);
  const back = s.mosaics.find((m) => local3.every((id) => m.tiles.some((t) => t.id === id)));
  check(!!back && s.mosaics.length === 3, "unlocking shared it again (all its tiles, a mosaic of its own)", s.mosaics.map((m) => [m.id, m.tiles.length]));
  check(!(await localOf(desk, 1)), "the desktop no longer marks it");
  const phoneTiles = await allTiles(phone);
  check(local3.every((id) => phoneTiles.includes(id)), "the phone shows it again", { local3, phoneTiles });
  check(!dupes(phoneTiles).length && !dupes(s.ids).length, "no duplicates anywhere");
  await desk.reload(2500);
  await phone.reload(2500);
  await sleep(1500);
  s = await server(probe);
  check(s.mosaics.length === 3 && !(await localOf(desk, 1)), "reloads keep it shared", s.mosaics.length);

  // 6) Locked while offline, reloaded before it could tell the server: still locked, and the server drops it.
  const victim = await ws(desk, `return m.workspace.orderOf(0)`);
  await offline(desk, true);
  await mock("/__mock/drop?hard=1");
  await sleep(150);
  await ws(desk, `m.setMosaicLocal(0, true)`);
  await sleep(300);
  await desk.reload(1200);
  await offline(desk, false);
  await sleep(6000);
  probe = await wsClient("probe2");
  s = await server(probe);
  check(await localOf(desk, 0), "locked offline and reloaded: still this device's");
  check((await orderOf(desk, 0)).length === victim.length, "…with all its tiles", await orderOf(desk, 0));
  check(!victim.some((id) => s.ids.includes(id)) && s.mosaics.length === 2, "the server dropped it once the desktop was back", s.mosaics.map((m) => [m.id, m.tiles.length]));
  await sleep(1000);
  check(!victim.some((id) => phoneTiles.includes(id)) || !(await allTiles(phone)).some((id) => victim.includes(id)), "the phone dropped it too");

  // 7) Locked while another device changed it: that change is kept (shared), the lock is kept (here).
  const target = s.mosaics[0];
  const targetSlot = (await ws(desk, `return Array.from({ length: 9 }, (_, i) => m.workspace.orderOf(i))`)).findIndex((o) => o.includes(target.tiles[0].id));
  await offline(desk, true);
  await mock("/__mock/drop?hard=1");
  await sleep(150);
  await ws(desk, `m.setMosaicLocal(${targetSlot}, true)`);
  probe = await wsClient("probe3");
  let L = (await probe.rpc("get_mosaics", {})).result;
  L.mosaics = L.mosaics.map((m) => (m.id === target.id ? { ...m, tiles: [...m.tiles, { id: "tile_elsewhere", session_id: "ses_91d0c3e7a8b2" }] } : m));
  const saved = await probe.rpc("save_mosaics", { layout: L });
  check(!saved.error, "another device edited that mosaic meanwhile", saved.error);
  await offline(desk, false);
  await sleep(6000);
  s = await server(probe);
  const kept = s.mosaics.find((m) => m.id === target.id);
  check(!!kept && kept.tiles.some((t) => t.id === "tile_elsewhere"), "the other device's edit stays shared", s.mosaics.map((m) => [m.id, m.tiles.map((t) => t.id)]));
  check(await localOf(desk, targetSlot), "the desktop kept its locked copy");
  const deskAll = await allTiles(desk);
  check(deskAll.includes("tile_elsewhere") && !dupes(deskAll).length, "the desktop shows the edited shared one too, no duplicates", deskAll);

  // 8) More shared mosaics than free slots here: the extra ones are kept on the server, not dropped.
  const sids = ["ses_7d3a91e5c4b2", "ses_5a3e8f10c2d9", "ses_1d9e4b7a0c52", "ses_a6c3f0e82b19", "ses_91d0c3e7a8b2", "ses_c47a2e9b1f03", "ses_wb7c1e5a9f20", "ses_7d3a91e5c4b2", "ses_5a3e8f10c2d9"];
  L = (await probe.rpc("get_mosaics", {})).result;
  const nine = sids.map((sid, k) => ({ id: `x${k + 1}`, name: `Mosaic ${k + 1}`, layout: "dwindle", tiles: [{ id: `tile_x${k + 1}`, session_id: sid }] }));
  const r9 = await probe.rpc("save_mosaics", { layout: { version: L.version, mosaics: nine } });
  check(!r9.error, "another device laid out nine mosaics", r9.error);
  await sleep(2000);
  const locals = await ws(desk, `return Array.from({ length: 9 }, (_, i) => m.workspace.localOf(i)).filter(Boolean).length`);
  const shown = (await allTiles(desk)).filter((id) => id.startsWith("tile_x")).length;
  check(locals === 2 && shown === 7, "the desktop shows the seven that fit beside its two own", { locals, shown });
  await ws(desk, `m.switchWorkspace(${targetSlot}); m.openSession('ses_1d9e4b7a0c52', { newPane: true })`);
  await sleep(2000);
  s = await server(probe);
  check(s.mosaics.length === 9 && nine.every((m) => s.ids.includes(m.tiles[0].id)), "a push from the desktop keeps all nine on the server", s.mosaics.map((m) => m.id));
  // Deleting one of its own frees a slot: the one that didn't fit shows up.
  await ws(desk, `m.deleteWorkspace(${targetSlot})`);
  await sleep(2000);
  const shown2 = (await allTiles(desk)).filter((id) => id.startsWith("tile_x")).length;
  check(shown2 === 8, "a freed slot takes one that didn't fit", shown2);
  s = await server(probe);
  check(s.mosaics.length === 9, "the server still has nine", s.mosaics.length);
  await desk.reload(2500);
  await sleep(1500);
  const shown3 = (await allTiles(desk)).filter((id) => id.startsWith("tile_x")).length;
  check(shown3 === 8 && (await ws(desk, `return Array.from({ length: 9 }, (_, i) => m.workspace.localOf(i)).filter(Boolean).length`)) === 1, "after a reload: eight shared shown, one of its own", shown3);
  s = await server(probe);
  check(s.mosaics.length === 9, "and the server still has nine", s.mosaics.length);

  console.log("phone errors:", phone.errors, "desktop errors:", desk.errors);
  check(!phone.errors.length && !desk.errors.length, "no page errors");
  await phone.close();
  await desk.close();

  // 9) A board for a chat whose only tile is in a mosaic kept on this device: the server can't find the
  // chat's tile, so it makes a mosaic up ("Main": the chat and the board). The board moves beside the chat
  // here, and the made-up mosaic goes instead of lingering as a duplicate or empty one.
  console.log("\n— a board for a chat kept on this device —");
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const probe9 = await wsClient("probe9");
  const d9 = await device();
  await d9.viewport(1440, 900);
  await signIn(d9, "Desktop 9");
  await ws(d9, `m.openSession('ses_wb7c1e5a9f20', { pane: m.workspace.focused() })`);
  await sleep(1500);
  await ws(d9, `m.setMosaicLocal(0, true)`);
  await sleep(1500);
  s = await server(probe9);
  check(s.mosaics.length === 0, "the only mosaic is kept on this device: nothing shared", s.mosaics.length);
  const bd = await mock("/__mock/board", { session_id: "ses_wb7c1e5a9f20", kind: "markdown", ask: false });
  await sleep(2500);
  const bpane = await ws(d9, `return m.paneShowingBoard('${bd.id}')`);
  check(bpane && (await ws(d9, `return m.workspace.paneWorkspace('${bpane}')`)) === 0, "the board sits beside its chat, in the mosaic kept here");
  check(JSON.stringify(await used(d9)) === "[0]", "no made-up mosaic is left in the strip", await used(d9));
  check((await d9.eval(`document.querySelectorAll('.ws-pill').length`)) <= 1, "…nor on screen");
  s = await server(probe9);
  check(!s.mosaics.length, "…nor on the server", s.mosaics.map((m) => [m.id, m.name, m.tiles.length]));
  // Control: when the server puts the board at the end of a real shared mosaic, that mosaic stays.
  await ws(d9, `m.switchWorkspace(1); m.openSession('ses_7d3a91e5c4b2')`);
  await sleep(400);
  await ws(d9, `m.switchWorkspace(0)`);
  await sleep(1800);
  s = await server(probe9);
  check(s.mosaics.length === 1 && s.mosaics[0].tiles.length === 1, "a shared mosaic with a chat of its own", s.mosaics.map((m) => m.tiles.length));
  const bd2 = await mock("/__mock/board", { session_id: "ses_wb7c1e5a9f20", kind: "markdown", ask: false });
  await sleep(2500);
  const bpane2 = await ws(d9, `return m.paneShowingBoard('${bd2.id}')`);
  check(bpane2 && (await ws(d9, `return m.workspace.paneWorkspace('${bpane2}')`)) === 0, "the next board also goes beside its chat");
  check(JSON.stringify(await used(d9)) === "[0,1]" && (await orderOf(d9, 1)).length === 1, "…and the shared mosaic it landed in stays, with its chat", { used: await used(d9), m2: await orderOf(d9, 1) });
  s = await server(probe9);
  check(s.mosaics.length === 1 && s.mosaics[0].tiles.length === 1 && s.mosaics[0].tiles[0].session_id === "ses_7d3a91e5c4b2", "…on the server too", s.mosaics.map((m) => m.tiles));
  console.log("errors:", d9.errors);
  check(!d9.errors.length, "no page errors (9)");
  finish();
});
