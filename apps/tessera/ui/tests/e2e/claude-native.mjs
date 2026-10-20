// Public plan resets stay localized in-app; browser OS notices stay generic.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';

// Drive both real backend event paths. T6 keeps the browser OS surface
// generic; T10 localizes the reset on the authenticated in-app surface.
async function alertBodies(p, ws, stamps, terminal = false) {
  const bodies = stamps.map(at => {
    // engine.rs finalization: format!("After {took}: {}", one_line(error, 200)).
    const phrase = `Your Claude plan's weekly limit is reached. It resets at ${at}. Send a message later to continue, or choose another model.`;
    return terminal ? `After 2 h 5 min: ${phrase}` : phrase;
  });
  const title = terminal ? 'Stopped by an error' : 'Waiting for your Claude plan';
  await p.eval(`__mod('lib/alerts.ts').then(async m => {
    window.__native27AlertOld = { notification: window.Notification, visibility: Object.getOwnPropertyDescriptor(document, 'visibilityState'), pref: localStorage.getItem('tessera.browserNotifications') };
    window.__native27Bodies = []; window.__native27Titles = [];
    window.Notification = class { static permission = 'granted'; constructor(title, options) { window.__native27Bodies.push(options.body); window.__native27Titles.push(title); } };
    Object.defineProperty(document, 'visibilityState', {configurable:true, get:() => 'hidden'});
    await m.setBrowserNotifications(true); m.watchAlerts();
  })`);
  const shownIds = [];
  try {
    for (const body of bodies) await ws.rpc('mock_notify', {title, body});
    await p.waitFor(`window.__native27Bodies.length === ${stamps.length}`,6000);
    const generic = await p.eval('({bodies:window.__native27Bodies,titles:window.__native27Titles})');
    check(generic.bodies.every(body => body === 'New activity. Open Tessera for details.') && generic.titles.every(title => title === 'Tessera'), 'browser OS plan notices reveal neither reset times nor chat details', generic);
    await p.eval(`Object.defineProperty(document,'visibilityState',{configurable:true,get:()=> 'visible'})`);
    const localized = [];
    for (const body of bodies) {
      const sent = (await ws.rpc('mock_notify', {title, body})).result;
      shownIds.push(sent.id);
      await p.waitFor(`__mod('lib/ui.ts').then(m => m.toasts().some(t => t.key === 'n${sent.id}'))`,6000);
      localized.push(await p.eval(`__mod('lib/ui.ts').then(m => m.toasts().find(t => t.key === 'n${sent.id}').message)`));
    }
    return localized;
  } finally {
    await p.eval(`__mod('lib/ui.ts').then(m => { for (const t of m.toasts()) if (${JSON.stringify(shownIds)}.some(id => t.key === 'n' + id)) m.dismissToast(t.id); })`);
    await p.eval(`(() => {const old=window.__native27AlertOld; window.Notification=old.notification; if(old.visibility) Object.defineProperty(document,'visibilityState',old.visibility); else delete document.visibilityState; if(old.pref===null) localStorage.removeItem('tessera.browserNotifications'); else localStorage.setItem('tessera.browserNotifications',old.pref); delete window.__native27AlertOld; delete window.__native27Bodies; delete window.__native27Titles; })()`);
  }
}

run(async () => {
  await mock('/__mock/reset');
  await mock('/__mock/claude-code?state=ready');
  const p = await device();
  await p.send('Emulation.setTimezoneOverride', { timezoneId: 'America/Chicago' });
  const ws = await wsClient('Claude native27 parity');
  await signIn(p, 'Claude native27');
  for (const [name, width, height, mobile] of [['desktop',1440,900,false], ['phone',344,800,true], ['fold',690,850,true]]) {
    await p.viewport(width,height,mobile);
    for (const theme of ['light','dark']) {
      await p.eval(`__mod('lib/prefs.ts').then(m => m.setPrefs('theme','${theme}'))`);
      for (const mode of ['weekly','missing','credits','waiting','dst-before','dst-after']) {
        const f = await mock(`/__mock/claude-native27?mode=${mode}`);
        check(typeof f.id === 'string',`${name}/${theme}/${mode}: fixture session created`,f);
        await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${f.id}',{pane:m.workspace.focused()}))`);
        try { await p.waitFor(`document.querySelector('.pane.focused .notice')`,6000); }
        catch (e) {
          console.log('native27 debug',f, await p.eval(`({text:document.querySelector('.pane.focused')?.textContent,panes:[...document.querySelectorAll('.pane')].map(e=>e.textContent.slice(0,300)),errors:window.__errors})`),p.errors);
          await p.shot(`${SHOTS}/claude-native27-error.png`);
          throw e;
        }
        await sleep(100);
        await p.eval(`document.querySelectorAll('.pane.focused .steps-head[aria-expanded="false"]').forEach(b => b.click())`);
        await p.waitFor(`document.querySelector('.pane.focused .subagents')`,6000);
        const shown = await p.eval(`(() => { const root = document.querySelector('.pane.focused'); const notice = root.querySelector('.notice'); return { text: notice.textContent, notices: root.querySelectorAll('.notice').length, width: notice.getBoundingClientRect().width, pageWidth: document.documentElement.scrollWidth, cards: root.querySelectorAll('.tool').length, helper: !!root.querySelector('.subagents'), raw: root.textContent.includes('mcp__tessera__') }; })()`);
        check(shown.helper, `${name}/${theme}/${mode}: native agent result uses the shared helper card`,shown);
        check(shown.notices === 1 && !shown.raw && !shown.text.includes('Claude Code:'), `${name}/${theme}/${mode}: one native notice, no raw prefix`,shown);
        check(shown.width <= width && shown.pageWidth <= width && shown.cards >= 4, `${name}/${theme}/${mode}: notice fits; four native file/reader cards`,shown);
        if (mode === 'weekly' || mode === 'waiting') check(/Oct 12.*11:00.*CDT/.test(shown.text) && !shown.text.includes('T16:00:00Z'),`${name}/${theme}/${mode}: reset in user's Chicago zone`,shown.text);
        if (mode === 'missing') check(shown.text.includes("didn't report a reset time") && !/resets at/.test(shown.text),`${name}/${theme}: missing reset is not guessed`,shown.text);
        if (mode === 'credits') check(shown.text.includes('needs usage credits') && !shown.text.includes("I'll continue"),`${name}/${theme}: purchase-needed is not retried`,shown.text);
        if (mode === 'dst-before') check(/Nov 1.*1:59.*CDT/.test(shown.text) && !shown.text.includes('T06:59:00Z'),`${name}/${theme}: before fall-back is 1:59 CDT`,shown.text);
        if (mode === 'dst-after') check(/Nov 1.*1:01.*CST/.test(shown.text) && !shown.text.includes('T07:01:00Z'),`${name}/${theme}: after fall-back is 1:01 CST`,shown.text);
        if (mode === 'waiting') {
          check(shown.text.includes('Stop cancels the wait'),`${name}/${theme}: unattended wait explains Stop`,shown.text);
          check(await p.eval(`!!document.querySelector('.pane.focused .send-btn.stop')`),`${name}/${theme}: waiting turn keeps native Stop control`);
        }
        if (mode === 'weekly') {
          await p.eval(`document.querySelector('.pane.focused .notice').scrollIntoView({block:'start'})`);
          await p.shot(`${SHOTS}/claude-native27-reset-${name}-${theme}.png`);
        }
      }
      const system = await alertBodies(p, ws, ['2026-11-01T06:59:00Z','2026-11-01T07:01:00Z']);
      check(system.length === 2 && /Nov 1.*1:59.*CDT/.test(system[0]) && /Nov 1.*1:01.*CST/.test(system[1]), `${name}/${theme}: in-app notifications use device-local reset on both DST sides`,system);
      const terminal = await alertBodies(p, ws, ['2026-11-01T06:59:00Z','2026-11-01T07:01:00Z'], true);
      check(terminal.length === 2 && terminal.every(body => body.startsWith("After 2 h 5 min: Your Claude plan's")) && /Nov 1.*1:59.*CDT/.test(terminal[0]) && /Nov 1.*1:01.*CST/.test(terminal[1]), `${name}/${theme}: finalization terminal envelope localizes the plan reset`,terminal);
      let nativeTransition;
      for (const [engine,model] of [['native',{provider:'anthropic',model:'claude-sonnet-5-5'}], ['bridge',{provider:'claude-code',model:'sonnet'}]]) {
        const info = (await ws.rpc('create_session',{title:`native27 ${engine}`,model})).result;
        await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${info.id}',{pane:m.workspace.focused()}))`);
        const t = '.pane.focused';
        await p.waitFor(`document.querySelector('${t} .composer')`,6000);
        const text = 'native27 streaming stays here. '.repeat(16);
        await ws.rpc('mock_stream',{session_id:info.id,text,thinking:600,chunk:[20,30],delay:[35,45]});
        await p.waitFor(`document.querySelector('${t} .thinking.live .thinking-head')`,6000);
        await p.eval(`document.querySelector('${t} .thinking.live .thinking-head').click()`);
        await p.waitFor(`document.querySelector('${t} .msg-assistant .md.streaming > p')`,6000);
        await p.eval(`document.querySelector('${t} .msg-assistant .md.streaming > p').__native27 = 1`);
        const samples = await p.eval(`new Promise(resolve => { const sizes=[]; let i=0; const timer=setInterval(() => { sizes.push(document.querySelector('${t} .msg-assistant .md')?.textContent.length ?? 0); if (++i===12) {clearInterval(timer);resolve(sizes);} },35); })`);
        check(samples.every((n,i) => i === 0 || n >= samples[i-1]), `${name}/${theme}/${engine}: streamed text never shrinks`,samples);
        await p.waitFor(`!document.querySelector('${t} .md.streaming') && !document.querySelector('${t} .send-btn.stop')`,6000);
        const final = await p.eval(`(() => { const md=document.querySelector('${t} .msg-assistant .md'); return {text:md.textContent,kept:md.firstElementChild.__native27===1,thinking:!!document.querySelector('${t} .thinking.open .thinking-body .md')}; })()`);
        check(final.text.trim()===text.trim() && final.thinking, `${name}/${theme}/${engine}: same final text and thinking`,final);
        if (engine === 'native') nativeTransition = final.kept;
        else check(final.kept === nativeTransition, `${name}/${theme}: bridge uses the native final DOM reconciliation`,{native:nativeTransition,bridge:final.kept});
      }
      await p.shot(`${SHOTS}/claude-native27-${name}-${theme}.png`);
    }
  }
  // A second device zone sees the same instant, not the server's local time.
  await p.send('Emulation.setTimezoneOverride', {timezoneId:'Asia/Tokyo'});
  const f = await mock('/__mock/claude-native27?mode=weekly');
  await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${f.id}',{pane:m.workspace.focused()}))`);
  await p.waitFor(`document.querySelector('.pane.focused .notice')`,6000);
  const text = await p.eval(`document.querySelector('.pane.focused .notice').textContent`);
  check(/Oct 13.*1:00/.test(text), 'reset crosses midnight correctly in Tokyo',text);
  const systemTokyo = await alertBodies(p, ws, ['2026-10-12T16:00:00Z']);
  check(systemTokyo.length === 1 && /Oct 13.*1:00/.test(systemTokyo[0]), 'in-app reset also crosses midnight in Tokyo',systemTokyo);
  const untouched = await p.eval(`__mod('lib/claude-limit.ts').then(m => m.claudeLimitText('An unrelated note. It resets at 2026-10-12T16:00:00Z.'))`);
  check(untouched.endsWith('T16:00:00Z.'), 'unrelated notices are not rewritten',untouched);
  ws.close();
  await p.close();
  finish('Claude native27');
});
