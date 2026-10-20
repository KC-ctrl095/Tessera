// Real App/rewind entry: delayed or failed code must leave a cancellable host,
// and neither retry nor loading may perform a destructive rewind.
import { check, device, finish, mock, run, signIn, sleep } from './cdp.mjs';

const SID = 'ses_7d3a91e5c4b2';
const until = async (fn, label) => {
  for (let i = 0; i < 125; i++) { const result = await fn(); if (result) return result; await sleep(80); }
  throw new Error(`Timed out: ${label}`);
};
run(async () => {
  for (const failed of [false, true]) {
    const label = failed ? 'failed' : 'delayed';
    await mock('/__mock/reset');
    await mock('/__mock/mosaics/reset');
    const p = await device();
    await p.viewport(1440, 900);
    await p.send('Network.enable');
    await p.send('Network.setCacheDisabled', { cacheDisabled: true });
    const requests = [], paused = [];
    const stopNetwork = p.on('Network.requestWillBeSent', e => requests.push(e.request.url));
    const stopFetch = p.on('Fetch.requestPaused', e => paused.push(e));
    await p.send('Fetch.enable', { patterns: [{ urlPattern: '*/src/components/chat/Rewind.tsx*', requestStage: 'Request' }] });
    await signIn(p, `Rewind chunk ${label}`);
    await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${SID}'))`);
    await p.waitFor(`document.querySelector('.pane.focused .msg-user')`);
    await p.eval(`__mod('lib/store.ts').then(st => {
      const real = st.api.rewind;
      window.__rewindCalls = { dry: 0, writes: 0 };
      st.api.rewind = (...args) => { window.__rewindCalls[args[2]?.dryRun ? 'dry' : 'writes']++; return real(...args); };
      window.__rewindKeep = { transcript: document.querySelector('.pane.focused .transcript'), composer: document.querySelector('.pane.focused .composer textarea') };
    })`);
    await p.click('.pane.focused .composer textarea');
    await p.type('Keep this draft while the rewind chunk loads');
    const intact = () => p.eval(`window.__rewindKeep.transcript === document.querySelector('.pane.focused .transcript') && window.__rewindKeep.composer === document.querySelector('.pane.focused .composer textarea') && window.__rewindKeep.composer.value === 'Keep this draft while the rewind chunk loads'`);
    const open = () => p.eval(`(async () => { const w = await __mod('lib/workspace.ts'); const st = await __mod('lib/store.ts'); const seq = st.state.sessions['${SID}'].entries.find(e => e.kind === 'user')?.seq; if (!seq) throw Error('fixture user message missing'); (await __mod('lib/rewind.ts')).openRewind('${SID}', seq, w.workspace.focused()); })()`);
    const calls = () => p.eval(`window.__rewindCalls`);
    check(!requests.some(url => /\/Rewind\.tsx|\/rewindAction\.ts/.test(url)), `${label}: ordinary chat does not load confirmation/action code`);
    await open();
    await until(() => paused[0], 'rewind import paused');
    await p.waitFor(`document.querySelector('.rewind-chunk [aria-busy="true"]')`);
    check(await intact() && (await calls()).writes === 0 && (await calls()).dry === 0, `${label}: loading host leaves chat/draft intact and sends no rewinds`);
    check(await p.eval(`!![...document.querySelectorAll('.rewind-chunk button')].find(b => b.textContent === 'Cancel') && !document.querySelector('.rewind-go')`), `${label}: loading has Cancel and cannot confirm`);
    if (failed) {
      await p.send('Fetch.failRequest', { requestId: paused[0].requestId, errorReason: 'Failed' });
      await p.waitFor(`document.querySelector('.rewind-chunk .part-error[role="alert"]')`);
      check(await p.eval(`!!document.querySelector('.rewind-chunk .part-retry') && !!document.querySelector('.rewind-chunk .part-reload') && [...document.querySelectorAll('.rewind-chunk button')].some(b => b.textContent === 'Cancel')`) && (await calls()).writes === 0, 'failed: an error offers Cancel, local Retry and Reload, with zero non-dry-run calls');
      await p.clickText('.rewind-chunk button', 'Cancel');
      await p.waitFor(`!document.querySelector('.rewind-dialog')`);
      check(await p.eval(`__mod('lib/ui.ts').then(m => m.rewindDialog() === null)`) && (await calls()).writes === 0 && await intact(), 'failed: Cancel clears the pending spec/shortcut blocker and preserves the chat');
      await open();
      await p.waitFor(`document.querySelector('.rewind-chunk .part-error')`);
      await p.click('.rewind-chunk .part-retry');
      await until(() => paused[1], 'fresh rewind retry URL');
      check(paused[1].request.url.includes('tessera_retry=') && (await calls()).writes === 0, 'failed: Retry makes a fresh request but performs no rewind');
      await p.send('Fetch.continueRequest', { requestId: paused[1].requestId });
    } else {
      await p.clickText('.rewind-chunk button', 'Cancel');
      await p.waitFor(`!document.querySelector('.rewind-dialog')`);
      await p.send('Fetch.continueRequest', { requestId: paused[0].requestId });
      await until(() => requests.some(url => /\/rewindAction\.ts/.test(url)), 'confirmation dependencies loaded after chunk release');
      await sleep(500);
      check(!await p.eval(`!!document.querySelector('.rewind-dialog')`) && await intact() && (await calls()).writes === 0 && (await calls()).dry === 0, 'delayed: Cancel survives the chunk arriving; no dialog resurrected and no dry/non-dry rewinds');
      await open();
    }
    await p.waitFor(`document.querySelector('.rewind-dialog .rewind-preview')`, 10000);
    check((await calls()).dry > 0 && (await calls()).writes === 0 && await intact(), `${label}: loaded/retried chunk shows only a dry-run preview and retains the original chat/draft`);
    check(!await p.eval(`!!document.querySelector('.part-error')`) && await p.eval(`!document.querySelector('.rewind-go').disabled`), `${label}: recovery needs no page reload and explicit confirmation is available`);
    await p.click('.rewind-go');
    await p.waitFor(`!document.querySelector('.rewind-dialog')`, 10000);
    check((await calls()).writes === 1, `${label}: exactly one non-dry-run rewind, only after explicit confirm`, await calls());
    check(!p.errors.length, `${label}: no uncaught chunk errors`, p.errors);
    stopFetch(); stopNetwork();
    await p.send('Fetch.disable');
    await p.close();
  }
  finish();
});
