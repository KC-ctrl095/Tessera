// Polish from the Android emulator: boards land beside a chat started a moment ago, board tiles swipe
// from the header or a grab bar (no edge strips: the system back gesture owns the screen edge),
// and web apps show as web views of the server's machine.
import { MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const sends = async () => (await (await fetch(MOCK + "/__mock/sends")).json());
const layout = async () => {
  const c = await wsClient("probe");
  const L = (await c.rpc("get_mosaics")).result;
  c.close();
  return L;
};
const mosaicOf = (L, pred) => L.mosaics.findIndex((m) => m.tiles.some(pred));

run(async () => {
  // ── 1a. a chat started in a fresh device's own tile is shared before the message goes out ──
  for (const [label, w, h, mobile] of [["phone", 375, 812, true], ["desktop", 1440, 900, false]]) {
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(w, h, mobile, mobile ? 2 : 1);
    await signIn(p, `Fresh ${label}`);
    await sleep(800);
    await p.eval(`document.querySelector('.pane .composer textarea').focus()`);
    await p.type("Show me my home network on a whiteboard");
    if (mobile) await p.click(".pane .send-btn");
    else await p.key("Enter", "Enter", 0, 13);
    await sleep(600);
    const s = (await sends()).at(-1);
    check(s?.tiled === true, `${label}: the new chat's tile was on the server before its first message`, s);
    await sleep(4500);
    const L = await layout();
    const sid = s?.session_id;
    const chatM = mosaicOf(L, (t) => t.session_id === sid);
    const boardM = mosaicOf(L, (t) => !!t.board_id);
    const tiles = L.mosaics[chatM]?.tiles ?? [];
    const ci = tiles.findIndex((t) => t.session_id === sid);
    check(chatM >= 0 && boardM === chatM && !!tiles[ci + 1]?.board_id, `${label}: the agent's board sits right after its chat`, L.mosaics.map((m) => m.tiles));
    const card = await p.eval(`document.querySelector('.board-card')?.textContent ?? ''`);
    check(!/another mosaic/i.test(card), `${label}: its card doesn't say "In another mosaic" (${card.replace(/\s+/g, " ").trim()})`);
    console.log(label, "errors", p.errors);
    await p.close();
  }

  // ── 1b. a board that still lands in another mosaic moves beside its chat on screen ──
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  {
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, "Relocate");
    await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_2b7e91c04d5a', { pane: m.workspace.focused() }))`);
    await sleep(500);
    await p.eval(`__mod('lib/workspace.ts').then(m => { m.switchWorkspace(1); m.openSession('ses_wb7c1e5a9f20', { newPane: true }); })`);
    await sleep(1500);
    let L = await layout();
    check(L.mosaics.length === 2, "two mosaics shared, the second on screen", L.mosaics.map((m) => m.tiles));
    await mock("/__mock/board", { session_id: "ses_wb7c1e5a9f20", kind: "markdown", place: "first" });
    await sleep(2500);
    L = await layout();
    const chatM = mosaicOf(L, (t) => t.session_id === "ses_wb7c1e5a9f20");
    const boardM = mosaicOf(L, (t) => !!t.board_id);
    check(boardM === chatM, "a board the server put in another mosaic was moved beside its chat, and that synced", L.mosaics.map((m) => m.tiles));
    const onScreen = await p.eval(`[...document.querySelectorAll('.pane')].filter(e => e.style.display !== 'none' && e.querySelector('.board')).length`);
    check(onScreen === 1, "it's on screen next to the chat");
    await p.shot(`${SHOTS}/board-relocated-desktop-dark.png`);
    console.log("relocate errors", p.errors);
    await p.close();
  }

  // ── 2 + 3. phones: swipe from the header or the grab bar; the notes read well ──
  for (const [w, h] of [[375, 812], [673, 841]]) {
    for (const theme of ["dark", "light"]) {
      await mock("/__mock/reset");
      await mock("/__mock/mosaics/reset");
      const p = await device();
      await p.viewport(w, h, true, 2);
      await signIn(p, `Grip ${w}`);
      await p.theme(theme);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_wb7c1e5a9f20', { pane: m.workspace.focused() }))`);
      await sleep(800);
      const lb = await mock("/__mock/board", { session_id: "ses_wb7c1e5a9f20", kind: "local" });
      await sleep(1500);
      const pid = await p.eval(`__mod('lib/workspace.ts').then(m => m.paneShowingBoard('${lb.id}'))`);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.focusPane('${pid}'))`);
      await sleep(1500);
      // Two tiles side by side (an unfolded Fold is two-up since the dual breakpoint moved to 640): nothing to swipe to.
      const dual = (await p.eval(`__mod('lib/workspace.ts').then(m => m.workspace.mode())`)) !== "mobile";
      if (theme === "dark") {
        check(!(await p.eval(`!!document.querySelector('.board-swipe')`)), `${w}: no edge strips over frames`);
        check(await p.eval(`!!document.querySelector('[data-pane-id="${pid}"] .board-grip')`), `${w}: the board has a grab bar`);
      }
      // A web app on the server's machine is a web view of it (the server's browser reaches it directly).
      await p.waitFor(`document.querySelector('[data-pane-id="${pid}"] .wv-canvas.shown')`, 6000).catch(() => {});
      const addr = await p.eval(`document.querySelector('[data-pane-id="${pid}"] .wv-input')?.value ?? ''`);
      if (theme === "dark") check(/^http:\/\/127\.0\.0\.1:\d+/.test(addr) && (await p.eval(`!!document.querySelector('[data-pane-id="${pid}"] .wv-canvas.shown')`)), `${w}: a web app shows as a web view of 127.0.0.1`, addr);
      await p.shot(`${SHOTS}/board-local-${w}x${h}-${theme}.png`);
      // swipe back to the chat from the grab bar, then forward again from the header
      const idx = () => p.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().indexOf(m.workspace.focused()))`);
      const swipe = async (sel, dir) => {
        const b = await p.box(sel);
        const y = b.cy;
        const x0 = dir > 0 ? b.x + b.w * 0.25 : b.x + b.w * 0.75;
        const x1 = x0 + dir * b.w * 0.5;
        await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: x0, y }] });
        for (let i = 1; i <= 10; i++) {
          await p.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x: x0 + ((x1 - x0) * i) / 10, y }] });
          await sleep(16);
        }
        await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
        await sleep(600);
      };
      if (!dual) {
        const i0 = await idx();
        await swipe(`[data-pane-id="${pid}"] .board-grip`, +1);
        const i1 = await idx();
        if (theme === "dark") check(i1 === i0 - 1, `${w}: swiping the grab bar goes to the chat (${i0} → ${i1})`);
        // (the "put up a whiteboard" toast sits over the header on phones: out of the way first)
        await p.eval(`__mod('lib/ui.ts').then(m => m.toasts().forEach(t => m.dismissToast(t.id)))`);
        await sleep(300);
        await swipe(`.pane.focused .pane-head`, -1);
        await sleep(300);
        const i2 = await idx();
        if (theme === "dark") check(i2 === i0, `${w}: swiping a header goes to the board again (${i1} → ${i2})`);
        await sleep(1200);
      }
      await p.shot(`${SHOTS}/board-grip-${w}x${h}-${theme}.png`);
      // a web page
      const ub = await mock("/__mock/board", { session_id: "ses_wb7c1e5a9f20", kind: "url" });
      await sleep(1200);
      const upid = await p.eval(`__mod('lib/workspace.ts').then(m => m.paneShowingBoard('${ub.id}'))`);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.focusPane('${upid}'))`);
      await sleep(2500);
      await p.shot(`${SHOTS}/board-url-${w}x${h}-${theme}.png`);
      console.log(w, theme, "errors", p.errors);
      await p.close();
    }
  }
  finish();
});
