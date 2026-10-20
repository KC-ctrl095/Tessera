import { check, device, finish, mock, run, signIn, sleep } from './cdp.mjs';
run(async () => {
  await mock('/__mock/reset');
  await mock('/__mock/mosaics/reset');
  const p = await device();
  await p.viewport(1440,900);
  await signIn(p, 'Late missing chat');
  const result = await p.eval(`Promise.all([__mod('lib/store.ts'), __mod('lib/workspace.ts')]).then(async ([s,w]) => {
    const original = s.api.getSession;
    let reject;
    s.api.getSession = id => id === 'missing-late' ? new Promise((_,no) => reject = no) : original(id);
    const pane = w.openSession('missing-late', {pane:w.workspace.focused()});
    await new Promise(r => setTimeout(r,100));
    w.openSession('ses_7d3a91e5c4b2', {pane});
    reject({code:'not_found'});
    await new Promise(r => setTimeout(r,100));
    s.api.getSession = original;
    return w.workspace.panes[pane].content.sessionId;
  })`);
  check(result === 'ses_7d3a91e5c4b2', 'late missing chat response does not clear a newer selected chat', result);
  await p.close();
  finish();
});
