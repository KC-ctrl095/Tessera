// UI + real mock RPC; the Windows shell/consent/installer are stand-ins.
// This deliberately never executes an installer or claims native acceptance.
import { APP, check, device, finish, mock, run, signIn, sleep } from './cdp.mjs';

function shell(token, supported = true) {
  return `(() => {
    const pending = new Map(), callbacks = {}, listeners = {};
    let ws, rid = 0, cid = 0;
    window.__launches = 0; window.__login = 0; window.__updateCalls = []; window.__server = 'srv_mock';
    const emit = (event, payload) => (listeners[event] || []).forEach(id => callbacks[id]?.({ event, id, payload }));
    const rpc = window.__rpc = request => new Promise((resolve, reject) => {
      const id = ++rid; pending.set(id, { resolve, reject }); ws.send(JSON.stringify({ id, ...request }));
    });
    const status = () => emit('tessera://status', { state:'connected', serverId:'srv_mock', endpoint:location.origin, encrypted:true, error:null });
    const offered = () => rpc({method:'get_app_update', params:{platform:'windows-x86_64-desktop',version:'0.1.0',installed_at:1}});
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
    window.__TAURI_INTERNALS__ = {
      metadata:{currentWindow:{label:'main'},currentWebview:{label:'main',windowLabel:'main'}},
      transformCallback(cb, once) { const id=++cid; callbacks[id]=x=>{if(once)delete callbacks[id];cb(x)}; return id; },
      async invoke(cmd,args={}) {
        if(cmd.startsWith('app_update_')) window.__updateCalls.push({cmd,args});
        if(cmd==='plugin:event|listen') { (listeners[args.event] ||= []).push(args.handler); return args.handler; }
        if(cmd==='plugin:event|unlisten') return null;
        if(cmd==='servers_get') return {servers:[{id:'srv_mock',name:'Mock server',endpoints:[location.origin],headers:{},token:'',serverKey:'',deviceName:'Update test'}],active:'srv_mock'};
        if(cmd==='server_connect') { if(ws){status();return null;} ws=new WebSocket(location.origin.replace(/^http/,'ws')+'/api/ws?token='+encodeURIComponent(${JSON.stringify(token)}));
          ws.onopen=status; ws.onmessage=m=>{const x=JSON.parse(m.data);if(x.event)emit('tessera://event',x.event);else {const p=pending.get(x.id);if(p){pending.delete(x.id);x.error?p.reject(x.error):p.resolve(x.result??null)}}}; return null; }
        if(cmd==='rpc') return rpc(args.request);
        if(cmd==='app_update_available') return ${supported};
        if(cmd==='app_update_check') {if(window.__unsupported)throw 'unknown method get_app_update'; return {serverId:window.__server,build:await offered()};}
        if(cmd==='app_update_install') {
          const login = window.__login;
          const stillCurrent = () => { if(login !== window.__login) throw 'The server login changed; check for updates again.'; };
          if(args.serverId!==window.__server)throw 'The server connection changed; check again.';
          const b=await offered(); if(!b||b.id!==args.id||b.sha256!==args.sha256)throw 'The offered update changed; check again.';
          const consent=await new Promise(resolve=>{const d=document.createElement('dialog');d.className='test-update-consent';
            d.textContent='Run the Windows setup?';for(const [label,yes] of [['Confirm',true],['Cancel',false]]){const btn=document.createElement('button');btn.textContent=label;btn.onclick=()=>{d.remove();resolve(yes)};d.append(btn)}document.body.append(d);d.showModal();});
          if(!consent)return false;
          stillCurrent();
          const chunks=[]; let off=0; while(off<b.size){const c=await rpc({method:'read_app_update',params:{id:b.id,offset:off,len:1048576}});
            stillCurrent();
            const bytes=Uint8Array.from(atob(c.data),ch=>ch.charCodeAt(0));const end=off+bytes.length;
            if(c.offset!==off||c.size!==b.size||!bytes.length||bytes.length>1048576||end>b.size||c.eof!==(end===b.size))throw 'The update download was incomplete or too large.';
            chunks.push(bytes);off=end;}
          const bytes=new Uint8Array(off);let pos=0;for(const part of chunks){bytes.set(part,pos);pos+=part.length;}
          const hash=[...new Uint8Array(await crypto.subtle.digest('SHA-256',bytes))].map(b=>b.toString(16).padStart(2,'0')).join('');
          if(hash!==b.sha256)throw "The update's SHA-256 didn't match; nothing was run.";
          stillCurrent(); window.__launches++;return true;
        }
        return null;
      }
    };
  })();`;
}
const help = async p => {
  await p.eval(`__mod('lib/ui.ts').then(u=>u.openSettings('help'))`);
  await p.waitFor(`document.querySelector('.help-version')`,8000);
};
const click = (p, selector) => p.eval(`document.querySelector(${JSON.stringify(selector)})?.click()`);
const text = p => p.eval(`document.querySelector('.app-updates')?.textContent ?? ''`);
const consent = async (p, yes) => {
  await p.waitFor(`document.querySelector('.test-update-consent')`,8000);
  await p.eval(`[...document.querySelectorAll('.test-update-consent button')].find(b=>b.textContent===${JSON.stringify(yes?'Confirm':'Cancel')}).click()`);
  await p.waitFor(`!document.querySelector('.app-update-check')?.disabled`,8000);
};
run(async()=>{
  await mock('/__mock/reset');
  const browser = await device(); await signIn(browser,'Browser updates'); await help(browser);
  check(!await browser.eval(`!!document.querySelector('.app-updates')`),'browser: no unsupported update offer');
  await browser.close();
  const p=await device(); await signIn(p,'Update shell');
  const token=await p.eval(`JSON.parse(localStorage.getItem('tessera.token'))`);
  await p.send('Page.addScriptToEvaluateOnNewDocument',{source:shell(token)});
  await p.goto(APP,600); await p.waitFor(`!!document.querySelector('.app-header')`,8000).catch(()=>{}); await help(p);
  await p.waitFor(`document.querySelector('.app-update-check')`,8000);
  check(await p.eval(`__updateCalls.every(x=>x.cmd==='app_update_available')`),'nothing checks or installs automatically');
  await click(p,'.app-update-check'); await p.waitFor(`document.querySelector('.app-update-message')`,8000);
  check(/No newer/.test(await text(p)),'no update is stated plainly');
  await p.eval(`__rpc({method:'mock_app_update',params:{notes:'<img src=x onerror=alert(1)>'}})`);
  await click(p,'.app-update-check'); await p.waitFor(`document.querySelector('.app-update-install')`,8000);
  check((await text(p)).includes('0.2.0')&&(await text(p)).includes('<img src=x'),'version and notes shown as text');
  check(!await p.eval(`!!document.querySelector('.app-updates img')`),'update notes never become HTML');
  check(await p.eval(`__launches===0 && !__updateCalls.some(x=>x.cmd==='app_update_install')`),'an offer alone does not install');
  await click(p,'.app-update-install'); await consent(p,false);
  check(/Cancelled/.test(await text(p))&&await p.eval(`__launches===0`),'native cancellation runs nothing');
  await click(p,'.app-update-install');
  await p.waitFor(`document.querySelector('.test-update-consent')`,8000);
  await p.eval(`__login++`); await consent(p,true);
  check(/login changed/.test(await text(p))&&await p.eval(`__launches===0`),'re-pairing the same saved server during consent runs nothing');
  await click(p,'.app-update-check'); await p.waitFor(`document.querySelector('.app-update-install')`,8000);
  await click(p,'.app-update-install'); await consent(p,true);
  check(/installer opened/.test(await text(p))&&await p.eval(`__launches===1`),'confirmed bytes download in pieces and match SHA-256 before simulated launch');
  for(const mode of ['corrupt','truncated']) {
    await p.eval(`__rpc({method:'mock_app_update',params:{mode:${JSON.stringify(mode)}}})`);
    await click(p,'.app-update-check'); await p.waitFor(`document.querySelector('.app-update-install')`,8000);
    await click(p,'.app-update-install'); await consent(p,true);
    check(await p.eval(`!!document.querySelector('.app-updates .form-error') && __launches===1`),mode+': error shown; nothing launched');
  }
  await p.eval(`__rpc({method:'mock_app_update',params:{mode:'normal',tag:'old'}})`);
  await click(p,'.app-update-check'); await p.waitFor(`document.querySelector('.app-update-install')`,8000);
  await p.eval(`__rpc({method:'mock_app_update',params:{tag:'new'}})`);
  await click(p,'.app-update-install'); await sleep(400);
  check(/changed/.test(await text(p))&&await p.eval(`__launches===1&&!document.querySelector('.test-update-consent')`),'offer changed before confirmation: must check again');
  await click(p,'.app-update-check'); await p.waitFor(`document.querySelector('.app-update-install')`,8000);
  await p.eval(`__server='different-server'`); await click(p,'.app-update-install'); await sleep(300);
  check(/connection changed/.test(await text(p))&&await p.eval(`__launches===1`),'changing servers cannot install an earlier offer');
  await p.eval(`__server='srv_mock';__unsupported=true`); await click(p,'.app-update-check'); await sleep(300);
  check(/unknown method/.test(await text(p))&&!await p.eval(`!!document.querySelector('.app-update-install')`),'old server error clears a previous offer');
  await p.viewport(344,882,true,2.625); await sleep(300);
  check(await p.eval(`document.documentElement.scrollWidth<=innerWidth+1`),'update controls fit a narrow screen');
  check(p.errors.length===0,'no script errors',p.errors); await p.close(); finish();
});
