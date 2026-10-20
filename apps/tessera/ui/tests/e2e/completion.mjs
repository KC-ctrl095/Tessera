// Completion protocol: both engines, persisted source markers and old-server fallbacks.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from './cdp.mjs';

run(async () => {
  await mock('/__mock/reset');
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, 'Completion protocol');
  const marked = await p.eval(`__mod('lib/store.ts').then(m => m.call({method:'mock_session', params:{title:'Marked protocol', entries:[
    {kind:'user',text:'Check it'}, {kind:'assistant',text:'The answer stands.'},
    {kind:'assistant',text:'Internal acknowledgement',author:'harness:completion'}]}}))`);
  const meaningful = await p.eval(`__mod('lib/store.ts').then(m => m.call({method:'mock_session',params:{title:'Meaningful checkmark',entries:[{kind:'user',text:'Check it'},{kind:'assistant',text:'✓ Tests pass'}]}}))`);
  const legacyLive = await p.eval(`__mod('lib/store.ts').then(m => m.call({method:'mock_session',params:{title:'Legacy live check',entries:[
    {kind:'user',text:'Check it'},
    {kind:'user',text:'[Completion check] Independent work remains',author:'harness'}]}}))`);
  await p.eval(`__mod('lib/store.ts').then(m=>m.call({method:'send_notification',params:{title:'Done ✓',body:'Old queued protocol',delay_secs:7200}}))`);
  for (const [name,w,h] of [['desktop',1440,900],['phone',344,740],['Fold',690,829]]) {
    await p.viewport(w,h,name !== 'desktop',name === 'desktop' ? 1 : 2);
    for (const theme of ['dark','light']) {
      await p.theme(theme);
      for (const [engine,id,answer] of [
        ['native','ses_2b7e91c04d5a','Applied to'],
        ['Claude','ses_cc4b2e81d9f3','reallocated sectors'],
        ['helper','ses_cc9d1f60a7e2','38 GB'],
        ['marker',marked.id,'answer stands'],
      ]) {
        await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${id}',{pane:m.workspace.focused()}))`);
        await p.waitFor(`document.querySelector('.pane.focused .msg-assistant')`,6000);
        await sleep(200);
        const out = await p.eval(`(() => {
          const pane=document.querySelector('.pane.focused');
          return {text:pane.querySelector('.transcript')?.textContent ?? pane.textContent,
            answers:[...pane.querySelectorAll('.msg-assistant')].map(e=>e.textContent),
            checkRows:pane.querySelectorAll('.completion-check,.turn-check').length};
        })()`);
        check(out.text.includes(answer) && !/[✓✔✅]/u.test(out.text) && !out.text.includes('Internal acknowledgement') && out.checkRows===0,
          `${name}-${theme}-${engine}: no protocol message or unexplained mark`,out);
        const copied = await p.eval(`__mod('lib/transcript.ts').then(async t => {const m=await __mod('lib/store.ts'); return t.turnText(m.state.sessions['${id}'].entries,1,99999);})`);
        check(copied.includes(answer) && !/[✓✔✅]/u.test(copied) && !copied.includes('Internal acknowledgement'),`${name}-${theme}-${engine}: copied answer is content only`,copied);
      }
      const preview = await p.eval(`__mod('lib/store.ts').then(m=>m.state.sessions['ses_cc4b2e81d9f3'].info.preview)`);
      check(!/[✓✔✅]/u.test(preview),`${name}-${theme}: old-server preview does not show the mark`,preview);
      await p.eval(`__mod('lib/store.ts').then(async m=>{await m.call({method:'send_notification',params:{title:'Finished protocol',body:'✓'}});await m.call({method:'send_notification',params:{title:'Done ✓',body:'Internal protocol'}});})`);
      await sleep(250);
      const notifications=await p.eval(`__mod('lib/ui.ts').then(m=>m.toasts().map(t=>({title:t.title,message:t.message})))`);
      check(!notifications.some(t=>/[✓✔✅]/u.test(t.title+' '+t.message) || /Finished protocol|Internal protocol/.test(t.title+' '+t.message)),`${name}-${theme}: acknowledgements never ring/toast`,notifications);
      await p.shot(`${SHOTS}/completion-${name}-${theme}.png`);
      await p.eval(`__mod('lib/workspace.ts').then(m=>m.openSession('${meaningful.id}',{pane:m.workspace.focused()}))`);
      await p.waitFor(`document.querySelector('.pane.focused .msg-assistant')?.textContent.includes('Tests pass')`,6000);
      check(await p.eval(`document.querySelector('.pane.focused .msg-assistant').textContent.includes('✓ Tests pass')`),`${name}-${theme}: meaningful checked answers stay visible`);
      await p.eval(`__mod('lib/ui.ts').then(m=>m.openSettings('notifications'))`);
      await p.waitFor(`document.querySelector('.settings-body')`,6000);
      await sleep(300);
      check(await p.eval(`!document.querySelector('.notify-scheduled')?.textContent.includes('Done ✓')`),`${name}-${theme}: old scheduled protocol does not appear in Settings`);
      await p.eval(`__mod('lib/ui.ts').then(m=>m.setSettingsTab(null))`);
      // Old native clients keep task tools/answers after an internal harness prompt.
      const real = await p.eval(`__mod('lib/transcript.ts').then(t=>t.deriveItems([
        {seq:1,kind:'user',content:[{type:'text',text:'[Completion check] Still open'}],author:'harness',at:0},
        {seq:2,kind:'assistant',content:[{type:'text',text:'Still failing; more work needed.'},{type:'tool_call',id:'real',name:'task',arguments:{add_notes:['failed']}}],at:0}
      ],new t.ItemCache(),false).flatMap(x=>x.k==='steps'?x.items:[x]).map(x=>({kind:x.k,text:x.k==='text'||x.k==='narr'?x.text:'',tool:x.k==='tool'?x.call.name:''})))`);
      check(real.some(x=>x.text.includes('Still failing')) && real.some(x=>x.kind==='tool'&&x.tool==='task'),`${name}-${theme}: real work after a check is not hidden`,real);
      // Old servers send unmarked live text after a harness check. Exercise
      // successive deltas without committing any assistant entry.
      await p.eval(`__mod('lib/workspace.ts').then(m=>m.openSession('${legacyLive.id}',{pane:m.workspace.focused()}))`);
      await p.waitFor(`document.querySelector('.pane.focused .transcript')`,6000);
      for (const [prefix,suffix] of [['✓',' Tests passed'],['D','one with the repair'],['Ch','ecked the files'],['','Still failing']]) {
        await p.eval(`__mod('lib/store.ts').then(m=>{m.applyEvent({type:'stream_started',session_id:'${legacyLive.id}',model:'mock/m',after_seq:m.state.sessions['${legacyLive.id}'].info.last_seq});m.applyEvent({type:'stream_delta',session_id:'${legacyLive.id}',delta:{part:'text',index:0,text:${JSON.stringify(prefix)}}});})`);
        await sleep(100);
        check(await p.eval(`!document.querySelector('.pane.focused .msg-assistant')`),`${name}-${theme}: ambiguous ${JSON.stringify(prefix)} prefix is buffered`);
        await p.eval(`__mod('lib/store.ts').then(m=>m.applyEvent({type:'stream_delta',session_id:'${legacyLive.id}',delta:{part:'text',index:0,text:${JSON.stringify(suffix)}}}))`);
        await p.waitFor(`document.querySelector('.pane.focused .msg-assistant')?.textContent.includes(${JSON.stringify(prefix+suffix)})`,6000);
        const live = await p.eval(`__mod('lib/store.ts').then(m=>({visible:document.querySelector('.pane.focused .msg-assistant')?.textContent,committed:m.state.sessions['${legacyLive.id}'].entries.some(e=>e.kind==='assistant')}))`);
        check(!live.committed && live.visible.includes(prefix+suffix),`${name}-${theme}: real short answer is released while still streaming`,live);
        await p.eval(`__mod('lib/store.ts').then(m=>m.setState('sessions','${legacyLive.id}','streaming',null))`);
      }
    }
  }
  check(p.errors.length===0,'no page errors',p.errors);
  await p.close();
  finish();
});
