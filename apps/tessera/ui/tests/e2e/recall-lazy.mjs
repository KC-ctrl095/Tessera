// The real chat can receive its first recall while streaming. Loading or failing
// its module must never suspend the transcript, discard the draft, or hide output.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';

const until = async (fn, label) => {
  for (let i = 0; i < 125; i++) {
    const value = await fn();
    if (value) return value;
    await sleep(80);
  }
  throw new Error(`Timed out: ${label}`);
};
const fixture = async (ws, phase) => {
  const reply = await ws.rpc('mock_recall_lazy', { phase });
  if (reply.error) throw new Error(JSON.stringify(reply.error));
  return reply.result;
};
const open = (p, id) => p.eval(`__mod('lib/workspace.ts').then(m => m.openSession(${JSON.stringify(id)}))`);
const panes = (p) => p.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order())`);

run(async () => {
  for (const failed of [false, true]) {
    const label = failed ? 'failed' : 'delayed';
    await mock('/__mock/reset');
    await mock('/__mock/mosaics/reset');
    const ws = await wsClient(`Recall ${label} fixture`);
    const f = await fixture(ws, 'seed');
    const p = await device();
    await p.viewport(1440, 900);
    await p.send('Network.enable');
    await p.send('Network.setCacheDisabled', { cacheDisabled: true });
    const requests = [];
    const stopNetwork = p.on('Network.requestWillBeSent', e => requests.push(e.request.url));
    const paused = [];
    const stopFetch = p.on('Fetch.requestPaused', e => paused.push(e));
    // Only the module under test is intercepted. The app, WebSocket and styles
    // keep using the real Vite/mock paths; no replacement component is served.
    await p.send('Fetch.enable', { patterns: [{ urlPattern: '*/src/components/chat/RecallCard.tsx*', requestStage: 'Request' }] });
    await signIn(p, `Recall lazy ${label}`);
    await open(p, f.id);
    await p.waitFor(`document.querySelector('.pane.focused .msg-assistant[data-seq="2"]')`);
    check(!requests.some(u => /\/RecallCard\.tsx|\/styles\/recall\.css/.test(u)) && !paused.length,
      `${label}: cold app and ordinary chat request neither RecallCard nor its stylesheet`);

    await p.click('.pane.focused .composer textarea');
    await p.type('Draft survives the recall module');
    await p.eval(`window.__recallKeep = {
      transcript: document.querySelector('.pane.focused .transcript'),
      neighbor: document.querySelector('.pane.focused .msg-assistant[data-seq="2"]'),
      composer: document.querySelector('.pane.focused .composer textarea')
    }`);
    const intact = () => p.eval(`(() => {
      const k = window.__recallKeep;
      return k.transcript === document.querySelector('.pane.focused .transcript') &&
        k.neighbor === document.querySelector('.pane.focused .msg-assistant[data-seq="2"]') &&
        k.neighbor.isConnected && k.neighbor.textContent.includes('neighbor stays mounted') &&
        k.composer === document.querySelector('.pane.focused .composer textarea') &&
        k.composer.value === 'Draft survives the recall module';
    })()`);

    await fixture(ws, 'stream');
    await until(() => paused[0], 'streaming recall requests its module');
    await p.waitFor(`document.querySelector('.pane.focused .tool:not(.recall) .tool-name')?.textContent === 'recall'`);
    check(await intact(), `${label}: first streaming recall keeps neighboring DOM and the typed draft`);
    check(await p.eval(`[...document.querySelectorAll('.pane.focused [role="status"][aria-busy="true"]')].some(e => e.textContent.includes('Loading recall controls'))`), `${label}: the wrapper exposes loading status without hiding generic output`);
    check(paused.length === 1, `${label}: the first recall starts just one module request`);
    if (failed) {
      await p.send('Fetch.failRequest', { requestId: paused[0].requestId, errorReason: 'Failed' });
      await until(() => p.logs.some(l => l.includes("couldn't load a part of the app")), 'caught failed import');
    }
    await fixture(ws, 'call');
    await p.waitFor(`document.querySelector('.pane.focused .tool:not(.recall)[data-status="running"]')`);
    await p.click('.pane.focused .tool:not(.recall) .tool-head');
    await p.waitFor(`document.querySelector('.pane.focused .tool:not(.recall) .tool-body')?.textContent.includes('Synthetic live recall output.')`);
    check(await p.eval(`document.querySelector('.pane.focused .tool:not(.recall) .tool-head').getAttribute('aria-expanded') === 'true' && !!document.querySelector('.pane.focused .tool:not(.recall) [aria-label="Copy input"]')`),
      `${label}: the actual generic fallback expands with live output and copy controls`);
    check(await intact(), `${label}: using fallback controls does not blank or remount the surrounding chat`);

    if (!failed) {
      // Observe the actual card's first insertion, not a stand-alone component.
      await p.eval(`window.__recallFirst = null; const mo = new MutationObserver(() => {
        const e = document.querySelector('.tool.recall .recall-count');
        if (e) { window.__recallFirst = getComputedStyle(e).fontVariantNumeric; mo.disconnect(); }
      }); mo.observe(document.documentElement, {childList:true, subtree:true});`);
      await p.send('Fetch.continueRequest', { requestId: paused[0].requestId });
      await p.waitFor(`document.querySelector('.pane.focused .tool.recall[data-status="running"]')`);
      check(await intact(), 'delayed: module arrival replaces only the fallback, not transcript/neighbor/composer');
      check(await p.eval(`document.querySelector('.pane.focused .tool.recall .tool-head').getAttribute('aria-expanded') === 'true' && !!document.querySelector('.pane.focused .tool.recall .tool-body')`),
        'delayed: expansion made in the loading wrapper survives the chunk arriving');
      await p.eval(`window.__recallCard = document.querySelector('.pane.focused .tool.recall')`);
    }
    await fixture(ws, 'result');
    if (failed) {
      await p.waitFor(`document.querySelector('.pane.focused .tool:not(.recall)[data-status="ok"] .tool-body')?.textContent.includes('Synthetic persisted recall output.')`);
      check(!await p.eval(`!!document.querySelector('.pane.focused .tool.recall')`) && await intact(),
        'failed: completed output remains accessible in the expanded fallback, with surrounding DOM/draft intact');
      check(await p.eval(`!!document.querySelector('.pane.focused .tool:not(.recall) [aria-label="Copy output"]')`), 'failed: the completed fallback still has its output-copy control');
      check(await p.eval(`!!document.querySelector('.part-error[role="alert"] .part-retry') && !!document.querySelector('.part-error .part-reload') && document.querySelector('.part-error').textContent.includes("Couldn't load")`),
        'failed: an actionable error offers local Retry and Reload for stale chunks');
      for (const [size, width, height, mobile] of [['phone',344,780,true], ['fold',768,900,true], ['desktop',1440,900,false]]) {
        for (const theme of ['dark', 'light']) {
          await p.viewport(width, height, mobile, mobile ? 2 : 1); await p.theme(theme); await sleep(120);
          check(await p.eval(`[...document.querySelectorAll('.part-error button')].every(b => {const r=b.getBoundingClientRect();return r.width>=44 && r.height>=44}) && document.documentElement.scrollWidth<=innerWidth+1`), `failed ${size}-${theme}: recovery buttons have 44px bounds without horizontal overflow`);
        }
      }
      await p.viewport(1440,900); await p.theme('dark');
      await p.click('.part-error .part-retry');
      await until(() => paused[1], 'Retry requests a fresh module URL');
      check(paused[1].request.url.includes('tessera_retry='), 'failed: Retry bypasses the cached failed import URL');
      await p.send('Fetch.continueRequest', { requestId: paused[1].requestId });
      await p.waitFor(`document.querySelector('.pane.focused .recall-count')?.textContent === '2 matches'`);
      check(await intact() && await p.eval(`document.querySelector('.pane.focused .tool.recall .tool-head').getAttribute('aria-expanded') === 'true' && document.querySelectorAll('.pane.focused .recall-hit').length === 2 && !document.querySelector('.part-error')`),
        'failed: local Retry recovers without reloading, retaining expansion, neighboring DOM and draft');
    } else {
      await p.waitFor(`document.querySelector('.pane.focused .recall-count')?.textContent === '2 matches'`);
      check(await p.eval(`window.__recallCard === document.querySelector('.pane.focused .tool.recall')`), 'delayed: result updates reactively without remounting the loaded card');
      check(await p.eval(`window.__recallFirst === 'tabular-nums' && getComputedStyle(document.querySelector('.recall-hits')).display === 'flex'`), 'delayed: result count and expanded hit list have recall styles from their first insertion');
      check(await intact(), 'delayed: updated results preserve neighboring DOM and draft');
    }

    // Recovery has already been proved without reload. This separate cold-restore
    // check verifies the server stores the exact call/result, not a client-only fixture.
    stopFetch();
    await p.send('Fetch.disable');
    await sleep(600);
    const stored = (await ws.rpc('get_session', { session_id: f.id })).result;
    check(stored.entries.some(e => e.kind === 'tool_result' && e.call_id === f.call && e.details?.recall?.hits.length === 2), `${label}: recall fixture is persisted in the mock snapshot`);
    await p.reload();
    await p.waitFor(`document.querySelector('.pane.focused .recall-count')?.textContent === '2 matches'`, 15000);
    check(await p.eval(`document.querySelector('.pane.focused .msg-assistant[data-seq="2"]')?.textContent.includes('neighbor stays mounted') && document.querySelector('.pane.focused .composer textarea')?.value === 'Draft survives the recall module'`),
      `${label}: cold reload restores persisted recall, neighbor and draft without snapshot contamination`);

    const before = await panes(p);
    await p.clickText('.pane.focused .recall-src', 'Synthetic recall main');
    await p.waitFor(`document.querySelector('.pane.focused [data-seq="1"].flash')`);
    check(JSON.stringify(await panes(p)) === JSON.stringify(before), `${label}: a same-chat source jumps to its message without opening another tile`);
    await p.click('.pane.focused .tool.recall .tool-head');
    await p.waitFor(`document.querySelectorAll('.pane.focused .recall-hit').length === 2`);
    check(await p.eval(`document.querySelector('.pane.focused .tool.recall .tool-head').getAttribute('aria-expanded') === 'true' && document.querySelector('.recall-hit-snippet').textContent === 'Synthetic same-chat source.'`), `${label}: expansion keeps the existing source formatting and snippets`);
    await p.clickText('.pane.focused .recall-hit', 'Synthetic recall source');
    await p.waitFor(`document.querySelector('.pane.focused [data-seq="1"].flash')?.textContent.includes('Synthetic other-chat source.')`);
    check((await panes(p)).length === before.length + 1, `${label}: an other-chat hit opens a new tile and jumps to the source`);
    await open(p, f.id);
    await p.waitFor(`document.querySelector('.pane.focused .tool.recall')`);

    for (const [size, width, height, mobile] of [['phone',344,780,true], ['fold',768,900,true], ['desktop',1440,900,false]]) {
      for (const theme of ['dark', 'light']) {
        await p.viewport(width, height, mobile, mobile ? 2 : 1);
        await p.theme(theme);
        await sleep(160);
        // Exercise expand/collapse at each size, including real touch on phones.
        const head = await p.box('.pane.focused .tool.recall .tool-head');
        if (mobile) await p.tap(head.cx, head.cy); else await p.click('.pane.focused .tool.recall .tool-head');
        check(await p.eval(`(() => {
          const card = document.querySelector('.pane.focused .tool.recall');
          const head = card?.querySelector('.tool-head');
          const open = head?.getAttribute('aria-expanded') === 'true';
          return !!card && card.getBoundingClientRect().width > 100 &&
            (open ? card.querySelectorAll('.recall-hit').length === 2 : card.querySelectorAll('.recall-src').length === 2) &&
            getComputedStyle(card.querySelector(open ? '.recall-hits' : '.recall-sources')).display === 'flex' &&
            document.querySelector('.pane.focused .composer textarea')?.value === 'Draft survives the recall module' &&
            document.documentElement.scrollWidth <= innerWidth + 1;
        })()`), `${label} ${size}-${theme}: styled recall controls, retained draft and no horizontal overflow`);
        if (!failed) await p.shot(`${SHOTS}/recall-lazy-${size}-${theme}.png`);
      }
    }
    check(!p.errors.length, `${label}: no uncaught errors (failed import is handled)`, p.errors);
    stopNetwork();
    ws.close();
    await p.close();
  }
  finish();
});
