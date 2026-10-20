import { check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
run(async () => {
  await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
  const probe = await wsClient('terminal race probe');
  const p = await device(); await p.viewport(1440,900); await signIn(p,'Terminal races');
  await probe.rpc('set_terminal_settings',{settings:{access:'sandboxed',idle_minutes:60}});
  for (const [label,w,h,mobile] of [['desktop',1440,900,false],['folded',344,780,true],['fold',841,701,true]]) for (const theme of ['light','dark']) {
    await p.viewport(w,h,mobile,mobile?2:1); await p.theme(theme);
    await p.eval(`__mod('lib/backend.ts').then(({backend}) => {
      window.termRace = {call:backend.call.bind(backend), closed:[], resolves:[]};
      backend.call = req => {
        if (req.method === 'terminal_open') return new Promise(resolve => termRace.resolves.push(resolve));
        if (req.method === 'terminal_close') termRace.closed.push(req.params.id);
        return termRace.call(req);
      };
    })`);
    const pane = await p.eval(`__mod('lib/workspace.ts').then(m => m.openPane({type:'terminal',machine:null}))`);
    await p.waitFor(`termRace.resolves.length === 1`);
    await p.eval(`__mod('lib/workspace.ts').then(m => m.closePane('${pane}'))`);
    const id = `late-${label}-${theme}`;
    await p.eval(`termRace.resolves[0]({id:'${id}',sandboxed:true,machine:null});`);
    await p.waitFor(`termRace.closed.includes('${id}')`);
    check(await p.eval(`termRace.closed.filter(id => id === '${id}').length === 1`),`${label}-${theme}: closing during open closes the late shell exactly once`);
    const watch = await p.eval(`__mod('lib/backend.ts').then(({backend}) => { backend.events.emit({type:'terminal_exit',id:'${id}',reason:'Ended',code:0}); backend.call = termRace.call; return !document.querySelector('[data-pane-id="${pane}"]'); })`);
    check(watch,`${label}-${theme}: late output/exit cannot recreate a disposed tile`);
  }
  probe.close(); await p.close(); finish();
});
