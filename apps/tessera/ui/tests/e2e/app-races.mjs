// Deferred/rejected RPCs must leave the same document and tile usable.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
run(async () => {
  await mock('/__mock/reset');
  await mock('/__mock/mosaics/reset');
  const probe = await wsClient('race probe');
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, 'App races');
  for (const [label, w, h, mobile] of [['desktop',1440,900,false], ['folded',344,780,true], ['fold',841,701,true]]) for (const theme of ['light','dark']) {
    await p.viewport(w,h,mobile,mobile ? 2 : 1);
    await p.theme(theme);
    const n = (await probe.rpc('save_note', {title:`Recovery ${label} ${theme}`, content:'Recovered after a failed first load'})).result;
    await p.eval(`__mod('lib/store.ts').then(s => { const get = s.api.getNote; s.api.getNote = async id => { s.api.getNote = get; throw Object.assign(new Error('Connection lost'),{code:'transport'}); }; })`);
    await p.eval(`__mod('lib/notes.ts').then(m => m.openNote('${n.id}', {newPane:true}))`);
    await p.waitFor(`document.querySelector('.pane.focused .board-loading')?.textContent.includes('Retry')`);
    check(await p.eval(`document.querySelector('.pane.focused .board-loading').textContent.includes("Couldn't open")`), `${label}-${theme}: failed first note load offers Retry`);
    if (theme === 'light') await p.eval(`document.querySelector('.pane.focused .board-loading button').click()`);
    else await p.eval(`__mod('lib/store.ts').then(m => m.bootstrap())`);
    await p.waitFor(`document.querySelector('.pane.focused .note-ed .cm-content')`);
    check(await p.eval(`document.querySelector('.pane.focused .note-title').value === ${JSON.stringify(n.title)}`), `${label}-${theme}: ${theme === 'light' ? 'Retry' : 'reconnect'} reopens a note with no base loaded`);
    await p.shot(`${SHOTS}/app-races-${label}-${theme}.png`);
    await p.eval(`__mod('lib/workspace.ts').then(m => m.closePane(m.workspace.focused()))`);
  }
  probe.close();
  await p.close();
  finish();
});
