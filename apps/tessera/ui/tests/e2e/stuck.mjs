// The activity line above the composer never sticks: lost end events, a
// snapshot crossing a turn's last events, a reconnect mid-turn, coming back
// from the background. Before the first word it says nothing ("Getting ready"
// is gone) but keeps its place, which is what these checks watch.
import { MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const SID = "ses_2b7e91c04d5a";
const open = (p) => p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${SID}', { pane: m.workspace.focused() }))`);
/** The line (or, while it has nothing to say, its quiet slot) above the composer. */
const line = (p) =>
  p.eval(`(() => { const a = document.querySelector('.pane.focused .activity'); if (a) return { verb: a.querySelector('.activity-verb')?.textContent ?? '', time: a.querySelector('.activity-time')?.textContent ?? '' }; return document.querySelector('.pane.focused .activity-slot') ? { verb: '', time: '' } : null; })()`);
const busy = (p) => p.eval(`!!document.querySelector('.pane.focused .send-btn.stop')`);
/** Wait until the line is gone and the composer is idle again; ms taken, or -1. */
async function settles(p, within) {
  const t0 = Date.now();
  while (Date.now() - t0 < within) {
    if (!(await line(p)) && !(await busy(p))) return Date.now() - t0;
    await sleep(250);
  }
  return -1;
}
async function send(p, text) {
  await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
  await p.type(text);
  await p.key("Enter", "Enter", 0, 13);
}
/** Wait for the assistant's reply to be streaming on screen. */
const streaming = async (p) => {
  await p.eval(`__mod('lib/store.ts').then((m) => { window.__st = m.state; return 1; })`);
  await p.waitFor(`window.__st.sessions['${SID}']?.streaming?.content?.some((x) => x && x.type === 'text' && x.text.length > 20)`, 15000);
};

run(async () => {
  for (const [label, w, h, mobile] of [["desktop", 1440, 900, false], ["phone", 375, 812, true]]) {
    await mock("/__mock/reset");
    const ws = await wsClient("stuck test");
    const p = await device();
    await p.viewport(w, h, mobile, mobile ? 2 : 1);
    await signIn(p, `Stuck ${label}`);
    await p.theme("dark");
    await open(p);
    await sleep(1200);

    // 1. A turn that ends where no one hears it: the chat must not stay busy.
    await ws.rpc("mock_composer", { stall: SID });
    await p.waitFor(`document.querySelector('.pane.focused .activity-slot, .pane.focused .activity')`, 4000);
    const first = await line(p);
    check(first && first.verb === "", `${label}: lost end — the line holds its place and says nothing`, first);
    await sleep(2500);
    check(!(await p.eval(`/Getting ready/i.test(document.querySelector('.pane.focused').textContent)`)), `${label}: "Getting ready" shows nowhere`);
    check(await busy(p), `${label}: …the stop button shows the chat is busy`);
    await p.shot(`${SHOTS}/stuck-quiet-${label}.png`);
    const took = await settles(p, 26000);
    check(took >= 0, `${label}: lost end — the line goes and the composer is idle again (${took >= 0 ? `${Math.round((took + 4400) / 100) / 10}s after it showed` : "still stuck after 30s"})`);
    await p.shot(`${SHOTS}/stuck-recovered-${label}.png`);

    // 2. Back from the background: checked at once, no waiting.
    await ws.rpc("mock_composer", { stall: SID });
    await p.waitFor(`document.querySelector('.pane.focused .activity-slot, .pane.focused .activity')`, 4000);
    await sleep(600);
    await p.eval(`document.dispatchEvent(new Event('visibilitychange'))`);
    const back = await settles(p, 4000);
    check(back >= 0, `${label}: back in front — corrected at once (${back}ms)`);

    if (label === "desktop") {
      // 3. A real turn whose last events are lost (muted mid-stream, unmuted after it ended).
      await send(p, "Quick question about the reconnect test");
      await streaming(p);
      await ws.rpc("mock_composer", { mute: { session_id: SID, on: true } });
      await sleep(7000);
      await ws.rpc("mock_composer", { mute: { session_id: SID, on: false } });
      const lost = await settles(p, 26000);
      check(lost >= 0, `desktop: a turn's lost ending is caught (${lost}ms after the server finished)`);
      const answered = await p.eval(`document.querySelectorAll('.pane.focused .transcript .msg-user').length`);
      check(answered > 0, "desktop: the reply is there after the check");

      // 4. A snapshot that left before the turn ended and lands after it: not the last word.
      await ws.rpc("mock_composer", { slow_snapshots: 20000 });
      await send(p, "Another quick question about the reconnect test");
      await streaming(p);
      const mark = ws.events.length;
      const asked = Date.now();
      await ws.rpc("mock_composer", { resync: true });
      await sleep(400);
      await ws.rpc("mock_composer", { slow_snapshots: 0 }); // only the resync's snapshot is late
      const ended = () => ws.events.slice(mark).some((e) => e.type === "session_updated" && e.session.id === SID && e.session.status === "idle");
      while (!ended() && Date.now() - asked < 19000) await sleep(200);
      check(ended() && !(await busy(p)), "desktop: the turn ended (and showed so) while the resync's snapshot was on its way");
      await sleep(Math.max(0, asked + 23000 - Date.now())); // the stale snapshot landed ~3s ago
      const after = { line: await line(p), busy: await busy(p) };
      check(!after.line && !after.busy, "desktop: a snapshot older than the turn's end doesn't leave the chat busy", after);

      // 5. The connection drops mid-turn and the turn ends while it's down.
      await send(p, "One more question about the reconnect test");
      await streaming(p);
      await fetch(MOCK + "/__mock/drop", { method: "POST" });
      const recon = await settles(p, 20000);
      check(recon >= 0, `desktop: reconnect mid-turn ends idle (${recon}ms)`);

      // 6. A resync mid-stream keeps the clocks running ("Thinking · 6s" doesn't drop back to nothing).
      const ws2 = await wsClient("stuck test, after the drop"); // the drop closed ours too
      await send(p, "Stream the long answer about the reconnect test");
      await p.eval(`__mod('lib/store.ts').then((m) => { window.__st = m.state; return 1; })`);
      await p.waitFor(`window.__st.sessions['${SID}']?.streaming?.content?.some((x) => x && x.type === 'thinking' && x.text.length > 40)`, 15000);
      const clocks = () => p.eval(`(() => { const s = window.__st.sessions['${SID}'].streaming; return s ? { started: s.startedAt, think: s.thinkStart ?? null } : null; })()`);
      const before = await clocks();
      await ws2.rpc("mock_composer", { resync: true });
      await sleep(900);
      const kept = await clocks();
      check(!!before?.think && kept?.started === before.started && kept?.think === before.think, "desktop: a resync mid-stream keeps the stream's clocks", { before, kept });
      await p.eval(`document.querySelector('.pane.focused .send-btn.stop')?.click()`);
      check((await settles(p, 8000)) >= 0, "desktop: stopping it ends idle");
      ws2.close();
    }
    console.log("errors", p.errors);
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    ws.close();
    await p.close();
  }
  finish();
});
