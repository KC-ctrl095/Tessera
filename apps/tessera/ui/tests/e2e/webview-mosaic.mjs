// A web view opened in mosaic 2 stays in mosaic 2. The report: "I open a new mosaic, open the web
// view, search something, and it pops back up in the 1st mosaic". The cause was another device
// viewing the same mosaics: it saw the new web view arrive, found the chat it belongs to open in
// its own mosaic and moved the tile there (a layout change that synced back), and the first device
// found its mosaic empty and was taken to mosaic 1. So, with a second device (desktop or phone)
// looking at mosaic 1:
//   · the user's own web view stays where it was made, on both devices and on the server, and
//     nobody is taken to another mosaic;
//   · an empty mosaic you just opened is not left because another device changed something;
//   · an agent's board or note still lands beside its chat and never takes the user to it;
//   · what the user asked the server to show beside a chat (/show) still follows the chat on the
//     device that asked, even in a mosaic kept on this device only;
//   · a mosaic emptied by another device still moves you to one that has tiles.
import { SHOTS, MOCK, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const active = (p) => ws(p, `return m.workspace.active()`);
/** What each of the first four mosaics holds on this device: chats as chat:xxxx, boards as wb:xxxx, notes as note:xxxx. */
const where = (p) =>
  ws(
    p,
    `return [0, 1, 2, 3].map(i => m.workspace.orderOf(i).map(id => { const c = m.workspace.panes[id].content; return c.type === 'whiteboard' ? 'wb:' + c.boardId : c.type === 'note' ? 'note:' + c.noteId : c.type === 'chat' ? 'chat:' + c.sessionId : c.type; }))`,
  );
/** Keep a log of every change of the active mosaic on this device. */
const watch = (p) =>
  p.eval(`(async () => { const m = await __mod('lib/workspace.ts'); window.__trail = []; let last = m.workspace.active(); window.__t0 = performance.now(); setInterval(() => { const a = m.workspace.active(); if (a !== last) { window.__trail.push([Math.round(performance.now() - window.__t0), last, a]); last = a; } }, 20); })()`);
const trail = (p) => p.eval(`window.__trail`);
const serverLayout = async () => {
  const c = await wsClient("probe");
  const L = (await c.rpc("get_mosaics")).result;
  c.close();
  return L;
};
const boardsOf = (L) => L.mosaics.map((m) => m.tiles.filter((t) => t.board_id).map((t) => t.board_id));
const boardPanes = async (p, id) => (await where(p)).flat().filter((x) => x === `wb:${id}`).length;
const urlBoards = async () => (await (await fetch(`${MOCK}/__mock/boards`)).json()).boards.filter((b) => b.content.type === "url");
/** The chat the web view will belong with when the new mosaic has none: the latest one (as spawn.ts picks it). */
const latestChats = async (probe) =>
  (await probe.rpc("list_sessions", { archived: false })).result
    .filter((s) => s.kind === "chat" && !s.private && !s.archived && !s.internal)
    .sort((a, b) => b.updated_at - a.updated_at)
    .map((s) => s.id);
const visBox = (p, sel) =>
  p.eval(`(() => { const e = [...document.querySelectorAll(${JSON.stringify(sel)})].find(e => { const r = e.getBoundingClientRect(); return r.width && r.left >= 0 && r.right <= innerWidth; }); if (!e) return null; const r = e.getBoundingClientRect(); return { cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);

/** Open `sids` in mosaic 1 on a device. */
async function layOut(p, sids) {
  await ws(p, `m.openSession('${sids[0]}', { pane: m.workspace.focused() })`);
  await sleep(500);
  for (const s of sids.slice(1)) await ws(p, `m.openSession('${s}', { newPane: true })`);
  await sleep(1200);
}

/** The user's steps: a new mosaic, then its web view, then a search. */
async function openWebViewInNewMosaic(p, phone, q) {
  if (phone) {
    await ws(p, `m.newWorkspace()`);
    await sleep(500);
    // An empty mosaic offers the launcher, with New at the top right like every tile: New → Web view.
    // Wait for each step to be on screen (a busy machine is slow).
    const shown = (sel) => `[...document.querySelectorAll(${JSON.stringify(sel)})].some((e) => { const r = e.getBoundingClientRect(); return r.width && r.left >= 0 && r.right <= innerWidth; })`;
    await p.waitFor(shown(".launcher-new .head-new"), 4000);
    let bx = await visBox(p, ".launcher-new .head-new");
    await p.tap(bx.cx, bx.cy);
    await p.waitFor(`document.querySelector('.menu-pop .menu-item')`, 3000);
    await sleep(200);
    bx = await p.eval(`(() => { const e = [...document.querySelectorAll('.menu-item')].find(e => e.textContent.includes('Web view')); const r = e.getBoundingClientRect(); return { cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);
    await p.tap(bx.cx, bx.cy);
    await p.waitFor(`!document.querySelector('.drawer-layer.open') && ${shown(".wv-input")}`, 5000);
    await sleep(300);
    bx = await visBox(p, ".wv-input");
    await p.tap(bx.cx, bx.cy);
    await p.waitFor(`document.activeElement?.classList.contains('wv-input')`, 3000).catch(() => {});
  } else {
    await p.key("2", "Digit2", 1, 50); // Alt+2: mosaic 2
    await sleep(500);
    await p.click(`.topbar .tb-new-more`);
    await sleep(300);
    await p.clickText(".menu-item", "Web view");
    await sleep(500);
  }
  await p.type(q);
  await p.key("Enter", "Enter");
  // The board is made once the server has it.
  for (let i = 0; i < 20 && !(await urlBoards()).length; i++) await sleep(250);
}

run(async () => {
  // ── 1. the report: a second device is looking at mosaic 1 ────────────────────────────────────
  for (const [label, aPhone, bPhone] of [
    ["desktop + desktop", false, false],
    ["phone + desktop", true, false],
    ["desktop + phone", false, true],
  ]) {
    console.log(`— new mosaic, web view, search: ${label}`);
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const probe = await wsClient("probe");
    const chats = await latestChats(probe);
    const A = await device();
    await A.viewport(aPhone ? 375 : 1440, aPhone ? 812 : 900, aPhone, aPhone ? 2 : 1);
    await signIn(A, `A ${aPhone ? "phone" : "desktop"}`);
    await layOut(A, chats.slice(0, 2));
    const B = await device();
    await B.viewport(bPhone ? 375 : 1440, bPhone ? 812 : 900, bPhone, bPhone ? 2 : 1);
    await signIn(B, `B ${bPhone ? "phone" : "desktop"}`);
    await sleep(1500);
    const before = JSON.stringify((await where(B))[0]);
    check(before === JSON.stringify(chats.slice(0, 2).map((s) => `chat:${s}`)), "the second device shows mosaic 1 with the same chats, the newest one among them", before);
    await watch(A);
    await watch(B);
    await openWebViewInNewMosaic(A, aPhone, "best pizza near me");
    // Sample for a while: the move used to happen within a second, the jump back a couple later.
    let moved = false;
    for (let i = 0; i < 20; i++) {
      await sleep(250);
      const [a, b] = [await where(A), await where(B)];
      if (a[0].some((x) => x.startsWith("wb:")) || b[0].some((x) => x.startsWith("wb:"))) moved = true;
    }
    const bs = await urlBoards();
    check(bs.length === 1 && bs[0].by_user, "one web view board, made by the user", bs.map((b) => b.content));
    const W = bs[0]?.id;
    check(!moved, "the web view never showed up in mosaic 1 on either device");
    // (A phone started from the launcher's New chat: its empty chat tile stays on that device.)
    const shared = (l) => l.filter((x) => x !== "chat:null");
    check(JSON.stringify(shared((await where(A))[1])) === JSON.stringify([`wb:${W}`]) && (await boardPanes(A, W)) === 1, "it is the only tile of mosaic 2 here, once", await where(A));
    check(JSON.stringify(shared((await where(B))[1])) === JSON.stringify([`wb:${W}`]) && (await boardPanes(B, W)) === 1, "…and on the second device, in mosaic 2 too", await where(B));
    const L = await serverLayout();
    check(JSON.stringify(boardsOf(L)) === JSON.stringify([[], [W]]), "the shared layout has it in mosaic 2 only", boardsOf(L));
    check((await active(A)) === 1 && (await trail(A)).length === 1, "the first device is still on mosaic 2 (it only moved there once)", await trail(A));
    check((await active(B)) === 0 && (await trail(B)).length === 0, "the second device was not moved either", await trail(B));
    check(await A.eval(`!!document.querySelector('.wv-canvas.shown')`), "the page is on screen where it was opened");
    await A.shot(`${SHOTS}/webview-mosaic-${label.replace(/\W+/g, "-")}.png`);
    check(!A.errors.length && !B.errors.length, "no page errors", [A.errors, B.errors]);
    probe.close();
    await A.close();
    await B.close();
  }

  // ── 2. the web view's chat is open in mosaic 1 on the other device, and the web view is made next to its chat ──
  {
    console.log("— a web view made beside its chat, which the other device has in another mosaic");
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const probe = await wsClient("probe");
    const [x, y] = await latestChats(probe);
    const A = await device();
    await A.viewport(1440, 900);
    await signIn(A, "A");
    await layOut(A, [x, y]);
    // The other device keeps the same chat in a mosaic of its own, which is the one on screen there.
    const B = await device();
    await B.viewport(1440, 900);
    await signIn(B, "B");
    await sleep(1200);
    await ws(B, `m.switchWorkspace(2); m.openPane({ type: 'chat', sessionId: '${x}' }, null)`);
    await sleep(1500);
    await watch(A);
    await watch(B);
    await ws(A, `m.focusPane(m.workspace.order().find(id => m.workspace.panes[id].content.sessionId === '${x}'))`);
    await A.click(`.topbar .tb-new-more`);
    await sleep(300);
    await A.clickText(".menu-item", "Web view");
    await sleep(500);
    await A.type("example.com");
    await A.key("Enter", "Enter");
    await sleep(4000);
    const W = (await urlBoards())[0]?.id;
    const L = await serverLayout();
    const at = L.mosaics.findIndex((m) => m.tiles.some((t) => t.board_id === W));
    check(at === 0 && boardsOf(L).flat().length === 1, "it stays in mosaic 1, beside the chat it was made next to", boardsOf(L));
    check((await boardPanes(B, W)) === 1 && (await where(B))[2].every((t) => !t.startsWith("wb:")), "the other device did not pull it into its own mosaic", await where(B));
    check((await trail(A)).length === 0 && (await active(B)) === 2 && (await trail(B)).length === 0, "nobody was taken anywhere", [await trail(A), await trail(B)]);
    probe.close();
    await A.close();
    await B.close();
  }

  // ── 3. an empty mosaic you just opened is yours until you fill it ─────────────────────────────
  for (const [label, w, h, mobile] of [["desktop", 1440, 900, false], ["phone", 375, 812, true]]) {
    console.log(`— an empty new mosaic while another device changes things: ${label}`);
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const probe = await wsClient("probe");
    const chats = await latestChats(probe);
    const A = await device();
    await A.viewport(w, h, mobile, mobile ? 2 : 1);
    await signIn(A, `A ${label}`);
    await layOut(A, chats.slice(0, 2));
    await ws(A, `m.newWorkspace()`);
    await sleep(500);
    check((await active(A)) === 1 && (await ws(A, `return m.workspace.order().length`)) === 0 && (await A.eval(`!!document.querySelector('.launcher')`)), "on an empty mosaic 2 with the launcher");
    await watch(A);
    await mock("/__mock/mosaics/touch"); // another device renames mosaic 1
    await sleep(1000);
    await mock("/__mock/board", { session_id: chats[0], kind: "markdown" }); // an agent shows a board in mosaic 1
    await sleep(1500);
    check((await ws(A, `return m.workspace.orderOf(0).some(id => m.workspace.panes[id].content.type === 'whiteboard')`)), "the changes arrived (the agent's board is in mosaic 1)", await where(A));
    check((await active(A)) === 1 && (await trail(A)).length === 0 && (await A.eval(`!!document.querySelector('.launcher')`)), "…and this device stayed on the empty mosaic", await trail(A));
    // A web view started there (still empty) survives the same kind of change.
    await ws(A, `m.openPane({ type: 'draft', kind: 'url', sessionId: null })`);
    await sleep(500);
    await mock("/__mock/mosaics/touch");
    await sleep(1000);
    check((await active(A)) === 1 && (await ws(A, `return m.workspace.order().map(id => m.workspace.panes[id].content.type)`))[0] === "draft", "an empty web view tile there stays too", await where(A));
    probe.close();
    await A.close();
  }

  // ── 4. a mosaic emptied by another device: this one moves to a mosaic that has tiles ──────────
  {
    console.log("— a mosaic emptied elsewhere");
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const probe = await wsClient("probe");
    const chats = await latestChats(probe);
    const A = await device();
    await A.viewport(1440, 900);
    await signIn(A, "A");
    await layOut(A, chats.slice(0, 2));
    await ws(A, `m.switchWorkspace(1); m.openSession('${chats[2]}', { newPane: true })`);
    await sleep(1500);
    check((await active(A)) === 1 && (await serverLayout()).mosaics.length === 2, "two shared mosaics, the second on screen");
    // Another device closes the second mosaic's only tile.
    const L = await serverLayout();
    const c = await wsClient("closer");
    await c.rpc("save_mosaics", { layout: { version: L.version, mosaics: [L.mosaics[0]] } });
    c.close();
    await sleep(1500);
    check((await active(A)) === 0 && (await ws(A, `return m.workspace.order().length`)) > 0 && !(await A.eval(`!!document.querySelector('.launcher')`)), "the first device lands on the mosaic that still has tiles, not an empty one", [await active(A), await where(A)]);
    probe.close();
    await A.close();
  }

  // ── 5. agents: a board or note lands beside its chat and never takes you anywhere ───────────────
  {
    console.log("— an agent shows a board and a note while you are in mosaic 2");
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const probe = await wsClient("probe");
    const chats = await latestChats(probe);
    const A = await device();
    await A.viewport(1440, 900);
    await signIn(A, "A");
    await layOut(A, chats.slice(0, 2));
    const B = await device();
    await B.viewport(375, 812, true, 2);
    await signIn(B, "B phone");
    await sleep(1200);
    await ws(A, `m.newWorkspace(); m.openSession('${chats[2]}', { newPane: true })`);
    await sleep(1500);
    await watch(A);
    await watch(B);
    await mock("/__mock/board", { session_id: chats[0], kind: "markdown", ask: false });
    await sleep(2500);
    let W = (await (await fetch(`${MOCK}/__mock/boards`)).json()).boards.at(-1)?.id;
    let L = await serverLayout();
    let tiles = L.mosaics[0].tiles;
    const ci = tiles.findIndex((t) => t.session_id === chats[0]);
    check(tiles[ci + 1]?.board_id === W, "the agent's board sits right after its chat in mosaic 1", boardsOf(L));
    check((await boardPanes(A, W)) === 1 && (await boardPanes(B, W)) === 1, "once on each device", [await where(A), await where(B)]);
    check((await active(A)) === 1 && (await trail(A)).length === 0 && (await active(B)) === 0 && (await trail(B)).length === 0, "nobody was taken to it", [await trail(A), await trail(B)]);
    // An agent opening a web view (the browser tool's "show") is the same: beside its chat, nobody moved.
    await mock("/__mock/board", { session_id: chats[0], kind: "url", ask: false });
    await sleep(2500);
    const V = (await urlBoards()).at(-1)?.id;
    L = await serverLayout();
    check(!!V && boardsOf(L)[0].includes(V) && !boardsOf(L)[1].includes(V), "the agent's web view is in mosaic 1 with its chat", boardsOf(L));
    check((await boardPanes(A, V)) === 1 && (await boardPanes(B, V)) === 1 && (await active(A)) === 1 && (await trail(A)).length === 0 && (await active(B)) === 0, "…once on each device, nobody taken to it", [await trail(A), await trail(B)]);
    // "Show" on its card is an invitation: it goes to the tile where it already is, without opening a second one.
    await ws(A, `m.openBoard('${V}')`);
    await sleep(500);
    check((await active(A)) === 0 && (await boardPanes(A, V)) === 1, "“Show” goes to the mosaic where the board already is", [await active(A), await where(A)]);
    // Closed everywhere, it opens in the mosaic on screen instead (and stays there).
    await ws(A, `m.switchWorkspace(1)`);
    await ws(A, `for (const [i, ids] of [0, 1, 2].map(i => [i, m.workspace.orderOf(i)])) for (const id of ids) if (m.workspace.panes[id].content.boardId === '${V}') m.closePane(id)`);
    await sleep(300);
    await ws(A, `m.openBoard('${V}')`);
    await sleep(500);
    check((await active(A)) === 1 && (await boardPanes(A, V)) === 1 && (await where(A))[1].includes(`wb:${V}`), "…and with no tile left it opens in the mosaic on screen, which stays on screen", [await active(A), await where(A)]);
    // A note an agent writes and shows ("note" with show: true) in the chat on mosaic 1.
    const moves = (await trail(A)).length;
    const ws2 = await wsClient("sender");
    await ws2.rpc("send", { session_id: chats[0], content: [{ type: "text", text: "write a note about the plan and show it" }], delivery: "auto" });
    await sleep(5000);
    L = await serverLayout();
    const noteAt = L.mosaics.findIndex((m) => m.tiles.some((t) => t.note_id));
    check(noteAt === 0, "the note it showed is in mosaic 1, beside its chat", L.mosaics.map((m) => m.tiles.map((t) => t.note_id ?? "")));
    check((await active(A)) === 1 && (await trail(A)).length === moves && (await active(B)) === 0 && (await trail(B)).length === 0, "…and nobody was taken to that either", [await trail(A), await trail(B)]);
    ws2.close();
    probe.close();
    await A.close();
    await B.close();
  }

  // ── 6. /show: a page you ask for beside a chat follows the chat on this device, even in a mosaic kept here ──
  {
    console.log("— /show in a mosaic kept on this device only");
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const probe = await wsClient("probe");
    const chats = await latestChats(probe);
    const A = await device();
    await A.viewport(1440, 900);
    await signIn(A, "A");
    await layOut(A, [chats[0]]);
    await ws(A, `m.switchWorkspace(1); m.openSession('${chats[1]}', { newPane: true }); m.setMosaicLocal(1, true)`);
    await sleep(1200);
    const local = await ws(A, `return m.workspace.localOf(1)`);
    check(local, "mosaic 2 is kept on this device (the server doesn't know its chat's tile)");
    const B = await device();
    await B.viewport(1440, 900);
    await signIn(B, "B");
    await sleep(1200);
    await watch(A);
    await watch(B);
    await A.eval(`__mod('lib/boards.ts').then(b => b.showBesideChat('${chats[1]}', 'Docs', { type: 'markdown', markdown: 'hello' }))`);
    await sleep(3500);
    const bs = (await (await fetch(`${MOCK}/__mock/boards`)).json()).boards.filter((b) => b.content.type === "markdown" && b.by_user);
    const W = bs[0]?.id;
    const here = await where(A);
    check(bs.length === 1 && here[1].includes(`wb:${W}`) && !here[0].includes(`wb:${W}`), "on the device that asked, it is beside the chat in mosaic 2", here);
    check((await active(A)) === 1 && (await trail(A)).length === 0, "…without leaving it", await trail(A));
    const there = await where(B);
    check(there.flat().filter((x) => x === `wb:${W}`).length <= 1, "the other device has it at most once", there);
    probe.close();
    await A.close();
    await B.close();
  }

  finish();
});
