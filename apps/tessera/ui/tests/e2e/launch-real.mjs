// Real `tessera serve` only, encrypted /web/ transport, production build.
// APP=http://localhost:18101/web/ LAUNCH_HOME=/throwaway E2E_OUT=/output node tests/e2e/launch-real.mjs
import fs from 'node:fs';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import { APP, SCRATCH, check, device, finish, run, sleep } from './cdp.mjs';
const home = process.env.LAUNCH_HOME;
if (!home || !fs.existsSync(path.join(home, 'LAUNCH-PERF-THROWAWAY'))) throw Error('Throwaway marker required');
const stats = a => { a.sort((x,y)=>x-y); return { n:a.length, p95:a[Math.floor(a.length*.95)]??0, max:a.at(-1)??0, total:a.reduce((x,y)=>x+y,0) }; };
const source = `(() => {
  const m = window.__launch = { at: performance.now(), calls: [], decryptMs: 0, parseMs: 0, installMs: [], snapshotMs: [], long: [], lag: [], sidebar: null, newest: null, firstComposer: null, pending: new Map() };
  m.sent = (id, method) => { const c = {id, method, at: performance.now()-m.at}; m.calls.push(c); m.pending.set(id,c); };
  m.received = (msg, bytes, ms) => { m.parseMs += ms; const c = m.pending.get(msg.id); if(c) { c.bytes = bytes; c.ms = performance.now()-m.at-c.at; c.parseMs=ms; m.pending.delete(msg.id); } };
  m.decrypt = ms => m.decryptMs += ms;
  m.installed = ms => m.installMs.push(ms);
  m.snapshot = ms => m.snapshotMs.push(ms);
  new PerformanceObserver(l => { for(const e of l.getEntries()) m.long.push(e.duration); }).observe({type:'longtask'});
  let prev=performance.now(); setInterval(() => {const now=performance.now(); m.lag.push(Math.max(0,now-prev-25));prev=now;},25);
  const tick = () => {
    const now=performance.now()-m.at, s=window.__launchStore?.state;
    if(m.sidebar === null && s?.sessionsLoaded && Object.keys(s.sessions).some(id=>id==='perf-chat-0411')) m.sidebar=now;
    const composer=document.querySelector('.pane.focused .composer textarea');
    if(m.firstComposer===null && composer) {m.firstComposer=now;composer.focus();}
    const t=document.querySelector('.pane.focused .transcript');
    const msg=t && [...t.querySelectorAll('.msg-assistant')].at(-1);
    if(m.newest === null && msg?.textContent.includes('Newest synthetic message perf-chat-0000.') && msg.getBoundingClientRect().top < t.getBoundingClientRect().bottom) requestAnimationFrame(()=>{m.newest ??= performance.now()-m.at;});
    requestAnimationFrame(tick);
  }; requestAnimationFrame(tick);
})()`;
async function result(p, label, typing) {
  const m = await p.eval(`(() => { const m=__launch; return {sidebar:m.sidebar,newest:m.newest,firstComposer:m.firstComposer,calls:m.calls,decryptMs:m.decryptMs,parseMs:m.parseMs,installMs:m.installMs,snapshotMs:m.snapshotMs,long:m.long,lag:m.lag,rows:Object.keys(__launchStore.state.sessions).length,typed:document.querySelector('.pane.focused .composer textarea')?.value};})()`);
  const out={label,...m,long:stats(m.long),lag:stats(m.lag),input:stats(typing)};
  fs.mkdirSync(SCRATCH,{recursive:true}); fs.writeFileSync(path.join(SCRATCH,label+'.json'),JSON.stringify(out,null,2));
  console.log('LAUNCH',JSON.stringify(out));
  check(m.newest!==null && m.sidebar!==null, label+' paints restored newest messages and pinned sidebar');
  check(!p.errors.length,label+' has no browser errors',p.errors);
  return out;
}
async function catalogChecks(p) {
  await p.waitFor(`Object.values(__launchStore.state.sessions).filter(s=>s.info.kind==='chat').length===412`);
  check(await p.eval(`Object.values(__launchStore.state.sessions).every(s=>s.info.kind!=='job')`), 'idle job histories are absent from the startup catalog');
  const pinTitle = await p.eval(`__launchStore.state.sessions['perf-chat-0411'].info.title`);
  const oldTitle = await p.eval(`__launchStore.state.sessions['perf-chat-0400'].info.title`);
  check(await p.eval(`Array.from(document.querySelectorAll('.srow-main')).some(e=>e.getAttribute('aria-label')===${JSON.stringify(pinTitle)}&&e.querySelector('.srow-pin'))`), 'old pinned chat is visible without scrolling through older pages');
  await p.click('.sidebar input[placeholder="Search"]');
  await p.type('Synthetic chat preview 400');
  await p.waitFor(`Array.from(document.querySelectorAll('.srow-main')).some(e=>e.getAttribute('aria-label')===${JSON.stringify(oldTitle)})`);
  check(true,'sidebar search finds an older-page chat');
  await p.click('.sidebar input[placeholder="Search"]');
  await p.key('a','KeyA',2,65); await p.key('Backspace','Backspace',0,8);
  check(await p.eval(`Object.values(__launchStore.state.sessions).filter(s=>s.info.parent_id==='perf-chat-0000'&&s.info.kind==='subagent').length===8`), 'restored chat retains its eight helpers');
  await p.click('[role="tab"][aria-label="Scheduled"]');
  check(await p.eval(`document.querySelector('.sidebar').textContent.includes('Synthetic schedule 0')`), 'scheduled-job definitions remain visible without startup run sessions');
  await p.eval(`__launchWorkspace.openSession('perf-job-3383')`);
  await p.waitFor(`__launchStore.state.sessions['perf-job-3383']?.loaded`);
  check(true,'an omitted historical job run opens on demand');
  await p.eval(`__launchStore.api.updateSession('perf-chat-0401',{archived:true})`);
  await p.eval(`__launchStore.refreshArchived()`);
  check(await p.eval(`__launchStore.state.archived.some(i=>i.id==='perf-chat-0401')`),'archived metadata is fetched on demand');
  await p.eval(`__launchStore.api.updateSession('perf-chat-0401',{archived:false})`);
  const created = await p.eval(`(async()=>{
    const api=__launchStore.api;
    const bot=await api.saveBot({id:'',name:'Synthetic launch bot',handle:'syntheticlaunch'+Date.now(),avatar:'S',color:'#55aaff',role:'Synthetic only',personality:'',instructions:'Do not make model requests',permission:'default',tools:'read_only'});
    const chat=await api.createSession({botId:bot.id,title:'Synthetic bot chat',model:{provider:'synthetic',model:'synthetic'}});
    const privateChat=await api.createSession({private:true,model:{provider:'synthetic',model:'synthetic'}});
    return {bot:bot.id,chat:chat.id,private:privateChat.id};
  })()`);
  await p.eval(`__launchBackend.disconnect().then(()=>__launchBackend.connect())`);
  await p.waitFor(`__launchStore.state.sessions[${JSON.stringify(created.private)}]?.info.private && __launchStore.state.sessions[${JSON.stringify(created.chat)}]?.info.bot_id===${JSON.stringify(created.bot)}`);
  check(true,'bot chats and this device\'s private chat survive reconnect');
  await p.eval(`(async()=>{const a=__launchStore.api;await a.deleteSession(${JSON.stringify(created.chat)});await a.deleteSession(${JSON.stringify(created.private)});await a.deleteBot(${JSON.stringify(created.bot)});__launchWorkspace.openSession('perf-chat-0000');})()`);
  await p.click('[role="tab"][aria-label="Chats"]');
  await sleep(1000);
}

async function measure(p,label,action) {
  const typing=[]; await action();
  // CDP input is sent from outside the renderer so long tasks count as latency,
  // not silently postponed by an in-page synthetic event timer.
  const started=Date.now();
  for(let i=0;i<200 && Date.now()-started<90000;i++) {const at=Date.now(); await p.send('Input.insertText',{text:'x'});await p.eval('new Promise(r=>requestAnimationFrame(r))');typing.push(Date.now()-at);await sleep(40);if(i>=24 && await p.eval('__launch.newest !== null && __launch.installMs.length > 0 && __launch.snapshotMs.length > 0 && __launch.pending.size===0')) break;}
  try { await p.waitFor('__launch.newest !== null && __launch.sidebar !== null',30000); } catch (error) { console.log('DUMP', await p.eval('JSON.stringify({launch:__launch,body:document.body.innerText.slice(0,3000),ready:__launchStore.state.ready,conn:__launchStore.state.conn})'), p.errors); throw error; }
  await sleep(1500); return result(p,label,typing);
}
run(async()=>{
  for(const [label,w,h,mobile,rate] of [['desktop',1440,900,false,1],['phone-4x',390,844,true,4],['cover-4x',344,844,true,4],['phone-6x',390,844,true,6],['cover-6x',344,844,true,6]]) {
    if(process.env.ONLY && !process.env.ONLY.split(',').includes(label)) continue;
    const p=await device(); await p.viewport(w,h,mobile,mobile?2:1);
    await p.goto(APP,1000); await p.waitFor('window.__launchBackend',60000);
    execFileSync('python3',['-c',`import sqlite3,time,sys; c=sqlite3.connect(sys.argv[1]+'/tessera.db'); c.execute('delete from pairing_codes'); c.execute('insert into pairing_codes values(?,?)',('1234-5678',int(time.time()*1000)+86400000)); c.commit()`,home]);
    const code='1234-5678';
    await p.eval(`__launchBackend.pair({endpoints:[],headers:{},code:${JSON.stringify(code)},deviceName:'Synthetic launch ${label}',name:'Synthetic'})`);
    // Obtain a fresh one-time code for the next isolated browser.
    // The runner inserts a fresh code into this throwaway database between cases.
    await p.eval('__launchBackend.connect()');
    await p.waitFor('__launchStore.state.ready',60000);
    // Use the real workspace API, then let its normal persistence settle.
    // Saving a raw layout during bootstrap races the normal mosaic sync.
    await sleep(2000); // mosaic sync and debounce must settle before the test setup
    await p.eval(`__launchWorkspace.openSession('perf-chat-0000')`);
    await p.waitFor(`__launchStore.state.sessions['perf-chat-0000']?.loaded`,60000);

    await sleep(2000);
    await p.send('Page.addScriptToEvaluateOnNewDocument',{source});
    await p.send('Emulation.setCPUThrottlingRate',{rate});
    await p.send('Network.clearBrowserCache');
    if(process.env.LAUNCH_PROFILE) { await p.send('Profiler.enable'); await p.send('Profiler.start'); }
    await measure(p,label+'-cold',()=>p.reload(500));
    if(process.env.LAUNCH_PROFILE) { const {profile}=await p.send('Profiler.stop');fs.writeFileSync(path.join(SCRATCH,label+'.cpuprofile'),JSON.stringify(profile)); }
    await measure(p,label+'-warm',()=>p.reload(500));
    await measure(p,label+'-reconnect',async()=>{
      await p.eval(source);
      await p.eval('(__launchBackend.disconnect().then(()=>__launchBackend.connect()))');
    });
    await p.send('Emulation.setCPUThrottlingRate',{rate:1});
    if(process.env.LAUNCH_CHECKS && label==='desktop') await catalogChecks(p);
    await p.conn.send('Target.disposeBrowserContext', {browserContextId:p.contextId});
  }
  finish();
});
