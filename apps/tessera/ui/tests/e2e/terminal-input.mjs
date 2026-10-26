import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
run(async () => {
  await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
  const probe=await wsClient('input probe');
  const p=await device(); await p.viewport(1440,900); await signIn(p,'Terminal input');
  const unit=await p.eval(`__mod('lib/terminal.ts').then(async m=>{
    const sent=[]; let errors=0, reject=true, alive=true;
    const q=new m.TerminalInputQueue(async data=>{sent.push(data);if(reject)throw {code:'refused'};},()=>errors++,()=>alive);
    const wait=()=>new Promise(r=>setTimeout(r,10));
    q.input('z'.repeat(70000));await wait();const stopped=sent.length===1;
    q.input('again');await wait();const once=errors===1;
    reject=false;q.input('abc');q.input('def');await wait();const ordered=sent.slice(-2).join('')==='abcdef';
    reject=true;q.input('after recovery');await wait();const reset=errors===2;
    alive=false;const before=sent.length;q.input('disposed');await wait();
    return {stopped,once,ordered,reset,disposed:sent.length===before};
  })`);
  for(const key of Object.keys(unit))check(unit[key],`input queue: ${key}`);
  await probe.rpc('set_terminal_settings',{settings:{access:'sandboxed',idle_minutes:60}});
  for(const [label,w,h,mobile] of [['desktop',1440,900,false],['folded',344,780,true],['fold',841,701,true]])for(const theme of ['light','dark']){
    await p.viewport(w,h,mobile,mobile?2:1);await p.theme(theme);
    await p.eval(`__mod('lib/terminal.ts').then(m=>m.setTerminalTheme('${theme}'))`);
    const pane=await p.eval(`__mod('lib/workspace.ts').then(m=>m.openPane({type:'terminal',machine:null}))`);
    await p.waitFor(`document.querySelector('.pane.focused .xterm-helper-textarea')`);await sleep(100);
    check(await p.eval(`document.querySelector('.pane.focused .term-screen').dataset.font === 'loaded'`), `${label}-${theme}: input starts with the real terminal font`);
    check(await p.eval(`document.querySelector('.pane.focused .term-body').dataset.termTheme === '${theme}'`), `${label}-${theme}: requested palette applies`);
    if (mobile) {
      const before = await p.eval(`document.querySelector('.pane.focused .pane-head').getBoundingClientRect().top`);
      await p.viewport(w,h-240,true,2); await sleep(200);
      check(await p.eval(`(() => { const tile=document.querySelector('.pane.focused'); const head=tile.querySelector('.pane-head').getBoundingClientRect(); const keys=tile.querySelector('.term-keys').getBoundingClientRect(); return Math.abs(head.top-${before})<1 && keys.bottom <= innerHeight+1 && tile.querySelector('.term-screen').getBoundingClientRect().height>80; })()`), `${label}-${theme}: shorter keyboard viewport keeps header steady and extra keys visible`);
      await p.viewport(w,h,true,2); await sleep(200);
    }
    await p.eval(`Promise.all([__mod('lib/backend.ts'),__mod('lib/ui.ts')]).then(([{backend},ui])=>{
      ui.toasts().forEach(t=>ui.dismissToast(t.id));window.inputCall=backend.call.bind(backend);window.inputAttempts=[];
      backend.call=req=>{if(req.method==='terminal_input'){inputAttempts.push(req.params.data);return Promise.reject(new Error('Terminal input is unavailable'));}return inputCall(req);};
      const clip=new DataTransfer();clip.setData('text/plain','z'.repeat(70000));document.querySelector('.pane.focused .xterm-helper-textarea').dispatchEvent(new ClipboardEvent('paste',{clipboardData:clip,bubbles:true,cancelable:true}));
    })`);
    await p.waitFor(`inputAttempts.length>0`);await sleep(100);
    check(await p.eval(`inputAttempts.length===1`),`${label}-${theme}: rejected paste stops after its first piece`);
    check(await p.eval(`__mod('lib/ui.ts').then(m=>m.toasts().some(t=>t.title.includes("Terminal input wasn't sent")))`),`${label}-${theme}: rejected input visibly explains the stopped paste`);
    await p.shot(`${SHOTS}/terminal-input-${label}-${theme}.png`);
    await p.eval(`__mod('lib/backend.ts').then(({backend})=>backend.call=inputCall)`);
    await p.eval(`__mod('lib/workspace.ts').then(m=>m.closePane('${pane}'))`);
  }
  probe.close();await p.close();finish();
});
