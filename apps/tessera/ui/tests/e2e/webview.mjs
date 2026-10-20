// The web view tile (the server's browser, drawn from frames; the mock pretends to be the browser):
// the New button and its menu, one action → one tile, input mapping (mouse, wheel, keys, text,
// paste), the toolbar (back, forward, reload, address, zoom), dialogs, attaching only while on
// screen with acknowledged frames, the agent's browser steps as compact cards (in the background, with
// pictures and "Watch"; "Show tile" once watched),
// whiteboards in the same chrome, and on a phone: the keyboard for fields, touch, pinch.
import { SHOTS, MOCK, check, device, finish, mock, run, signIn, sleep, wsClient, menuLabels } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const A_SID = "ses_7d3a91e5c4b2";
const B_SID = "ses_5a3e8f10c2d9";
const browser = async () => (await fetch(`${MOCK}/__mock/browser`)).json();
const resetLog = () => fetch(`${MOCK}/__mock/browser/reset`, { method: "POST" });
const log = async (pred = () => true) => (await browser()).log.filter(pred);
const inputs = async (type, kind) => (await log((l) => l.event && l.event.type === type && (!kind || l.event.kind === kind))).map((l) => l.event);
const tabOf = async (id) => (await browser()).tabs.find((t) => t.board_id === id);
const order = (p) => ws(p, `return m.workspace.order()`);
const boardPanes = (p, boardId) => ws(p, `return m.workspace.order().filter(id => m.workspace.panes[id].content.boardId === '${boardId}')`);
/** A point on the page (CSS px of the tab), as a screen point. */
const pagePoint = async (p, pane, x, y) => {
  const r = await p.eval(`(() => { const c = document.querySelector('[data-pane-id="${pane}"] .wv-canvas'); const r = c.getBoundingClientRect(); return { x: r.x, y: r.y }; })()`);
  return { x: r.x + x, y: r.y + y };
};
const mclick = async (p, pt, opts = {}) => {
  const button = opts.button ?? "left";
  await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: pt.x, y: pt.y });
  await p.send("Input.dispatchMouseEvent", { type: "mousePressed", x: pt.x, y: pt.y, button, clickCount: opts.count ?? 1, buttons: button === "right" ? 2 : 1 });
  await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: pt.x, y: pt.y, button, clickCount: opts.count ?? 1 });
  await sleep(opts.wait ?? 250);
};
const waitFor = async (fn, ms = 4000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const v = await fn();
    if (v) return v;
    await sleep(80);
  }
  return null;
};
const touch = (p, type, pts) => p.send("Input.dispatchTouchEvent", { type, touchPoints: pts.map(([x, y], id) => ({ x, y, id })) });

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const probe = await wsClient("probe");
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Desktop");
  await ws(p, `m.openSession('${A_SID}', { pane: m.workspace.focused() })`);
  await sleep(700);
  const [A] = await order(p);

  // ── the New button ────────────────────────────────────────────────────
  console.log("— new button");
  check(!(await p.eval(`!!document.querySelector('.topbar .ws-add, .topbar [aria-label="Add a tile"]')`)), "the top bar has no new-mosaic plus and no separate tile plus");
  check(await p.eval(`!!document.querySelector('.topbar .ws-pill') && !!document.querySelector('.topbar [aria-label="All mosaics"]')`), "with one mosaic its tab and All mosaics are already visible");
  check((await p.eval(`document.querySelectorAll('.topbar [aria-label="New chat"]').length`)) === 1, "one New chat button");
  await p.click(`.topbar .tb-new-more`);
  await sleep(250);
  const menu = await menuLabels(p);
  check(JSON.stringify(menu) === JSON.stringify(["Chat", "Private chat", "Note", "Web view", "More"]), "its menu: Chat, Private chat, Note, Web view, then More", menu);
  await p.shot(`${SHOTS}/new-menu-desktop.png`, await p.eval(`(() => { const r = document.querySelector('.menu-pop').getBoundingClientRect(); return { x: r.x - 220, y: 0, width: r.width + 240, height: r.bottom + 10 }; })()`));
  await p.key("Escape");
  await sleep(150);
  let n0 = (await order(p)).length;
  await p.click(`.topbar .tb-new-main`);
  await sleep(400);
  check((await order(p)).length === n0 + 1 && (await p.eval(`document.activeElement?.closest('.composer') != null`)), "a click on New chat opens a chat at once, ready to type");
  await ws(p, `m.closePane(m.workspace.focused())`);
  await sleep(300);

  // ── a web view: one action, one tile ──────────────────────────────────
  console.log("— one tile per web view");
  await ws(p, `m.focusPane('${A}')`);
  await p.click(`.topbar .tb-new-more`);
  await sleep(250);
  await p.clickText(".menu-item", "Web view");
  await sleep(500);
  const W = await ws(p, `return m.workspace.order().find(id => m.workspace.panes[id].content.type === 'draft')`);
  await p.waitFor(`document.activeElement === document.querySelector('[data-pane-id="${W}"] .wv-input')`, 4000);
  check(!!W && (await p.eval(`document.activeElement?.classList.contains('wv-input')`)), "Web view opens an empty browser tile, its address field ready");
  await p.shot(`${SHOTS}/webview-new-desktop.png`);
  n0 = (await order(p)).length;
  await p.type("example.com");
  await p.key("Enter", "Enter");
  // A second Enter (or a double tap on Go) never makes a second one.
  await p.key("Enter", "Enter");
  await sleep(1500);
  let boards = (await probe.rpc("list_boards", { session_id: A_SID })).result.filter((b) => b.content.type === "url");
  check(boards.length === 1 && boards[0].content.url === "https://example.com/" && boards[0].by_user, "one web view board, for https://example.com/", boards.map((b) => b.content));
  const B = boards[0]?.id;
  check((await order(p)).length === n0 && JSON.stringify(await boardPanes(p, B)) === JSON.stringify([W]), "shown in the tile it was typed into, and nowhere else", { order: await order(p), panes: await boardPanes(p, B) });
  const tiles = (await probe.rpc("get_mosaics", {})).result.mosaics.flatMap((m) => m.tiles).filter((t) => t.board_id === B);
  check(tiles.length === 1, "the shared layout has it once", tiles);
  check(tiles[0]?.id === W, "that is the tile it was typed into (the server added none of its own)", { tiles, W });
  const shown = await waitFor(() => p.eval(`!!document.querySelector('[data-pane-id="${W}"] .wv-canvas.shown')`));
  let b = await browser();
  check(shown && b.attached[B] === 1, "the page is drawn from the server's tab (attached once)", { shown, attached: b.attached });
  check(b.frames >= 1 && b.frames - b.acks <= 1, "every frame is acknowledged (the next only after that)", { frames: b.frames, acks: b.acks });
  const size = await p.eval(`(() => { const c = document.querySelector('[data-pane-id="${W}"] .wv-canvas'); return { w: c.width, h: c.height, cw: c.clientWidth, ch: c.clientHeight, dpr: devicePixelRatio }; })()`);
  check(size.w === Math.round(size.cw * size.dpr) && size.h === Math.round(size.ch * size.dpr), "the picture is drawn at the tile's size × the pixel ratio", size);
  check((await p.eval(`document.querySelector('[data-pane-id="${W}"] .wv-input').value`)) === "example.com", "the address field shows the page's address");

  // ── input ─────────────────────────────────────────────────────────────
  console.log("— input");
  await resetLog();
  await mclick(p, await pagePoint(p, W, 60, 100), { wait: 700 });
  const downs = await inputs("mouse", "down");
  const ups = await inputs("mouse", "up");
  check(downs.length === 1 && Math.abs(downs[0].x - 60) <= 1 && Math.abs(downs[0].y - 100) <= 1 && downs[0].button === "left" && downs[0].clicks === 1, "a click arrives at the same spot in the page (CSS px)", downs[0]);
  check(ups.length === 1 && ups[0].button === "left", "…pressed and released", ups[0]);
  let tab = await tabOf(B);
  check(tab.url === "https://example.com/next" && tab.can_go_back, "the page followed its link", tab);
  check((await p.eval(`document.querySelector('[data-pane-id="${W}"] .wv-input').value`)) === "example.com/next", "the address follows");
  check(!(await p.eval(`document.querySelector('[data-pane-id="${W}"] [aria-label="Back"]').disabled`)), "Back is now possible");
  boards = (await probe.rpc("list_boards", { session_id: A_SID })).result.filter((x) => x.id === B);
  check(boards[0].content.url === "https://example.com/next" && boards[0].version === 1, "the board follows the page (no new version for navigating)", boards[0] && { url: boards[0].content.url, v: boards[0].version });
  await resetLog();
  await mclick(p, await pagePoint(p, W, 300, 400), { count: 1, wait: 30 });
  await mclick(p, await pagePoint(p, W, 300, 400), { count: 2 });
  check((await inputs("mouse", "down")).map((e) => e.clicks).join() === "1,2", "a double click counts its clicks", await inputs("mouse", "down"));
  await resetLog();
  await mclick(p, await pagePoint(p, W, 320, 420), { button: "right" });
  check((await log(l=>l.page)).some(l=>l.page.type==='context') && (await p.eval(`!!document.querySelector('.menu-pop')`)), "a right click opens the device copy/link/paste menu for that page point");
  await p.key('Escape');
  await sleep(150);
  await resetLog();
  const wp = await pagePoint(p, W, 300, 500);
  await p.send("Input.dispatchMouseEvent", { type: "mouseWheel", x: wp.x, y: wp.y, deltaX: 0, deltaY: 60 });
  // A fast spin: five more within one frame.
  await p.eval(`(() => { const c = document.querySelector('[data-pane-id="${W}"] .wv-canvas'); for (let i = 0; i < 5; i++) c.dispatchEvent(new WheelEvent('wheel', { clientX: ${wp.x}, clientY: ${wp.y}, deltaY: 60, bubbles: true, cancelable: true })); })()`);
  await sleep(300);
  const wheels = await inputs("mouse", "wheel");
  check(wheels.length >= 1 && wheels.length < 6 && wheels.reduce((a, e) => a + e.dy, 0) === 360, "wheel turns scroll the page, folded into fewer messages", wheels.map((e) => e.dy));
  // Keys: into the field.
  await resetLog();
  await mclick(p, await pagePoint(p, W, 100, 160), { wait: 500 });
  check(await p.eval(`document.activeElement?.classList.contains('wv-keys')`), "after a click the keyboard goes to the page");
  await p.key("H", "KeyH", 8);
  await p.key("i", "KeyI");
  await p.key("Backspace", "Backspace", 0, 8);
  await p.key("a", "KeyA", 2);
  await p.key("ArrowLeft", "ArrowLeft", 8);
  await sleep(300);
  const keys = await inputs("key", "down");
  check(keys.length === 5 && keys[0].key === "H" && keys[0].text === "H" && keys[0].modifiers === 8 && keys[0].code === "KeyH", "typed keys arrive with their text, code and Shift", keys[0]);
  check(keys[2].key === "Backspace" && !keys[2].text, "special keys arrive as keys", keys[2]);
  check(keys[3].key === "a" && keys[3].modifiers === 2 && !keys[3].text, "Ctrl+A arrives as a shortcut (no text)", keys[3]);
  check(keys[4].key === "ArrowLeft" && keys[4].modifiers === 8, "Shift+← too", keys[4]);
  check((await inputs("key", "up")).length === 5, "…each released");
  // Alt+W stays the app's (close tile) — not sent.
  await resetLog();
  await p.send("Input.insertText", { text: "héllo 👋" });
  await sleep(300);
  check((await inputs("text")).map((e) => e.text).join("") === "héllo 👋", "text from an input method arrives as text", await inputs("text"));
  await resetLog();
  await p.eval(`(() => { const t = document.querySelector('[data-pane-id="${W}"] .wv-keys'); const dt = new DataTransfer(); dt.setData('text/plain', 'pasted words'); t.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true })); })()`);
  await sleep(200);
  check((await inputs("text")).map((e) => e.text).join("") === "pasted words", "paste sends this device's clipboard as text");
  await p.key("Enter", "Enter", 0, 13);
  await sleep(600);
  tab = await tabOf(B);
  check(/\?q=/.test(tab.url), "Enter submits in the page", tab.url);

  // ── toolbar ───────────────────────────────────────────────────────────
  console.log("— toolbar");
  await p.click(`[data-pane-id="${W}"] [aria-label="Back"]`);
  await sleep(500);
  tab = await tabOf(B);
  check(tab.url === "https://example.com/next" && tab.can_go_forward, "Back goes back", tab.url);
  await p.click(`[data-pane-id="${W}"] [aria-label="Forward"]`);
  await sleep(500);
  check(/\?q=/.test((await tabOf(B)).url), "Forward goes forward");
  await resetLog();
  await p.click(`[data-pane-id="${W}"] [aria-label="Reload"]`);
  await sleep(60);
  const stopShown = await p.eval(`!!document.querySelector('[data-pane-id="${W}"] [aria-label="Stop"]')`);
  await sleep(500);
  check((await log((l) => l.nav)).some((l) => l.nav.type === "reload"), "Reload reloads");
  check(stopShown && (await p.eval(`!!document.querySelector('[data-pane-id="${W}"] [aria-label="Reload"]')`)), "while loading it's a Stop button, then Reload again");
  // The address.
  await p.click(`[data-pane-id="${W}"] .wv-input`);
  await sleep(150);
  check((await p.eval(`(() => { const i = document.activeElement; return i.selectionStart === 0 && i.selectionEnd === i.value.length && /^https:/.test(i.value); })()`)), "the address is selected for typing over (full URL while editing)");
  await p.type("example.org/docs");
  await p.key("Enter", "Enter");
  await sleep(600);
  tab = await tabOf(B);
  check(tab.url === "https://example.org/docs", "typing an address goes there", tab.url);
  await p.click(`[data-pane-id="${W}"] .wv-input`);
  await sleep(100);
  await p.type("solid signals tutorial");
  await p.key("Enter", "Enter");
  await sleep(600);
  check((await tabOf(B)).url.startsWith("https://duckduckgo.com/?q=solid%20signals%20tutorial"), "words are a search", (await tabOf(B)).url);
  await p.click(`[data-pane-id="${W}"] .wv-input`);
  await sleep(100);
  await p.type("ftp://example.com/x");
  await p.key("Enter", "Enter");
  await sleep(300);
  check(/Only web pages/.test(await p.eval(`document.querySelector('[data-pane-id="${W}"] .wv-error')?.textContent ?? ''`)), "a non-web address is refused in the tile");
  await p.key("Escape");
  await sleep(150);
  check(!(await p.eval(`!!document.querySelector('[data-pane-id="${W}"] .wv-error')`)) && (await p.eval(`document.querySelector('[data-pane-id="${W}"] .wv-input').value`)).startsWith("duckduckgo.com"), "Esc puts the address back");
  // Zoom.
  await p.click(`[data-pane-id="${W}"] [aria-label="Web view options"]`);
  await sleep(250);
  const wmenu = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-label')].map(e => e.textContent)`);
  check(["Zoom in", "Zoom out", "Actual size", "Ask about this", "Open in your browser"].every((x) => wmenu.includes(x)), "zoom lives in a small menu (with ask, open, copy)", wmenu);
  await p.shot(`${SHOTS}/webview-menu-desktop.png`, await p.eval(`(() => { const r = document.querySelector('.menu-pop').getBoundingClientRect(); return { x: r.x - 300, y: r.y - 60, width: r.width + 320, height: r.height + 70 }; })()`));
  await p.clickText(".menu-item", "Zoom in");
  await sleep(400);
  check((await tabOf(B)).zoom === 1.1 && (await p.eval(`document.querySelector('[data-pane-id="${W}"] .wv-zoom')?.textContent`)) === "110%", "Zoom in: 110%, shown in the bar", (await tabOf(B)).zoom);
  await p.click(`[data-pane-id="${W}"] .wv-zoom`);
  await sleep(400);
  check((await tabOf(B)).zoom === 1 && !(await p.eval(`!!document.querySelector('[data-pane-id="${W}"] .wv-zoom')`)), "a click on it: actual size");
  const zp = await pagePoint(p, W, 300, 500);
  await p.send("Input.dispatchMouseEvent", { type: "mouseWheel", x: zp.x, y: zp.y, deltaX: 0, deltaY: -120, modifiers: 2 });
  await sleep(400);
  check((await tabOf(B)).zoom > 1, "Ctrl+wheel (and a touchpad pinch) zooms", (await tabOf(B)).zoom);
  await ws(p, `Promise.resolve()`);
  await p.eval(`__mod('lib/browserView.ts').then(m => m.nav('${B}', { type: 'zoom', factor: 1 }))`);
  // A dialog.
  await resetLog();
  await mclick(p, await pagePoint(p, W, 60, 220), { wait: 500 });
  check(/Leave this page/.test(await p.eval(`document.querySelector('[data-pane-id="${W}"] .wv-dialog-msg')?.textContent ?? ''`)), "the page's dialog shows in the tile");
  await p.shot(`${SHOTS}/webview-dialog-desktop.png`);
  await p.clickText(`[data-pane-id="${W}"] .wv-dialog button`, "OK");
  await sleep(500);
  const ans = await log((l) => l.dialog);
  check(ans.length === 1 && ans[0].dialog.accept === true && !(await p.eval(`!!document.querySelector('.wv-dialog')`)), "OK answers it", ans);
  await p.shot(`${SHOTS}/webview-desktop.png`);

  // ── on screen only ────────────────────────────────────────────────────
  console.log("— attached only while on screen");
  await ws(p, `m.switchWorkspace(3)`);
  await sleep(1400);
  b = await browser();
  check(!b.attached[B], "another mosaic: the tile lets go of the tab", b.attached);
  const f0 = b.frames;
  await ws(p, `m.switchWorkspace(0)`);
  await sleep(900);
  b = await browser();
  check(b.attached[B] === 1 && b.frames > f0, "back: attached again, frames flowing", { attached: b.attached, frames: b.frames, f0 });
  // Resizing the tile resizes the tab.
  await ws(p, `m.focusPane('${W}'); m.toggleMaximize('${W}')`);
  await sleep(700);
  const t2 = (await browser()).tabs.find((t) => t.board_id === B);
  const cw = await p.eval(`document.querySelector('[data-pane-id="${W}"] .wv-canvas').clientWidth`);
  await mclick(p, await pagePoint(p, W, cw - 20, 500), { wait: 300 });
  const far = (await inputs("mouse", "down")).pop();
  check(far && Math.abs(far.x - (cw - 20)) <= 1, "after a resize, points still land where they're clicked", { far, cw, t2 });
  await ws(p, `m.toggleMaximize('${W}')`);
  await sleep(300);

  // ── Ask about this: the picture comes from the tile ───────────────────
  console.log("— ask about this");
  await p.click(`[data-pane-id="${W}"] [aria-label="Web view options"]`);
  await sleep(250);
  await p.clickText(".menu-item", "Ask about this");
  await sleep(500);
  const draftText = await ws(p, `return m.getDraft('${A}')`);
  const att = await ws(p, `return m.stagedAttachments('${A}').length`).catch(() => null);
  const shownAtt = await p.eval(`document.querySelectorAll('[data-pane-id="${A}"] .composer .attachment').length`);
  check(draftText.includes(B), "“Ask about this” puts a reference in the chat's box", draftText);
  check((att ?? 0) > 0 || shownAtt > 0, "…with a picture of the page (taken from the tile itself)", { att, shownAtt });
  await ws(p, `m.setDraft('${A}', '')`);

  // ── the agent browses: in the background, compact steps ───────────────
  console.log("— the agent's browser steps");
  await ws(p, `m.openSession('${B_SID}', { newPane: true })`);
  await sleep(600);
  const C = await ws(p, `return m.workspace.order().find(id => m.workspace.panes[id].content.sessionId === '${B_SID}')`);
  await ws(p, `m.focusComposer('${C}')`);
  await p.type("go to example.com and have a look");
  await p.key("Enter", "Enter");
  await waitFor(() => p.eval(`document.querySelectorAll('[data-pane-id="${C}"] .tool-browser .sq-ok').length === 3`), 15000);
  // Then the turn ends and its steps fold.
  await waitFor(async () => (await probe.rpc("get_session", { session_id: B_SID })).result?.info?.status === "idle", 15000);
  await sleep(500);
  const agentBoards = (await probe.rpc("list_boards", { session_id: B_SID })).result.filter((x) => x.content.type === "url");
  check(agentBoards.length === 0, "the agent browses in the background: no web view", agentBoards.length);
  const fold = `[data-pane-id="${C}"] .steps-row`;
  check((await p.eval(`document.querySelectorAll('${fold} .tool-watch').length`)) === 1, "its folded steps offer Watch");
  await p.eval(`document.querySelector('${fold} .steps-head').click()`);
  await sleep(300);
  const steps = await p.eval(`[...document.querySelectorAll('[data-pane-id="${C}"] .tool-browser .tool-sum')].map(e => e.textContent)`);
  check(JSON.stringify(steps) === JSON.stringify(["open example.com", "read the page", "click “Next page”"]), "its steps read as short phrases", steps);
  check((await p.eval(`document.querySelectorAll('[data-pane-id="${C}"] .tool-browser .tool-watch').length`)) === 1, "opened, the latest step offers Watch");
  const outline = await p.eval(`(() => { const rows = [...document.querySelectorAll('[data-pane-id="${C}"] .tool-browser')]; const r = rows[1]; r.querySelector('.tool-head').click(); return new Promise(res => setTimeout(() => res(r.textContent), 250)); })()`);
  await p.eval(`document.querySelector('${fold} .steps-head').click()`);
  await sleep(300);
  // Boards are broadcast before Watch returns; the device places its tile
  // only after that answer. list_boards is not a Watch-completed barrier.
  await p.eval(`__mod('lib/backend.ts').then(({backend}) => {
    window.watchCall = backend.call.bind(backend);
    window.watchPending = false;
    backend.call = req => {
      if (req.method !== 'browser_watch') return watchCall(req);
      watchPending = true;
      return watchCall(req).then(r => new Promise(resolve => { window.releaseWatch = () => {watchPending=false;resolve(r);}; }));
    };
  })`);
  await p.click(`${fold} .tool-watch`);
  const AB = await waitFor(async () => (await probe.rpc("list_boards", { session_id: B_SID })).result.find((x) => x.content.type === "url")?.id, 6000);
  await p.waitFor(`watchPending && typeof releaseWatch === 'function'`, 4000);
  check(!!AB && (await boardPanes(p, AB)).length === 0, "a watched board is registered before the RPC returns and its device places a tile");
  await p.eval(`releaseWatch()`);
  const ready = await waitFor(() => p.eval(`Promise.all([__mod('lib/boards.ts'),__mod('lib/store.ts')]).then(([b,s]) => !watchPending && Object.values(s.state.sessions['${B_SID}'].results).some(r => r.tool === 'browser' && b.watchedBoard(r.details?.tab) === '${AB}'))`));
  check(ready, "Watch has completed and its board identity is registered before closing");
  check(!!AB && (await boardPanes(p, AB)).length === 1, "Watch opens it in one tile", AB && (await boardPanes(p, AB)));
  await p.eval(`__mod('lib/backend.ts').then(({backend}) => backend.call=watchCall)`);
  check(!(await p.eval(`!!document.querySelector('[data-pane-id="${C}"] .tool-show')`)), "no “Show tile” or Watch while its tile is on screen");
  await ws(p, `m.closePane(m.paneShowingBoard('${AB}'))`);
  // Waited for (not a fixed pause): under load the closing tile can take a moment to go.
  const closedAt = Date.now();
  await waitFor(() => p.eval(`document.querySelectorAll('${fold} .tool-show').length === 1`), 3000);
  const showMs = Date.now() - closedAt;
  const showBtns = await p.eval(`document.querySelectorAll('${fold} .tool-show').length`);
  if (showBtns !== 1) console.log('SHOW_TILE_DEBUG', await p.eval(`Promise.all([__mod('lib/workspace.ts'),__mod('lib/boards.ts'),__mod('lib/store.ts')]).then(([w,b,s])=>({pane:w.paneShowingBoard('${AB}'),order:w.workspace.order(),boards:Object.values(s.state.boards).map(b=>({id:b.id,session:b.session_id,url:b.content.url})),watched:Object.values(s.state.sessions['${B_SID}'].results).filter(r=>r.tool==='browser').map(r=>({details:r.details,watch:b.watchedBoard(r.details?.tab)})),fold:document.querySelector('${fold}')?.textContent,steps:[...document.querySelectorAll('[data-pane-id="${C}"] .steps')].map(e=>e.textContent)}))`));
  check(showBtns === 1 && showMs < 1500, "closed: the steps offer “Show tile”", { showBtns, showMs });
  await p.shot(`${SHOTS}/browser-steps-desktop.png`, await p.eval(`(() => { const r = document.querySelector('[data-pane-id="${C}"]').getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; })()`));
  await p.click(`${fold} .tool-show`);
  await sleep(600);
  check((await boardPanes(p, AB)).length === 1, "“Show tile” brings back one tile");
  check(/textbox "Search"/.test(outline), "a step opens to what the agent saw", outline.slice(0, 120));

  // ── whiteboards: the same tile chrome, no address ─────────────────────
  console.log("— whiteboard chrome");
  const hb = await mock("/__mock/board", { session_id: A_SID, kind: "chart", ask: false });
  await sleep(1200);
  const hp = await ws(p, `return m.paneShowingBoard('${hb.id}')`);
  const heads = await p.eval(`(() => { const h = document.querySelector('[data-pane-id="${hp}"] .pane-head'); const w = document.querySelector('[data-pane-id="${W}"] .pane-head'); return { html: h?.getBoundingClientRect().height, web: w?.getBoundingClientRect().height, addr: !!h?.querySelector('.wv-addr'), more: !!h?.querySelector('[aria-label="Whiteboard options"]') }; })()`);
  check(heads.html === heads.web && !heads.addr && heads.more, "a whiteboard has the same bar, without the address", heads);
  check(await p.eval(`!!document.querySelector('[data-pane-id="${hp}"] iframe[sandbox="allow-scripts"]')`), "…its page still runs in the sandboxed frame here");
  await p.shot(`${SHOTS}/whiteboard-and-webview-desktop.png`);

  console.log("desktop errors:", p.errors);
  check(!p.errors.length, "no page errors (desktop)");
  await p.close();

  // ── a phone ───────────────────────────────────────────────────────────
  console.log("— phone");
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const ph = await device();
  await ph.viewport(375, 812, true, 2);
  await signIn(ph, "Phone");
  await ws(ph, `m.openSession('${A_SID}', { pane: m.workspace.focused() })`);
  await sleep(800);
  const visBox = (sel) => ph.eval(`(() => { const e = [...document.querySelectorAll(${JSON.stringify(sel)})].find(e => { const r = e.getBoundingClientRect(); return r.width && r.left >= 0 && r.right <= innerWidth; }); if (!e) return null; const r = e.getBoundingClientRect(); return { cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);
  let bx = await visBox(".head-menu");
  await ph.tap(bx.cx, bx.cy);
  await sleep(400);
  check(!(await visBox(".drawer .sb-new")), "the side menu has no New of its own");
  await ph.eval(`history.back()`);
  await sleep(400);
  bx = await visBox(".pane .head-new");
  await ph.tap(bx.cx, bx.cy);
  await ph.waitFor(`document.querySelector('.menu-pop .menu-item')`, 3000).catch(() => {});
  await sleep(200);
  const pmenu = await menuLabels(ph);
  check(pmenu.includes("Web view") && pmenu.includes("Note") && !pmenu.includes("Image") && !pmenu.includes("Web app"), "the phone's new menu: web view and note (no image, no web app)", pmenu);
  await ph.shot(`${SHOTS}/new-menu-phone.png`);
  bx = await ph.eval(`(() => { const e = [...document.querySelectorAll('.menu-item')].find(e => e.textContent.includes('Web view')); const r = e.getBoundingClientRect(); return { cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);
  await ph.tap(bx.cx, bx.cy);
  await sleep(900);
  const PW = await ws(ph, `return m.workspace.focused()`);
  check((await ws(ph, `return m.workspace.panes['${PW}'].content.type`)) === "draft", "a web view tile, on screen");
  check(!(await ph.eval(`document.activeElement?.classList.contains('wv-input')`)), "…the keyboard doesn't jump up by itself");
  bx = await visBox(".wv-input");
  await ph.tap(bx.cx, bx.cy);
  await sleep(200);
  await ph.type("example.com");
  await ph.key("Enter", "Enter");
  await sleep(1800);
  const pb = (await (await fetch(`${MOCK}/__mock/boards`)).json()).boards.filter((x) => x.content.type === "url");
  check(pb.length === 1, "one board for example.com", pb.length);
  const PB = pb[0]?.id;
  check(JSON.stringify(await boardPanes(ph, PB)) === JSON.stringify([PW]) && (await ws(ph, `return m.workspace.focused()`)) === PW, "one tile: the one typed into, still in view", { panes: await boardPanes(ph, PB) });
  await waitFor(() => ph.eval(`!!document.querySelector('[data-pane-id="${PW}"] .wv-canvas.shown')`));
  const pt = (await browser()).tabs.find((t) => t.board_id === PB);
  check(pt && (await browser()).attached[PB] === 1, "attached as a phone", pt);
  await ph.shot(`${SHOTS}/webview-phone.png`);
  // Tap the field: the phone's keyboard comes up for it.
  await mock("/__mock/browser/reset");
  let pp = await pagePoint(ph, PW, 120, 160);
  await ph.tap(pp.x, pp.y);
  await sleep(600);
  const kf = await ph.eval(`(() => { const k = document.querySelector('[data-pane-id="${PW}"] .wv-keys'); return { focused: document.activeElement === k, mode: k.inputMode }; })()`);
  check(kf.focused && kf.mode === "text", "tapping a text field in the page focuses the keyboard catcher (the phone's keyboard)", kf);
  const tt = await inputs("touch");
  check(tt.length === 2 && tt[0].kind === "start" && tt[1].kind === "end" && Math.abs(tt[0].points[0].x - 120) <= 1, "the tap arrives as a touch at the same spot", tt);
  // What the page's field holds: the keyboard's text and backspaces, in order.
  const fieldText = async () => {
    const seg = new Intl.Segmenter();
    let s = "";
    for (const { event: e } of await log((l) => l.event && (l.event.type === "text" || (l.event.type === "key" && l.event.kind === "down")))) {
      if (e.type === "text") s += e.text;
      else if (e.key === "Backspace") s = [...seg.segment(s)].slice(0, -1).map((g) => g.segment).join("");
    }
    return s;
  };
  const keysOf = (p, pane) => p.eval(`(() => { const k = document.querySelector('[data-pane-id="${pane}"] .wv-keys'); return { focused: document.activeElement === k, value: k.value, type: k.type, cap: k.autocapitalize, enter: k.enterKeyHint }; })()`);
  const hintShown = (p, pane) => p.eval(`!!document.querySelector('[data-pane-id="${pane}"] .wv-kbd')`);
  await sleep(500);
  check(await hintShown(ph, PW), "a field has focus and no keyboard came up (headless): the Type button offers one");
  check((await keysOf(ph, PW)).enter === "go", "…the keyboard's action key is Go for a one-line field", await keysOf(ph, PW));
  await ph.send("Input.insertText", { text: "abc" });
  await sleep(200);
  check((await fieldText()) === "abc", "the phone's keyboard types into the page", await fieldText());
  check(!(await hintShown(ph, PW)), "…and the Type button goes once it does");
  // The keyboard's delete key (an edit in the box, not a key press).
  await ph.eval(`document.execCommand('delete')`);
  await sleep(200);
  check((await fieldText()) === "ab" && (await inputs("key", "down")).some((e) => e.key === "Backspace"), "…its delete key takes a character there", await fieldText());
  // A word composed a letter at a time, then committed (as Android keyboards do): it
  // shows as it is typed, and nothing doubles when the keyboard commits it.
  for (const w of ["w", "wo", "wro"]) await ph.send("Input.imeSetComposition", { text: w, selectionStart: w.length, selectionEnd: w.length });
  await sleep(200);
  check((await fieldText()) === "abwro", "a word being composed reaches the page as it is typed", await fieldText());
  await ph.send("Input.insertText", { text: "world " });
  await sleep(200);
  check((await fieldText()) === "abworld ", "…corrected and committed, it reads right (no doubled or lost letters)", await fieldText());
  check((await keysOf(ph, PW)).value.endsWith("world "), "…and the keyboard keeps its word in the box while typing on", await keysOf(ph, PW));
  // An emoji goes in and comes out whole.
  await ph.send("Input.insertText", { text: "👍🏽" });
  await sleep(150);
  await ph.eval(`document.execCommand('delete')`);
  await sleep(200);
  check((await fieldText()) === "abworld ", "an emoji is deleted as one character", await fieldText());
  // Deleting past what was typed in the box still deletes in the page.
  for (let k = 0; k < 12; k++) await ph.eval(`document.execCommand('delete')`);
  await sleep(300);
  check((await fieldText()) === "", "deleting goes on past what the box held", { text: await fieldText(), keys: await keysOf(ph, PW) });
  await ph.send("Input.insertText", { text: "hi" });
  await sleep(150);
  // Keyboard put away: tapping the field (still focused in the page, so it says nothing new)
  // brings it back.
  await ph.eval(`document.activeElement.blur()`);
  check(!(await keysOf(ph, PW)).focused, "(the keyboard was put away)");
  pp = await pagePoint(ph, PW, 140, 160);
  await ph.tap(pp.x, pp.y);
  await sleep(400);
  check((await keysOf(ph, PW)).focused, "tapping the field again brings the keyboard back", await keysOf(ph, PW));
  check((await fieldText()) === "hi", "…and what was typed stays", await fieldText());
  await ph.shot(`${SHOTS}/webview-keyboard-phone.png`);
  pp = await pagePoint(ph, PW, 120, 420);
  await ph.tap(pp.x, pp.y);
  await sleep(500);
  check(!(await ph.eval(`document.activeElement?.classList.contains('wv-keys')`)), "tapping elsewhere lets the keyboard go");
  check(!(await hintShown(ph, PW)), "…and the Type button with it");
  // The page focusing a field by itself (no tap here) doesn't take the keyboard: not
  // when nothing has focus, and never away from a field the user is typing in.
  await sleep(1600);
  await mock(`/__mock/browser/focus?board=${PB}`);
  await sleep(500);
  check(!(await keysOf(ph, PW)).focused, "a page focusing its field by itself doesn't bring the keyboard up", await keysOf(ph, PW));
  await ph.eval(`(() => { const t = document.createElement('textarea'); t.id = 'e2e-elsewhere'; t.style.cssText = 'position:fixed;left:0;top:0;width:40px;height:20px'; document.body.append(t); t.focus(); })()`);
  await mock(`/__mock/browser/focus?board=${PB}`);
  await sleep(500);
  check(await ph.eval(`document.activeElement?.id === 'e2e-elsewhere'`), "…nor away from another field being typed in", await ph.eval(`document.activeElement?.className`));
  await ph.eval(`document.getElementById('e2e-elsewhere').remove()`);
  // A tap on the page's field still brings it up (right after the tap, the page's focus is followed).
  pp = await pagePoint(ph, PW, 120, 160);
  await ph.tap(pp.x, pp.y);
  await sleep(500);
  check((await keysOf(ph, PW)).focused, "…while a tap on its field still does", await keysOf(ph, PW));
  pp = await pagePoint(ph, PW, 120, 420);
  await ph.tap(pp.x, pp.y);
  await sleep(500);
  // A password field: the phone's keyboard gets a password box (no suggestions, nothing learnt).
  const backTo = (await tabOf(PB)).url;
  const go = (u) => ph.eval(`__mod('lib/browserView.ts').then(b => b.nav('${PB}', { type: 'go', url: '${u}' }))`);
  await go("https://example.com/password");
  await sleep(600);
  pp = await pagePoint(ph, PW, 120, 160);
  await ph.tap(pp.x, pp.y);
  await sleep(500);
  const pk = await keysOf(ph, PW);
  check(pk.focused && pk.type === "password" && (pk.cap === "off" || pk.cap === "none"), "a password field gets a password box for the keyboard", pk);
  await ph.send("Input.insertText", { text: "s3cret" });
  await sleep(200);
  check((await fieldText()).endsWith("s3cret"), "…and types into the page", await fieldText());
  await go(backTo);
  await sleep(600);
  check((await keysOf(ph, PW)).type === "text", "another page: a plain box again", await keysOf(ph, PW));
  // One finger scrolls the page (and doesn't swipe the tiles); two fingers pinch-zoom.
  await mock("/__mock/browser/reset");
  const idx0 = await ws(ph, `return m.workspace.order().indexOf(m.workspace.focused())`);
  pp = await pagePoint(ph, PW, 200, 500);
  await touch(ph, "touchStart", [[pp.x, pp.y]]);
  for (let k = 1; k <= 8; k++) {
    await touch(ph, "touchMove", [[pp.x - k * 25, pp.y - k * 20]]);
    await sleep(16);
  }
  await touch(ph, "touchEnd", []);
  await sleep(500);
  check((await inputs("touch", "move")).length >= 3, "a drag on the page is the page's (touch moves)");
  check((await ws(ph, `return m.workspace.order().indexOf(m.workspace.focused())`)) === idx0 && (await ws(ph, `return m.workspace.focused()`)) === PW, "…the tiles don't swipe");
  check(await ph.eval(`!!document.querySelector('[data-pane-id="${PW}"] .board-grip')`), "the tiles swipe from the bar under the page instead");
  await touch(ph, "touchStart", [[pp.x - 40, pp.y], [pp.x + 40, pp.y]]);
  for (let k = 1; k <= 8; k++) {
    await touch(ph, "touchMove", [[pp.x - 40 - k * 8, pp.y], [pp.x + 40 + k * 8, pp.y]]);
    await sleep(16);
  }
  await touch(ph, "touchEnd", []);
  await sleep(500);
  const z = (await browser()).tabs.find((t) => t.board_id === PB).zoom;
  check(z > 1.5, "pinching out zooms the page", z);
  await ph.eval(`__mod('lib/browserView.ts').then(m => m.nav('${PB}', { type: 'zoom', factor: 1 }))`);
  // The agent on the phone: still one tile.
  await ws(ph, `m.openSession('${B_SID}', { newPane: true })`);
  await sleep(600);
  const PC = await ws(ph, `return m.workspace.focused()`);
  await ws(ph, `m.focusComposer('${PC}')`);
  await ph.type("go to example.com please");
  bx = await visBox(`[data-pane-id="${PC}"] .send-btn[aria-label="Send"]`);
  await ph.tap(bx.cx, bx.cy);
  const urlBoards = async () => (await (await fetch(`${MOCK}/__mock/boards`)).json()).boards.filter((x) => x.content.type === "url" && x.session_id === B_SID).length;
  const ab0 = await urlBoards();
  await waitFor(() => ph.eval(`document.querySelectorAll('[data-pane-id="${PC}"] .tool-browser .sq-ok').length >= 3`), 15000);
  await sleep(800);
  check((await urlBoards()) === ab0 && (await ws(ph, `return m.workspace.focused()`)) === PC, "on the phone too, the agent browses in the background (no tile)", { ab0, now: await urlBoards() });
  console.log("phone errors:", ph.errors);
  check(!ph.errors.length, "no page errors (phone)");
  await ph.close();

  // ── the Fold ──────────────────────────────────────────────────────────
  console.log("— fold");
  const fd = await device();
  await fd.viewport(673, 841, true, 2.625);
  await signIn(fd, "Fold");
  await sleep(1200);
  await ws(fd, `m.switchWorkspace(0); const w = m.workspace.order().find(id => m.workspace.panes[id].content.type === 'whiteboard'); m.focusPane(w)`);
  await sleep(1500);
  await fd.shot(`${SHOTS}/webview-fold-673x841.png`);
  await fd.viewport(841, 701, true, 2.625);
  await sleep(900);
  await fd.shot(`${SHOTS}/webview-fold-841x701.png`);
  check(await fd.eval(`document.querySelectorAll('.wv-canvas.shown').length >= 1`), "the Fold shows its web views too");
  console.log("fold errors:", fd.errors);
  check(!fd.errors.length, "no page errors (fold)");
  await fd.close();
  probe.close();
  finish();
});
