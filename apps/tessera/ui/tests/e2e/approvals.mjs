// Deny declines an action, not the chat. Exercise the real card/transcript
// on desktop, a narrow phone and a Fold, in both themes (mock tool engine).
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';

run(async () => {
  await mock('/__mock/reset');
  const ws = await wsClient('approval checks');
  const p = await device();
  await signIn(p, 'Approvals');
  for (const width of [1440, 344, 800]) {
    await p.viewport(width, 900, width < 1000, width < 1000 ? 2 : 1);
    for (const theme of ['dark', 'light']) {
      await p.theme(theme);
      const sid = (await ws.rpc('create_session', { title: `Deny ${width} ${theme}` })).result.id;
      await ws.rpc('update_session', { session_id: sid, permission: 'ask' });
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession(${JSON.stringify(sid)}))`);
      await sleep(300);
      await ws.rpc('send', { session_id: sid, content: [{ type: 'text', text: 'delete the build cache please' }] });
      await p.waitFor(`document.querySelector('.approval-actions .danger-text')`, 15000).catch(async (e) => {
        console.log('snapshot', JSON.stringify((await ws.rpc('get_session', { session_id: sid })).result));
        console.log('page', await p.eval(`document.body.textContent`));
        await p.shot(`${SHOTS}/approval-timeout-${width}-${theme}.png`);
        throw e;
      });
      check(await p.eval(`document.querySelector('.approval-actions .danger-text').textContent.trim() === 'Deny'`), `${width}/${theme}: Deny stays separate from Stop`);
      await p.click('.approval-actions .danger-text');
      await p.waitFor(`!document.querySelector('.approval') && [...document.querySelectorAll('.tool')].some(t => t.dataset.status === 'error')`, 15000);
      const tool = await p.eval(`[...document.querySelectorAll('.tool')].find(t => t.dataset.status === 'error')?.querySelector('.tool-head')?.textContent ?? ''`);
      await p.eval(`[...document.querySelectorAll('.tool')].find(t => t.dataset.status === 'error')?.querySelector('.tool-head')?.click()`);
      await p.waitFor(`document.body.textContent.includes("The user declined this bash action.")`, 10000);
      check(await p.eval(`document.body.textContent.includes("carry on with other work") && !document.body.textContent.includes('STOP what you are doing')`), `${width}/${theme}: refusal says carry on, never invents Stop`, tool);
      await p.waitFor(`__mod('lib/store.ts').then(m => m.state.sessions[${JSON.stringify(sid)}]?.info.status === 'idle')`, 15000);
      await p.waitFor(`document.body.textContent.includes('Understood: I left')`, 15000);
      check(await p.eval(`!document.body.textContent.includes('Stopped.')`), `${width}/${theme}: the chat answers after Deny`);
      await p.shot(`${SHOTS}/approval-denied-${width}-${theme}.png`);
    }
  }
  check(!p.errors.length, 'no page errors', p.errors);
  finish();
});
