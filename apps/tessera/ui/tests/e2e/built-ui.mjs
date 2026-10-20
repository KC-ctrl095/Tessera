// Real emitted JS/CSS and a real encrypted handshake (cold scenario). Failure
// fixtures use the actual built chat and mock protocol, never stand-in components.
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { APP, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';

const manifest=JSON.parse(readFileSync(join(process.env.BUILT_DIST,'.vite/manifest.json')));
const parts=JSON.parse(readFileSync(join(process.env.BUILT_DIST,'parts.json')));
const until=async(fn,label)=>{for(let i=0;i<150;i++){const value=await fn();if(value)return value;await sleep(80);}throw Error(`Timed out: ${label}`);};
run(async()=>{
 if(process.env.BUILT_SCENARIO==='cold'){
  const peer=await wsClient('Built cold pairing');
  await peer.rpc('complete_setup');
  await peer.rpc('set_remote_access',{urls:[APP.replace(/^http:/,'https:').replace(/\/web\/$/,'')]});
  await peer.rpc('set_web_access',{on:true});
  const pairing=(await peer.rpc('create_pairing')).result;
  const p=await device();await p.viewport(1440,900);
  await p.goto(APP);await p.waitFor(`document.querySelector('.pair-box input')`);
  await p.click('.pair-box input');await p.type(pairing.code);await p.clickText('button','Pair & connect');
  await p.waitFor(`/You're in/.test(document.body.textContent) || !!document.querySelector('.workspace')`,20000);
  await p.goto(APP);await p.waitFor(`document.querySelector('.workspace .pane')`,20000);
  const records=[];let ready=false;
  await p.send('Network.enable');await p.send('Network.setCacheDisabled',{cacheDisabled:true});await p.send('Network.clearBrowserCache');
  await p.send('Runtime.addBinding',{name:'__chatReady'});
  p.on('Runtime.bindingCalled',e=>{if(e.name==='__chatReady')ready=true;});
  p.on('Network.requestWillBeSent',e=>{if(!ready&&/\.js(?:\?|$)/.test(e.request.url))records.push(e.request.url);});
  await p.send('Page.addScriptToEvaluateOnNewDocument',{source:`new MutationObserver(function(_,m){if(document.querySelector('.workspace .pane')){window.__chatReady('ready');m.disconnect();}}).observe(document,{childList:true,subtree:true});`});
  await p.goto(APP,0);await until(()=>ready,'paired built chat pane');
  const proof=JSON.parse(readFileSync(process.env.BUDGET_PROOF));
  const expected=proof.paths.paired.files.filter(f=>!f.fileName.includes('#')).map(f=>f.fileName).sort();
  const observed=[...new Set(records.map(url=>new URL(url).pathname.replace(/^\/web\//,'')))].sort();
  assert.deepEqual(observed,expected,'paired gate equals recorded real cold-start network file set');
  check(true,'paired emitted file set matches real encrypted cold-start network recording');
  check(expected.some(f=>f===manifest['src/lib/e2e.ts'].file),'real cold-start includes the awaited encryption chunk');
  const report={expected,observed,documentedPostChatExtras:[],requests:records,scope:'fresh cache /web/ reload after real pairing, recorded until first workspace chat pane insertion'};
  mkdirSync(SHOTS,{recursive:true});writeFileSync(join(SHOTS,'paired-cold-network.json'),JSON.stringify(report,null,2)+'\n');
  console.log('PAIRED_COLD_NETWORK_OK',JSON.stringify({expected:expected.length,observed:observed.length,extras:[]}));
  await p.close();peer.close();finish();return;
 }
 for(const kind of ['css','js','helper','voice','stale']){
  await fetch(new URL('/__test/reset',APP));await mock('/__mock/reset');await mock('/__mock/mosaics/reset');
  const peer=await wsClient(`Built ${kind}`);
  const f=(await peer.rpc('mock_recall_lazy',{phase:'seed'})).result;
  if (kind==='voice') await peer.rpc('save_voice',{config:{enabled:true,provider:'custom',base_url:'http://synthetic.test/voice',model:'synthetic',language:''}});
  const p=await device();await p.viewport(1440,900);
  await p.send('Network.enable');await p.send('Network.setCacheDisabled',{cacheDisabled:true});
  const urls=[];p.on('Network.requestWillBeSent',e=>urls.push(e.request.url));
  const paused=[];p.on('Fetch.requestPaused',e=>paused.push(e));
  const target=kind==='voice'?parts.voice.file:kind==='css'?parts.RecallCard.css.find(f=>/RecallCard/.test(f)):parts.RecallCard.file;
  assert.ok(target,`actual built ${kind} asset`);
  const helper = manifest['src/lib/chunkRecovery.ts'].file;
  await p.send('Fetch.enable',{patterns:[{urlPattern:`*/${target}*`,requestStage:'Request'}, ...(kind === 'helper' ? [{urlPattern:`*/${helper}*`,requestStage:'Request'}] : [])]});
  await signIn(p,`Built ${kind}`);
  await p.click('.srow-main[aria-label="Synthetic recall main"]');
  await p.waitFor(`document.querySelector('.pane.focused .msg-assistant[data-seq="2"]')`);
  await p.click('.pane.focused .composer textarea');await p.type('Built draft kept');
  await p.eval(`window.__kept=document.querySelector('.pane.focused .composer textarea');window.__page=crypto.randomUUID();`);
  const page=await p.eval('window.__page');
  if (kind==='voice') {
   await p.eval(`{const ta=document.querySelector('.composer textarea');ta.value='';ta.dispatchEvent(new Event('input',{bubbles:true}));window.__micCalls=0;Object.defineProperty(navigator,'mediaDevices',{configurable:true,value:{getUserMedia:async()=>{window.__micCalls++;throw new DOMException('Synthetic permission denied','NotAllowedError')}}});}`);
   await p.waitFor(`document.querySelector('.mic-btn')`);await p.click('.mic-btn');await until(()=>paused[0], 'emitted voice capture request');
   await p.send('Fetch.failRequest',{requestId:paused[0].requestId,errorReason:'Failed'});await p.waitFor(`document.querySelector('.composer .part-retry') && document.querySelector('.mic-btn')`);
   check(await p.eval(`window.__micCalls===0`),'voice: emitted failure displays shared recovery controls without accessing microphone');
   await p.click('.mic-btn');await until(()=>paused[1], 'second Record emitted voice retry');
   check(paused[1].request.url.includes('tessera_retry='),'voice: second Record uses shared uncached emitted part URL');
   await p.send('Fetch.continueRequest',{requestId:paused[1].requestId});await p.waitFor(`window.__micCalls===1 && !!document.querySelector('.mic-btn')`);
   check(await p.eval(`window.__page===${JSON.stringify(page)} && !document.querySelector('.composer .part-error') && window.__micCalls===1`),'voice: actual emitted capture recovers in-page on second Record');
   check(!p.errors.length, 'voice: no uncaught recovery errors',p.errors);
   await p.send('Fetch.disable');await p.close();peer.close();continue;
  }
  if(kind==='stale')await fetch(new URL('/__test/update',APP));
  await peer.rpc('mock_recall_lazy',{phase:'stream'});await until(()=>paused[0],`${kind} emitted asset request`);
  if(kind==='stale') await p.send('Fetch.continueRequest',{requestId:paused[0].requestId}); // B genuinely returns404 for A's deleted hash.
  else await p.send('Fetch.failRequest',{requestId:paused[0].requestId,errorReason:'Failed'});
  await p.waitFor(`document.querySelector('.part-error[role="alert"]')`);
  await peer.rpc('mock_recall_lazy',{phase:'call'});await peer.rpc('mock_recall_lazy',{phase:'result'});
  if(kind==='stale'){
   check(await p.eval(`document.querySelector('.part-error').textContent.includes('Tessera was updated.') && !document.querySelector('.part-retry') && document.querySelector('.part-reload').textContent==='Reload (drafts kept)'`),'stale emitted hash: honest update + drafts-kept Reload, no futile Retry');
   check((await fetch(paused[0].request.url)).status===404,'stale old emitted hash is actually absent on the updated deploy');
   check(urls.filter(url=>url.includes(target)).length===1,'stale deploy does not retry the deleted old hash');
   await p.eval(`window.__setItem=Storage.prototype.setItem;Storage.prototype.setItem=function(key,value){if(key==='tessera.drafts.v2')throw new Error('synthetic storage full');return window.__setItem.call(this,key,value)};document.querySelector('.part-reload').click();`);
   check(await p.eval(`window.__page===${JSON.stringify(page)} && document.querySelector('.part-error').textContent.includes('synthetic storage full') && window.__kept.value==='Built draft kept'`),'stale: storage failure blocks reload instead of losing the draft');
   await p.eval(`Storage.prototype.setItem=window.__setItem;`);
   for (const silent of [false, true]) {
    await p.eval(`Storage.prototype.setItem=function(key,value){if(key.startsWith('tessera.workspace.'))${silent ? 'return;' : "throw new Error('synthetic workspace full');"}return window.__setItem.call(this,key,value)};window.__getItem=Storage.prototype.getItem;${silent ? "Storage.prototype.getItem=function(key){return key.startsWith('tessera.workspace.')?'{}':window.__getItem.call(this,key)};" : ''}document.querySelector('.part-reload').click();`);
    check(await p.eval(`window.__page===${JSON.stringify(page)} && document.querySelector('.part-error').textContent.includes(${JSON.stringify(silent ? 'Workspace save failed' : 'synthetic workspace full')}) && window.__kept.value==='Built draft kept'`),`stale: ${silent ? 'silent' : 'throwing'} workspace identity write failure blocks reload`);
    await p.eval(`Storage.prototype.setItem=window.__setItem;Storage.prototype.getItem=window.__getItem;`);
   }
   await p.eval(`window.__imageSrc=Object.getOwnPropertyDescriptor(HTMLImageElement.prototype,'src');Object.defineProperty(HTMLImageElement.prototype,'src',{...window.__imageSrc,set(value){if(value.startsWith('blob:'))window.__decode=()=>window.__imageSrc.set.call(this,value);else window.__imageSrc.set.call(this,value)}});{const dt=new DataTransfer();dt.items.add(new File([Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aWGsAAAAASUVORK5CYII='),c=>c.charCodeAt(0))],'preparing.png',{type:'image/png'}));const input=document.querySelector('.composer input[type="file"][multiple]');input.files=dt.files;input.dispatchEvent(new Event('change',{bubbles:true}));document.querySelector('.part-reload').click();}`);
   check(await p.eval(`window.__page===${JSON.stringify(page)} && !document.querySelector('.composer .attachment') && document.querySelector('.part-error').textContent.includes('Still preparing') && document.querySelector('.part-copy').disabled && document.querySelector('.part-save').disabled`),'stale: synchronously accepted image blocks Reload/backup during delayed decode');
   await p.eval(`Object.defineProperty(HTMLImageElement.prototype,'src',window.__imageSrc);window.__decode();`);await p.waitFor(`document.querySelector('.composer .attachment')`);
   await p.eval(`document.querySelector('.part-reload').click();`);
   check(await p.eval(`window.__page===${JSON.stringify(page)} && document.querySelector('.part-error').textContent.includes('memory-only') && !document.querySelector('.part-save').disabled`),'stale: completed preparation remains a recoverable memory-only blocker');
   await p.eval(`document.querySelector('.composer .attachment-x').click();`);
   for (const memory of ['attachment', 'paste']) {
    if (memory === 'attachment') await p.eval(`{const dt=new DataTransfer();dt.items.add(new File(['Synthetic file bytes'],'unsent.bin',{type:'application/octet-stream'}));const input=document.querySelector('.composer input[type="file"][multiple]');input.files=dt.files;input.dispatchEvent(new Event('change',{bubbles:true}));}`);
    else await p.eval(`{window.__paste='Large synthetic paste\\n'.repeat(200);const dt=new DataTransfer();dt.setData('text/plain',window.__paste);document.querySelector('.composer textarea').dispatchEvent(new ClipboardEvent('paste',{clipboardData:dt,bubbles:true,cancelable:true}));}`);
    await p.waitFor(`document.querySelector('.composer .file-chip')`);
    await p.eval(`document.querySelector('.part-reload').click();`);
    check(await p.eval(`window.__page===${JSON.stringify(page)} && document.querySelector('.part-error').textContent.includes('memory-only') && !document.querySelector('.part-reload').textContent.includes('drafts kept') && !!document.querySelector('.part-copy') && !!document.querySelector('.part-save')`),`stale: unsent ${memory} blocks reload with honest warning and copy/save options`);
    await p.eval(`window.__backup=null;window.__anchorClick=HTMLAnchorElement.prototype.click;HTMLAnchorElement.prototype.click=function(){fetch(this.href).then(r=>r.json()).then(v=>window.__backup=v)};document.querySelector('.part-save').click();`);
    await p.waitFor(`!!window.__backup`);
    check(await p.eval(`window.__backup.drafts.some(d=>d.text==='Built draft kept') && window.__backup.attachments.some(a=>${memory === 'paste' ? "a.pasted===window.__paste" : "a.name==='unsent.bin' && atob(a.data)==='Synthetic file bytes'"})`),`stale: saved backup contains full ${memory} plus the draft text`);
    await p.eval(`window.__copied='';Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async text=>{window.__copied=text}}});document.querySelector('.part-copy').click();`);
    await p.waitFor(`!!window.__copied`);
    check(await p.eval(`JSON.stringify(JSON.parse(window.__copied))===JSON.stringify(window.__backup)`),`stale: copy option retains the same full ${memory} backup`);
    await p.eval(`HTMLAnchorElement.prototype.click=window.__anchorClick;document.querySelector('.composer .file-chip-x').click();`);
    await p.waitFor(`!document.querySelector('.composer .file-chip')`);
   }
   // Type immediately before reload, before the 400ms debounce could save it.
   await p.eval(`document.querySelector('.composer textarea').value='Last-keystroke draft kept';document.querySelector('.composer textarea').dispatchEvent(new Event('input',{bubbles:true}));document.querySelector('.part-reload').click();`);
   await p.waitFor(`document.querySelector('.composer textarea')?.value==='Last-keystroke draft kept'`,15000);
   check(await p.eval(`!window.__page && !document.querySelector('.part-error')`),'stale deployment really reloads, loads new hash and restores the last unsaved keystroke');
   await p.waitFor(`document.querySelector('.recall-count')?.textContent==='2 matches'`,15000);
   check(await p.eval(`document.querySelector('.recall-count')?.textContent==='2 matches'`),'stale reload restores persisted server recall output with the new build');
  }else{
   check(await p.eval(`!!document.querySelector('.part-retry') && !document.querySelector('.tool.recall')`),`${kind}: transient failure remains generic until code and CSS recover together`);
   await p.click('.part-retry');await until(()=>paused[1],`${kind} fresh retry`);
   let retryRequest = paused[1];
   if (kind === 'helper') {
    check(retryRequest.request.url.includes(helper), 'helper: first local Retry requests the actual emitted recovery helper');
    await p.send('Fetch.fulfillRequest', {requestId:retryRequest.requestId,responseCode:200,responseHeaders:[{name:'Content-Type',value:'text/javascript'}],body:Buffer.from("throw new Error('URL-less synthetic helper failure');").toString('base64')});
    await p.waitFor(`document.querySelector('.part-error .part-retry')`);
    check(await p.eval(`window.__page===${JSON.stringify(page)} && window.__kept.value==='Built draft kept' && document.querySelector('.part-error').textContent.includes('URL-less synthetic helper failure') && !document.querySelector('.tool.recall')`), 'helper: URL-less evaluation failure stays recoverable without losing the draft');
    await p.click('.part-retry');await until(()=>paused[2], 'uncached helper retry');
    check(paused[2].request.url.includes(helper) && paused[2].request.url.includes('tessera_retry='), 'helper: second local Retry bypasses cached helper failure');
    await p.send('Fetch.continueRequest', {requestId:paused[2].requestId});
    await until(()=>paused[3], 'part request after repaired helper');retryRequest=paused[3];
   }
   check(retryRequest.request.url.includes('tessera_retry='),`${kind}: Retry uses an uncached actual emitted asset URL`);
   check(!await p.eval(`!!document.querySelector('.tool.recall')`),`${kind}: no partially loaded recall card while retry is paused`);
   await p.send('Fetch.continueRequest',{requestId:retryRequest.requestId});
   await p.waitFor(`document.querySelector('.recall-count')?.textContent==='2 matches'`,15000);
   check(await p.eval(`window.__page===${JSON.stringify(page)} && window.__kept===document.querySelector('.pane.focused .composer textarea') && window.__kept.value==='Built draft kept' && getComputedStyle(document.querySelector('.recall-count')).fontVariantNumeric==='tabular-nums' && !document.querySelector('.part-error')`),`${kind}: built JS + extracted CSS recover in-page with original draft/DOM`);
  }
  check(!p.errors.length,`${kind}: no uncaught runtime recovery errors`,p.errors);
  await p.send('Fetch.disable');await p.close();peer.close();
 }
 finish();
});
