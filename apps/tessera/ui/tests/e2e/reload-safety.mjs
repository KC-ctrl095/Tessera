// Native-backed backup UI uses the real PartError/workspace/saveText path.
import { APP, MOCK, check, device, finish, mock, run, sleep, wsClient } from './cdp.mjs';

function shell(token) {
 return `(()=>{
  const callbacks={},listeners={},pending=new Map();let id=0,rid=0,ws;
  window.__saved=[];window.__saveMode='success';
  const emit=(event,payload)=>(listeners[event]||[]).forEach(id=>callbacks[id]?.({event,id:1,payload}));
  window.__TAURI_EVENT_PLUGIN_INTERNALS__={unregisterListener(){}};
  window.__TAURI_INTERNALS__={metadata:{currentWindow:{label:'main'},currentWebview:{label:'main',windowLabel:'main'}},transformCallback(cb){callbacks[++id]=cb;return id},async invoke(cmd,args={}){
   switch(cmd){
    case 'plugin:event|listen':(listeners[args.event]||=[]).push(args.handler);return args.handler;
    case 'plugin:event|unlisten':return null;
    case 'servers_get':return {servers:[{id:'srv_mock',name:'Mock',endpoints:[location.origin],headers:{},token:'',serverKey:'',deviceName:'Backup app'}],active:'srv_mock'};
    case 'servers_save':return null;
    case 'server_connect':if(!ws){ws=new WebSocket(location.origin.replace(/^http/,'ws')+'/api/ws?token='+encodeURIComponent(${JSON.stringify(token)}));ws.onopen=()=>emit('tessera://status',{state:'connected',serverId:'srv_mock',endpoint:location.origin,error:null,encrypted:true});ws.onmessage=e=>{const m=JSON.parse(e.data);if(m.event)emit('tessera://event',m.event);else if(pending.has(m.id)){const p=pending.get(m.id);pending.delete(m.id);m.error?p.reject(m.error):p.resolve(m.result??null)}}}return null;
    case 'rpc':return new Promise((resolve,reject)=>{const id=++rid;pending.set(id,{resolve,reject});ws.send(JSON.stringify({id,method:args.request.method,params:args.request.params}))});
    case 'app_resumed':case 'server_disconnect':case 'notifications_get':return null;
    case 'file_save':window.__saved.push(args);if(window.__saveMode==='cancel')throw 'cancelled';if(window.__saveMode==='failure')throw 'Synthetic native save failed';return {name:args.name,location:'/tmp/synthetic-backup.json',place:'Chosen folder'};
    default:throw 'command '+cmd+' not found';
   }
  }};
  localStorage.setItem('tessera.drafts.v2',JSON.stringify({'["other-server","retained-private",null]':{paneId:'retained-private',text:'UNRELATED PRIVATE TEXT',private:true,title:'Other retained private chat'},'["other-server","retained-normal",null]':{paneId:'retained-normal',text:'UNRELATED NORMAL TEXT',private:false}}));
 })()`;
}
run(async()=>{
 await mock('/__mock/reset');await mock('/__mock/mosaics/reset');
 const peer=await wsClient('Backup fixture');const f=(await peer.rpc('mock_recall_lazy',{phase:'seed'})).result;
 const token=(await (await fetch(MOCK+'/api/pair',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({code:'123456',device_name:'Backup app'})})).json()).token;
 const p=await device();await p.viewport(344,780,true,2);
 // This shell mocks the desktop native save result (a user-chosen folder),
 // not Android's Downloads contract. Keep its narrow touch viewport while
 // making the native platform agree with that result and the exact assertion.
 await p.send('Emulation.setUserAgentOverride',{userAgent:'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36'});
 await p.send('Page.addScriptToEvaluateOnNewDocument',{source:shell(token)});
 const paused=[];p.on('Fetch.requestPaused',e=>paused.push(e));
 await p.send('Fetch.enable',{patterns:[{urlPattern:'*/src/components/chat/RecallCard.tsx*',requestStage:'Request'}]});
 await p.goto(APP);await p.waitFor(`document.querySelector('.workspace')`,15000);
 await p.eval(`__mod('lib/workspace.ts').then(m=>{window.__heroPane=m.workspace.focused();m.setPaneContent(window.__heroPane,{type:'chat',sessionId:null,draft:{private:true}});window.__heroIdentity=m.draftIdentity(window.__heroPane);})`);
 await p.waitFor(`document.querySelector('.private-hero .composer textarea')`);await p.click('.private-hero .composer textarea');await p.type('RETAINED HERO PRIVATE TEXT');
 await p.eval(`window.__heroComposer=document.querySelector('.private-hero .composer textarea');__mod('lib/workspace.ts').then(m=>m.openSession(${JSON.stringify(f.id)},{pane:window.__heroPane}))`);
 check(await p.eval(`!window.__heroComposer.isConnected`),'private hero unmounted after switching to a public chat');
 await p.waitFor(`document.querySelector('.composer textarea')`);await p.click('.composer textarea');await p.type('Blocking public composer');
 await peer.rpc('mock_recall_lazy',{phase:'stream'});for(let i=0;i<125&&!paused[0];i++)await sleep(80);
 await p.send('Fetch.failRequest',{requestId:paused[0].requestId,errorReason:'Failed'});await p.waitFor(`document.querySelector('.part-error')`);
 await p.eval(`{const dt=new DataTransfer();dt.items.add(new File(['Only blocking bytes'],'blocking.bin'));const el=document.querySelector('.composer input[type="file"][multiple]');el.files=dt.files;el.dispatchEvent(new Event('change',{bubbles:true}));}`);
 await p.waitFor(`document.querySelector('.file-chip')`);await p.click('.part-reload');
 for(const mode of ['success','failure','cancel']){
  await p.eval(`window.__saveMode=${JSON.stringify(mode)};document.querySelector('.part-save').click();`);await p.waitFor(`window.__saved.length===${['success','failure','cancel'].indexOf(mode)+1}`);await sleep(200);
  check(await p.eval(`window.__saved.at(-1).name==='tessera-draft-backup.json' && !atob(window.__saved.at(-1).data).includes('UNRELATED') && !atob(window.__saved.at(-1).data).includes('RETAINED HERO PRIVATE TEXT')`),`${mode}: backup uses native file_save and excludes unrelated retained/private drafts`);
  if(mode==='success')check(await p.eval(`document.body.textContent.includes('Saved to Chosen folder') && JSON.parse(atob(window.__saved[0].data)).attachments[0].name==='blocking.bin'`),'native success feedback and blocking attachment bytes');
  if(mode==='failure')check(await p.eval(`document.body.textContent.includes('Synthetic native save failed')`),'native failure feedback is visible');
  check(await p.eval(`!!document.querySelector('.part-error') && !!document.querySelector('.file-chip')`),`${mode}: save/cancel never silently reloads or drops blocking state`);
 }
 // A retained private hero remains a Reload blocker even though its Composer unmounted.
 check(await p.eval(`!![...document.querySelectorAll('.private-reload-draft')].find(row=>row.dataset.draft===window.__heroIdentity && row.textContent.includes('New private chat') && row.querySelector('.part-open-private') && row.querySelector('.part-discard-private'))`),'retained private hero has a named banner with Open/Discard while public chat is shown');
 await p.eval(`[...document.querySelectorAll('.private-reload-draft')].find(row=>row.dataset.draft===window.__heroIdentity).querySelector('.part-open-private').click();`);
 check(await p.eval(`__mod('lib/workspace.ts').then(m=>m.getDraft(window.__heroPane)==='Blocking public composer' && m.draftIdentity(window.__heroPane)!==window.__heroIdentity && document.body.textContent.includes('Nothing was moved'))`),'Open refuses an occupied destination and preserves both drafts');
 await p.eval(`__mod('lib/workspace.ts').then(m=>{m.setDraft(window.__heroPane,'');document.querySelector('.file-chip-x').click();})`);
 await p.eval(`[...document.querySelectorAll('.private-reload-draft')].find(row=>row.dataset.draft===window.__heroIdentity).querySelector('.part-open-private').click();`);
 await p.waitFor(`document.querySelector('.pane.focused .composer textarea')?.value==='RETAINED HERO PRIVATE TEXT'`);
 check(await p.eval(`document.querySelector('.pane.focused .private-hero .composer textarea').value==='RETAINED HERO PRIVATE TEXT'`),'Open restores the retained private hero and text');
 await p.eval(`__mod('lib/workspace.ts').then(m=>{window.__openedHeroIdentity=m.draftIdentity(m.workspace.focused());m.openSession(${JSON.stringify(f.id)},{pane:window.__heroPane});})`);
 await p.waitFor(`document.querySelector('.part-error')`);
 await p.eval(`[...document.querySelectorAll('.private-reload-draft')].find(row=>row.dataset.draft===window.__openedHeroIdentity).querySelector('.part-discard-private').click();`);
 check(await p.eval(`__mod('lib/workspace.ts').then(m=>!m.privateReloadDrafts().some(d=>d.identity===window.__openedHeroIdentity || d.identity===window.__heroIdentity))`),'Discard explicitly clears the selected retained private draft, not unrelated private text');
 check(await p.eval(`window.__openedHeroIdentity===window.__heroIdentity`),'Open re-shows the original identity without transferring text');
 // Attachment-only destinations must not be overwritten; empty destinations reattach by identity.
 await p.eval(`__mod('lib/workspace.ts').then(m=>{m.setPaneContent(window.__heroPane,{type:'chat',sessionId:null});window.__destination=m.draftIdentity(window.__heroPane);const a={id:'dest',name:'destination.bin',mime:'application/octet-stream',data:btoa('DESTINATION BYTES')};m.setComposerAttachments(window.__destination,[a]);window.__targetPane=m.openPane({type:'chat',sessionId:null,draft:{private:true}});window.__target=m.draftIdentity(window.__targetPane);m.setComposerAttachments(window.__target,[{...a,id:'source',name:'source.bin'}]);m.closePane(window.__targetPane);m.openPrivateDraft(window.__target);})`);
 check(await p.eval(`__mod('lib/workspace.ts').then(m=>m.draftIdentity(window.__targetPane)===window.__target && m.composerAttachments(window.__destination)[0].name==='destination.bin' && m.composerAttachments(window.__target)[0].name==='source.bin')`),'attachment-only focused destination is preserved; closed own pane reopens at its own identity');
 // Replace the source's pane with another chat identity carrying only attachments.
 await p.eval(`__mod('lib/workspace.ts').then(m=>{m.setPaneContent(window.__targetPane,{type:'chat',sessionId:${JSON.stringify(f.other)}});m.setComposerAttachments(m.draftIdentity(window.__targetPane),[{id:'occupied',name:'occupied.bin',mime:'application/octet-stream',data:'QQ=='}]);window.__beforeOpen=JSON.stringify(m.workspace.panes);m.openPrivateDraft(window.__target);})`);
 check(await p.eval(`__mod('lib/workspace.ts').then(m=>JSON.stringify(m.workspace.panes)===window.__beforeOpen && m.composerAttachments(m.draftIdentity(window.__targetPane))[0].name==='occupied.bin' && m.composerAttachments(window.__target)[0].name==='source.bin')`),'Open refuses an attachment-only original-pane collision and preserves both identities');
 // Same session in two panes: no openSession reuse, move or merge.
 await p.eval(`__mod('lib/store.ts').then(m=>m.setState('sessions',${JSON.stringify(f.id)},'info','private',true))`);
 await p.eval(`__mod('lib/workspace.ts').then(m=>{window.__sameA=m.openPane({type:'chat',sessionId:${JSON.stringify(f.id)}});m.setDraft(window.__sameA,'FIRST SAME SESSION');window.__sameIdentity=m.draftIdentity(window.__sameA);m.setComposerAttachments(window.__sameIdentity,[{id:'same',name:'same.bin',mime:'application/octet-stream',data:'QQ=='}]);window.__sameB=m.openPane({type:'chat',sessionId:${JSON.stringify(f.id)}});m.setDraft(window.__sameB,'SECOND SAME SESSION');m.setPaneContent(window.__sameA,{type:'chat',sessionId:${JSON.stringify(f.other)}});})`);
 // Mark this retained session draft private in the fixture without changing the other draft.
 await p.eval(`__mod('lib/workspace.ts').then(m=>m.openPrivateDraft(window.__sameIdentity))`);
 check(await p.eval(`__mod('lib/workspace.ts').then(m=>m.getDraft(window.__sameA)==='FIRST SAME SESSION' && m.getDraft(window.__sameB)==='SECOND SAME SESSION' && m.draftIdentity(window.__sameA)===window.__sameIdentity)`),'same-session Open preserves both panes and their distinct drafts');
 // Mounted and retained attachment-bearing Discard clears the whole identity.
 for(const retained of [false,true]){
  await p.eval(`__mod('lib/workspace.ts').then(m=>{window.__discardPane=m.openPane({type:'chat',sessionId:null,draft:{private:true}});window.__discardId=m.draftIdentity(window.__discardPane);m.setDraft(window.__discardPane,'DISCARD TEXT');m.setComposerAttachments(window.__discardId,[{id:'discard',name:'discard.bin',mime:'application/octet-stream',data:'QQ=='}]);m.stageAttachments(window.__discardPane,[{id:'staged',name:'staged.bin',mime:'application/octet-stream',data:'QQ=='}]);${retained?'m.closePane(window.__discardPane);':''}m.discardPrivateDraft(window.__discardId);})`);
  check(await p.eval(`__mod('lib/workspace.ts').then(m=>!m.getDraft(window.__discardPane,window.__discardId) && !m.composerAttachments(window.__discardId).length && !m.privateReloadDrafts().some(d=>d.identity===window.__discardId))`),`${retained?'retained':'mounted'} Discard clears text, staged/retained attachments and removes its blocker`);
 }
 await p.eval(`__mod('lib/workspace.ts').then(m=>{window.__privatePane=m.openPane({type:'chat',sessionId:null,draft:{private:true}});m.setDraft(window.__privatePane,'BLOCKING PRIVATE TEXT');window.__privateIdentity=m.draftIdentity(window.__privatePane);})`);
 await p.waitFor(`!![...document.querySelectorAll('.private-reload-draft')].find(row=>row.dataset.draft===window.__privateIdentity)`);
 await p.eval(`[...document.querySelectorAll('.private-reload-draft')].find(row=>row.dataset.draft===window.__privateIdentity).querySelector('input[type="checkbox"]').click();`);
 check(await p.eval(`document.querySelector('.part-error').textContent.includes('selected private drafts')`),'selected private backup has an explicit private-data warning');
 await p.eval(`window.__saveMode='success';document.querySelector('.part-save').click();`);await p.waitFor(`window.__saved.length===4`);
 check(await p.eval(`atob(window.__saved[3].data).includes('BLOCKING PRIVATE TEXT') && !atob(window.__saved[3].data).includes('UNRELATED PRIVATE TEXT')`),'explicitly warned blocking private composer is backed up, unrelated private drafts are excluded');
 check(!p.errors.length,'no uncaught native backup errors',p.errors);
 await p.send('Fetch.disable');await p.close();peer.close();finish();
});
