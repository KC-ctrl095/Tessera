// Older server payloads, no helper_owner/capabilities. Synthetic only.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from './cdp.mjs';
const open = (p, id) => p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${id}', { pane: m.workspace.focused() }))`);
const cards = p => p.eval(`[...document.querySelectorAll('.pane.focused .subagent')].map(c => ({ name: c.querySelector('.subagent-name').textContent.replace(/^↳\\s*/, '').trim(), status: c.dataset.status, activity: c.querySelector('.subagent-activity').textContent, controls: [...c.querySelectorAll('button')].map(b => b.getAttribute('aria-label') || b.textContent.trim()) }))`);
const expand = async p => {
  await p.eval(`[...document.querySelectorAll('.pane.focused .steps-head[aria-expanded="false"]')].forEach(b => b.click())`);
  await sleep(250);
};
const perCall = (p, f) => p.eval(`Promise.all([__mod('components/chat/SubagentGate.tsx'), __mod('lib/store.ts')]).then(([g,m]) => {
  const s = m.state.sessions['${f.id}'];
  const calls = s.entries.flatMap(e => e.kind === 'assistant' ? e.content.filter(c => c.type === 'tool_call') : []);
  const tools = [...document.querySelectorAll('.pane.focused .tool')];
  return calls.map((c,i) => ({ id:c.id, refs:g.refsForCall(s, {id:c.id,name:c.name,args:c.arguments,status:'running',result:s.results[c.id]}).map(r => r.sessionId), cards:[...(tools[i]?.querySelectorAll('.subagent') ?? [])].map(card => ({name:card.querySelector('.subagent-name').textContent.replace(/^↳\\s*/, '').trim(), controls:[...card.querySelectorAll('button')].map(b => b.getAttribute('aria-label') || b.textContent.trim())})) }));
})`);
const checkCalls = async (p, f, expected, label) => {
  const rows = await perCall(p, f);
  for (let i = 0; i < f.calls.length; i++) {
    const row = rows[i];
    check(row?.id === f.calls[i] && JSON.stringify(row.refs) === JSON.stringify(expected[i]) && row.cards.length === expected[i].length && row.cards.every(c => c.controls.includes('Open') && c.controls.some(t => t.startsWith('Stop'))), `${label}: ${f.calls[i]} has only its own refs/cards/controls`, row);
  }
};
run(async () => {
  await mock('/__mock/reset');
  await mock('/__mock/claude-code?state=ready');
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, 'Helper display');
  for (const mode of ['native-ambiguous', 'native-overlap']) {
    const f = await mock(`/__mock/helper-display?mode=${mode}`);
    await open(p, f.id);
    await p.waitFor(`document.querySelector('.pane.focused .agents-bar')`, 8000);
    await expand(p);
    await p.waitFor(`document.querySelectorAll('.pane.focused .tool').length === 2`, 8000);
    const expected = mode === 'native-ambiguous' ? [[], []] : [[f.children[0]], []];
    await checkCalls(p, f, expected, mode);
    if (mode === 'native-ambiguous') check(await p.eval(`__mod('lib/store.ts').then(m => { const entries = m.state.sessions['${f.id}'].entries.filter(e => e.kind === 'assistant'); return entries.length === 1 && entries[0].content.filter(c => c.type === 'tool_call').length === 2; })`), 'duplicate-name fixture really has both calls in the same assistant message');
    await p.click('.pane.focused .agents-head');
    await p.waitFor(`document.querySelectorAll('.pane.focused .helper-row').length === ${f.children.length}`, 8000);
    check(await p.eval(`document.querySelectorAll('.pane.focused .helper-row .bot-row-open:not(:disabled)').length === ${f.children.length} && document.querySelectorAll('.pane.focused .helper-row [aria-label^="Stop"]').length === ${f.children.length}`), `${mode}: ambiguous or not, real children stay visible/controllable in the top bar`);
    await p.eval(`window.displayCards = [...document.querySelectorAll('.pane.focused .subagent')]; window.displayRows = [...document.querySelectorAll('.pane.focused .helper-row')];`);
    for (let i = 0; i < 10; i++) await mock('/__mock/helper-display?mode=pulse');
    await sleep(600);
    await checkCalls(p, f, expected, `${mode} after text deltas`);
    check(await p.eval(`window.displayCards.every(c => c.isConnected) && window.displayRows.every(c => c.isConnected)`), `${mode}: streamed text keeps existing cards and top-bar rows`);
    await p.shot(`${SHOTS}/helper-display-${mode}.png`);
    if (mode === 'native-ambiguous') {
      await mock('/__mock/helper-display?mode=associate');
      await p.waitFor(`document.querySelectorAll('.pane.focused .subagent').length === 2`, 8000);
      await checkCalls(p, f, [[f.children[0]], [f.children[1]]], 'explicit result metadata resolves identical names');
    }
    await mock('/__mock/helper-display?mode=finish');
    await p.waitFor(`!document.querySelector('.pane.focused .agents-bar')`, 8000);
  }
  for (const mode of ['native', 'bridge', 'cli']) {
    const f = await mock(`/__mock/helper-display?mode=${mode}`);
    await open(p, f.id);
    await p.waitFor(`document.querySelector('.pane.focused .agents-bar')`, 8000);
    await expand(p);
    await sleep(800);
    let list = await cards(p);
    check(list.length === (mode === 'cli' ? 3 : 2), `${mode}: exactly one card per helper (legacy metadata)`, list);
    check(new Set(list.map(c => c.name)).size === list.length, `${mode}: no duplicated names`, list);
    if (mode === 'native') check(await p.eval(`Promise.all([__mod('components/chat/SubagentGate.tsx'), __mod('lib/store.ts')]).then(([g,s]) => g.refsForCall(s.state.sessions['${f.id}'], {id:'partial-cli-call',name:'Agent',args:'{"description":',status:'pending'}).length === 0)`), 'partial Claude CLI call never guesses unrelated native helpers');
    if (mode === 'native') await checkCalls(p, f, [[f.children[0]], [f.children[1]]], 'distinct legacy native calls');
    if (mode === 'cli') {
      const bg = list.find(c => c.name === 'Older background helper');
      const approval = list.find(c => c.name === 'Helper awaiting approval');
      check(bg?.status === 'unknown' && /unknown/i.test(bg.activity) && !bg.controls.length, 'old call-only background result is not fake Done, has no unsupported controls', bg);
      check(approval?.status === 'waiting' && /approval/i.test(approval.activity), 'approval is not presented as a helper already working', approval);
    } else {
      check(list.filter(c => c.status === 'waiting').length === 1 && list.filter(c => c.status === 'running').length === 1, `${mode}: working/queued status correct`, list);
      check(list.every(c => c.controls.includes('Open') && c.controls.some(t => t.startsWith('Stop'))), `${mode}: both running and queued helpers have real Open/Stop`, list);
    }
    await p.click('.pane.focused .agents-head');
    await p.waitFor(`document.querySelectorAll('.pane.focused .agent-row').length >= 1`);
    await p.eval(`window.displayCards = [...document.querySelectorAll('.pane.focused .subagent')]; window.displayRows = [...document.querySelectorAll('.pane.focused .agent-row')];`);
    for (let i = 0; i < 10; i++) await mock('/__mock/helper-display?mode=pulse');
    await sleep(600);
    check(await p.eval(`window.displayCards.every(c => c.isConnected) && window.displayRows.every(c => c.isConnected)`), `${mode}: text deltas do not remount all helper cards/rows`);
    // Reload really goes back to the mock snapshot, not retained client state.
    await p.conn.send('Page.reload', {}, p.sid);
    await p.waitFor(`document.querySelector('.pane.focused .agents-bar')`, 10000);
    await p.waitFor(`document.querySelector('.pane.focused .transcript .tool, .pane.focused .transcript .steps')`, 10000);
    await expand(p);
    await p.waitFor(`document.querySelectorAll('.pane.focused .subagent').length === ${mode === 'cli' ? 3 : 2}`, 8000);
    list = await cards(p);
    check(list.length === (mode === 'cli' ? 3 : 2), `${mode}: same helper cards after reload`, list);
    if (mode !== 'native') {
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${f.children[0]}', { newPane: true }))`);
      await p.waitFor(`document.querySelector('.pane.focused .pane-head')?.textContent.includes('${mode === 'bridge' ? 'GPT running helper' : 'CLI native helper'}')`, 8000);
      await p.waitFor(`document.querySelector('.pane.focused .dm-note')`, 8000);
      check(!await p.eval(`!!document.querySelector('.pane.focused .composer textarea')`), `${mode}: child tile is watch-only`);
      await p.shot(`${SHOTS}/helper-display-${mode}-watch.png`);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.closePane(m.workspace.focused()))`);
    }
    await open(p, f.id);
    for (const [label, w, h, mobile] of [['desktop',1440,900,false], ['folded',344,780,true], ['fold',841,701,true]]) for (const theme of ['dark','light']) {
      await p.viewport(w,h,mobile,mobile ? 2 : 1);
      await p.theme(theme);
      await expand(p);
      await p.eval(`document.querySelector('.pane.focused .agents-head[aria-expanded="false"]')?.click()`);
      await sleep(300);
      const fit = await p.eval(`(() => { const pane = document.querySelector('.pane.focused'); return { page: document.documentElement.scrollWidth <= innerWidth + 1, cards: [...pane.querySelectorAll('.subagent')].every(c => c.scrollWidth <= c.clientWidth + 1), bar: pane.querySelector('.agents-bar').scrollWidth <= pane.querySelector('.agents-bar').clientWidth + 1 }; })()`);
      check(fit.page && fit.cards && fit.bar, `${mode}-${label}-${theme}: helpers fit without horizontal overflow`, fit);
      await p.shot(`${SHOTS}/helper-display-${mode}-${label}-${theme}.png`);
    }
    if (mode === 'cli') {
      await p.eval(`__mod('lib/store.ts').then(m => { window.realDisplayAbort = m.api.abort; window.displayAbortIds = []; m.api.abort = async id => { window.displayAbortIds.push(id); throw new Error('Synthetic child stop unavailable'); }; })`);
      await p.eval(`document.querySelector('.pane.focused .helper-row [aria-label^="Stop"]')?.click()`);
      await sleep(300);
      check(await p.eval(`window.displayAbortIds.length === 1 && window.displayAbortIds[0] === '${f.children[0]}' && !document.querySelector('.modal-backdrop')`), 'failed child Stop never offers or sends a parent Stop');
      await p.eval(`__mod('lib/store.ts').then(m => m.api.abort = window.realDisplayAbort)`);
    }
    await mock('/__mock/helper-display?mode=finish');
    await p.waitFor(`!document.querySelector('.pane.focused .agents-bar')`, 8000);
    await expand(p);
    check((await cards(p)).filter(c => /native helper|GPT|First native|Second native/.test(c.name)).every(c => c.status === 'error' && /Stopped/.test(c.activity)), `${mode}: stopped children keep honest cards, strip disappears`);
    await p.viewport(1440,900);
  }
  check(await p.eval(`__mod('components/chat/SubagentGate.tsx').then(m => m.detailsChildren({subagents: [{name:'One',session_id:'legacy-child',background:true},{name:'One',session_id:'legacy-child',background:true}]}).length === 1)`), 'duplicate legacy result references collapse to one child');
  check(await p.eval(`__mod('components/chat/SubagentGate.tsx').then(m => m.isHelperStart('Agent', {tasks:[{name:'Legacy native'}]}) && !m.isHelperStart('agents', {action:'apply'}) && !m.isHelperStart('task', {plan:[]}) )`), 'legacy native Agent stays supported; apply and ledger calls do not duplicate helper cards');
  // Phone controls: close and new together, aligned fast icon and flat options.
  await open(p, 'ses_7d3a91e5c4b2');
  await p.eval(`__mod('lib/store.ts').then(async m => { await m.api.completeOauth('openai-codex', 'fixture'); await m.api.updateSession('ses_7d3a91e5c4b2', { model: { provider: 'openai-codex', model: 'gpt-6' }, fast: true }); })`);
  await p.waitFor(`document.querySelector('.pane.focused .pane-head')?.textContent.includes('healthz')`, 8000);
  for (const theme of ['light','dark']) {
    await p.viewport(344,780,true,2);
    await p.theme(theme);
    await sleep(500);
    check(await p.eval(`(() => { const h = document.querySelector('.pane.focused .pane-head'); return !!h.querySelector('.head-close') && !!h.querySelector('.head-new'); })()`), `folded-${theme}: close X and New together`);
    await p.waitFor(`(() => { const t = document.querySelector('.pane.focused .title-menu'), r = t?.getBoundingClientRect(); return r && r.left >= 0 && r.right <= innerWidth && t.contains(document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2)); })()`, 8000);
    await p.click('.pane.focused .title-menu');
    try { await p.waitFor(`document.querySelector('.tile-opts')`); } catch (e) {
      console.log('options click diagnostic', await p.eval(`({title: document.querySelector('.pane.focused .pane-head')?.textContent, dialogs: [...document.querySelectorAll('[role="dialog"]')].map(d => d.textContent), buttons: [...document.querySelectorAll('.pane.focused .title-menu')].map(b => ({expanded:b.getAttribute('aria-expanded'), box:b.getBoundingClientRect().toJSON()}))})`));
      await p.shot(`${SHOTS}/helper-display-phone-options-failed.png`);
      throw e;
    }
    const opts = await p.eval(`(() => { const o = document.querySelector('.tile-opts'), m = o.querySelector('.opt-model'), bolt = m?.querySelector('.fast-mark'), r = bolt?.getBoundingClientRect(), b = m?.getBoundingClientRect(); return { nested: !!o.querySelector('.opt-more'), helpers: !!o.querySelector('[aria-label="Helpers"]'), fast: !!o.querySelector('[aria-label="Fast mode"]'), aligned: r && r.top >= b.top && r.bottom <= b.bottom && Math.abs((r.top + r.bottom) / 2 - (b.top + b.bottom) / 2) < 3, fit: o.scrollWidth <= o.clientWidth + 1 }; })()`);
    check(!opts.nested && opts.helpers && opts.fast && opts.fit, `folded-${theme}: compact flat options preserve capabilities`, opts);
    check(opts.aligned, `folded-${theme}: fast lightning aligned with model row`, opts);
    await p.shot(`${SHOTS}/helper-display-phone-options-${theme}.png`);
    await p.key('Escape','Escape',0,27);
  }
  check(!p.errors.length, 'no page errors', p.errors);
  await p.close();
  finish();
});
