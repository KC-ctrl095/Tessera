// A brand-new installation, with no credentials and no reachable local AI.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from './cdp.mjs';

run(async () => {
  for (const [tag, w, h, mobile] of [['desktop', 1440, 900, false], ['cover', 344, 882, true], ['fold', 884, 1104, true]]) {
    for (const theme of ['dark', 'light']) {
      await mock('/__mock/reset?fresh=1&unconfigured=1');
      const p = await device();
      await p.viewport(w, h, mobile, 1);
      await signIn(p, `Fresh ${tag} ${theme}`);
      await p.theme(theme);
      await p.waitFor(`document.querySelector('.setup-way-chatgpt')`, 8000);
      await sleep(1600); // let the empty local discovery finish
      const prefix = `${tag} ${theme}`;
      check(await p.eval(`document.querySelector('.setup-step.on')?.textContent.includes('Your AI')`), `${prefix}: clean install opens AI setup first`);
      check(await p.eval(`document.querySelector('.setup-model-now')?.textContent.includes('No AI is connected yet') && !document.querySelector('.setup-model-now.ok')`), `${prefix}: preset is not presented as ready`);
      check(await p.eval(`document.querySelector('.setup-foot .btn-primary')?.disabled`), `${prefix}: continue waits for a connected model`);
      check(await p.eval(`!document.querySelector('.setup select option[value*="gpt-6-astra"]')`), `${prefix}: signed-out preset is not offered as a usable default`);
      check(await p.eval(`__mod('lib/store.ts').then(m => m.defaultModel() === null)`), `${prefix}: no default without credentials`);
      // An older server can still send a stale default. Do not show it ready.
      await p.eval(`__mod('lib/store.ts').then(m => m.setState('server', 'default_model', {provider:'openai-codex',model:'gpt-6-astra'}))`);
      check(await p.eval(`__mod('lib/store.ts').then(m => m.defaultModel() === null)`), `${prefix}: stale server default also needs sign-in`);
      check(await p.eval(`!document.querySelector('.setup-model-now.ok')`), `${prefix}: stale preset does not turn setup green`);
      check(await p.eval(`(() => { const c=document.querySelector('.setup-card'); const r=c.getBoundingClientRect(); return r.left >= -1 && r.right <= innerWidth+1 && c.scrollWidth <= c.clientWidth+1; })()`), `${prefix}: setup fits the screen`);
      await p.shot(`${SHOTS}/fresh-install-${tag}-${theme}.png`);
      await p.click('.setup-later');
      await p.waitFor(`!document.querySelector('.setup-wizard')`, 4000);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.newChat({newPane:true}))`);
      await sleep(500);
      const direct = await p.eval(`(() => { const b=[...document.querySelectorAll('.pane.focused .model-chip, .pane.focused .model-sub')].find(e=>e.offsetParent); b?.click(); return !!b; })()`);
      if (!direct) {
        await p.click('.pane.focused .title-menu');
        await p.waitFor(`document.querySelector('.tile-opts .opt-model')`, 5000);
        await p.click('.tile-opts .opt-model');
      }
      await p.waitFor(`document.querySelector('.model-menu')`, 4000);
      check(await p.eval(`[...document.querySelectorAll('.model-menu .model-item')].every(b => b.disabled) && !document.querySelector('.model-menu .model-item.selected')`), `${prefix}: picker cannot select a signed-out model`);
      check(await p.eval(`document.querySelector('.model-menu')?.textContent.includes('Connect an AI account') && [...document.querySelectorAll('.model-menu .link-btn')].some(b=>b.textContent.trim()==='Sign in')`), `${prefix}: picker explains how to connect`);
      await p.shot(`${SHOTS}/fresh-picker-${tag}-${theme}.png`);
      check(p.errors.length === 0, `${prefix}: no page errors`, p.errors);
      await p.close();
    }
  }
  finish();
});
