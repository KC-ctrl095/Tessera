// The user's own Claude Code shows up as "Claude (your subscription)" in the model menu and Settings.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/claude-code?state=ready");
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Claude check");
  await sleep(1200);
  await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_2b7e91c04d5a', { pane: m.workspace.focused() }))`);
  await sleep(800);
  const prov = await p.eval(`__mod('lib/store.ts').then(m => JSON.stringify(m.state.providers.find(x => x.id === 'claude-code') ?? null))`);
  const pv = JSON.parse(prov);
  check(pv && pv.kind === "claude_code" && pv.authenticated, "the claude-code provider is listed", pv);
  // The model menu groups it under its own name.
  await p.eval(`[...document.querySelectorAll('.pane.focused .model-chip, .pane.focused .model-sub')].find((e) => e.offsetParent)?.click()`);
  await p.waitFor(`document.querySelector('.pop-model .model-menu')`, 3000).catch(() => {});
  await sleep(500);
  const heads = await p.eval(`[...document.querySelectorAll('.model-group-head')].map(e => e.textContent)`);
  check(heads.some((h) => h.includes("Claude (your subscription)")), "the model menu lists Claude (your subscription)", heads);
  await p.shot(`${SHOTS}/claude-model-menu.png`);
  const status = await p.eval(`__mod('lib/store.ts').then(m => m.api.claudeCodeStatus(true))`);
  check(status?.signed_in && status?.plan === "max", "claude_code_status answers through the api helper", status);
  await p.eval(`__mod('components/Settings.tsx').catch(() => null)`);
  // Wave 3: the same compacting status/card and truthful opaque summary,
  // plus explicit reported/estimated context labels at all device widths.
  await p.eval(`document.body.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true})); document.body.click()`);
  const chat = await p.eval(`__mod('lib/store.ts').then(m => m.api.createSession({ model:{provider:'claude-code',model:'opus'}, title:'Claude work state' }))`);
  await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${chat.id}', { pane:m.workspace.focused() }))`);
  await p.waitFor(`document.querySelector('.pane.focused .composer textarea')`, 6000);
  await p.eval(`__mod('lib/store.ts').then(async m => { await m.api.setGoal('${chat.id}', 'Keep work state through compaction'); await m.api.send('${chat.id}', [{type:'text',text:'Hello'}], 'auto'); })`);
  await p.waitFor(`document.querySelector('.pane.focused .goal')`, 6000);
  await p.eval(`__mod('lib/store.ts').then(m => { window.__wave3Store = m; })`);
  await p.waitFor(`window.__wave3Store.state.sessions['${chat.id}']?.info.status === 'idle' && window.__wave3Store.state.sessions['${chat.id}']?.entries.some(e => e.kind === 'assistant')`, 12000);
  await p.eval(`__mod('lib/store.ts').then(m => m.api.compact('${chat.id}'))`);
  await p.waitFor(`document.querySelector('.pane.focused')?.textContent.includes('Compacting')`, 4000);
  check(true, 'Claude compaction uses native in-progress status');
  await p.waitFor(`document.querySelector('.pane.focused .compaction')`, 6000);
  for (const [name,w,h] of [['desktop',1440,900],['folded',344,740],['fold',690,829]]) {
    await p.viewport(w,h,name !== 'desktop', name === 'desktop' ? 1 : 2);
    for (const theme of ['dark','light']) {
      await p.theme(theme);
      await p.eval(`document.querySelector('.pane.focused .compaction-head')?.click()`);
      await p.waitFor(`document.querySelector('.pane.focused .compaction-body')`, 2000).catch(async () => { await p.eval(`document.querySelector('.pane.focused .compaction-head')?.click()`); });
      await p.eval(`document.querySelector('.pane.focused .compaction')?.scrollIntoView({block:'center'})`);
      await p.wait(120);
      check(await p.eval(`document.documentElement.scrollWidth <= innerWidth + 1`), `${name}-${theme}: work state fits viewport`);
      const text = await p.eval(`document.querySelector('.pane.focused .compaction')?.textContent`);
      check(text?.includes('private summary is not available') && !text.includes('0 →'), `${name}-${theme}: opaque Claude checkpoint is honest`, text);
      check((await p.eval(`document.querySelector('.pane.focused .goal')?.textContent`))?.includes('Keep work state'), `${name}-${theme}: goal survives compaction`);
      await p.shot(`${SHOTS}/claude-work-state-${name}-${theme}.png`);
    }
  }
  await p.viewport(1440,900);
  await p.eval(`document.querySelector('.pane.focused .opts-btn')?.click()`);
  await p.waitFor(`document.querySelector('.pop-options')`, 3000);

  await p.waitFor(`document.querySelector('.pop-options .opt-more-body')?.textContent.includes('Chat length')`, 3000);
  check((await p.eval(`document.querySelector('.pop-options')?.textContent`))?.includes('reported'), 'context usage is labelled reported');
  await p.shot(`${SHOTS}/claude-context-reported.png`);
  await p.eval(`__mod('lib/store.ts').then(m => m.setState('sessions','${chat.id}','info','context','source','estimated'))`);
  check((await p.eval(`document.querySelector('.pop-options')?.textContent`))?.includes('estimated'), 'fallback context is labelled estimated');
  await p.shot(`${SHOTS}/claude-context-estimated.png`);
  await p.eval(`document.body.click()`);
  console.log("errors", p.errors);
  check(p.errors.length === 0, "no page errors", p.errors);
  await p.close();
  finish();
});
