import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
run(async () => {
  await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
  const probe = await wsClient('note draft probe');
  const p = await device(); await p.viewport(1440,900); await signIn(p,'Note drafts');
  for (const [label,w,h,mobile] of [['desktop',1440,900,false],['folded',344,780,true],['fold',841,701,true]]) for (const theme of ['light','dark']) {
    await p.viewport(w,h,mobile,mobile?2:1); await p.theme(theme);
    for (const conflict of [false,true]) {
      const n = (await probe.rpc('save_note',{title:'Base title',content:'Base text'})).result;
      const open = () => p.eval(`__mod('lib/notes.ts').then(m => m.openNote('${n.id}',{newPane:true}))`);
      await open(); await p.waitFor(`document.querySelector('.pane.focused .note-ed .cm-content')`);
      await p.eval(`__mod('lib/store.ts').then(m => { window.realSave = m.api.saveNote; window.realGet = m.api.getNote; m.api.saveNote = async () => {throw Object.assign(new Error('Connection lost'),{code:'transport'});}; })`);
      const mine = `My unsaved ${label} ${theme}`;
      await p.eval(`(() => { const el=document.querySelector('.pane.focused .note-title'); el.value=${JSON.stringify(mine)}; el.dispatchEvent(new Event('input',{bubbles:true})); })()`);
      if (conflict) {
        await probe.rpc('save_note',{id:n.id,title:'Changed elsewhere',content:n.content,base_version:n.version});
        await p.waitFor(`document.querySelector('.pane.focused .note-bar.warn')`);
      }
      await p.eval(`__mod('lib/workspace.ts').then(m => m.closePane(m.workspace.focused()))`);
      await sleep(80);
      if (!conflict) await p.eval(`__mod('lib/store.ts').then(m => m.api.getNote = async () => {throw Object.assign(new Error('Connection lost'),{code:'transport'});})`);
      await open(); await p.waitFor(`document.querySelector('.pane.focused .note-ed .cm-content')`);
      await sleep(350);
      const fit = await p.eval(`(() => {const b=document.querySelector('.pane.focused .note-bar[role=status] button');const r=b.getBoundingClientRect();const pane=b.closest('.pane').getBoundingClientRect();return {fits:r.left>=0&&r.right<=innerWidth+1,left:r.left,right:r.right,width:innerWidth,pane:{left:pane.left,width:pane.width}};})()`);
      check(fit.fits,`${label}-${theme}: recovered draft's Discard action fits on screen`,fit);
      check(await p.eval(`document.querySelector('.pane.focused .note-title').value === ${JSON.stringify(mine)}`),`${label}-${theme}: closing ${conflict?'conflicted':'offline'} note retains the draft`);
      check(await p.eval(`document.querySelector('.pane.focused .note-bar[role=status]')?.textContent.includes('Recovered unsaved')`),`${label}-${theme}: reopening offers the retained draft`);
      await p.shot(`${SHOTS}/note-draft-${label}-${theme}-${conflict?'conflict':'offline'}.png`);
      if (conflict) {
        await p.eval(`document.querySelector('.pane.focused .note-bar[role=status] button').click()`);
        check(await p.eval(`document.querySelector('.pane.focused .note-title').value === 'Changed elsewhere'`),`${label}-${theme}: explicit discard takes the server version`);
      } else {
        await p.eval(`__mod('lib/store.ts').then(m => { m.api.saveNote = realSave; m.api.getNote = realGet; return m.bootstrap(); })`);
        await p.waitFor(`document.querySelector('.pane.focused .note-status')?.dataset.state === 'saved'`);
        check((await probe.rpc('get_note',{id:n.id})).result.title === mine,`${label}-${theme}: retained offline draft saves after reconnect`);
      }
      await p.eval(`__mod('lib/store.ts').then(m => {m.api.saveNote = realSave; m.api.getNote = realGet;})`);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.closePane(m.workspace.focused()))`);
      const kept = await p.eval(`Promise.all([__mod('lib/noteDrafts.ts'),__mod('lib/workspace.ts')]).then(([d,w]) => !!d.noteDraft(w.workspaceServer(),'${n.id}'))`);
      check(!kept,`${label}-${theme}: ${conflict?'discarded':'saved'} draft is removed from the cache`);
    }
  }
  const isolated = await p.eval(`__mod('lib/noteDrafts.ts').then(d => { const base={id:'same-note',title:'Base',content:'',version:1}; d.keepNoteDraft('server-a',{base,title:'Mine',content:''}); const isolated=!d.noteDraft('server-b','same-note'); d.noteDraftSaved('server-a',{...base,version:2,title:'Earlier'},{title:'Earlier',content:''}); const retained=d.noteDraft('server-a','same-note').title==='Mine'; d.discardNoteDraft('server-a','same-note'); return {isolated,retained}; })`);
  check(isolated.isolated,'note drafts are keyed by server as well as note');
  check(isolated.retained,'a late save answer never discards newer edits');
  probe.close(); await p.close(); finish();
});
