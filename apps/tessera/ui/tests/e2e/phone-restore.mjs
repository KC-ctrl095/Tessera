// Cold restore after pairing, not a fresh-install launch: long chats on several saved mosaics.
import fs from "node:fs";
import { SHOTS, SCRATCH, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const trace = `(() => {
  const b = window.__boot = { long: [], frames: [], resizes: {}, requests: {}, active: true };
  new PerformanceObserver(l => { if (b.active) for (const e of l.getEntries()) b.long.push(e.duration); }).observe({ type: 'longtask', buffered: true });
  const RO = window.ResizeObserver;
  window.ResizeObserver = class extends RO { constructor(cb) { super((entries, self) => { for (const e of entries) { const key = e.target.className || e.target.tagName; b.resizes[key] = (b.resizes[key] || 0) + 1; } cb(entries, self); }); } };
  const send = WebSocket.prototype.send;
  WebSocket.prototype.send = function(data) { if (typeof data === 'string') { try { const m = JSON.parse(data); if(m.method) b.requests[m.method] = (b.requests[m.method] || 0) + 1; } catch {} } return send.call(this, data); };
  let last = performance.now();
  const frame = now => { if (!b.active) return; b.frames.push(now-last); last=now; requestAnimationFrame(frame); };
  requestAnimationFrame(frame);
})()`;

async function firstSnapshotReconnect(heldFailure) {
  const label = heldFailure ? 'pending-first-snapshot-race' : 'failed-first-snapshot';
  await mock('/__mock/reset');
  const control = await wsClient(label);
  const seeded = (await control.rpc('mock_phone_lag', { calls: 100, chats: 8, helpers: 0 })).result;
  const id = seeded.parents[0].id;
  await control.rpc('mock_composer', { slow_snapshots: 4000 });
  const p = await device();
  await p.viewport(344, 812, true, 2);
  await p.send('Page.addScriptToEvaluateOnNewDocument', { source: trace });
  await signIn(p, label);
  await p.eval(`(async () => {
    const m = await __mod('lib/store.ts');
    window.__firstStore = m;
    const original = m.api.getSession;
    const probe = window.__firstSnapshot = { id: ${JSON.stringify(id)}, requests: 0, rejected: false, held: ${heldFailure} };
    m.api.getSession = (sid, since) => {
      if (sid !== probe.id) return original(sid, since);
      const first = ++probe.requests === 1;
      return original(sid, since).catch(error => {
        if (!first) throw error;
        probe.rejected = true;
        if (!probe.held) throw error;
        // Native transport can report an old failed call after the new connection bootstraps.
        return new Promise((_, reject) => { probe.release = () => reject(error); });
      });
    };
    const w = await __mod('lib/workspace.ts');
    w.openSession(probe.id, { pane: w.workspace.focused() });
  })()`);
  await p.waitFor(`window.__firstStore.state.sessions[${JSON.stringify(id)}]?.loading && !window.__firstStore.state.sessions[${JSON.stringify(id)}]?.loaded`, 10000);
  await p.eval(`window.__firstSnapshot.chat = document.querySelector('.pane.focused .chat'); window.__firstSnapshot.composer = document.querySelector('.pane.focused .composer textarea')`);
  await mock('/__mock/drop?hard=1');
  control.close();
  await p.waitFor(`window.__firstSnapshot.rejected`, 10000);
  await p.waitFor(`window.__boot.requests.list_notes >= 2 && window.__firstStore.state.conn.state === 'connected'`, 15000);
  await p.eval(`__mod('lib/store.ts').then(m => m.call({ method: 'mock_composer', params: { slow_snapshots: 0 } }))`);
  if (heldFailure) {
    // A fresh connection loads independently even while an old native failure is held.
    await p.eval(`__mod('lib/store.ts').then(m => { void m.resyncOpen(); return true; })`);
    await p.waitFor(`window.__firstSnapshot.requests >= 2`, 10000);
    check(await p.eval(`typeof window.__firstSnapshot.release === 'function' && window.__firstSnapshot.requests >= 2`), `${label}: a fresh history request bypasses the old pending connection`);
    await p.eval(`window.__firstSnapshot.release()`);
  }
  await p.waitFor(`window.__firstStore.state.sessions[${JSON.stringify(id)}]?.loaded`, 15000);
  const recovered = await p.eval(`(async () => {
    const m = await __mod('lib/store.ts'), b = window.__firstSnapshot;
    return { entries: m.state.sessions[b.id].entries.length, requests: b.requests, loading: m.state.sessions[b.id].loading,
      sameChat: b.chat === document.querySelector('.pane.focused .chat'), sameComposer: b.composer === document.querySelector('.pane.focused .composer textarea'),
      unseen: ${JSON.stringify(seeded.parents.slice(1).map(s => s.id))}.filter(id => m.state.sessions[id]?.loaded || m.state.sessions[id]?.loading).length };
  })()`);
  check(recovered.entries === seeded.entries && recovered.requests >= 2 && !recovered.loading, `${label}: failed first history recovers fully after reconnect`, recovered);
  check(recovered.sameChat && recovered.sameComposer, `${label}: recovery needs no remount or re-pair`, recovered);
  check(recovered.unseen === 0, `${label}: seven unseen histories remain unloaded`, recovered);
  await p.shot(`${SHOTS}/phone-restore-${label}.png`);
  check(!p.errors.length, `${label}: no page errors`, p.errors);
  await p.close();
}
run(async () => {
  const ws = await wsClient("Phone restore probe");
  for (const [label, width, height, theme, chats] of [
    ["one-folded", 344, 812, "dark", 1], ["many-folded", 344, 812, "dark", 8],
    ["many-fold", 768, 900, "light", 8],
  ]) {
    if (process.env.ONLY && label !== process.env.ONLY) continue;
    await mock("/__mock/reset");
    const result = await ws.rpc("mock_phone_lag", { calls: Number(process.env.LAG_CALLS ?? 3000), chats, helpers: 24, mosaics: true });
    if (result.error) throw Error(JSON.stringify(result.error));
    const ids = result.result.parents.map(s => s.id);
    const p = await device();
    await p.viewport(width, height, true, 2);
    await signIn(p, label);
    await p.theme(theme);
    // Wait for the first visit to save its paired endpoint and server layout, then cold-reload it.
    await p.waitFor(`document.querySelector('.pane .transcript .item')`, 90000);
    await sleep(1500);
    await p.send("Page.addScriptToEvaluateOnNewDocument", { source: trace });
    await p.send("Emulation.setCPUThrottlingRate", { rate: 4 });
    await p.send("Profiler.enable");
    await p.send("Profiler.start");
    const at = Date.now();
    await p.reload(0);
    await p.waitFor(`!!window.__boot`, 90000);
    await p.waitFor(`document.querySelector('.pane .transcript .item')`, 90000);
    const restoreMs = Date.now() - at;
    await sleep(1800);
    const stats = await p.eval(`(async () => {
      const m = await __mod('lib/store.ts');
      const b = window.__boot; b.active = false;
      const stats = a => { a.sort((x,y) => x-y); return { n:a.length, p95:a[Math.floor(a.length*.95)] ?? 0, max:a.at(-1) ?? 0, total:a.reduce((x,y) => x+y,0) }; };
      return { time: performance.now(), long: stats(b.long), frame: stats(b.frames), resizes: b.resizes, requests: b.requests, chats: document.querySelectorAll('.chat').length, composers: document.querySelectorAll('.composer textarea').length, transcripts: document.querySelectorAll('.transcript').length, panes: document.querySelectorAll('.pane').length, loaded: ${JSON.stringify(ids)}.filter(id=>m.state.sessions[id]?.loaded).length };
    })()`);
    const { profile } = await p.send("Profiler.stop");
    await p.send("Emulation.setCPUThrottlingRate", { rate: 1 });
    fs.mkdirSync(SCRATCH, { recursive: true });
    fs.writeFileSync(`${SCRATCH}/${label}-restore-profile.json`, JSON.stringify(profile));
    fs.writeFileSync(`${SCRATCH}/${label}-restore.json`, JSON.stringify({ restoreMs, ...stats }, null, 2));
    console.log(label, JSON.stringify({ restoreMs, ...stats }));
    await p.shot(`${SHOTS}/phone-restore-${label}.png`);
    check(stats.panes === chats, `${label}: every saved chat tile is preserved`, stats);
    check(stats.chats <= (width < 600 ? 1 : 2), `${label}: unseen restored chats do not mount heavy views`, stats.chats);
    check(stats.loaded <= (width < 600 ? 1 : 2), `${label}: unseen chats do not fan out history snapshots at launch`, stats.loaded);
    // Selecting an unseen mosaic and tile still loads its full history through the normal path.
    if (chats > 1) {
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession(${JSON.stringify(ids.at(-1))}))`);
      await p.waitFor(`document.querySelector('.pane.focused .transcript .item')`, 20000);
      const accessible = await p.eval(`__mod('lib/store.ts').then(m => m.state.sessions[${JSON.stringify(ids.at(-1))}]?.entries.length)`);
      check(accessible === result.result.entries, `${label}: selecting a previously unseen tile fetches all ${result.result.entries} entries`, accessible);
    }
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    await p.close();
  }
  ws.close();
  await firstSnapshotReconnect(false);
  await firstSnapshotReconnect(true);
  finish();
});
