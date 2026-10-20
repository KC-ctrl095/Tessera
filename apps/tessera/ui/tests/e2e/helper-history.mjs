// Completed helpers must not scan their parent's whole history on every update.
// The running-only phone-lag fixture does not enter this path.
import fs from 'node:fs';
import { SCRATCH, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';

run(async () => {
  await mock('/__mock/reset');
  const p = await device();
  await p.viewport(344, 780, true, 2);
  await signIn(p, 'Synthetic completed helpers');
  const ws = await wsClient();
  const seeded = (await ws.rpc('mock_phone_lag', { calls: 3000, helpers: 120, completed_helpers: true })).result;
  const id = seeded.parents[0].id;
  await p.eval(`__mod('lib/store.ts').then(async m => {
    await m.ensureLoaded(${JSON.stringify(id)});
    const parent = m.state.sessions[${JSON.stringify(id)}];
    window.historyFixture = { id: parent.info.id, model: parent.info.model, seq: parent.info.last_seq };
  })`);
  await p.send('Emulation.setCPUThrottlingRate', { rate: 4 });
  const opened = await p.eval(`__mod('lib/workspace.ts').then(m => { const t = performance.now(); m.openSession(${JSON.stringify(id)}, {pane: m.workspace.focused()}); return performance.now() - t; })`);
  await p.waitFor(`document.querySelector('.pane.focused .transcript .item')`, 60000);
  // A failed/delayed lazy import must not turn an absent helper bar into a false green.
  const helpersReady = await p.eval(`__mod('components/chat/SubagentGate.tsx').then(async m => !!(await m.subagentParts.load()) && m.childSessions(${JSON.stringify(id)}).length === 120)`);
  check(helpersReady, 'helper components loaded and all completed children are present');
  if (!helpersReady) throw new Error('The completed-helper workload did not mount');
  await sleep(100);
  const samples = [];
  for (let i = 0; i < 3; i++) samples.push(await p.eval(`__mod('lib/store.ts').then(m => {
    const f = window.historyFixture; const t = performance.now();
    m.applyEvent({type:'entry_added',session_id:f.id,entry:{kind:'assistant',seq:++f.seq,ts:1700010000000+f.seq,model:f.model,usage:{input:1,output:1,cache_read:0,cache_write:0},stop:'tool_use',content:[{type:'tool_call',id:'new-history-'+f.seq,name:'read',arguments:{path:'/synthetic/updated'}}]}});
    return performance.now()-t;
  })`));
  await p.send('Emulation.setCPUThrottlingRate', { rate: 1 });
  const metrics = { entries: seeded.entries, completedHelpers: 120, throttle: 4, opened, samples };
  console.log('HISTORY_METRICS', JSON.stringify(metrics));
  fs.mkdirSync(SCRATCH, {recursive:true});
  fs.writeFileSync(`${SCRATCH}/helper-history.json`, JSON.stringify(metrics, null, 2));
  check(!await p.eval(`!!document.querySelector('.pane.focused .agents-bar')`), 'completed helpers do not appear as waiting');
  check(Math.max(...samples) < 1000, 'new entries do not repeatedly scan every completed helper history', samples);
  await p.click('.pane.focused .composer textarea');
  await p.type('The saved chat remains usable');
  check(await p.eval(`document.querySelector('.pane.focused .composer textarea').value.includes('remains usable')`), 'typing works with a long history and completed helpers');
  for (const [label, width, height, mobile] of [['folded',344,780,true], ['fold',841,701,true], ['desktop',1440,900,false]]) for (const theme of ['light','dark']) {
    await p.viewport(width, height, mobile, mobile ? 2 : 1);
    await p.theme(theme);
    await sleep(100);
    check(await p.eval(`(() => { const el = document.querySelector('.pane.focused .composer textarea'); return !!el && el.value.includes('remains usable') && document.documentElement.scrollWidth <= innerWidth + 1; })()`), `${label}-${theme}: restored chat stays usable without horizontal overflow`);
  }
  check(await p.eval(`__mod('components/chat/SubagentGate.tsx').then(m => m.childSessions(${JSON.stringify(id)}).length === 120)`), 'all completed helpers remain in the workload');
  await p.reload();
  await p.waitFor(`document.querySelector('.pane.focused .transcript .item')`, 60000);
  const restored = await p.eval(`__mod('components/chat/SubagentGate.tsx').then(async m => !!(await m.subagentParts.load()) && m.childSessions(${JSON.stringify(id)}).length === 120)`);
  check(restored && !await p.eval(`!!document.querySelector('.pane.focused .agents-bar')`), 'cold reload restores the same completed helpers without making them wait');
  ws.close();
  await p.close();
  finish();
});
