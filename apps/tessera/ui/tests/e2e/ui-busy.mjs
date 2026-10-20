// Idle timers and one entry per second in a long-lived Claude chat. Keep the
// workload in server state, and wait for lazy helper cards before measuring.
import fs from 'node:fs';
import { SCRATCH, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
import { helperStatusChecks } from './helper-status.mjs';

const T = '.pane.focused .transcript';
const stats = (a) => {
  a.sort((x, y) => x - y);
  return { n: a.length, p50: a[Math.floor(a.length * .5)] ?? 0, p95: a[Math.floor(a.length * .95)] ?? 0, max: a.at(-1) ?? 0, total: a.reduce((x, y) => x + y, 0) };
};
function hot(profile) {
  const nodes = new Map(profile.nodes.map(n => [n.id, n]));
  const time = new Map();
  profile.samples?.forEach((id, i) => {
    const f = nodes.get(id).callFrame;
    const name = `${f.functionName || '(anonymous)'} ${f.url.split('/').slice(-2).join('/')}#${f.lineNumber + 1}`;
    time.set(name, (time.get(name) ?? 0) + profile.timeDeltas[i]);
  });
  return [...time].sort((a, b) => b[1] - a[1]).slice(0, 25).map(([name, us]) => [name, Math.round(us / 1000)]);
}
async function monitor(p) {
  await p.eval(`(() => {
    const m = window.__busy = { on: true, frames: [], long: [], prev: performance.now() };
    const frame = now => { if (!m.on) return; m.frames.push(now - m.prev); m.prev = now; requestAnimationFrame(frame); };
    requestAnimationFrame(frame);
    m.observer = new PerformanceObserver(l => { for (const e of l.getEntries()) if (m.on) m.long.push(e.duration); });
    m.observer.observe({ type: 'longtask' });
  })()`);
}
async function sample(p, label, during, profileDuring = () => sleep(3000)) {
  await monitor(p);
  // CDP metrics can answer mid-task. Yield through a page task first so the
  // task and PerformanceObserver record are included in the same window.
  await p.eval(`new Promise(r => setTimeout(r, 0))`);
  const before = Object.fromEntries((await p.send('Performance.getMetrics')).metrics.map(m => [m.name, m.value]));
  await during();
  await p.eval(`new Promise(r => setTimeout(r, 0))`);
  const after = Object.fromEntries((await p.send('Performance.getMetrics')).metrics.map(m => [m.name, m.value]));
  const measured = await p.eval(`(() => { const m = window.__busy; m.frames.push(performance.now() - m.prev); m.on = false; m.observer.disconnect(); return { frames: m.frames, long: m.long }; })()`);
  let profile;
  if (process.env.BUSY_PROFILE) {
    await p.send('Profiler.start');
    await profileDuring();
    ({ profile } = await p.send('Profiler.stop'));
    fs.writeFileSync(`${SCRATCH}/${label}-profile.json`, JSON.stringify(profile));
  }
  return { seconds: after.Timestamp - before.Timestamp, cpu: (after.TaskDuration - before.TaskDuration) / (after.Timestamp - before.Timestamp), work: Object.fromEntries(['TaskDuration', 'ScriptDuration', 'LayoutDuration', 'RecalcStyleDuration', 'LayoutCount', 'RecalcStyleCount'].map(k => [k, after[k] - before[k]])), frames: stats(measured.frames), long: stats(measured.long), hot: profile ? hot(profile) : [] };
}
run(async () => {
  fs.mkdirSync(SCRATCH, { recursive: true });
  await mock('/__mock/reset');
  await mock('/__mock/mosaics/reset');
  await mock('/__mock/claude-code?state=ready');
  const ws = await wsClient('UI busy fixture');
  const f = (await ws.rpc('mock_ui_busy', { phase: 'seed', zero_steps: Number(process.env.BUSY_ZERO_STEPS ?? 24), legacy: Number(process.env.BUSY_LEGACY ?? 0) })).result;
  let statusChecked = false;
  for (const [label, w, h, mobile] of [['phone', 344, 812, true], ['fold', 841, 701, true], ['desktop', 1440, 900, false]]) {
    if (process.env.ONLY && process.env.ONLY !== label) continue;
    await mock('/__mock/mosaics/reset');
    const p = await device();
    await p.viewport(w, h, mobile, mobile ? 2 : 1);
    await signIn(p, `UI busy ${label}`);
    await p.theme('dark');
    await p.send('Performance.enable');
    await p.send('Profiler.enable');
    await p.send('Profiler.setSamplingInterval', { interval: 1000 });
    await p.send('Emulation.setCPUThrottlingRate', { rate: 4 });
    const began = Date.now();
    await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession(${JSON.stringify(f.id)}, { pane: m.workspace.focused() }))`);
    await p.waitFor(`document.querySelector('${T} .tool[data-status="running"]')`, 90000);
    const opened = Date.now() - began;
    await p.waitFor(`__mod('components/chat/SubagentGate.tsx').then(async m => !!(await m.subagentParts.load()) && m.childSessions(${JSON.stringify(f.id)}).length === 143)`, 60000);
    await p.waitFor(`document.querySelector('.pane.focused .composer textarea')`, 10000);
    // This suite measures updates after a reader has visited the whole history,
    // not the intentionally bounded first-open state covered by history-paging.
    await p.eval(`__mod('lib/history.ts').then(m => m.historyTo(${JSON.stringify(f.id)}, 0))`);
    await sleep(2000);
    const workload = await p.eval(`__mod('lib/store.ts').then(m => { const s = m.state.sessions[${JSON.stringify(f.id)}], children = Object.values(m.state.sessions).filter(s => s.info.parent_id === ${JSON.stringify(f.id)}); return { entries: s.entries.length, helpers: children.length, legacy: children.filter(s => !s.info.helper_owner).length, completed: children.filter(s => s.info.status === 'idle').length, zeroSteps: children.filter(s => s.info.steps === 0).length, runningHelpers: children.filter(s => s.info.status === 'running').length, running: Object.keys(s.running), timer: !!document.querySelector('${T} .tool-time.live'), rows: document.querySelectorAll('${T} .item').length }; })`);
    check(workload.entries >= 17000 && workload.helpers === 143 && workload.legacy === Number(process.env.BUSY_LEGACY ?? 0) && workload.completed === 140 && workload.runningHelpers === 3 && workload.zeroSteps === Number(process.env.BUSY_ZERO_STEPS ?? 24) && workload.timer && workload.running.includes(f.tool), `${label}: the full saved workload and running timer are mounted`, workload);
    if (!process.env.BUSY_B24) {
      await p.waitFor(`__mod('lib/inboxState.ts').then(m => m.inboxItems().length >= 80)`, 10000);
      check(true, `${label}: notification inbox is populated`);
    }
    const idle = await sample(p, `${label}-idle`, () => sleep(6000));
    const input = [], scroll = [], ticks = [];
    const updates = await sample(p, `${label}-updates`, async () => {
      await p.click('.pane.focused .composer textarea');
      for (let i = 0; i < 8; i++) {
        const at = Date.now();
        const sent = (await ws.rpc('mock_ui_busy', { phase: 'tick' })).result;
        const typedAt = Date.now();
        await p.send('Input.insertText', { text: `draft${i} ` });
        await p.eval(`new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))`);
        input.push(Date.now() - typedAt);
        await p.waitFor(`__mod('lib/store.ts').then(m => m.state.sessions[${JSON.stringify(f.id)}].entries.length >= ${sent.entries})`, 60000);
        ticks.push(Date.now() - at);
        const box = await p.box(T), scrollAt = Date.now();
        await p.send('Input.dispatchMouseEvent', { type: 'mouseWheel', x: box.x + 8, y: box.cy, deltaX: 0, deltaY: -120 });
        await p.eval(`new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))`);
        scroll.push(Date.now() - scrollAt);
        await sleep(Math.max(0, 1000 - (Date.now() - at)));
      }
    }, async () => {
      for (let i = 0; i < 3; i++) {
        const sent = (await ws.rpc('mock_ui_busy', { phase: 'tick' })).result;
        await p.waitFor(`__mod('lib/store.ts').then(m => m.state.sessions[${JSON.stringify(f.id)}].entries.length >= ${sent.entries})`, 60000);
        await sleep(1000);
      }
    });
    const result = { label, throttle: 4, opened, workload, idle, updates, input: stats(input), scroll: stats(scroll), ticks: stats(ticks) };
    fs.writeFileSync(`${SCRATCH}/${label}-metrics.json`, JSON.stringify(result, null, 2));
    console.log('BUSY_METRICS', JSON.stringify(result));
    check(result.ticks.max < 1000, `${label}: each new entry is visible within a second at 4x CPU`, result.ticks);
    check(result.updates.long.max < 1000, `${label}: periodic entries never freeze the main thread for a second`, result.updates.long);
    check(result.input.p95 < 1000 && result.scroll.p95 < 500, `${label}: composer and scroll remain responsive at 4x CPU`, { input: result.input, scroll: result.scroll });
    const expectedDraft = Array.from({ length: 8 }, (_, i) => `draft${i} `).join('');
    const actualDraft = await p.eval(`document.querySelector('.pane.focused .composer textarea').value`);
    check(actualDraft === expectedDraft, `${label}: every composer insertion survives periodic entries`, { expectedDraft, actualDraft });
    for (const theme of ['light', 'dark']) {
      await p.theme(theme);
      check(await p.eval(`document.documentElement.scrollWidth <= innerWidth + 1`), `${label}-${theme}: no horizontal overflow`);
    }
    check(await p.eval(`document.querySelector('.pane.focused .agents-sum')?.textContent === '3 working'`), `${label}: completed zero-step helpers are not mistaken for queued work`);
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    await p.send('Emulation.setCPUThrottlingRate', { rate: 1 });
    if (!statusChecked) { await helperStatusChecks(p); statusChecked = true; }
    await p.close();
  }
  ws.close();
  finish();
});
