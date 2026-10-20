// Cold note search: loading rows keep their geometry; a delayed/failed real module
// recovers locally, after which keyboard activation and the existing note menu work.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
import { settled } from './settled.mjs';
const until = async (fn, label) => { for (let i=0;i<100;i++) { const v=await fn(); if(v)return v; await sleep(80); } throw Error(`Timed out: ${label}`); };
run(async () => {
  for (const [shape,w,h] of [['desktop',1440,900],['phone',344,882],['fold',884,1104]]) for (const theme of ['light','dark']) for (const density of ['comfortable','compact']) for (const failed of [false,true]) {
    const tag=`${shape} ${theme} ${density} ${failed?'failed':'delayed'}`, sb=shape==='desktop'?'.sidebar:not(.drawer)':'.drawer-layer.open .sidebar';
    await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
    const probe=await wsClient(`Note views ${tag}`);
    const first=(await probe.rpc('save_note',{title:'Lazyrow preview',content:'Readable preview words for a cold note search.'})).result;
    await probe.rpc('save_note',{title:'Lazyrow empty',content:''});
    const p=await device(); await p.viewport(w,h,shape!=='desktop',shape==='desktop'?1:2);
    await p.send('Network.enable'); await p.send('Network.setCacheDisabled',{cacheDisabled:true});
    const paused=[], requests=[];
    const stopFetch=p.on('Fetch.requestPaused',e=>paused.push(e));
    const stopNetwork=p.on('Network.requestWillBeSent',e=>requests.push(e.request.url));
    await p.send('Fetch.enable',{patterns:[{urlPattern:'*/src/components/notes/NotesListViews.tsx*',requestStage:'Request'}]});
    await signIn(p,`Notes views ${tag}`); await p.theme(theme);
    await p.eval(`__mod('lib/prefs.ts').then(m => m.setPrefs('density', ${JSON.stringify(density)}))`);
    await p.waitFor(`document.documentElement.dataset.density === ${JSON.stringify(density)}`);
    if(shape!=='desktop') {
      await p.eval(`__mod('lib/ui.ts').then(m=>m.setDrawerOpen(true))`);
      await p.waitFor(`document.querySelector('.drawer-layer.open .drawer-panel')`);
      await settled(p, `document.querySelector('.drawer-layer.open .drawer-panel')`);
    }
    check(!requests.some(u=>u.includes('/NotesListViews.tsx')),`${tag}: cold ordinary chat has no note-list view request`);
    await p.click(`${sb} .sb-search input`); await p.type('lazyrow');
    await until(()=>paused[0],'note search requests view module');
    await p.waitFor(`document.querySelectorAll('${sb} .nrow-loading').length === 2`);
    const geometry=()=>p.eval(`[...document.querySelectorAll('${sb} .nrow')].map(e=>{ const r=e.getBoundingClientRect(),m=e.querySelector('.nrow-main').getBoundingClientRect(); return {title:e.querySelector('.nrow-title').textContent,w:r.width,h:r.height,top:r.top,mainH:m.height};})`);
    const before=await geometry();
    check(before.length===2&&before.every(r=>r.h>=40&&r.w>120),`${tag}: cold search has normal full-size rows with and without previews`,before);
    await p.shot(`${SHOTS}/notes-list-${shape}-${theme}-${density}-${failed?'failed':'delayed'}-loading.png`);
    if(failed){
      await p.send('Fetch.failRequest',{requestId:paused[0].requestId,errorReason:'Failed'});
      await p.waitFor(`document.querySelector('${sb} .note-list-error .part-retry')`);
      check(await p.eval(`!!document.querySelector('${sb} .note-list-error [role="alert"] .part-reload') && document.querySelector('${sb} .sb-search input').value === 'lazyrow'`),`${tag}: failed import has Retry/Reload and retains the search`);
      const retry=`${sb} .note-list-error .part-retry`;
      await p.eval(`document.querySelector(${JSON.stringify(retry)}).focus()`); await p.key('Enter','Enter',0,13);
      await until(()=>paused[1],'keyboard Retry requests fresh view module');
      check(paused[1].request.url.includes('tessera_retry='),`${tag}: keyboard Retry bypasses the cached failed module`);
      await p.send('Fetch.continueRequest',{requestId:paused[1].requestId});
    }else await p.send('Fetch.continueRequest',{requestId:paused[0].requestId});
    await p.waitFor(`document.querySelectorAll('${sb} .nrow button.nrow-main').length === 2 && !document.querySelector('${sb} .nrow-loading')`);
    const after=await geometry();
    check(before.every(b=>{const a=after.find(a=>a.title===b.title);return a&&Math.abs(a.w-b.w)<1&&Math.abs(a.h-b.h)<1&&Math.abs(a.mainH-b.mainH)<1&&Math.abs(a.top-b.top)<1;}),`${tag}: arrival/retry replaces placeholders without moving or resizing note rows`,{before,after});
    check(!await p.eval(`!!document.querySelector('${sb} .note-list-error')`),`${tag}: local recovery removes the error without reloading`);
    const row=`[...document.querySelectorAll('${sb} .nrow')].find(e=>e.querySelector('.nrow-title').textContent === 'Lazyrow preview')`;
    await p.eval(`(${row}).querySelector('.srow-more').focus()`); await p.key('Enter','Enter',0,13);
    await p.waitFor(`document.querySelector('.menu [role="menuitem"]')`);
    const menu=await p.eval(`[...document.querySelectorAll('.menu [role="menuitem"]')].map(e=>e.textContent.trim())`);
    check(menu.some(s=>s.includes('Rename'))&&menu.some(s=>s.includes('Delete'))&&menu.some(s=>s.includes('Open beside')),`${tag}: the recovered row's keyboard menu keeps the existing actions`,menu);
    await p.key('Escape','Escape',0,27);
    await p.eval(`(${row}).querySelector('button.nrow-main').focus()`); await p.key('Enter','Enter',0,13);
    await p.waitFor(`document.querySelector('.pane.focused .note-ed .cm-content')`,10000);
    check(await p.eval(`document.querySelector('.pane.focused .note-title').value === 'Lazyrow preview'`),`${tag}: Enter on the recovered note row opens its real note`);
    check((await probe.rpc('get_note',{id:first.id})).result.content==='Readable preview words for a cold note search.',`${tag}: keyboard/menu use does not mutate the note`);
    stopFetch(); stopNetwork(); await p.send('Fetch.disable'); probe.close(); await p.close();
  }
  finish();
});
