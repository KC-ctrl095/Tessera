// Real anchored options dialog: no startup fetch, bounded visible loading,
// local recovery, and no stale reopening after cancellation or pane teardown.
import { check, device, finish, mock, run, signIn, sleep } from './cdp.mjs';
import { settled } from './settled.mjs';
const until = async (fn, label) => {
  for (let i = 0; i < 125; i++) { const value = await fn(); if (value) return value; await sleep(80); }
  throw Error(`Timed out: ${label}`);
};
run(async () => {
  for (const scenario of ['delayed', 'cancel', 'unmount', 'failure', 'escape', 'permission']) {
    await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
    const p = await device(); await p.viewport(344, 780, true, 2);
    const paused = [], requests = [];
    p.on('Fetch.requestPaused', e => paused.push(e));
    p.on('Network.requestWillBeSent', e => requests.push(e.request.url));
    await p.send('Network.enable'); await p.send('Network.setCacheDisabled', { cacheDisabled: true });
    await p.send('Fetch.enable', { patterns: [{ urlPattern: '*/src/components/chat/TileOptions.tsx*', requestStage: 'Request' }] });
    await signIn(p, `Options ${scenario}`);
    await p.waitFor(`document.querySelector('.pane.focused .title-menu') && document.querySelector('.pane.focused .composer textarea')`);
    check(!requests.some(url => /\/TileOptions\.tsx/.test(url)), `${scenario}: cold chat does not fetch options controls`);
    await p.click('.pane.focused .composer textarea'); await p.type('Keep this options draft');
    await p.eval(`window.__optionsKeep={composer:document.querySelector('.pane.focused .composer textarea'),page:crypto.randomUUID()};window.__optionsPage=window.__optionsKeep.page;`);
    const intact = () => p.eval(`window.__optionsKeep.composer === document.querySelector('.pane.focused .composer textarea') && window.__optionsKeep.composer.value === 'Keep this options draft' && window.__optionsPage === window.__optionsKeep.page`);
    if (scenario === 'permission') await p.eval(`Promise.all([__mod('lib/bus.ts'),__mod('lib/workspace.ts')]).then(([b,w])=>b.paneAction(w.workspace.focused(),'permission'))`);
    else await p.click('.pane.focused .title-menu');
    await until(() => paused[0], 'options chunk');
    await p.waitFor(`document.querySelector('.pop-options [aria-busy="true"]')`);
    check(await intact(), `${scenario}: loading keeps the composer and draft mounted`);
    check(await p.eval(`!![...document.querySelectorAll('.pop-options button')].find(b=>b.textContent==='Cancel') && document.querySelector('.pop-options').textContent.includes('Loading chat options')`), `${scenario}: loading is visible and cancellable`);
    await settled(p, `document.querySelector('.pop-options')`);
    const before = await p.eval(`(() => { const r=document.querySelector('.pop-options').getBoundingClientRect(); return {x:r.x,y:r.y,w:r.width,h:r.height,vh:innerHeight}; })()`);
    check(before.x >= 0 && before.x + before.w <= 345 && before.y >= 0 && before.y + before.h <= before.vh + 1 && before.h >= 300, `${scenario}: 344px placeholder reserves bounded sheet height`, before);
    check(await p.eval(`document.querySelector('.pop-options').contains(document.activeElement) && document.activeElement.textContent === 'Cancel'`), `${scenario}: loading focus enters Cancel`);
    if (scenario === 'failure') {
      await p.send('Fetch.failRequest', { requestId: paused[0].requestId, errorReason: 'Failed' });
      await p.waitFor(`document.querySelector('.pop-options .part-retry')`);
      check(await intact() && await p.eval(`!!document.querySelector('.pop-options .part-reload')`), 'failure: actionable local error retains the draft');
      await p.clickText('.pop-options button', 'Cancel');
      await p.click('.pane.focused .title-menu'); await sleep(150);
      check(paused.length === 1 && await p.eval(`!!document.querySelector('.pop-options .part-retry')`), 'failure: reopening does not silently retry');
      await p.click('.pop-options .part-retry'); await until(() => paused[1], 'fresh options module');
      check(paused[1].request.url.includes('tessera_retry='), 'failure: explicit Retry uses a fresh recovery URL');
      await p.send('Fetch.continueRequest', { requestId: paused[1].requestId });
    } else {
      if (scenario === 'escape') {
        await p.key('Escape', 'Escape', 0, 27);
        check(!await p.eval(`!!document.querySelector('.pop-options')`), 'Escape closes while import is pending');
        check(await p.eval(`document.activeElement === document.querySelector('.pane.focused .title-menu')`), 'Escape restores anchor focus while loading');
      }
      if (scenario === 'cancel') await p.clickText('.pop-options button', 'Cancel');
      if (scenario === 'unmount') await p.eval(`__mod('lib/workspace.ts').then(m=>m.closePane(m.workspace.focused()))`);
      await p.send('Fetch.continueRequest', { requestId: paused[0].requestId });
    }
    if (scenario === 'delayed' || scenario === 'failure' || scenario === 'permission') {
      await p.waitFor(`document.querySelector('.pop-options .opt-model')`);
      await p.waitFor(`document.querySelector('.pop-options .tile-opts').contains(document.activeElement)`);
      await settled(p, `document.querySelector('.pop-options')`);
      const after = await p.eval(`(() => { const r=document.querySelector('.pop-options').getBoundingClientRect(); return {x:r.x,y:r.y,w:r.width,h:r.height,vh:innerHeight}; })()`);
      check(after.x >= 0 && after.x + after.w <= 345 && after.y >= 0 && after.y + after.h <= after.vh + 1 && Math.abs(after.h-before.h) <= 180, `${scenario}: loaded 344px sheet stays bounded and close to its placeholder`, {before,after});
      check(await p.eval(`document.querySelector('.pop-options .tile-opts').contains(document.activeElement)`), `${scenario}: module arrival moves focus into loaded controls`);
      if (scenario === 'permission') check(await p.eval(`document.querySelector('.pop-options [data-focus]').contains(document.activeElement) && document.activeElement.matches('[role="radio"]')`), 'permission focus prop selects its permission controls');
      check(await intact() && await p.eval(`!document.querySelector('.pop-options .part-error')`), `${scenario}: arrival replaces only the loading/error host`);
      await p.click('.pop-options .opt-model'); await p.waitFor(`document.querySelector('.pop-model .model-menu')`);
      check(await intact(), `${scenario}: original Model action and close callback still work`);
    } else {
      await sleep(250);
      check(!await p.eval(`!!document.querySelector('.pop-options')`), `${scenario}: module arrival never reopens a closed options dialog`);
      if (scenario === 'cancel') check(await intact(), 'cancel: draft survives cancellation and later module arrival');
    }
    check(!p.errors.length, `${scenario}: no uncaught options loader errors`, p.errors);
    await p.send('Fetch.disable'); await p.close();
  }
  finish();
});
