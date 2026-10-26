// The real Composer/voice loader: delayed code cannot touch the microphone after
// cancellation/unmount; failures are retried through the shared onDemand path.
import { check, device, finish, mock, run, signIn, sleep } from './cdp.mjs';
const until=async(fn,label)=>{for(let i=0;i<125;i++){const value=await fn();if(value)return value;await sleep(80);}throw Error(`Timed out: ${label}`);};
run(async()=>{
 for(const scenario of ['cancel','unmount','restart','failure']){
  await mock('/__mock/reset');await mock('/__mock/mosaics/reset');
  const p=await device();await p.viewport(344,780,true,2);
  const paused=[],requests=[];p.on('Fetch.requestPaused',e=>paused.push(e));
  await p.send('Network.enable');await p.send('Network.setCacheDisabled',{cacheDisabled:true});p.on('Network.requestWillBeSent',e=>requests.push(e.request.url));
  await p.send('Fetch.enable',{patterns:[{urlPattern:'*/src/lib/voice/conversation.ts*',requestStage:'Request'}]});
  await signIn(p,`Voice ${scenario}`);
  await p.eval(`__mod('lib/store.ts').then(async m=>{await m.api.saveVoice({...await m.api.getVoice(),engine:'service',enabled:true,provider:'local',base_url:'http://127.0.0.1:9000/v1',model:'whisper-1'},null);m.setState('server',{...m.state.server,voice:true,voice_engine:'service'});});window.__micCalls=0;Object.defineProperty(navigator,'mediaDevices',{configurable:true,value:{getUserMedia:async()=>{window.__micCalls++;throw new DOMException('Synthetic permission denied','NotAllowedError')}}});window.__page=crypto.randomUUID();`);
  await p.waitFor(`document.querySelector('.mic-btn')`);
  check(!requests.some(url=>/\/src\/lib\/voice\/conversation\.ts/.test(url)),`${scenario}: ordinary chat does not fetch voice capture`);
  await p.click('.mic-btn');await until(()=>paused[0],'delayed voice chunk');
  check(await p.eval(`window.__micCalls===0 && !![...document.querySelectorAll('.composer button')].find(b=>b.textContent==='Cancel')`),`${scenario}: loading is cancellable before any microphone permission request`);
  if(scenario==='failure'){
   await p.send('Fetch.failRequest',{requestId:paused[0].requestId,errorReason:'Failed'});
   await p.waitFor(`document.querySelector('.composer .part-retry') && document.querySelector('.mic-btn')`);
   check(await p.eval(`window.__micCalls===0 && document.querySelector('.composer .part-error').textContent.includes("Couldn't load")`),'failure: shared recoverable error appears with no microphone access');
   await p.click('.mic-btn');await until(()=>paused[1],'second Record fresh voice asset');
   check(paused[1].request.url.includes('tessera_retry='),'failure: second Record uses uncached shared recovery URL');
   await p.send('Fetch.continueRequest',{requestId:paused[1].requestId});
   await p.waitFor(`window.__micCalls===1 && !!document.querySelector('.mic-btn')`);
   check(await p.eval(`window.__micCalls===1 && !document.querySelector('.composer .part-error') && !!window.__page`),'failure: second Record recovers in-page and requests permission only once');
  }else{
   if(scenario==='unmount')await p.eval(`__mod('lib/workspace.ts').then(m=>m.closePane(m.workspace.focused()))`);
   else await p.clickText('.composer button','Cancel');
   if(scenario==='restart'){
    await p.waitFor(`document.querySelector('.mic-btn')`);await p.click('.mic-btn');
    check(await p.eval(`window.__micCalls===0`),'restart: new Record still waits for the shared delayed chunk');
   }
   await p.send('Fetch.continueRequest',{requestId:paused[0].requestId});await sleep(500);
   check(await p.eval(`window.__micCalls===${scenario==='restart'?1:0}`),`${scenario}: cancelled generation never starts the microphone${scenario==='restart'?'; only the new Record does':''}`);
  }
  check(!p.errors.length,`${scenario}: no uncaught voice loader errors`,p.errors);
  await p.send('Fetch.disable');await p.close();
 }
 for (const action of ['Escape', 'End', 'Finish']) {
  await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
  const p = await device(); await signIn(p, `Host ${action}`);
  await p.eval(`window.__lateMic = 0; const original = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices); navigator.mediaDevices.getUserMedia = (...args) => { __lateMic++; return original(...args); };`);
  await p.send('Fetch.enable', { patterns: [{ urlPattern: '*/src/lib/voice/host.ts*', requestStage: 'Request' }] });
  p.on('Fetch.requestPaused', ({ requestId }) => { setTimeout(() => { void p.send('Fetch.continueRequest', { requestId }).catch(() => {}); }, 2400); });
  await p.click('[aria-label="Voice input"]');
  await until(() => p.eval(`document.querySelector('.voice-status')?.dataset.phase === 'starting'`), 'deferred host starting');
  if (action === 'End') await p.click('.voice-status button');
  else if (action === 'Escape') await p.key('Escape', 'Escape', 0, 27);
  else await p.key('Enter', 'Enter', 0, 13);
  await sleep(2800);
  const fixture = await p.eval(`__mod('lib/store.ts').then(m => m.call({ method: 'mock_voice', params: {} }))`);
  check(await p.eval(`__lateMic === 0 && !document.querySelector('.voice-status')`) && fixture.requests.length === 0, `${action} before host import ends prevents late recording or engine download`);
  check(!p.errors.length, `${action} deferred host has no browser errors`, p.errors); await p.close();
 }
 finish();
});
