import fs from 'node:fs';
import { SHOTS, SCRATCH, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
run(async()=>{
  await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
  const p=await device(); await p.viewport(344,780,true,2); await signIn(p,'Long bot history');
  const unit=await p.eval(`__mod('lib/transcript.ts').then(m=>{
    let reads=0;const entries=[];let seq=0;
    const user=()=>({kind:'user',seq:++seq,ts:seq,author:'chat:sender',content:[{type:'text',text:'Continue'}]});
    const assistant=()=>({kind:'assistant',seq:++seq,ts:seq,author:'bot-a',content:[{type:'text',text:'Reply'}],model:{provider:'test',model:'test'},stop:'end_turn',usage:{}});
    for(let i=0;i<3000;i++)for(const e of [user(),assistant()])entries.push(new Proxy(e,{get(o,k){if(k==='kind')reads++;return o[k];}}));
    entries.push(user());const opts={speakers:true,defaultAuthor:'bot-a',userAuthor:'chat:'};const d=new m.TranscriptDeriver();d.derive(entries,true,opts);
    reads=0;entries.push(assistant());const actual=d.derive(entries,true,opts);const oldReads=reads;
    const expected=m.deriveItems(entries,new m.ItemCache(),true,opts);const same=JSON.stringify(actual)===JSON.stringify(expected);
    entries.splice(10,entries.length-10);const rewind=JSON.stringify(d.derive(entries,false,opts,1))===JSON.stringify(m.deriveItems(entries,new m.ItemCache(),false,opts));
    return {oldReads,same,rewind};
  })`);
  check(unit.oldReads<100,'authored bot turns keep completed history outside the live suffix',unit);
  check(unit.same,'cached bot transcript matches full derivation, including speakers',unit);
  check(unit.rewind,'rewinds invalidate the authored prefix cache',unit);
  const probe=await wsClient('bot history probe');
  const seeded=(await probe.rpc('mock_phone_lag',{calls:3000,helpers:0,authored_messages:true})).result;
  const id=seeded.parents[0].id;
  await p.eval(`__mod('lib/store.ts').then(m=>m.ensureLoaded('${id}'))`);
  await p.send('Emulation.setCPUThrottlingRate',{rate:4});
  await p.eval(`__mod('lib/workspace.ts').then(m=>m.openSession('${id}',{pane:m.workspace.focused()}))`);
  await p.waitFor(`document.querySelector('.pane.focused .transcript .item')`,60000);
  const samples=[];
  for(let i=0;i<3;i++)samples.push(await p.eval(`__mod('lib/store.ts').then(m=>{const s=m.state.sessions['${id}'];const seq=s.info.last_seq+1;const t=performance.now();m.applyEvent({type:'entry_added',session_id:'${id}',entry:{kind:'assistant',seq,ts:1700010000000+seq,author:s.info.bot_id,model:s.info.model,usage:{input:1,output:1,cache_read:0,cache_write:0},stop:'tool_use',content:[{type:'tool_call',id:'bot-history-'+seq,name:'read',arguments:{path:'/synthetic/new'}}]}});return performance.now()-t;})`));
  await p.send('Emulation.setCPUThrottlingRate',{rate:1});
  const metrics={entries:seeded.entries,throttle:4,samples};console.log('BOT_HISTORY_METRICS',JSON.stringify(metrics));fs.mkdirSync(SCRATCH,{recursive:true});fs.writeFileSync(`${SCRATCH}/bot-history.json`,JSON.stringify(metrics,null,2));
  check(Math.max(...samples)<1000,'long bot conversations stay responsive on append at 4x CPU',samples);
  for(const [label,w,h,mobile]of [['desktop',1440,900,false],['folded',344,780,true],['fold',841,701,true]])for(const theme of ['light','dark']){
    await p.viewport(w,h,mobile,mobile?2:1);await p.theme(theme);await sleep(100);
    check(await p.eval(`!!document.querySelector('.pane.focused .transcript .item') && document.documentElement.scrollWidth<=innerWidth+1`),`${label}-${theme}: long bot transcript renders without overflow`);
    await p.shot(`${SHOTS}/bot-history-${label}-${theme}.png`);
  }
  probe.close();await p.close();finish();
});
