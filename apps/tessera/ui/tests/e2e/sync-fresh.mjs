// A second device signing in (fresh install, or an install without a stored merge base) must not
// merge its placeholder tile into the shared mosaic, and every tile closed on either side stays closed.
import { MOCK, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const order = (p) => p.eval(`__mod('lib/workspace.ts').then(m => [...Array(9).keys()].flatMap(i => m.workspace.orderOf(i)))`);
const panes = (p) => p.eval(`__mod('lib/workspace.ts').then(m => Object.values(m.workspace.panes).map(x => ({ id: x.id, c: x.content.type === 'chat' ? (x.content.sessionId ?? 'new') : x.content.type === 'whiteboard' ? 'board:' + x.content.boardId : 'other' })))`);
const server = async () => {
  const c = await wsClient("probe");
  const L = (await c.rpc("get_mosaics")).result;
  c.close();
  return L.mosaics.flatMap((m) => m.tiles.map((t) => ({ id: t.id, c: t.session_id ?? (t.board_id ? "board:" + t.board_id : "new") })));
};
const has = (list, id) => list.some((x) => (x.id ?? x) === id);

export async function freshDeviceCases() {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  // 1) desktop with a synced layout: two chats, a board, an empty "new chat" tile
  const desk = await device();
  await desk.viewport(1440, 900);
  await signIn(desk, "Desktop");
  await desk.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_2b7e91c04d5a', { pane: m.workspace.focused() }))`);
  await sleep(500);
  await desk.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_91d0c3e7a8b2', { newPane: true }))`);
  await sleep(500);
  await desk.eval(`__mod('lib/workspace.ts').then(m => m.newChat({ newPane: true }))`);
  await sleep(500);
  const board = await mock("/__mock/board", { session_id: "ses_2b7e91c04d5a", kind: "markdown" });
  await sleep(2000);
  let S = await server();
  check(S.length === 4 && S.some((t) => t.c === "new") && S.some((t) => t.c.startsWith("board:")), "desktop shared chats, a board and an empty tile", S);
  const deskIds = S.map((t) => t.id);

  // 2) the phone signs in fresh: its own placeholder must not join the shared mosaic
  const phone = await device();
  await phone.viewport(375, 812, true, 2);
  await signIn(phone, "Phone");
  await sleep(2500);
  S = await server();
  check(S.length === 4 && S.every((t) => deskIds.includes(t.id)), "a fresh phone adopts the shared layout (no placeholder merged in)", S);
  const P = await panes(phone);
  check(P.length === 4 && P.every((x) => deskIds.includes(x.id)), "the phone shows exactly the shared tiles", P);

  // 3) close on each side: the empty tile, a board and a chat on the phone; a chat on the desktop
  const emptyTile = S.find((t) => t.c === "new").id;
  const boardTile = S.find((t) => t.c.startsWith("board:")).id;
  const chatA = S.find((t) => t.c === "ses_91d0c3e7a8b2").id;
  const chatB = S.find((t) => t.c === "ses_2b7e91c04d5a").id;
  for (const id of [emptyTile, boardTile]) {
    await phone.eval(`__mod('lib/actions.ts').then(m => m.requestClosePane('${id}'))`);
    await sleep(900);
  }
  await desk.eval(`__mod('lib/actions.ts').then(m => m.requestClosePane('${chatA}'))`);
  await sleep(2500);
  const closed = [emptyTile, boardTile, chatA];
  const allGone = async (label) => {
    S = await server();
    const po = await order(phone);
    const dO = await order(desk);
    check(closed.every((id) => !has(S, id)), `${label}: gone from the server`, S);
    check(closed.every((id) => !po.includes(id)), `${label}: gone on the phone`, po);
    check(closed.every((id) => !dO.includes(id)), `${label}: gone on the desktop`, dO);
    return { S, po, dO };
  };
  await allGone("closed empty, board and chat tiles");
  check(!!(await (await fetch(MOCK + "/__mock/boards")).json()).boards.find((b) => b.id === board.id), "the closed board itself still exists");

  // 4) reload both (existing installs with a stored base), wait for sync
  await phone.reload(2500);
  await desk.reload(2500);
  await sleep(1500);
  await allGone("after reloading both");

  // 5) the phone is an existing install without a stored base (from before bases were saved)
  await phone.eval(`(() => { for (const k of Object.keys(localStorage)) if (k.startsWith('tessera.workspace.')) { const v = JSON.parse(localStorage.getItem(k)); delete v.sync; localStorage.setItem(k, JSON.stringify(v)); } })()`);
  await phone.eval(`__mod('lib/workspace.ts').then(m => { m.setSyncState(null); })`);
  await phone.reload(3000);
  await sleep(1500);
  await allGone("phone without a stored base, reloaded");

  // 6) the last chat closed on the phone; the desktop's own remaining tiles close too; nothing comes back
  await phone.eval(`__mod('lib/actions.ts').then(m => m.requestClosePane('${chatB}'))`);
  await sleep(2500);
  closed.push(chatB);
  const r = await allGone("the last shared tile closed on the phone");
  check(r.S.length === 0, "the shared layout is empty", r.S);
  await phone.reload(2500);
  await desk.reload(2500);
  await sleep(2000);
  const r2 = await allGone("after reloading both again");
  check(r2.S.length === 0, "still empty after reloads: no placeholder was pushed by either device", r2.S);

  // 7) the phone's own initial tile (fresh install, empty server): closing it sticks after a reload
  await mock("/__mock/mosaics/reset");
  const phone2 = await device();
  await phone2.viewport(375, 812, true, 2);
  await signIn(phone2, "Phone 2");
  await sleep(2000);
  const init = await order(phone2);
  S = await server();
  check(!init.some((id) => has(S, id)), "a fresh device's initial tile isn't pushed to the server", { init, S });
  for (const id of init) await phone2.eval(`__mod('lib/actions.ts').then(m => m.requestClosePane('${id}'))`);
  await sleep(1500);
  await phone2.reload(2500);
  await sleep(1000);
  const after = await order(phone2);
  check(!after.some((id) => init.includes(id)) && after.length === 0, "the phone's initial tile stays closed after a reload (no new tile invented)", { init, after });
  S = await server();
  check(!S.length, "and it never reached the server", S);
  // a tile the phone actually uses is shared as usual
  await phone2.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_5a3e8f10c2d9', { newPane: true }))`);
  await sleep(1500);
  S = await server();
  check(S.some((t) => t.c === "ses_5a3e8f10c2d9"), "a chat opened on the fresh phone is shared", S);
  console.log("errors:", desk.errors, phone.errors, phone2.errors);
  check(!desk.errors.length && !phone.errors.length && !phone2.errors.length, "no page errors");
  for (const p of [desk, phone, phone2]) await p.close();
}

if (import.meta.url === `file://${process.argv[1]}`) run(async () => {
  await freshDeviceCases();
  finish();
});
