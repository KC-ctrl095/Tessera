// Whiteboards end to end: every kind, asks (signature drawn with a pointer), close/reopen, delete,
// pausing off screen, phones and unfolded foldables.
import { MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const SID = "ses_wb7c1e5a9f20";
const boards = async () => (await (await fetch(MOCK + "/__mock/boards")).json());
const paneOf = (p, title) => p.eval(`(() => { const t = [...document.querySelectorAll('.pane')].find(e => e.querySelector('.board-head .title-static')?.textContent === ${JSON.stringify(title)}); return t ? t.dataset.paneId : null; })()`);

async function drawSignature(p, sel) {
  const b = await p.box(sel);
  const pts = [];
  for (let i = 0; i <= 60; i++) {
    const t = i / 60;
    pts.push([b.x + b.w * (0.18 + 0.6 * t), b.y + b.h * (0.55 - 0.22 * Math.sin(t * Math.PI * 3) * (1 - t * 0.4))]);
  }
  await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: pts[0][0], y: pts[0][1] });
  await p.send("Input.dispatchMouseEvent", { type: "mousePressed", x: pts[0][0], y: pts[0][1], button: "left", buttons: 1, clickCount: 1 });
  for (const [x, y] of pts.slice(1)) {
    await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y, button: "left", buttons: 1 });
    await sleep(8);
  }
  const last = pts[pts.length - 1];
  await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: last[0], y: last[1], button: "left", clickCount: 1 });
  // a second stroke: a flourish
  const y2 = b.y + b.h * 0.7;
  await p.send("Input.dispatchMouseEvent", { type: "mousePressed", x: b.x + b.w * 0.3, y: y2, button: "left", buttons: 1, clickCount: 1 });
  for (let i = 1; i <= 20; i++) {
    await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: b.x + b.w * (0.3 + i * 0.02), y: y2 - Math.sin(i / 3) * 4, button: "left", buttons: 1 });
    await sleep(8);
  }
  await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: b.x + b.w * 0.7, y: y2, button: "left", clickCount: 1 });
}

run(async () => {
  // ── desktop: every kind, both themes ──────────────────────────────────
  for (const theme of ["dark", "light"]) {
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, `WB ${theme}`);
    await p.theme(theme);
    await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${SID}', { pane: m.workspace.focused() }))`);
    await sleep(900);
    for (const kind of ["topology", "chart", "markdown", "image", "url", "local"]) {
      const b = await mock("/__mock/board", { session_id: SID, kind });
      await sleep(kind === "topology" || kind === "chart" ? 3200 : kind === "url" ? 2500 : 1500);
      const pid = kind === "url" || kind === "local" ? await p.eval(`__mod('lib/workspace.ts').then(m => m.paneShowingBoard('${b.id}'))`) : await paneOf(p, b.title);
      if (theme === "dark") check(!!pid, `${kind}: its tile was put next to the chat`);
      if (theme === "dark" && (kind === "url" || kind === "local")) {
        await p.waitFor(`document.querySelector('[data-pane-id="${pid}"] .wv-canvas.shown')`, 6000).catch(() => {});
        check(await p.eval(`!!document.querySelector('[data-pane-id="${pid}"] .wv-canvas.shown') && !document.querySelector('[data-pane-id="${pid}"] iframe')`), `${kind}: shown as a web view (the server's browser), not a frame`);
      }
      if (pid) await p.eval(`__mod('lib/workspace.ts').then(m => { m.focusPane('${pid}'); m.toggleMaximize('${pid}'); })`);
      await sleep(kind === "image" ? 600 : 400);
      await p.shot(`${SHOTS}/wb-${kind}-desktop-${theme}.png`);
      if (kind === "image" && theme === "dark") {
        const v = await p.box(`[data-pane-id="${pid}"] .img-view`);
        await p.mouse(v.x + v.w * 0.55, v.y + v.h * 0.42);
        await sleep(400);
        const zoomed = await p.eval(`!!document.querySelector('[data-pane-id="${pid}"] .img-view.zoomed')`);
        check(zoomed, "image: a click zooms in");
        await p.shot(`${SHOTS}/wb-image-zoomed-desktop-${theme}.png`);
        await p.mouse(v.x + v.w * 0.5, v.y + v.h * 0.5);
        await sleep(350);
      }
      // keep three boards up for the mosaic shot; close the rest with their close button
      if (pid && !["topology", "chart", "image"].includes(kind)) {
        await p.click(`[data-pane-id="${pid}"] .head-close`);
        await sleep(400);
        if (theme === "dark") check(!(await p.eval(`!!document.querySelector('[data-pane-id="${pid}"]')`)) && (await boards()).boards.some((x) => x.id === b.id), `${kind}: the close button closes the tile, the board stays`);
      } else if (pid) await p.eval(`__mod('lib/workspace.ts').then(m => m.toggleMaximize('${pid}'))`);
      await sleep(200);
    }
    // the whole mosaic with several boards
    await sleep(300);
    await p.shot(`${SHOTS}/wb-mosaic-desktop-${theme}.png`);
    // signature ask: draw, sign, collapse
    const sb = await mock("/__mock/board", { session_id: SID, kind: "signature" });
    await sleep(2400);
    const spid = await paneOf(p, sb.title);
    await p.eval(`__mod('lib/workspace.ts').then(m => { m.focusPane('${spid}'); m.toggleMaximize('${spid}'); })`);
    await sleep(700);
    await p.shot(`${SHOTS}/wb-ask-signature-desktop-${theme}.png`);
    await drawSignature(p, `[data-pane-id="${spid}"] .sig-pad`);
    await sleep(200);
    await p.shot(`${SHOTS}/wb-ask-signed-drawn-desktop-${theme}.png`);
    await p.click(`[data-pane-id="${spid}"] .ask .btn-primary`);
    await sleep(900);
    const st = await boards();
    const sig = st.answers.find((a) => a.board_id === sb.id)?.answer;
    if (theme === "dark") {
      check(sig?.type === "signature" && sig.bytes > 200, "signature sent as a PNG", sig);
      check(sig?.color_type === 6, "the PNG has an alpha channel (transparent background)", sig);
      const pad = await p.box(`[data-pane-id="${spid}"] .ask-done, [data-pane-id="${spid}"] .board-body`);
      check(sig && sig.width < 1400 && sig.height < 400, `trimmed to the ink (${sig?.width}×${sig?.height})`, pad);
      check(await p.eval(`!!document.querySelector('[data-pane-id="${spid}"] .ask-done')`), "the ask collapses to “Signed”");
      check(!st.boards.find((b) => b.id === sb.id)?.ask, "the server cleared the ask");
    }
    await p.shot(`${SHOTS}/wb-ask-signed-desktop-${theme}.png`);
    await p.eval(`__mod('lib/workspace.ts').then(m => m.toggleMaximize('${spid}'))`);
    // choice + confirm + text asks (and one where several may be picked), answered in the chat mirror with the tile closed
    for (const kind of ["choice", "confirm", "text", "several"]) {
      const ab = await mock("/__mock/board", { session_id: SID, kind });
      await sleep(1500);
      const apid = await paneOf(p, ab.title);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.focusPane('${apid}'))`);
      await sleep(400);
      await p.shot(`${SHOTS}/wb-ask-${kind}-desktop-${theme}.png`);
      if (theme === "dark") {
        // close its tile: the chat still offers the question, with a badge
        await p.eval(`__mod('lib/workspace.ts').then(m => m.closePane('${apid}'))`);
        await sleep(700);
        const still = (await boards()).boards.find((b) => b.id === ab.id);
        check(!!still, `${kind}: closing the tile keeps the whiteboard`);
        const mirror = await p.eval(`!!document.querySelector('.chat-ask .ask')`);
        check(mirror, `${kind}: with the tile closed the chat offers the question`);
        const badge = await p.eval(`!!document.querySelector('.pane-head .needs-you') && !!document.querySelector('.srow.active .sq-approval, .srow .sq-approval')`);
        check(badge, `${kind}: “needs you” in the chat header and the sidebar`);
        await p.eval(`document.querySelector('.chat-ask .ask')?.scrollIntoView({block:'center'})`);
        await sleep(200);
        await p.shot(`${SHOTS}/wb-ask-${kind}-in-chat-desktop-${theme}.png`);
        if (kind === "choice") await p.click(".chat-ask .ask-choice:nth-child(1)");
        if (kind === "several") {
          const several = ".chat-ask .ask-choices[role=group]";
          check(await p.eval(`document.querySelector('${several}').closest('.ask').querySelector('.ask-actions .btn-primary').disabled`), "several: nothing ticked, nothing to send");
          await p.eval(`document.querySelectorAll('${several} .ask-choice')[0].click()`);
          await p.eval(`document.querySelectorAll('${several} .ask-choice')[2].click()`);
          await sleep(150);
          const ticked = await p.eval(`[...document.querySelectorAll('${several} .ask-choice')].map((b) => b.getAttribute('aria-checked'))`);
          check(JSON.stringify(ticked) === JSON.stringify(["true", "false", "true", "false"]), "several: options tick on and off", ticked);
          await p.shot(`${SHOTS}/wb-ask-several-ticked-desktop-${theme}.png`);
          await p.eval(`document.querySelector('${several}').closest('.ask').querySelector('.ask-actions .btn-primary').click()`);
        }
        if (kind === "confirm") await p.click(".chat-ask .ask-actions .btn-primary");
        if (kind === "text") {
          await p.click(".chat-ask textarea");
          await p.type("I like building calm tools.");
          await p.click(".chat-ask .ask-actions .btn-primary");
        }
        await sleep(900);
        const ans = (await boards()).answers.find((a) => a.board_id === ab.id)?.answer;
        check(ans && ans.type === (kind === "several" ? "choices" : kind), `${kind}: answered from the chat`, ans);
        if (kind === "several") check(JSON.stringify(ans?.indexes) === "[0,2]", "several: both picks go back", ans);
        check(await p.eval(`!!document.querySelector('.chat-ask .ask-done')`), `${kind}: the chat card collapses to one line`);
      } else {
        await p.eval(`__mod('lib/workspace.ts').then(m => m.closePane('${apid}'))`);
        await sleep(300);
      }
    }
    if (theme === "dark") {
      // the transcript card reopens a closed tile
      const cards = await p.eval(`document.querySelectorAll('.board-card').length`);
      check(cards >= 1, "the whiteboard tool call shows as a board card");
      const homeClosed = await p.eval(`__mod('lib/workspace.ts').then(m => { const id = m.paneShowingBoard('wb_seed7home'); if (id) m.closePane(id); return !m.paneShowingBoard('wb_seed7home'); })`);
      await sleep(300);
      await p.eval(`[...document.querySelectorAll('.board-card')].find(c => c.textContent.includes('Home network'))?.click()`);
      await sleep(1200);
      check(homeClosed && (await p.eval(`__mod('lib/workspace.ts').then(m => !!m.paneShowingBoard('wb_seed7home'))`)), "the board card reopens a closed tile");
      // pause off screen: another mosaic → no frame; back → frame again
      const before = await p.eval(`document.querySelectorAll('.board-iframe').length`);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.switchWorkspace(3))`);
      await sleep(1400);
      const hidden = await p.eval(`document.querySelectorAll('.board-iframe').length`);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.switchWorkspace(0))`);
      await sleep(1600);
      const back = await p.eval(`document.querySelectorAll('.board-iframe').length`);
      check(before > 0 && hidden === 0 && back === before, `frames unload off screen and come back (${before} → ${hidden} → ${back})`);
      // delete from the tile menu
      const hid = await p.eval(`__mod('lib/workspace.ts').then(m => m.paneShowingBoard('wb_seed7home'))`);
      await p.click(`[data-pane-id="${hid}"] [aria-label="Whiteboard options"]`);
      await sleep(300);
      await p.shot(`${SHOTS}/wb-menu-desktop-${theme}.png`);
      await p.clickText(".menu-item", "Delete whiteboard");
      await sleep(300);
      await p.clickText(".modal button", "Delete whiteboard");
      await sleep(900);
      const gone = !(await boards()).boards.some((b) => b.id === "wb_seed7home");
      check(gone && !(await p.eval(`__mod('lib/workspace.ts').then(m => !!m.paneShowingBoard('wb_seed7home'))`)), "Delete whiteboard removes it and its tile");
      check(await p.eval(`[...document.querySelectorAll('.board-card')].some(c => c.textContent.includes('Deleted'))`), "its card in the chat says it was deleted");
    }
    console.log(theme, "errors", p.errors);
    await p.close();
  }

  // ── phone 375 and unfolded foldable ───────────────────────────────────
  for (const [label, w, h] of [["phone", 375, 812], ["fold", 841, 701]]) {
    for (const theme of ["dark", "light"]) {
      await mock("/__mock/reset");
      await mock("/__mock/mosaics/reset");
      const p = await device();
      await p.viewport(w, h, true, 2);
      await signIn(p, `WB ${label}`);
      await p.theme(theme);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${SID}', { pane: m.workspace.focused() }))`);
      await sleep(900);
      const b = await mock("/__mock/board", { session_id: SID, kind: "topology" });
      await sleep(2200);
      if (theme === "dark" && label === "phone") check(await p.eval(`[...document.querySelectorAll('.toast')].some(t => /whiteboard/i.test(t.textContent))`), "phone: a toast says a whiteboard went up");
      await p.shot(`${SHOTS}/wb-toast-${label}-${theme}.png`);
      const pid = await paneOf(p, b.title);
      if (label === "phone") {
        await p.eval(`__mod('lib/workspace.ts').then(m => m.focusPane('${pid}'))`);
        await sleep(3000);
        await p.shot(`${SHOTS}/wb-html-${label}-${theme}.png`);
        const sb = await mock("/__mock/board", { session_id: SID, kind: "signature" });
        await sleep(1200);
        const spid = await paneOf(p, sb.title);
        await p.eval(`__mod('lib/workspace.ts').then(m => m.focusPane('${spid}'))`);
        await sleep(1500);
        await p.shot(`${SHOTS}/wb-ask-signature-${label}-${theme}.png`);
        if (theme === "dark") {
          // off-page frames pause: the topology tile is not the current page now
          await sleep(1200);
          const live = await p.eval(`[...document.querySelectorAll('.pane')].filter(e => e.querySelector('.board-iframe')).map(e => e.querySelector('.title-static')?.textContent)`);
          check(!live.includes("Home network"), "phone: the hidden page's frame is paused", live);
        }
      } else {
        await sleep(2500);
        const shown = await p.eval(`[...document.querySelectorAll('.pane')].filter(e => { const r = e.getBoundingClientRect(); return r.right > 4 && r.left < innerWidth - 4 && e.style.display !== 'none'; }).length`);
        if (theme === "dark") check(shown === 2, `fold: chat and board side by side (${shown} panes)`);
        await p.shot(`${SHOTS}/wb-html-${label}-${theme}.png`);
        const sb = await mock("/__mock/board", { session_id: SID, kind: "signature" });
        await sleep(1500);
        const spid = await paneOf(p, sb.title);
        await p.eval(`__mod('lib/workspace.ts').then(m => m.focusPane('${spid}'))`);
        await sleep(1500);
        await p.shot(`${SHOTS}/wb-ask-signature-${label}-${theme}.png`);
      }
      console.log(label, theme, "errors", p.errors);
      await p.close();
    }
  }
  finish();
});
