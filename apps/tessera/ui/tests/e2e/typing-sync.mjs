import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
run(async () => {
  await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
  const probe = await wsClient('typing sync probe');
  const sessions = (await probe.rpc('list_sessions',{archived:false})).result.filter(s=>s.kind==='chat');
  const a=sessions[0].id, b=sessions[1].id;
  const p=await device(); await p.viewport(1440,900); await signIn(p,'Typing identity');
  for (const [label,w,h,mobile] of [['desktop',1440,900,false],['folded',344,780,true],['fold',841,701,true]]) for (const theme of ['light','dark']) {
    await p.viewport(w,h,mobile,mobile?2:1); await p.theme(theme);
    const pane=await p.eval(`__mod('lib/workspace.ts').then(m=>m.openSession('${a}',{pane:m.workspace.focused()}))`);
    await p.waitFor(`document.querySelector('.pane.focused .composer textarea')`);
    await p.eval(`__mod('lib/mosaicSync.ts').then(m=>m.syncLayoutNow())`); await sleep(100);
    const raw=`Draft for original chat ${label} ${theme}`;
    await p.eval(`Promise.all([__mod('lib/backend.ts'),__mod('lib/workspace.ts')]).then(([{backend},w])=>{
      window.typingCall=backend.call.bind(backend); window.sentTo=[];
      backend.call=req=>{if(req.method==='send')sentTo.push(req.params);return typingCall(req);};
      w.setDraft('${pane}',${JSON.stringify(raw)}); document.querySelector('.pane.focused textarea').focus();
    })`);
    const layout=(await probe.rpc('get_mosaics')).result;
    const remote={...layout,mosaics:layout.mosaics.map(m=>({...m,tiles:m.tiles.map(t=>t.id===pane?{id:pane,session_id:b}:t)}))};
    const saved=await probe.rpc('save_mosaics',{layout:remote});
    if(saved.error) throw new Error(`remote mosaic replacement failed: ${JSON.stringify(saved.error)}`);
    check(saved.result.version===layout.version+1 && saved.result.mosaics.some(m=>m.tiles.some(t=>t.id===pane&&t.session_id===b)),`${label}-${theme}: server accepted the other device's replacement`);
    await sleep(150);
    const protectedPane=await p.eval(`__mod('lib/workspace.ts').then(m=>m.workspace.panes['${pane}'].content.sessionId==='${a}' && m.getDraft('${pane}')===${JSON.stringify(raw)})`);
    check(protectedPane,`${label}-${theme}: shared replacement cannot switch a typing pane's chat`);
    await p.shot(`${SHOTS}/typing-sync-${label}-${theme}.png`);
    await p.eval(`document.querySelector('.pane.focused .composer .send-btn:not(.stop)').click()`);
    await p.waitFor(`sentTo.length===1`);
    check(await p.eval(`sentTo[0].session_id==='${a}' && sentTo[0].content.some(p=>p.text===${JSON.stringify(raw)})`),`${label}-${theme}: draft sends only to the chat where it was typed`);
    await p.eval(`document.activeElement?.blur()`);
    await p.waitFor(`__mod('lib/workspace.ts').then(m=>m.workspace.panes['${pane}'].content.sessionId==='${b}')`);
    check(await p.eval(`__mod('lib/workspace.ts').then(m=>m.getDraft('${pane}')==='')`),`${label}-${theme}: deferred replacement arrives without another chat's draft`);
    const isolated=await p.eval(`__mod('lib/workspace.ts').then(m=>{m.openSession('${a}',{pane:'${pane}'}); m.setDraft('${pane}','Kept for A'); m.openSession('${b}',{pane:'${pane}'}); const empty=m.getDraft('${pane}')===''; m.openSession('${a}',{pane:'${pane}'}); const restored=m.getDraft('${pane}')==='Kept for A'; m.setDraft('${pane}',''); return empty && restored;})`);
    check(isolated,`${label}-${theme}: draft identity also survives deliberate chat switching`);
    await p.eval(`__mod('lib/backend.ts').then(({backend})=>backend.call=typingCall)`);
    await sleep(700);
  }
  const scopes = await p.eval(`__mod('lib/workspace.ts').then(m=>{const pane=m.workspace.focused();const current=m.draftIdentity(pane);const parts=JSON.parse(current);parts[0]='another-server';const other=JSON.stringify(parts);m.setDraft(pane,'Other server draft',other);m.setDraft(pane,'Current server draft');const isolated=m.getDraft(pane)==='Current server draft'&&m.getDraft(pane,other)==='Other server draft';m.closePane(pane);const kept=m.getDraft(pane,other)==='Other server draft'&&m.getDraft(pane,current)==='';m.setDraft(pane,'',other);return {isolated,kept};})`);
  check(scopes.isolated,'chat drafts are isolated by server as well as pane and chat');
  check(scopes.kept,'closing a pane on one server never clears another server\'s draft');
  probe.close(); await p.close(); finish();
});
