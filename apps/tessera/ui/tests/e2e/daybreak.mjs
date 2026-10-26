// Eligible-only per-chat Daybreak Blue, on all launch form factors and themes.
import { APP, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
import { settled } from './settled.mjs';
const toggle = '.pop-options [role="switch"][aria-label="Daybreak Blue"]';
const ref = { provider: 'openai-codex', model: 'gpt-5.5' };
async function options(p) {
  await p.eval(`(() => { const root = document.querySelector('.pane.focused'); const buttons = [...root.querySelectorAll('.opts-btn, .title-menu')]; buttons.find(b => b.getClientRects().length)?.click(); })()`);
  await p.waitFor(`document.querySelector('.pop-options .opt-more-body')`);
  await settled(p, `document.querySelector('.pop-options')`);
}
async function close(p) { await p.key('Escape', 'Escape', 0, 27); await p.waitFor(`!document.querySelector('.pop-options')`); }
async function catalog(p, query, expected) {
  await mock('/__mock/daybreak?' + query);
  await p.waitFor(`__mod('lib/store.ts').then(s => { const p=s.state.providers.find(p=>p.id==='openai-codex'); const m=p.models.find(m=>m.id==='gpt-5.5'); return ${expected}; })`);
}
run(async () => {
  for (const [label, width, height, mobile, dpr] of [['desktop', 1440, 900, false, 1], ['phone', 390, 844, true, 2], ['cover', 344, 882, true, 2], ['fold-small', 673, 841, true, 2], ['fold', 880, 1000, true, 2]]) {
    for (const theme of ['dark', 'light']) {
      const tag = `${label}-${theme}`;
      if (process.env.ONLY && !tag.includes(process.env.ONLY)) continue;
      await mock('/__mock/reset');
      const p = await device(); await p.viewport(width, height, mobile, dpr);
      await signIn(p, `Daybreak ${tag}`); await p.theme(theme);
      const w = await wsClient(`Daybreak control ${tag}`);
      const created = await w.rpc('create_session', { model: ref, title: 'Daybreak options' });
      const sid = created.result.id;
      await p.eval(`__mod('lib/workspace.ts').then(w=>w.openSession(${JSON.stringify(sid)}))`);
      await p.waitFor(`__mod('lib/store.ts').then(s=>s.state.sessions[${JSON.stringify(sid)}]?.loaded)`);
      for (const [mode, expectation] of [['missing', '!m.cyber_access_programs'], ['empty', 'm.cyber_access_programs?.length === 0'], ['standard', 'm.cyber_access_programs?.join() === "standard"'], ['red', 'm.cyber_access_programs?.includes("daybreak_red")']]) {
        await catalog(p, `programs=${mode}`, expectation);
        await options(p);
        check(!await p.eval(`!!document.querySelector(${JSON.stringify(toggle)})`), `${tag} hides Blue for ${mode} discovery`);
        await close(p);
      }
      const denied = await w.rpc('set_session_daybreak', { session_id: sid, enabled: true });
      check(!!denied.error, `${tag} server refuses unconfirmed Blue access`);
      // Blue on the account's other model only: the toggle shows, says where it moves the chat, and does.
      await catalog(p, 'programs=elsewhere', 'm.cyber_access_programs?.join() === "standard"');
      await options(p);
      await p.waitFor(`document.querySelector(${JSON.stringify(toggle)})`);
      check(await p.eval(`document.querySelector('.pop-options').textContent.includes('Turning it on switches this chat to GPT-6.')`), `${tag} a model without Blue names the model it moves to`);
      await p.click(toggle);
      await p.waitFor(`__mod('lib/store.ts').then(s => { const i = s.state.sessions[${JSON.stringify(sid)}].info; return i.daybreak === true && i.model.model === 'gpt-6'; })`);
      const moved = (await w.rpc('get_session', { session_id: sid })).result.info;
      check(moved.daybreak === true && moved.model.model === 'gpt-6', `${tag} turning it on moves the chat to a model that offers Blue`, moved.model);
      if (tag === 'phone-dark') await p.shot(`${SHOTS}/daybreak-elsewhere-${tag}.png`);
      await close(p);
      await w.rpc('set_session_daybreak', { session_id: sid, enabled: false });
      await w.rpc('update_session', { session_id: sid, model: ref });
      await p.waitFor(`__mod('lib/store.ts').then(s => s.state.sessions[${JSON.stringify(sid)}].info.model.model === 'gpt-5.5')`);
      await catalog(p, 'programs=blue&authenticated=0', '!p.authenticated');
      await options(p);
      check(!await p.eval(`!!document.querySelector(${JSON.stringify(toggle)})`), `${tag} signed-out account hides the control even with catalog metadata`);
      await close(p);
      await catalog(p, 'programs=blue', 'p.authenticated && m.cyber_access_programs?.includes("daybreak_blue")');
      await options(p);
      await p.waitFor(`document.querySelector(${JSON.stringify(toggle)})`);
      check(await p.eval(`document.querySelector(${JSON.stringify(toggle)}).getAttribute('aria-checked') === 'false'`), `${tag} eligible chat starts off`);
      check(await p.eval(`document.querySelector('.pop-options').textContent.includes('OpenAI’s program for defensive security work. Access comes from OpenAI.')`), `${tag} plain explanation names who grants access`);
      const geometry = await p.eval(`(() => { const b=document.querySelector(${JSON.stringify(toggle)}); b.scrollIntoView({block:'center'}); const r=b.getBoundingClientRect(), row=b.closest('.opt-line'), text=row.querySelector('.opt-line-label'); const t=text.getBoundingClientRect(); return { width:r.width, height:r.height, fits:r.x>=0 && r.right<=innerWidth && r.y>=0 && r.bottom<=innerHeight, overlap:t.right>r.left, hit:b.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)), overflow:document.documentElement.scrollWidth>innerWidth }; })()`);
      check(geometry.fits && geometry.hit && !geometry.overlap && !geometry.overflow && geometry.height >= 40 && geometry.width >= 40, `${tag} toggle is reachable and fits without overlap`, geometry);
      await p.click(toggle);
      await p.waitFor(`document.querySelector(${JSON.stringify(toggle)}).getAttribute('aria-checked') === 'true'`);
      check((await w.rpc('get_session', { session_id: sid })).result.info.daybreak === true, `${tag} on is saved by the server`);
      await p.shot(`${SHOTS}/daybreak-${tag}.png`);
      await close(p);
      await p.goto(APP);
      await p.waitFor(`document.querySelector('.pane.focused .composer')`);
      await p.eval(`__mod('lib/workspace.ts').then(w=>w.openSession(${JSON.stringify(sid)}))`);
      await p.waitFor(`__mod('lib/store.ts').then(s=>s.state.sessions[${JSON.stringify(sid)}]?.loaded)`);
      await options(p);
      check(await p.eval(`document.querySelector(${JSON.stringify(toggle)})?.getAttribute('aria-checked') === 'true'`), `${tag} choice survives a page reload`);
      await p.click(toggle);
      await p.waitFor(`document.querySelector(${JSON.stringify(toggle)}).getAttribute('aria-checked') === 'false'`);
      check(!(await w.rpc('get_session', { session_id: sid })).result.info.daybreak, `${tag} off is saved by the server`);
      check(!p.errors.length, `${tag} has no uncaught UI errors`, p.errors);
      await p.click(toggle);
      await p.waitFor(`document.querySelector(${JSON.stringify(toggle)}).getAttribute('aria-checked') === 'true'`);
      await close(p);
      await catalog(p, 'programs=red', 'm.cyber_access_programs?.includes("daybreak_red")');
      await options(p);
      check(!await p.eval(`!!document.querySelector(${JSON.stringify(toggle)})`), `${tag} losing Blue access hides the Daybreak toggle even for a saved opt-in`);
      const recovery = await p.eval(`(() => { const b=Array.from(document.querySelectorAll('.pop-options button')).find(b => b.textContent.includes('Use standard safeguards')); b.scrollIntoView({block:'center'}); const r=b.getBoundingClientRect(); return {width:r.width,height:r.height,fits:r.x>=0 && r.right<=innerWidth && r.y>=0 && r.bottom<=innerHeight}; })()`);
      check(recovery.fits && recovery.width >= 40 && recovery.height >= 40, `${tag} safe opt-out recovery has an accessible target`, recovery);
      if (tag === 'cover-light') await p.shot(`${SHOTS}/daybreak-recovery-${tag}.png`);
      await p.eval(`Array.from(document.querySelectorAll('.pop-options button')).find(b => b.textContent.includes('Use standard safeguards')).click()`);
      await p.waitFor(`__mod('lib/store.ts').then(s=>!s.state.sessions[${JSON.stringify(sid)}].info.daybreak)`);
      check(!(await w.rpc('get_session', { session_id: sid })).result.info.daybreak, `${tag} saved opt-in can be cleared when access disappears`);
      await close(p);
      await catalog(p, 'programs=blue', 'm.cyber_access_programs?.includes("daybreak_blue")');
      // Draft settings use the same control before a server session exists.
      await p.eval(`__mod('lib/workspace.ts').then(w=>w.newChat())`);
      await p.eval(`Promise.all([__mod('lib/actions.ts'), __mod('lib/workspace.ts')]).then(([a,w])=>a.patchPane(w.workspace.focused(),{model:${JSON.stringify(ref)}}))`);
      await options(p); await p.click(toggle);
      check(await p.eval(`Promise.all([__mod('lib/actions.ts'),__mod('lib/workspace.ts')]).then(([a,w])=>a.paneSettings(w.workspace.focused()).daybreak)`), `${tag} new-chat draft keeps its opt-in`);
      await close(p);
      await p.eval(`Promise.all([__mod('lib/actions.ts'), __mod('lib/workspace.ts')]).then(([a,w])=>a.patchPane(w.workspace.focused(),{model:{provider:'local',model:'qwen3.8-27b'}}))`);
      await options(p);
      check(!await p.eval(`!!document.querySelector(${JSON.stringify(toggle)})`), `${tag} non-OpenAI model hides the toggle`);
      check(!await p.eval(`Promise.all([__mod('lib/actions.ts'),__mod('lib/workspace.ts')]).then(([a,w])=>a.paneSettings(w.workspace.focused()).daybreak)`), `${tag} switching to an unsupported draft model clears the opt-in`);
      await w.close(); await p.close();
    }
  }
  finish();
});
