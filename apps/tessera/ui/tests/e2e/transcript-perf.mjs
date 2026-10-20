// Repeatable 3,008-entry rich transcript at 4x CPU. PERF_BASELINE records without
// enforcing targets. PERF_PROFILE saves CDP profiles. The report keeps target misses
// explicit even on software-rendered CI, where 60 Hz is not guaranteed.
import fs from 'node:fs';
import { SCRATCH, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
const T = '.pane.focused .transcript';
const stats = a => { a.sort((x, y) => x - y); return { n: a.length, p95: a[Math.floor(a.length * .95)] ?? 0, max: a.at(-1) ?? 0, total: a.reduce((x, y) => x + y, 0) }; };
const frame = p => p.eval('new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))');
async function monitor(p) {
  await p.eval(`(() => {
    const m = window.__perf = { on: true, frames: [], long: [], prev: null };
    const tick = now => { if (!m.on) return; if (m.prev !== null) m.frames.push(now - m.prev); m.prev = now; requestAnimationFrame(tick); };
    requestAnimationFrame(tick);
    m.observer = new PerformanceObserver(l => { if (m.on) for (const e of l.getEntries()) m.long.push(e.duration); });
    m.observer.observe({ type: 'longtask' });
  })()`);
}
async function stop(p) {
  await p.eval('new Promise(r => setTimeout(r, 0))');
  const m = await p.eval('(() => { const m = window.__perf; m.on = false; m.observer.disconnect(); return { frames: m.frames, long: m.long }; })()');
  return { frameTiming: stats(m.frames), dropped: m.frames.reduce((n, d) => n + Math.max(0, Math.round(d / (1000 / 60)) - 1), 0), long: stats(m.long) };
}
async function sweep(p) {
  // Native scrolling stays in charge of touch/keyboard momentum. This deterministic
  // rAF sweep covers every part of the history instead of sampling just the tail.
  return p.eval(`new Promise(resolve => {
    const t = document.querySelector('${T}'); let direction = -1, frames = 0, distance = 0, prev = performance.now();
    t.dispatchEvent(new WheelEvent('wheel', { deltaY: -100, bubbles: true }));
    const tick = now => {
      // Constant velocity, not a fixed distance per delivered frame. A stalled
      // renderer must show up as dropped frames, not silently slow the gesture.
      const step = Math.max(1, now - prev) * 30, before = t.scrollTop; prev = now;
      t.scrollTop += direction * step; distance += Math.abs(t.scrollTop - before); frames++;
      if (direction < 0 && t.scrollTop <= 1) direction = 1;
      else if (direction > 0 && t.scrollTop >= t.scrollHeight - t.clientHeight - 1) return resolve({ frames, distance, velocityPxPerSecond: 30000 });
      requestAnimationFrame(tick);
    }; requestAnimationFrame(tick);
  })`);
}
async function profile(p, label, action) {
  if (process.env.PERF_PROFILE) await p.send('Profiler.start');
  const result = await action();
  if (process.env.PERF_PROFILE) { const { profile } = await p.send('Profiler.stop'); fs.writeFileSync(`${SCRATCH}/${label}.cpuprofile`, JSON.stringify(profile)); }
  return result;
}
run(async () => {
  fs.mkdirSync(SCRATCH, { recursive: true });
  await mock('/__mock/reset');
  const ws = await wsClient('Transcript performance fixture');
  for (const [label, w, h, mobile] of [['desktop', 1440, 900, false], ['phone', 390, 844, true], ['cover', 344, 844, true], ['fold', 841, 701, true]]) {
    if (process.env.ONLY && !process.env.ONLY.split(',').includes(label)) continue;
    const f = (await ws.rpc('mock_transcript_perf', { turns: Number(process.env.PERF_TURNS ?? 750) })).result;
    await mock('/__mock/mosaics/reset');
    const p = await device(); await p.viewport(w, h, mobile, mobile ? 2 : 1);
    await p.send('Page.addScriptToEvaluateOnNewDocument', { source: `(() => {
      const intervals = window.__intervals = new Set(), set = window.setInterval, clear = window.clearInterval;
      window.setInterval = (...args) => { const id = set(...args); intervals.add(id); return id; };
      window.clearInterval = id => { intervals.delete(id); return clear(id); };
      const d = Object.getOwnPropertyDescriptor(WebSocket.prototype, 'onmessage');
      Object.defineProperty(WebSocket.prototype, 'onmessage', { configurable: true, get: d.get, set(fn) {
        d.set.call(this, fn && function(e) { if (window.__arrivalArmed && typeof e.data === 'string' && e.data.includes('ses_transcript_perf') && e.data.includes('entries')) { window.__dataAt = performance.now(); window.__arrivalArmed = false; } return fn.call(this, e); });
      }});
    })()` });
    await signIn(p, `Transcript perf ${label}`); await p.theme('dark');
    await p.send('Performance.enable'); await p.send('Profiler.enable');
    await p.send('Emulation.setCPUThrottlingRate', { rate: 4 });
    const opened = await profile(p, `${label}-open`, () => p.eval(`(async () => {
      const m = await __mod('lib/workspace.ts'); window.__arrivalArmed = true; window.__dataAt = null;
      const began = performance.now(); m.openSession('${f.id}', { pane: m.workspace.focused() });
      return new Promise((resolve, reject) => { const tick = () => {
        const t = document.querySelector('${T}'), msg = t?.querySelector('.msg-assistant:last-of-type');
        const last = t && [...t.querySelectorAll('.msg-assistant')].at(-1);
        if (last?.textContent.includes('Newest synthetic message.') && last.getBoundingClientRect().top < t.getBoundingClientRect().bottom) requestAnimationFrame(() => resolve({ openMs: performance.now() - began, arrivalToPaintMs: window.__dataAt === null ? null : performance.now() - window.__dataAt }));
        else if (performance.now() - began > 90000) reject(new Error('Newest message never painted')); else requestAnimationFrame(tick);
      }; requestAnimationFrame(tick); });
    })()`));
    await sleep(1000);
    // Load the entire fixture through the real reader gate, including async pages.
    // Page loading cost stays separate from steady-state scrolling.
    await monitor(p);
    const load = await profile(p, `${label}-history`, () => p.eval(`(async () => {
      const began = performance.now(); let pages = 0;
      while (document.querySelector('${T} .load-more button') && pages < 200) {
        document.querySelector('${T} .load-more button').click(); pages++;
        const until = performance.now() + 20000;
        do {
          await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
          if (performance.now() > until) throw new Error('Older page did not finish');
        } while (document.querySelector('${T} .load-more button:disabled'));
      }
      return { ms: performance.now() - began, pages };
    })()`));
    const loading = await stop(p);
    const expanded = await p.eval(`(() => {
      const t = document.querySelector('${T}');
      const tools = [...t.querySelectorAll('.tool-head')].filter((_, i) => i % 75 === 0);
      const thoughts = [...t.querySelectorAll('.thinking-head')].filter((_, i) => i % 150 === 0);
      for (const h of [...tools, ...thoughts]) h.click();
      return { tools: tools.length, thoughts: thoughts.length };
    })()`); await frame(p);
    await p.eval(`(() => { const t = document.querySelector('${T}'); t.focus(); t.dispatchEvent(new KeyboardEvent('keydown', { key: 'End', bubbles: true })); })()`); await frame(p);
    await p.send('HeapProfiler.collectGarbage');
    const heap = async () => (await p.send('Runtime.getHeapUsage')).usedSize / 1048576;
    const heapBefore = await heap();
    const scroll = [];
    for (let i = 0; i < 2; i++) { await monitor(p); const path = await profile(p, `${label}-scroll-${i}`, () => sweep(p)); scroll.push({ ...await stop(p), ...path, heapMiB: await heap() }); }
    await p.send('HeapProfiler.collectGarbage'); const heapAfter = await heap();
    const anchor = await p.eval(`(async () => {
      const t = document.querySelector('${T}'); t.dispatchEvent(new WheelEvent('wheel', { deltaY: -200, bubbles: true })); t.scrollTop -= t.clientHeight * 2;
      const wait = () => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r))); await wait();
      const r = t.getBoundingClientRect(), hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
      const probe = hit.closest('.md > *, .bubble, .tool-head, .item'); const at = probe.getBoundingClientRect().top;
      const rows = [...t.querySelectorAll('.transcript-inner > .item')];
      const above = rows.filter(e => e.getBoundingClientRect().bottom < r.top - 20).at(-1); if (!above) return { unavailable: true };
      const grow = document.createElement('div'); grow.style.height = '240px'; above.prepend(grow); await wait(); const grown = probe.getBoundingClientRect().top;
      grow.remove(); await wait(); return { maxJumpPx: Math.max(Math.abs(grown - at), Math.abs(probe.getBoundingClientRect().top - at)) };
    })()`);
    await ws.rpc('mock_stream', { session_id: f.id, text: ('Synthetic streaming **reply** with a [link](https://example.com).\n\n').repeat(1000), chunk: [12, 30], delay: [20, 35], seed: 7 });
    await p.waitFor(`document.querySelector('${T} .md.streaming')`);
    await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
    const typing = []; await monitor(p);
    await profile(p, `${label}-input`, async () => {
      for (let i = 0; i < 24; i++) {
        const at = Date.now(); await p.send('Input.insertText', { text: 'x' });
        await p.eval('new Promise(r => requestAnimationFrame(r))'); typing.push(Date.now() - at); await sleep(70);
      }
    });
    const streaming = await stop(p);
    const input = stats(typing);
    const intervals = await p.eval('window.__intervals.size');
    const result = { label, intervals, viewport: [w, h], throttle: 4, entries: f.entries, expanded, ...opened, load, loading, scroll, input, streaming, heapBefore, heapAfter, anchor,
      targets: { paint: opened.arrivalToPaintMs !== null && opened.arrivalToPaintMs < 500, scroll: scroll.every(s => s.long.max <= 50), input: input.p95 < 50, anchor: anchor.maxJumpPx <= 1, heap: heapAfter < heapBefore + 32 } };
    fs.writeFileSync(`${SCRATCH}/${label}-transcript-metrics.json`, JSON.stringify(result, null, 2)); console.log('TRANSCRIPT_METRICS', JSON.stringify(result));
    check(f.entries >= 3000 && opened.arrivalToPaintMs !== null, `${label}: rich history measured from actual WebSocket data arrival`);
    check(await p.eval(`document.querySelector('.pane.focused .composer textarea').value === 'x'.repeat(24)`), `${label}: typing survives streaming`);
    if (!process.env.PERF_BASELINE) {
      check(opened.arrivalToPaintMs < 1000 && input.p95 < 100, `${label}: paint and input stay responsive at 4x CPU`, { opened, input });
      check(anchor.maxJumpPx <= 1 && heapAfter < heapBefore + 32, `${label}: anchored and retained heap bounded`, { anchor, heapBefore, heapAfter });
      check(intervals < 40, `${label}: finished history does not keep per-card clock timers`, { intervals });
      if (process.env.PERF_STRICT) check(Object.values(result.targets).every(Boolean), `${label}: all performance targets`, result.targets);
    }
    // Selection spans messages and survives subsequent streaming changes.
    const selected = await p.eval(`(() => { const t = document.querySelector('${T}'), rows = [...t.querySelectorAll('.bubble-text, .msg-assistant p')].filter(e => e.getBoundingClientRect().height); const a = rows.at(-4), b = rows.at(-2); const r = document.createRange(); r.setStart(a, 0); r.setEnd(b, b.childNodes.length); const s = getSelection(); s.removeAllRanges(); s.addRange(r); return s.toString(); })()`);
    await sleep(300); check(selected.length > 30 && await p.eval('getSelection().toString()') === selected, `${label}: native cross-message selection survives streaming`);
    await p.eval('getSelection().removeAllRanges()');
    await p.eval(`(async () => { const w = await __mod('lib/workspace.ts'), u = await __mod('lib/ui.ts'); u.setJumpTarget({ paneId: w.workspace.focused(), sessionId: '${f.id}', seq: 1 }); })()`);
    await p.waitFor(`(() => { const t = document.querySelector('${T}'), e = t.querySelector('[data-seq="1"].flash'); if (!e) return false; const r = e.getBoundingClientRect(), v = t.getBoundingClientRect(); return r.top < v.bottom && r.bottom > v.top; })()`);
    check(true, `${label}: search jumps into the earliest history`);
    await p.eval(`document.querySelector('${T}').focus()`);
    await p.key('End', 'End', 0, 35); await frame(p);
    check(await p.eval(`(() => { const t = document.querySelector('${T}'); return t.scrollHeight - t.scrollTop - t.clientHeight <= 2; })()`), `${label}: End resumes streaming follow`);
    await p.key('PageUp', 'PageUp', 0, 33); await sleep(300);
    check(await p.eval(`(() => { const t = document.querySelector('${T}'); return t.scrollHeight - t.scrollTop - t.clientHeight > 20; })()`), `${label}: PageUp leaves live output without being pulled back`);
    await p.key('Home', 'Home', 0, 36); await sleep(800);
    check(await p.eval(`document.querySelector('${T}').scrollTop <= 2`), `${label}: Home reaches loaded history start`);
    await p.key('End', 'End', 0, 35); await frame(p);
    for (const theme of ['light', 'dark']) { await p.theme(theme); check(await p.eval('document.documentElement.scrollWidth <= innerWidth + 1'), `${label}-${theme}: no horizontal overflow`); await p.shot(`${SHOTS}/transcript-perf-${label}-${theme}.png`); }
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    await ws.rpc('abort', { session_id: f.id });
    await p.send('Emulation.setCPUThrottlingRate', { rate: 1 });
    await p.close();
  }
  ws.close(); finish();
});
