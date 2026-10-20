// The real Composer + voice module: delayed import/grant and identity replacement.
import { check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
const until=async(fn,label)=>{for(let i=0;i<125;i++){if(await fn())return;await sleep(80);}throw Error(`Timed out: ${label}`);};
run(async()=>{
 for(const stage of ['import','grant','live'])for(const action of ['cancel','unmount','replace','server']){
  await mock('/__mock/reset');await mock('/__mock/mosaics/reset');
  const peer=await wsClient('Voice intent fixture');const fixture=(await peer.rpc('mock_recall_lazy',{phase:'seed'})).result;
  const p=await device();await p.viewport(344,780,true,2);
  const paused=[];p.on('Fetch.requestPaused',e=>paused.push(e));
  if(stage==='import')await p.send('Fetch.enable',{patterns:[{urlPattern:'*/src/lib/voice/conversation.ts*',requestStage:'Request'}]});
  let transcribes=0;p.on('Network.webSocketFrameSent',e=>{try{if(JSON.parse(e.response.payloadData).method==='transcribe')transcribes++;}catch{}});await p.send('Network.enable');
  await signIn(p,`Intent ${stage} ${action}`);
  await p.eval(`__mod('lib/workspace.ts').then(m=>m.openSession(${JSON.stringify(fixture.id)}))`);
  await p.waitFor(`document.querySelector('.composer-dock .composer textarea')`);
  await p.eval(`__mod('lib/store.ts').then(async m=>{await m.api.saveVoice({...await m.api.getVoice(),engine:'service',enabled:true,provider:'local',base_url:'http://127.0.0.1:9000/v1',model:'whisper-1'},null);m.setState('server',{...m.state.server,voice:true,voice_engine:'service'});});window.__mic=window.__construct=window.__starts=window.__stopped=0;window.__composer=document.querySelector('.composer-dock .composer textarea');Object.defineProperty(window,'AudioContext',{configurable:true,value:class{constructor(){window.__construct++;this.sampleRate=16000;this.destination={}}createMediaStreamSource(){return{connect(){},disconnect(){}}}createScriptProcessor(){return{connect(){},disconnect(){},onaudioprocess:null}}createGain(){return{gain:{value:0},connect(){},disconnect(){}}}async resume(){window.__starts++}async close(){}}});Object.defineProperty(navigator,'mediaDevices',{configurable:true,value:{getUserMedia:()=>{window.__mic++;return new Promise(resolve=>window.__grant=()=>resolve({getTracks:()=>[{stop(){window.__stopped++}},{stop(){window.__stopped++}}]}))}}});`);
  await p.waitFor(`document.querySelector('.mic-btn')`);await p.click('.mic-btn');
  if(stage==='import')await until(()=>paused.length===1,'delayed voice import');
  else{
   await p.waitFor(`window.__mic===1 && !!window.__grant`);
   if(stage==='live'){await p.eval('window.__grant()');await p.waitFor(`window.__starts===1 && !!document.querySelector('.voice-status[data-phase="listening"]')`);}
  }
  if(action==='cancel')await p.clickText('.composer button','Cancel');
  else await p.eval(`__mod('lib/workspace.ts').then(m=>${action==='unmount'?'m.closePane(m.workspace.focused())':action==='server'?"m.loadWorkspace('synthetic-other-server')":`m.openSession(${JSON.stringify(fixture.other)},{pane:m.workspace.focused()})`})`);
  if(action==='replace'){
   await p.waitFor(`document.querySelector('.pane.focused .composer-dock .composer textarea')===window.__composer`);
   check(await p.eval('window.__composer.isConnected'),`${stage}: chat replacement keeps the Composer mounted`);
  }
  if(stage==='import')await p.send('Fetch.continueRequest',{requestId:paused[0].requestId});
  if(stage==='grant')await p.eval('window.__grant()');
  if(stage==='grant'||stage==='live')await p.waitFor(`window.__stopped===2`);else await sleep(160);
  check(await p.eval(`window.__starts===${stage==='live'?1:0} && window.__construct===${stage==='live'?1:0}`),`${stage}-${action}: stale intent never constructs or starts a recorder after cancellation`);
  check(await p.eval(`window.__mic===${stage==='import'?0:1} && window.__stopped===${stage==='import'?0:2}`),`${stage}-${action}: no unwanted permission request and every granted track stopped`);
  check(transcribes===0,`${stage}-${action}: stopped intent is never sent for transcription`);
  check(!p.errors.length,`${stage}-${action}: no uncaught voice errors`,p.errors);
  if(stage==='import')await p.send('Fetch.disable');await p.close();peer.close();
 }
 for(const event of ['delayed','missing'])for(const action of ['replace','unmount']){
  await mock('/__mock/reset');await mock('/__mock/mosaics/reset');
  const peer=await wsClient('Finish fixture');const fixture=(await peer.rpc('mock_recall_lazy',{phase:'seed'})).result;
  const p=await device();await signIn(p,`Finish ${event} ${action}`);
  await p.eval(`__mod('lib/workspace.ts').then(m=>m.openSession(${JSON.stringify(fixture.id)}))`);
  await p.waitFor(`document.querySelector('.composer-dock .composer textarea')`);
  let transcribes=0;await p.send('Network.enable');p.on('Network.webSocketFrameSent',e=>{try{if(JSON.parse(e.response.payloadData).method==='transcribe')transcribes++;}catch{}});
  await p.eval(`__mod('lib/store.ts').then(async m=>{await m.api.saveVoice({...await m.api.getVoice(),engine:'service',enabled:true,provider:'local',base_url:'http://127.0.0.1:9000/v1',model:'whisper-1'},null);m.setState('server',{...m.state.server,voice:true,voice_engine:'service'});});window.__stopped=0;window.__oldComposer=document.querySelector('.composer-dock .composer textarea');Object.defineProperty(window,'AudioContext',{configurable:true,value:class{constructor(){this.sampleRate=16000;this.destination={}}createMediaStreamSource(){return{connect(){},disconnect(){}}}createScriptProcessor(){return window.__node={connect(){},disconnect(){},onaudioprocess:null}}createGain(){return{gain:{value:0},connect(){},disconnect(){}}}async resume(){window.__onstop=()=>window.__node.onaudioprocess?.({inputBuffer:{getChannelData:()=>new Float32Array(512).fill(.2)}})}async close(){}}});Object.defineProperty(navigator,'mediaDevices',{configurable:true,value:{getUserMedia:async()=>({getTracks:()=>[{stop(){window.__stopped++}},{stop(){window.__stopped++}}]})}});`);
  await p.waitFor(`document.querySelector('.mic-btn')`);await p.click('.mic-btn');await p.waitFor(`document.querySelector('.voice-status[data-phase="listening"]')`);
  await p.click('.mic-btn');
  check(await p.eval(`window.__stopped===2`),`Finish-${event}-${action}: all tracks stop synchronously before navigation`);
  await p.eval(`__mod('lib/workspace.ts').then(m=>${action==='unmount'?'m.closePane(m.workspace.focused())':`m.openSession(${JSON.stringify(fixture.other)},{pane:m.workspace.focused()})`})`);
  if(event==='delayed')await p.eval(`window.__onstop()`);
  check(await p.eval(`window.__stopped===2`),`Finish-${event}-${action}: release remains idempotent after identity/unmount`);
  check(transcribes===0,`Finish-${event}-${action}: discarded final data is never transcribed`);
  check(!p.errors.length,`Finish-${event}-${action}: no uncaught finishing errors`,p.errors);
  await p.close();peer.close();
 }
 // Enter cancels all three pending phases; listening alone finishes into the draft.
 for(const stage of ['import','grant','transcribing']){
  await mock('/__mock/reset');await mock('/__mock/mosaics/reset');
  const peer=await wsClient('Enter fixture');const fixture=(await peer.rpc('mock_recall_lazy',{phase:'seed'})).result;
  const p=await device();await p.viewport(344,780,true,2);
  const paused=[];p.on('Fetch.requestPaused',e=>paused.push(e));
  if(stage==='import')await p.send('Fetch.enable',{patterns:[{urlPattern:'*/src/lib/voice/conversation.ts*',requestStage:'Request'}]});
  let sent=0;await p.send('Network.enable');p.on('Network.webSocketFrameSent',e=>{try{if(JSON.parse(e.response.payloadData).method==='send_message')sent++;}catch{}});
  await signIn(p,`Enter ${stage}`);
  await p.eval(`__mod('lib/workspace.ts').then(m=>m.openSession(${JSON.stringify(fixture.id)}))`);
  await p.waitFor(`document.querySelector('.composer-dock .composer textarea')`);
  await p.eval(`__mod('lib/store.ts').then(async m=>{
    await m.api.saveVoice({...await m.api.getVoice(),engine:'service',enabled:true,provider:'local',base_url:'http://127.0.0.1:9000/v1',model:'whisper-1'},null);
    m.setState('server',{...m.state.server,voice:true,voice_engine:'service'});
    window.__transcribes=0;m.api.transcribe=()=>{window.__transcribes++;return new Promise(resolve=>window.__lateTranscript=()=>resolve({text:'cancelled late transcript'}));};
  })`);
  await p.eval(`window.__mic=window.__construct=window.__stopped=0;
    Object.defineProperty(window,'AudioContext',{configurable:true,value:class{
      constructor(){window.__construct++;this.sampleRate=16000;this.destination={}}
      createMediaStreamSource(){return{connect(){},disconnect(){}}}
      createScriptProcessor(){return this.node={connect(){},disconnect(){},onaudioprocess:null}}
      createGain(){return{gain:{value:0},connect(){},disconnect(){}}}
      async resume(){this.node.onaudioprocess?.({inputBuffer:{getChannelData:()=>new Float32Array(8000).fill(.2)}})}
      async close(){}
    }});
    Object.defineProperty(navigator,'mediaDevices',{configurable:true,value:{getUserMedia:()=>{
      window.__mic++;return new Promise(resolve=>window.__grant=()=>resolve({getTracks:()=>[{stop(){window.__stopped++}},{stop(){window.__stopped++}}]}));
    }}});
    document.querySelector('.composer-dock .composer textarea').value='keep this draft';
    document.querySelector('.composer-dock .composer textarea').dispatchEvent(new Event('input',{bubbles:true}));`);
  await p.waitFor(`document.querySelector('.mic-btn')`);await p.click('.mic-btn');
  if(stage==='import')await until(()=>paused.length===1,'Enter delayed import');
  else{
    await p.waitFor(`window.__mic===1 && !!window.__grant`);
    if(stage==='transcribing'){
      await p.eval('window.__grant()');await p.waitFor(`document.querySelector('.voice-status[data-phase="listening"]')`);
      await p.click('.mic-btn');await p.waitFor(`window.__transcribes===1 && document.querySelector('.voice-status[data-phase="sending"]')`);
    }
  }
  await p.eval(`document.querySelector('.composer-dock .composer textarea').focus()`);await p.key('Enter');
  await p.waitFor(`!document.querySelector('.voice-status') && !!document.querySelector('.mic-btn')`);
  check(await p.eval(`document.querySelector('.composer-dock .composer textarea').value==='keep this draft'`),`Enter-${stage}: pending work cancels without sending or clearing the draft`);
  if(stage==='import')await p.send('Fetch.continueRequest',{requestId:paused[0].requestId});
  if(stage==='grant')await p.eval('window.__grant()');
  if(stage==='transcribing')await p.eval('window.__lateTranscript()');
  if(stage==='grant')await p.waitFor('window.__stopped===2');await sleep(200);
  check(await p.eval(`window.__mic===${stage==='import'?0:1} && window.__construct===${stage==='transcribing'?1:0} && window.__stopped===${stage==='import'?0:2}`),`Enter-${stage}: late work cannot start capture and granted tracks release exactly once`);
  check(await p.eval(`window.__transcribes===${stage==='transcribing'?1:0} && document.querySelector('.composer-dock .composer textarea').value==='keep this draft'`),`Enter-${stage}: no new transcription or stale transcript insertion`);
  check(sent===0,`Enter-${stage}: never sends a chat message`);
  check(!p.errors.length,`Enter-${stage}: no uncaught errors`,p.errors);
  if(stage==='import')await p.send('Fetch.disable');await p.close();peer.close();
 }
 finish();
});
