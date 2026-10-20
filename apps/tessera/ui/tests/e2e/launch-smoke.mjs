// Launch setup regressions. All requests go to the mock, never a model service.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const sizes = [["desktop", 1440, 900, false], ["phone", 390, 844, true], ["cover", 344, 882, true], ["fold", 884, 1104, true]];
const click = async (p, selector, text) => {
  const target = `(() => [...document.querySelectorAll(${JSON.stringify(selector)})].find(e => (e.textContent || '').includes(${JSON.stringify(text)})))()`;
  await p.eval(`${target}?.scrollIntoView({block:'nearest',inline:'nearest'})`);
  await sleep(100);
  const point = await p.eval(`(() => { const r = ${target}?.getBoundingClientRect(); return r ? {x:r.x+r.width/2,y:r.y+r.height/2} : null; })()`);
  if (!point) throw new Error(`No ${selector} with ${text}`);
  await p.mouse(point.x, point.y);
  await sleep(100);
};
const fits = (p) => p.eval(`(() => { const c = document.querySelector('.setup-card'); const r = c?.getBoundingClientRect(); return !!r && r.left >= -1 && r.right <= innerWidth + 1 && c.scrollWidth <= c.clientWidth + 1; })()`);
const safety = (p) => p.eval(`document.querySelector('.safety-list')?.textContent ?? ''`);
const summary = async (p, lan) => {
  await p.eval(`__mod('lib/store.ts').then(m => {
    window.__setupStatus ??= m.api.setupStatus;
    m.api.setupStatus = async () => ({ ...await window.__setupStatus(), lan: ${lan}, remote: false });
  })`);
  await p.eval(`__mod('lib/ui.ts').then(u => u.openSetup())`);
  await p.waitFor(`document.querySelector('.setup-step')`, 6000);
  await click(p, ".setup-step", "Done");
  await p.waitFor(`document.querySelector('.safety-list')`, 4000);
  await sleep(300);
};

run(async () => {
  for (const [tag, w, h, mobile] of sizes) {
    for (const theme of ["dark", "light"]) {
      await mock("/__mock/reset");
      const p = await device();
      await p.viewport(w, h, mobile, 1);
      await signIn(p, `Launch ${tag} ${theme}`);
      await p.theme(theme);
      await summary(p, false);
      const words = await safety(p);
      check(words.includes("Only on this computer") && words.includes("Other devices can't connect yet"), `${tag} ${theme}: loopback-only summary does not promise phone access`, words);
      check(!words.includes("going online need your OK"), `${tag} ${theme}: approvals summary does not claim all web access asks`);
      check(await fits(p), `${tag} ${theme}: setup fits the screen`);
      await p.shot(`${SHOTS}/launch-summary-${tag}-${theme}.png`);
      await p.eval(`__mod('lib/store.ts').then(m => {
        window.__completeSetup = m.api.completeSetup;
        m.api.completeSetup = async () => { throw new Error('Please try saving again'); };
      })`);
      await click(p, ".setup-foot button", "Start using Tessera");
      await sleep(250);
      check(await p.eval(`!!document.querySelector('.setup-wizard') && !document.querySelector('.setup-foot .btn-primary').disabled`), `${tag} ${theme}: a failed save keeps setup open with a retry`);
      check(await p.eval(`[...document.querySelectorAll('.toast')].some(t => /Could not save setup/.test(t.textContent))`), `${tag} ${theme}: failed save explains what happened`);
      await p.shot(`${SHOTS}/launch-save-retry-${tag}-${theme}.png`);
      await p.eval(`__mod('lib/store.ts').then(m => m.api.completeSetup = window.__completeSetup)`);
      await click(p, ".setup-foot button", "Start using Tessera");
      await p.waitFor(`!document.querySelector('.setup-wizard')`, 4000);
      check(true, `${tag} ${theme}: retry saves and closes setup`);
      await p.eval(`__mod('components/Tour.tsx').then(t => t.startTour(true))`);
      await summary(p, true);
      check(!await p.eval(`!!document.querySelector('.tour')`), `${tag} ${theme}: reopening setup hides unfinished coach tips`);
      check((await safety(p)).includes("Only on your home network"), `${tag} ${theme}: LAN-only summary names its scope`);
      await p.shot(`${SHOTS}/launch-summary-lan-${tag}-${theme}.png`);
      await p.eval(`document.querySelectorAll('.setup-step')[0].click()`);
      await p.waitFor(`document.querySelector('.setup-way-key')`, 4000);
      await click(p, '.setup-way-key .chip', 'OpenRouter, OpenAI or another');
      await p.waitFor(`document.querySelector('.preset-row .chip')`, 4000);
      await click(p, '.advanced-block .disclosure', 'Advanced');
      const format = () => p.eval(`document.querySelector('.advanced [role="radio"][aria-checked="true"]')?.textContent.trim()`);
      await click(p, '.preset-row .chip', 'OpenAI');
      check(await format() === 'Responses', `${tag} ${theme}: OpenAI preset chooses its Responses API`);
      await click(p, '.preset-row .chip', 'llama.cpp');
      check(await format() === 'Chat Completions', `${tag} ${theme}: local preset resets API format after OpenAI`);
      await click(p, '.advanced [role="radio"]', 'Responses');
      await click(p, '.preset-row .chip', 'Ollama');
      check(await format() === 'Chat Completions', `${tag} ${theme}: Ollama resets a manually changed API format`);
      await click(p, '.preset-row .chip', 'OpenRouter');
      check(await format() === 'Chat Completions', `${tag} ${theme}: OpenRouter uses Chat Completions`);
      check(await p.eval(`document.querySelector('.form-card').textContent.includes("not your phone's address")`), `${tag} ${theme}: address hint explains which computer it means`);
      check(await fits(p), `${tag} ${theme}: presets fit without horizontal clipping`);
      await p.shot(`${SHOTS}/launch-provider-presets-${tag}-${theme}.png`);
      check(p.errors.length === 0, `${tag} ${theme}: no page errors`, p.errors);
      await p.close();
    }
  }
  finish();
});
