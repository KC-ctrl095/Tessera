// Real assist boot/event entry and voice host with a synthetic Android IPC bridge.
import { APP, MOCK, SHOTS, check, device, finish, mock, run, sleep, signIn } from './cdp.mjs';

function shell(token, cold = true) {
  return `(() => {
    const callbacks={}, listeners={}, pending=new Map(); let id=0,rid=0,ws,assist;
    window.__assistPending=${cold}; window.__locked=false; window.__starts=0; window.__sent=0; window.__said=[];
    window.__assist=()=>{window.__assistPending=true;assist?.onmessage({ignoredText:'This must never become a prompt'});};
    const emit=(event,payload)=>(listeners[event]||[]).forEach(id=>callbacks[id]?.({event,id:1,payload}));
    window.__TAURI_EVENT_PLUGIN_INTERNALS__={unregisterListener(){}};
    window.__TAURI_INTERNALS__={metadata:{currentWindow:{label:'main'},currentWebview:{label:'main',windowLabel:'main'}},transformCallback(cb){callbacks[++id]=cb;return id},unregisterCallback(){},async invoke(cmd,args={}){
      switch(cmd){
        case 'plugin:event|listen':(listeners[args.event]||=[]).push(args.handler);return args.handler;
        case 'plugin:event|unlisten':return null;
        case 'servers_get':return {servers:[{id:'srv_mock',name:'Mock',endpoints:[location.origin],headers:{},token:'',serverKey:'',deviceName:'Assistant test'}],active:'srv_mock'};
        case 'servers_save':return null;
        case 'server_connect':if(!ws){ws=new WebSocket(location.origin.replace(/^http/,'ws')+'/api/ws?token='+encodeURIComponent(${JSON.stringify(token)}));ws.onopen=()=>emit('tessera://status',{state:'connected',serverId:'srv_mock',endpoint:location.origin,error:null,encrypted:true});ws.onmessage=e=>{const m=JSON.parse(e.data);if(m.event)emit('tessera://event',m.event);else if(pending.has(m.id)){const p=pending.get(m.id);pending.delete(m.id);m.error?p.reject(m.error):p.resolve(m.result??null)}}}return null;
        case 'rpc':if(args.request.method==='send')window.__sent++;return new Promise((resolve,reject)=>{const id=++rid;pending.set(id,{resolve,reject});ws.send(JSON.stringify({id,method:args.request.method,params:args.request.params}))});
        case 'assistant_watch':assist=args.events;return null;
        case 'assistant_take_pending':{if(window.__locked)return {pending:false,listen:false};const value=!!window.__assistPending;window.__assistPending=false;return {pending:value,listen:value&&!window.__fromApp};}
        case 'voice_status':return {listen:true,speak:true};
        case 'voice_listen':window.__starts++;window.__words=(text)=>args.events.onmessage({text,final:true});return null;
        case 'voice_say':window.__said.push(args.text);return null;
        case 'voice_cancel':case 'voice_finish':case 'voice_quiet':case 'app_resumed':case 'server_disconnect':case 'notifications_get':case 'notifications_seen':case 'notifications_take_opened':return null;
        default:throw 'command '+cmd+' not found';
      }
    }};
  })()`;
}
const sid = `__mod('lib/workspace.ts').then(m=>m.workspace.panes[m.workspace.focused()]?.content.sessionId)`;
const listening = `document.querySelector('.voice-status[data-phase="listening"]')`;
run(async () => {
  for (const [label,w,h] of [['cover',344,812],['fold',841,701],['tablet',1280,800]]) {
    for (const theme of ['dark','light']) {
      await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
      const token=(await(await fetch(MOCK+'/api/pair',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({code:'123456',device_name:'Assistant test'})})).json()).token;
      const p=await device();await p.viewport(w,h,true,2);
      await p.send('Page.addScriptToEvaluateOnNewDocument',{source:shell(token)});
      await p.goto(APP);await p.waitFor(listening,20000);await p.theme(theme);
      const first=await p.eval(sid);
      check(await p.eval(`__starts===1 && __sent===0 && !!document.querySelector('[aria-label="End voice conversation"]')`),`${label}/${theme} cold assist listens once, visible End, no message sent`);
      check(await p.eval(`__mod('lib/store.ts').then(m=>m.state.sessions[${JSON.stringify(first)}].info.title==='Assistant' && JSON.stringify(m.state.sessions[${JSON.stringify(first)}].info.model)===JSON.stringify(m.defaultModel()))`),`${label}/${theme} assistant uses normal default model`);
      await p.shot(`${SHOTS}/assistant-${label}-${theme}-cold.png`);
      await p.click('[aria-label="End voice conversation"]');await p.eval(`__assist()`);await p.waitFor(listening);
      check(await p.eval(sid)===first,`${label}/${theme} quick repeat continues previous assistant chat`);
      await p.eval(`__words('Please tell me a short story about the moon.')`);
      const spoke = await p.waitFor(`__said.length>0`,20000).then(()=>true,()=>false);
      check(spoke && await p.eval(`__sent===1 && __said.length>0`),`${label}/${theme} actual speech reaches normal agent and answer is spoken`);
      await p.shot(`${SHOTS}/assistant-${label}-${theme}-warm.png`);
      await p.click('[aria-label="End voice conversation"]');
      await p.eval(`__mod('lib/store.ts').then(m=>m.setState('sessions',${JSON.stringify(first)},'info','updated_at',0));localStorage.setItem('tessera.phone-assistant.srv_mock.recent',JSON.stringify({id:${JSON.stringify(first)},at:Date.now()-301000}));__assist()`);
      await p.waitFor(listening);
      check(await p.eval(sid)!==first,`${label}/${theme} after five minutes opens a fresh chat`);
      await p.click('[aria-label="End voice conversation"]');await p.eval(`__mod('lib/ui.ts').then(m=>m.setSettingsTab('web'))`);
      await p.waitFor(`document.querySelector('[aria-label="Assistant chat"]')`);
      check(await p.eval(`document.querySelector('#set-phone-assistant').textContent.includes('Side button')`),`${label}/${theme} Android instructions present`);
      check(await p.eval(`(()=>{const e=document.querySelector('[aria-label="Assistant chat"]');e.scrollIntoView({block:'center'});const r=e.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth+1&&r.height>=43.5})()`),`${label}/${theme} setting fits with 44px target`);
      await p.shot(`${SHOTS}/assistant-${label}-${theme}-settings.png`);
      await p.eval(`__mod('lib/phoneAssistant.ts').then(m=>m.setAssistantChoice('continue'));__assist()`);await p.waitFor(listening);
      const fixed=await p.eval(sid);await p.click('[aria-label="End voice conversation"]');
      await p.eval(`localStorage.setItem('tessera.phone-assistant.srv_mock.continue',JSON.stringify({id:${JSON.stringify(fixed)},at:1}));__assist()`);await p.waitFor(listening);
      check(await p.eval(sid)===fixed,`${label}/${theme} always-continue ignores age`);
      await p.click('[aria-label="End voice conversation"]');
      // Sent by another app (not the system's gesture): the chat opens, the microphone stays off until a tap.
      const before=await p.eval('__starts');await p.eval(`__fromApp=true;__assist()`);await p.waitFor(`[...document.querySelectorAll('.toast')].some(t=>t.textContent.includes('Tap the microphone to talk'))`);await sleep(300);
      check(await p.eval(`__starts===${before}&&!document.querySelector('.voice-status')&&__sent===1`),`${label}/${theme} another app's assist opens the chat without listening`);
      await p.eval(`__fromApp=false`);
      const starts=await p.eval('__starts');await p.eval(`__locked=true;__assist()`);await sleep(200);
      check(await p.eval(`__starts===${starts}&&!document.querySelector('.voice-status')`),`${label}/${theme} locked native entry cannot start listening`);
      await p.eval(`__locked=false;__assist()`);await p.waitFor(listening);
      await p.click('[aria-label="End voice conversation"]');
      const bot=await p.eval(`__mod('lib/store.ts').then(m=>m.state.bots[0]?.id)`);
      if(bot){await p.eval(`__mod('lib/phoneAssistant.ts').then(m=>m.setAssistantChoice('bot:'+${JSON.stringify(bot)}));__assist()`);await p.waitFor(listening);check(await p.eval(`__mod('lib/store.ts').then(async m=>m.state.sessions[await (${sid})].info.bot_id===${JSON.stringify(bot)})`),`${label}/${theme} selected bot receives its own assistant chat`);await p.click('[aria-label="End voice conversation"]');}
      check(!p.errors.length,`${label}/${theme} no uncaught errors`,p.errors);await p.close();
    }
  }
  // Combined A/V contract. A opens the normal host, which offers V's explicit
  // engine choice and resumes this same assistant after the user opts in.
  await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
  {
    const token=(await(await fetch(MOCK+'/api/pair',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({code:'123456',device_name:'Voice off test'})})).json()).token;
    const p=await device();await p.viewport(344,812,true,2);
    await p.send('Page.addScriptToEvaluateOnNewDocument',{source:shell(token,false)});await p.goto(APP);
    await p.waitFor(`__mod('lib/store.ts').then(m=>m.state.ready)`);
    await p.eval(`__mod('lib/store.ts').then(async m=>{const c=await m.api.getVoice();await m.api.saveVoice({...c,engine:'off'},null);m.setState('server',await m.api.hello());__assist();})`);
    await p.waitFor(`document.querySelector('.voice-choice')`);
    check(await p.eval(`__starts===0&&__sent===0&&document.body.innerText.includes('Voice is off')`),'voice-off assist opens explicit choice without listening or sending');
    await p.shot(`${SHOTS}/assistant-cover-dark-voice-off.png`);
    await p.eval(`Array.from(document.querySelectorAll('.voice-choice button')).find(b=>b.textContent.includes('On this device')).click()`);
    await p.waitFor(listening);
    check(await p.eval(`__starts===1&&__sent===0`),'explicit device choice immediately resumes assistant listening');
    await p.click('[aria-label="End voice conversation"]');await p.close();
  }
  // Android browsers and desktop shells must not advertise a native phone feature.
  for(const mobile of [false,true]){
    await mock('/__mock/reset');const p=await device();await p.viewport(mobile?344:1440,900,mobile,1);await signIn(p,'Assistant browser');
    await p.eval(`__mod('lib/ui.ts').then(m=>m.setSettingsTab('web'))`);await p.waitFor(`document.querySelector('#set-voice')`);
    check(await p.eval(`!document.querySelector('#set-phone-assistant')`),`${mobile?'Android browser':'desktop browser'} hides Phone assistant`);await p.close();
  }
  finish();
});
