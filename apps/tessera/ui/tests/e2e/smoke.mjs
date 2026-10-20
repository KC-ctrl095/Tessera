// Things that must keep working: setup wizard, sidebar drag and drop, swipe (also over a board frame),
// notification settings, /goal and /steer, tiles from newer clients, voice input, avatars.
import { APP, MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

run(async () => {
  // ── setup wizard on a first run ──
  await mock("/__mock/reset?fresh=1");
  await mock("/__mock/mosaics/reset");
  {
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, "Smoke setup");
    // The wizard loads on demand after the server says setup isn't done: on a
    // busy machine that takes longer than a fixed pause (it showed up only in
    // the picture taken after the check).
    await p.waitFor(`document.querySelector('.setup-wizard')`, 8000).catch(() => {});
    const wizard = await p.eval(`!!document.querySelector('.setup-wizard')`);
    check(wizard, "the setup wizard opens on a first run");
    await p.shot(`${SHOTS}/smoke-setup-desktop-dark.png`);
    console.log("setup errors", p.errors);
    await p.close();
  }
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  // ── desktop: drag a sidebar chat into the right edge of a tile; slash commands; voice; settings ──
  {
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, "Smoke desktop");
    await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_2b7e91c04d5a', { pane: m.workspace.focused() }))`);
    await sleep(900);
    const row = await p.eval(`(() => { const e = [...document.querySelectorAll('.sidebar .srow')].find(r => r.textContent.includes('Summarize RFC')); const r = e.querySelector('.srow-main').getBoundingClientRect(); return [r.x + 40, r.y + r.height / 2]; })()`);
    const pane = await p.box(".pane");
    await p.down(row[0], row[1]);
    const tx = pane.x + pane.w * 0.9, ty = pane.y + pane.h * 0.5;
    for (let i = 1; i <= 16; i++) {
      await p.move(row[0] + ((tx - row[0]) * i) / 16, row[1] + ((ty - row[1]) * i) / 16);
      await sleep(16);
    }
    await sleep(200);
    await p.up(tx, ty);
    await sleep(800);
    const n = await p.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().length)`);
    check(n === 2, "a chat dragged from the sidebar opens beside the tile", n);
    // /steer autocomplete and /goal
    await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
    await p.type("/st");
    await sleep(200);
    const pop = await p.eval(`[...document.querySelectorAll('.pane.focused .slash-item')].map(e => e.textContent)`);
    check(pop.some((t) => t.includes("/steer") && /next step/.test(t)), "/steer shows in the command list with its help", pop);
    await p.shot(`${SHOTS}/smoke-slash-steer-desktop-dark.png`);
    await p.eval(`(() => { const t = document.querySelector('.pane.focused .composer textarea'); t.value = ''; t.dispatchEvent(new Event('input', { bubbles: true })); })()`);
    await p.type("/goal Ship the whiteboard release");
    await p.key("Enter", "Enter", 0, 13);
    await sleep(700);
    const sid = await p.eval(`__mod('lib/workspace.ts').then(m => m.paneSession(m.workspace.focused())?.info.id)`);
    const goal = await p.eval(`__mod('lib/store.ts').then(m => m.state.sessions['${sid}']?.info.task?.goal ?? null)`);
    check(goal === "Ship the whiteboard release", "/goal still sets the goal", goal);
    // voice
    await p.eval(`__mod('lib/store.ts').then(m => m.setState('server', 'voice', true))`);
    await sleep(200);
    const mic = await p.eval(`!!document.querySelector('.pane.focused .mic-btn')`);
    check(mic, "the mic button shows in the slim composer when voice is set up");
    await p.click(".pane.focused .mic-btn");
    await sleep(1500);
    await p.shot(`${SHOTS}/smoke-voice-desktop-dark.png`);
    const rec = await p.eval(`!!document.querySelector('.pane.focused .voice-status')`);
    check(rec, "recording shows the listening indicator in the composer");
    await p.key("Escape", "Escape", 0, 27);
    await sleep(300);
    // settings → notifications
    await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings('notifications'))`);
    await sleep(700);
    check(await p.eval(`!!document.querySelector('.modal')`), "notification settings open");
    await p.shot(`${SHOTS}/smoke-notifications-desktop-dark.png`);
    await p.key("Escape", "Escape", 0, 27);
    await sleep(300);
    // a tile from a newer client survives a round trip untouched
    const kept = await p.eval(`__mod('lib/workspace.ts').then(m => {
      const ms = m.exportMosaics();
      ms[0].tiles.push({ id: 'tile_future', terminal_id: 'term_1', note: 'from the future' });
      ms[0].tiles.push({ id: 'tile_extra', session_id: 'ses_91d0c3e7a8b2', pinned: true });
      m.importMosaics(ms);
      const out = m.exportMosaics()[0].tiles;
      return { future: out.find(t => t.id === 'tile_future'), extra: out.find(t => t.id === 'tile_extra') };
    })`);
    check(kept.future?.terminal_id === "term_1" && kept.future?.note === "from the future", "an unknown tile is kept as it is", kept.future);
    check(kept.extra?.pinned === true && kept.extra?.session_id === "ses_91d0c3e7a8b2", "unknown fields of a known tile are kept", kept.extra);
    await sleep(400);
    await p.shot(`${SHOTS}/smoke-unknown-tile-desktop-dark.png`);
    console.log("desktop errors", p.errors);
    await p.close();
  }
  // ── phone: swipe between pages, including across a board frame ──
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  {
    const p = await device();
    await p.viewport(375, 812, true, 2);
    await signIn(p, "Smoke phone");
    await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_wb7c1e5a9f20', { pane: m.workspace.focused() }))`);
    await sleep(900);
    await mock("/__mock/board", { session_id: "ses_wb7c1e5a9f20", kind: "chart" });
    await sleep(1500);
    const focusedIdx = () => p.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().indexOf(m.workspace.focused()))`);
    const swipe = async (x0, x1, y) => {
      await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: x0, y }] });
      for (let i = 1; i <= 10; i++) {
        await p.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x: x0 + ((x1 - x0) * i) / 10, y }] });
        await sleep(16);
      }
      await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
      await sleep(600);
    };
    const i0 = await focusedIdx();
    await swipe(300, 60, 500); // over the chat transcript
    const i1 = await focusedIdx();
    check(i1 === i0 + 1, `swiping left moves to the next page (${i0} → ${i1})`);
    await sleep(1800);
    await p.shot(`${SHOTS}/smoke-swipe-board-phone-dark.png`);
    // Back from the board page: touches in its frame never reach the app and
    // the screen edge is the system's back gesture, so it swipes on its grip bar.
    const gripY = await p.eval(`(() => { const r = document.querySelector('.board-grip')?.getBoundingClientRect(); return r ? r.y + r.height / 2 : null; })()`);
    check(gripY !== null, "a board page on a phone has a grip bar to swipe on");
    await swipe(60, 330, gripY ?? 790);
    const i2 = await focusedIdx();
    check(i2 === i0, `swiping right on a board page's grip bar goes back (${i1} → ${i2})`);
    console.log("phone errors", p.errors);
    await p.close();
  }
  finish();
});
