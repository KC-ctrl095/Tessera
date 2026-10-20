// Synthetic long-lived chats: thousands of entries, large tool payloads,
// helper sessions and a continuous reply while typing and scrolling.
import fs from "node:fs";
import { SHOTS, SCRATCH, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const T = ".pane.focused .transcript";
const profileSummary = (profile) => {
  const names = new Map(profile.nodes.map((n) => [n.id, n]));
  const time = new Map();
  for (let i = 0; i < (profile.samples ?? []).length; i++) {
    const n = names.get(profile.samples[i]);
    const key = `${n.callFrame.functionName || '(anonymous)'} ${n.callFrame.url.split('/').slice(-2).join('/')}#${n.callFrame.lineNumber + 1}`;
    time.set(key, (time.get(key) ?? 0) + (profile.timeDeltas?.[i] ?? 0));
  }
  return [...time].sort((a, b) => b[1] - a[1]).slice(0, 25).map(([name, us]) => [name, Math.round(us / 1000)]);
};

async function seed(p, ws, calls) {
  const seeded = (await ws.rpc('mock_phone_lag', { calls, chats: 1, helpers: 24, single_turn: !!process.env.LAG_SINGLE_TURN })).result;
  const info = seeded.parents[0];
  await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession(${JSON.stringify(info.id)}, { pane: m.workspace.focused() }))`);
  await p.waitFor(`document.querySelector('${T}')`, 90000);
  // The stress phase keeps the entire saved workload in memory, as if read.
  // First-open paging and real reader gates are exercised by history-paging.
  if (!process.env.LAG_BASELINE) await p.eval(`__mod('lib/history.ts').then(m => m.historyTo(${JSON.stringify(info.id)}, 0))`);
  await sleep(800);
  await p.eval(`(async () => {
    const m = await __mod('lib/store.ts');
    const id = ${JSON.stringify(info.id)};
    const s = m.state.sessions[id];
    const model = s.info.model;
    const entries = s.entries;
    const usage = { input: 4000, output: 500, cache_read: 3000, cache_write: 0 };
    for (let i = 0; i < 24; i++) {
      const cid = id + '-helper-' + i;
      m.upsertSession({ ...s.info, id: cid, kind: 'subagent', parent_id: id, parent_call: 'lag-agent', title: 'Synthetic helper ' + i, status: 'running', steps: 10, created_at: 1700000000000 + i });
      m.setState('sessions', cid, 'loaded', true);
    }
    window.__lag = { id, model, usage, seq: s.info.last_seq, entries: entries.length, costs: [], frames: [], long: [], ticks: 0, active: false };
    return entries.length;
  })()`);
  await p.waitFor(`document.querySelector('${T}')`, 10000);
  await sleep(1000);
  return info.id;
}

async function measure(p, label) {
  await p.send("Profiler.enable");
  await p.send("Profiler.setSamplingInterval", { interval: 1000 });
  await p.send("Emulation.setCPUThrottlingRate", { rate: Number(process.env.LAG_THROTTLE ?? 4) });
  await p.send("Profiler.start");
  await p.eval(`(async () => {
    const m = await __mod('lib/store.ts');
    const l = window.__lag;
    m.setState('sessions', l.id, 'info', 'status', 'running');
    m.applyEvent({ type: 'stream_started', session_id: l.id, model: l.model });
    l.active = true;
    l.observer = new PerformanceObserver(list => { for (const e of list.getEntries()) if(l.active) l.long.push(e.duration); });
    l.observer.observe({ type: 'longtask' });
    let prev = performance.now();
    const frame = (now) => { if(!l.active) return; l.frames.push(now - prev); prev = now; requestAnimationFrame(frame); };
    requestAnimationFrame(frame);
    const chunk = 'Continuous synthetic reply with **bold words** and details. ';
    l.timer = setInterval(() => {
      for (let i = 0; i < 8; i++) m.applyEvent({ type: 'stream_delta', session_id: l.id + (i ? '-helper-' + (i-1) : ''), delta: { index: 0, part: 'text', text: chunk } });
      const t = performance.now(); m.flushDeltas(); l.costs.push(['delta', performance.now() - t]);
      if (++l.ticks % 10 !== 0) return;
      const t2 = performance.now();
      m.applyEvent({ type: 'entry_added', session_id: l.id, entry: { kind: 'assistant', seq: ++l.seq, ts: 1700010000000 + l.seq * 1000, model: l.model, usage: l.usage, stop: 'tool_use', content: [{ type: 'tool_call', id: 'lag-new-' + l.seq, name: 'read', arguments: { path: '/synthetic/new-' + l.seq } }] } });
      l.costs.push(['entry', performance.now() - t2]);
    }, 40);
  })()`);
  await sleep(1800);
  const composer = ".pane.focused .composer textarea";
  if (await p.eval(`!!document.querySelector('${composer}')`)) {
    await p.click(composer);
    await p.type("Typing while eight synthetic helpers stream");
  }
  const box = await p.eval(`(() => { const r = document.querySelector('${T}').getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()`);
  await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: box.x, y: box.y - 90 }] });
  for (let i = 1; i <= 8; i++) {
    await p.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x: box.x, y: box.y - 90 + i * 25 }] });
    await sleep(30);
  }
  await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  await sleep(1800);
  const metrics = await p.eval(`(() => {
    const l = window.__lag; l.active = false; clearInterval(l.timer); l.observer.disconnect();
    const stats = a => { a.sort((x,y) => x-y); return { n: a.length, p50: a[Math.floor(a.length * .5)] ?? 0, p95: a[Math.floor(a.length * .95)] ?? 0, max: a.at(-1) ?? 0, total: a.reduce((x,y) => x+y, 0) }; };
    return { entries: l.entries, ticks: l.ticks, delta: stats(l.costs.filter(c => c[0] === 'delta').map(c => c[1])), entry: stats(l.costs.filter(c => c[0] === 'entry').map(c => c[1])), frame: stats(l.frames), long: stats(l.long), rows: document.querySelectorAll('${T} .item').length, typed: document.querySelector('.pane.focused .composer textarea')?.value, detached: !!document.querySelector('.activity-jump') };
  })()`);
  const { profile } = await p.send("Profiler.stop");
  await p.send("Emulation.setCPUThrottlingRate", { rate: 1 });
  const summary = profileSummary(profile);
  fs.mkdirSync(SCRATCH, { recursive: true });
  fs.writeFileSync(`${SCRATCH}/${label}-profile.json`, JSON.stringify(profile));
  fs.writeFileSync(`${SCRATCH}/${label}-metrics.json`, JSON.stringify({ ...metrics, profile: summary }, null, 2));
  console.log(label, JSON.stringify(metrics));
  console.log("PROFILE", label, JSON.stringify(summary));
  await p.shot(`${SHOTS}/phone-lag-${label}.png`);
  check(metrics.rows <= 65, `${label}: recent rows are bounded, older history remains available`, metrics.rows);
  check(metrics.typed?.includes("Typing while"), `${label}: typing during streams works`);
  check(metrics.detached, `${label}: touch scroll detaches from streaming tail`);
  check(metrics.delta.p95 < 20 && metrics.entry.p95 < 100, `${label}: store updates leave time for phone input (delta p95 ${metrics.delta.p95.toFixed(1)} ms; entry p95 ${metrics.entry.p95.toFixed(1)} ms)`, metrics);
  return metrics;
}

run(async () => {
  await mock("/__mock/reset");
  const ws = await wsClient("Synthetic phone lag");
  for (const [label, width, height, theme] of [
    ["folded-light", 344, 812, "light"], ["folded-dark", 344, 812, "dark"],
    ["fold-light", 768, 900, "light"], ["fold-dark", 768, 900, "dark"],
  ]) {
    if (process.env.ONLY && label !== process.env.ONLY) continue;
    const p = await device();
    await p.viewport(width, height, true, 2);
    await signIn(p, label);
    await p.theme(theme);
    await seed(p, ws, Number(process.env.LAG_CALLS ?? 3000));
    const equivalence = await p.eval(`(async () => {
      const m = await __mod('lib/transcript.ts'), t = await __mod('lib/tools.ts'), store = await __mod('lib/store.ts');
      if (!m.TranscriptDeriver || !t.ToolDurationIndex) return ['uncached baseline'];
      const source = store.state.sessions[window.__lag.id].entries.slice(0, 165).map(e => JSON.parse(JSON.stringify(e)));
      // The server stores the time it measured with a result; give every third one so measured and gap-based calls mix.
      source.forEach((e, i) => { if (e.kind === 'tool_result' && i % 3 === 0) e.details = { ...(e.details ?? {}), duration_ms: 1500 + i }; });
      const errors = [];
      for (const opts of [{ }, { speakers: true, defaultAuthor: 'bot:synthetic' }, { speakers: true, collapseAll: true, defaultAuthor: 'bot:synthetic', userAuthor: 'chat:' }]) {
        const d = new m.TranscriptDeriver(), times = new t.ToolDurationIndex(); let v = 0; const entries = [];
        const verify = () => {
          for (const running of [true, false]) {
            const actual = d.derive(entries, running, opts, v), expected = m.deriveItems(entries, new m.ItemCache(), running, opts);
            if(JSON.stringify(actual) !== JSON.stringify(expected)) errors.push('derive at ' + entries.length + ' running=' + running + ' opts=' + JSON.stringify(opts));
          }
          if(JSON.stringify(times.update(entries, v)) !== JSON.stringify(t.toolDurations(entries))) errors.push('timing at ' + entries.length);
        };
        for (const e of source) { entries.push(e); verify(); }
        entries.splice(5, 1); v++; verify(); // rewrite without append
        entries.splice(20); v++; verify(); // rewind
        const replacement = entries.slice();
        if(JSON.stringify(d.derive(replacement, false, opts, v)) !== JSON.stringify(m.deriveItems(replacement, new m.ItemCache(), false, opts))) errors.push('replacement');
      }
      return errors;
    })()`);
    const measured = await p.eval(`(async () => {
      const t = await __mod('lib/tools.ts');
      const call = (id) => ({ type: 'tool_call', id, name: 'bash', arguments: {} });
      const entries = [
        { kind: 'assistant', seq: 1, ts: 1000, content: [call('a'), call('b'), call('c')] },
        { kind: 'tool_result', seq: 2, ts: 1500, call_id: 'a', text: 'ok', is_error: false, details: { duration_ms: 40 } },
        { kind: 'tool_result', seq: 3, ts: 2500, call_id: 'b', text: 'ok', is_error: false },
        { kind: 'tool_result', seq: 4, ts: 2600, call_id: 'c', text: 'ok', is_error: false, details: { duration_ms: 'soon' } },
        { kind: 'tool_result', seq: 5, ts: 2700, call_id: 'orphan', text: 'ok', is_error: false, details: { duration_ms: 9 } },
      ];
      const index = new t.ToolDurationIndex(), grown = [];
      let last;
      for (const e of entries) { grown.push(e); last = JSON.stringify(index.update(grown, 0)); }
      return { full: t.toolDurations(entries), index: JSON.parse(last) };
    })()`);
    const wanted = { a: 40, b: 1000, c: 100 };
    check(JSON.stringify(measured.full) === JSON.stringify(wanted) && JSON.stringify(measured.index) === JSON.stringify(wanted), `${label}: a call's measured time beats the gap between entries; the gap is used only when none was stored`, measured);
    if (!process.env.LAG_BASELINE) check(!equivalence.length, `${label}: cached transcript/duration output matches full history derivation across append, rewrite, rewind and DM modes`, equivalence.slice(0, 4));
    await measure(p, label);
    const beforeHistory = await p.eval(`(() => {
      const persisted = [...document.querySelectorAll('${T} .item [data-seq], ${T} .item[data-seq]')].filter(el => Number.isFinite(Number(el.dataset.seq)) && Number(el.dataset.seq) > 0 && Number(el.dataset.seq) <= window.__lag.seq);
      return { count: new Set(persisted.map(el => el.closest('.item'))).size, first: Math.min(...persisted.map(el => Number(el.dataset.seq))) };
    })()`);
    await p.eval(`(() => { const button = document.querySelector('${T} .load-more button'); if (!button) throw Error('Missing Show earlier control'); button.scrollIntoView({ block: 'start' }); })()`);
    await p.click(`${T} .load-more button`); // Required: a missing control must fail, not silently skip.
    await sleep(300);
    const afterHistory = await p.eval(`(async () => {
      const m = await __mod('lib/store.ts');
      const persisted = [...document.querySelectorAll('${T} .item [data-seq], ${T} .item[data-seq]')].filter(el => Number.isFinite(Number(el.dataset.seq)) && Number(el.dataset.seq) > 0 && Number(el.dataset.seq) <= window.__lag.seq);
      const entry = m.state.sessions[window.__lag.id].entries.find(e => e.kind === 'user' && !e.author && e.seq < ${beforeHistory.first} && persisted.some(el => Number(el.dataset.seq) === e.seq));
      const el = entry && persisted.find(el => Number(el.dataset.seq) === entry.seq);
      if (el) el.scrollIntoView({ block: 'center' });
      return { count: new Set(persisted.map(el => el.closest('.item'))).size, seq: entry?.seq, text: entry?.content.find(p => p.type === 'text')?.text };
    })()`);
    await sleep(200);
    check(afterHistory.count > beforeHistory.count, `${label}: Show earlier adds persisted history rows, not live rows`, { beforeHistory, afterHistory });
    await p.eval(`(() => { const el = document.querySelector('${T} .msg-user[data-seq="${afterHistory.seq}"]'); if (!el) throw Error('Earlier persisted entry is missing'); el.closest('.item').scrollIntoView({ block: 'center' }); })()`);
    await sleep(200);
    const earlierVisible = await p.eval(`(() => {
      const el = document.querySelector('${T} .msg-user[data-seq="${afterHistory.seq}"]');
      const text = el.innerText;
      const r = el.getBoundingClientRect(), viewport = document.querySelector('${T}').getBoundingClientRect();
      return { textMatches: text.includes(${JSON.stringify(afterHistory.text ?? '')}), top: r.top, bottom: r.bottom, viewportTop: viewport.top, viewportBottom: viewport.bottom };
    })()`);
    check(!!afterHistory.seq && earlierVisible.textMatches && earlierVisible.bottom > earlierVisible.viewportTop && earlierVisible.top < earlierVisible.viewportBottom, `${label}: an actual earlier user entry is visible after loading it`, { afterHistory, earlierVisible });
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    await p.close();
  }
  ws.close();
  finish();
});
