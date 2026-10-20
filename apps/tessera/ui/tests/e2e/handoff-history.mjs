import fs from 'node:fs';
import { SHOTS, SCRATCH, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
run(async()=>{
  await mock('/__mock/reset');await mock('/__mock/mosaics/reset');
  const p=await device();await p.viewport(344,780,true,2);await signIn(p,'Completed handoff history');
  const unit=await p.eval(`__mod('lib/handoffIndex.ts').then(m=>{
    let seq=0,reads=0;const entries=[];const add=e=>entries.push(new Proxy({...e,seq:++seq,ts:seq},{get(o,k){if(k==='kind')reads++;return o[k];}}));
    for(let i=0;i<6000;i++)add({kind:'notice',text:'Old history'});
    for(let i=0;i<120;i++){add({kind:'assistant',content:[{type:'tool_call',id:'call-'+i}]});add({kind:'user',author:'bot-a',content:[{type:'text',text:'[Reply from @a · 1 min · session dm]\\n\\nReply '+i}]});}
    const s={entries,entriesVersion:0};const first=Array.from({length:120},(_,i)=>m.handoffReply(s,'call-'+i,'@a','dm'));
    reads=0;add({kind:'notice',text:'Appended'});const stable=first.every((r,i)=>m.handoffReply(s,'call-'+i,'@a','dm')===r);const appendedReads=reads;
    const association=first.every((r,i)=>r.reply.body==='Reply '+i);
    const rewritten=entries[6001];rewritten.content=[{type:'text',text:'[Reply from @a · session dm]\\n\\nRewritten'}];s.entriesVersion++;
    const rewrite=m.handoffReply(s,'call-0','@a','dm').reply.body==='Rewritten';
    s.entries=entries.slice(0,6001);s.entriesVersion++;const rewind=m.handoffReply(s,'call-0','@a','dm')===null;
    return {stable,appendedReads,association,rewrite,rewind};
  })`);
  check(unit.stable&&unit.appendedReads<=2,'120 completed cards index the appended entry once and retain stable replies',unit);
  check(unit.association,'repeated handoffs to the same DM associate each first reply after its own call',unit);
  check(unit.rewrite&&unit.rewind,'history revision and rewind invalidate the shared handoff index',unit);
  const probe=await wsClient('handoff history probe');const seeded=(await probe.rpc('mock_phone_lag',{calls:3000,helpers:0,handoffs:120})).result;const id=seeded.parents[0].id;
  // Explicitly request the long-history stress workload. New opens are paged.
  await p.eval(`__mod('lib/history.ts').then(m=>m.historyTo('${id}',0))`);
  await p.send('Emulation.setCPUThrottlingRate',{rate:4});
  await p.eval(`__mod('lib/workspace.ts').then(m=>m.openSession('${id}',{pane:m.workspace.focused()}))`);
  await p.waitFor(`document.querySelector('.pane.focused .bot-handoff .subagent-activity')?.textContent==='Replied'`,60000);
  const ready=await p.eval(`__mod('lib/store.ts').then(m=>Object.values(m.state.sessions['${id}'].results).filter(r=>r.details?.background).length===120)`);check(ready,'all 120 persisted handoffs are in the workload');
  if(!ready)throw Error('Handoff workload did not mount');
  const samples=[];
  for(let i=0;i<3;i++)samples.push(await p.eval(`Promise.all([__mod('lib/store.ts'),__mod('lib/handoffIndex.ts')]).then(([s,h])=>{const parent=s.state.sessions['${id}'];const results=Object.values(parent.results).filter(r=>r.details?.background);const before=results.map(r=>h.handoffReply(parent,r.call_id,r.details.bot,r.details.session_id));const seq=parent.info.last_seq+1;const t=performance.now();s.applyEvent({type:'entry_added',session_id:'${id}',entry:{kind:'notice',seq,ts:1700010000000+seq,level:'info',text:'Appended history '+seq}});const stable=results.every((r,i)=>before[i]===h.handoffReply(parent,r.call_id,r.details.bot,r.details.session_id));return {ms:performance.now()-t,stable};})`));
  await p.send('Emulation.setCPUThrottlingRate',{rate:1});const metrics={entries:seeded.entries,handoffs:120,throttle:4,samples};console.log('HANDOFF_HISTORY_METRICS',JSON.stringify(metrics));fs.mkdirSync(SCRATCH,{recursive:true});fs.writeFileSync(`${SCRATCH}/handoff-history.json`,JSON.stringify(metrics,null,2));
  check(samples.every(s=>s.stable&&s.ms<1000),'long-history completed handoffs stay stable and responsive at 4x CPU',samples);
  for(const [label,w,h,mobile]of [['desktop',1440,900,false],['folded',344,780,true],['fold',841,701,true]])for(const theme of ['light','dark']){await p.viewport(w,h,mobile,mobile?2:1);await p.theme(theme);await sleep(100);check(await p.eval(`!!document.querySelector('.pane.focused .bot-handoff')&&document.documentElement.scrollWidth<=innerWidth+1`),`${label}-${theme}: completed handoff cards stay usable`);await p.shot(`${SHOTS}/handoff-history-${label}-${theme}.png`);}
  probe.close();await p.close();finish();
});
