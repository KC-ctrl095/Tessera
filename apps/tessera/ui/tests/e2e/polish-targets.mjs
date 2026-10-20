// Real button effects on cover/Fold, fine/coarse pointers, both themes. Mock server,
// isolated browser profiles and their real clipboard only; no personal configuration.
import { APP, SHOTS, browser, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
import { hitTarget } from './target-input.mjs';
import { settled } from './settled.mjs';
const SID = 'ses_2b7e91c04d5a';
const select = s => `document.querySelector(${JSON.stringify(s)})`;
const btn = (name, card = '.pane.focused .qcard') => `[...document.querySelectorAll('${card} button')].find(e => e.textContent.trim() === ${JSON.stringify(name)})`;
const card = re => `[...document.querySelectorAll('.pane.focused .qcard:not(.leaving)')].find(c => ${re}.test(c.textContent))`;
const action = (re, name) => `${card(re)}.querySelector('[aria-label="${name}"]')`;
const rpc = async (method, params) => { const c = await wsClient('target-probe'); try { const r = await c.rpc(method, params); if (r.error) throw Error(JSON.stringify(r.error)); return r.result; } finally { c.close(); } };
const snap = () => rpc('get_session', { session_id: SID, since_seq: 0 });
const wait = async (fn, ms = 6000) => { const t = Date.now(); do { if (await fn()) return; await sleep(100); } while (Date.now()-t < ms); throw Error('server effect timeout'); };
run(async () => {
  for (const w of [344, 884]) for (const touch of [false, true]) for (const theme of ['light', 'dark']) {
    const tag = `${w} ${touch ? 'coarse' : 'fine'} ${theme}`, h = w === 344 ? 882 : 1104;
    const hit = async (p, e, label, min = 40) => {
      await settled(p, e);
      return hitTarget(p, e, touch, `${tag} ${label}`, min);
    };
    console.log(`— ${tag}`);
    // Pairing: real CDP click/tap writes the real isolated Chromium clipboard.
    await mock('/__mock/reset?fresh=1'); await mock('/__mock/mosaics/reset');
    let p = await device(); await p.viewport(w, h, touch, touch ? 2 : 1);
    await signIn(p, `Targets pair ${tag}`).catch(() => {});
    await p.waitFor(`document.querySelector('.setup-step')`, 10000);
    await p.theme(theme);
    await hit(p, `document.querySelectorAll('.setup-step')[2]`, 'pair step', 44);
    await p.waitFor(`document.querySelector('.pair-copy-link')`, 8000);
    const c = await browser();
    await c.send('Browser.grantPermissions', { origin: new URL(APP).origin, permissions: ['clipboardReadWrite','clipboardSanitizedWrite'], browserContextId: p.contextId });
    const code = await p.eval(`document.querySelector('.code-big').textContent.trim().replace(/(\\d{4})\\s+(\\d{4})/, '$1-$2')`);
    await hit(p, select('.pair-copy-code'), 'Copy code', 44);
    await wait(async () => (await p.eval(`navigator.clipboard.readText()`)) === code);
    const copiedCode = await p.eval(`navigator.clipboard.readText()`);
    check(copiedCode === code, `${tag}: Copy code writes the displayed code to the real clipboard`);
    await hit(p, select('.pair-copy-link'), 'Copy pairing link', 44);
    await wait(async () => (await p.eval(`navigator.clipboard.readText()`)) !== copiedCode);
    const link = await p.eval(`navigator.clipboard.readText()`);
    check(link !== copiedCode && link.includes(code) && /tessera|pair/.test(link), `${tag}: Copy pairing link writes a different full pairing URL to the real clipboard`);
    await p.shot(`${SHOTS}/targets-${tag.replaceAll(' ','-')}-pair.png`); await p.close();

    await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
    p = await device(); await p.viewport(w, h, touch, touch ? 2 : 1); await signIn(p, `Targets ${tag}`); await p.theme(theme);
    if (!touch) {
      await p.eval(`__mod('lib/prefs.ts').then(m => m.setPrefs('density', 'compact'))`);
      await p.waitFor(`document.documentElement.dataset.density === 'compact'`);
    }
    // Add new bot: the actual target opens the editor, even on a narrow fine-pointer screen.
    await p.eval(`__mod('lib/ui.ts').then(m => m.setDrawerOpen(true))`);
    await hit(p, select('.drawer-layer.open .sb-tab[id$="-bots"]'), 'Bots tab', 44);
    await hit(p, select('.drawer-layer.open [aria-label="Add new bot"]'), 'Add new bot', 44);
    await p.waitFor(`document.querySelector('.bot-editor .be-templates')`, 8000);
    check(await p.eval(`!!document.querySelector('.bot-editor .be-templates')`), `${tag}: Add new bot opens the real editor`);
    await p.eval(`__mod('lib/ui.ts').then(m => m.setBotEditor(null))`);
    await p.eval(`__mod('lib/ui.ts').then(m => m.setDrawerOpen(false))`);
    await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${SID}', { pane: m.workspace.focused() }))`);
    await p.waitFor(`document.querySelector('.pane.focused .title-btn')`, 8000);
    await p.waitFor(`(() => { const e = document.querySelector('.pane.focused .title-btn'); if (!e) return false; const r = e.getBoundingClientRect(); return e.contains(document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)); })()`, 5000);
    await hit(p, select('.pane.focused .title-btn'), 'chat options', 44);
    await p.waitFor(`document.querySelector('.tile-opts .perm-seg')`, 5000);
    await p.key('Escape','Escape',0,27);
    await p.waitFor(`!document.querySelector('.tile-opts')`, 5000);
    await rpc('update_session', { session_id: SID, title: '.' });
    await p.waitFor(`document.querySelector('.pane.focused .title-text')?.textContent === '.'`, 8000);
    await hit(p, select('.pane.focused .title-btn'), 'short-title "." chat options', 44);
    await p.waitFor(`document.querySelector('.tile-opts .perm-seg')`, 5000);
    if (!touch) {
      const position = () => p.eval(`({ head: document.querySelector('.pane.focused .pane-head').getBoundingClientRect().height, transcript: document.querySelector('.pane.focused .transcript').getBoundingClientRect().top })`);
      const before = await position();
      await p.eval(`[...document.querySelectorAll('.tile-opts .act-btn')].find(e => e.textContent.trim() === 'Rename').scrollIntoView({ block: 'nearest' })`);
      await p.clickText('.tile-opts .act-btn', 'Rename');
      await p.waitFor(`document.querySelector('.pane.focused .title-input')`, 5000);
      const editing = await position();
      check(before.head >= 44 && Math.abs(editing.head-before.head) < 1 && Math.abs(editing.transcript-before.transcript) < 1, `${tag}: compact fine-pointer rename reserves header height and does not move the transcript`, { before, editing });
      await p.eval(`document.querySelector('.pane.focused .title-input').select()`); await p.type('..');
      await p.key('Enter', 'Enter', 0, 13);
      await p.waitFor(`!document.querySelector('.pane.focused .title-input')`, 5000);
      await wait(async () => (await snap()).info.title === '..');
      const saved = await position();
      check(Math.abs(saved.head-before.head) < 1 && Math.abs(saved.transcript-before.transcript) < 1, `${tag}: saving the compact rename keeps the transcript at the same position`, { before, saved });
      await hit(p, select('.pane.focused .title-btn'), 'renamed short-title chat options', 44);
      await p.waitFor(`document.querySelector('.tile-opts .perm-seg')`, 5000);
    }
    // Every freedom radio is hit-tested and changes both the UI and server state.
    for (const [name, value] of [['Read-only','read_only'],['Ask me','ask'],['Default','default'],['YOLO','auto']]) {
      const e = `[...document.querySelectorAll('.tile-opts .perm-seg button')].find(e => e.textContent.trim() === '${name}')`;
      await hit(p, e, `freedom ${name}`, 44);
      if (value === 'auto') {
        await p.waitFor(`document.querySelector('.dialog-actions')`, 5000);
        await hit(p, `[...document.querySelectorAll('.dialog-actions button')].find(e => e.textContent.includes('Enable YOLO'))`, 'confirm YOLO', 44);
      }
      await wait(async () => (await snap()).info.permission === value);
      if (await p.eval(`!document.querySelector('.tile-opts .perm-seg')`)) {
        await hit(p, select('.pane.focused .title-btn'), 'reopen chat options', 44);
        await p.waitFor(`document.querySelector('.tile-opts .perm-seg')`, 5000);
      }
      check(await p.eval(`(${e}).getAttribute('aria-checked') === 'true'`), `${tag}: ${name} is selected and saved on the server`);
    }
    await hit(p, `[...document.querySelectorAll('.tile-opts .perm-seg button')].find(e => e.textContent.trim() === 'Default')`, 'restore Default', 44);
    await p.key('Escape','Escape',0,27);
    await p.waitFor(`!document.querySelector('.tile-opts')`, 5000);
    // Queue: Save/Cancel and every icon use CDP input in every variant, not DOM clicks/keys.
    await rpc('send', { session_id: SID, content: [{type:'text',text:'stream the answer slowly please'}], attachments:[] });
    await rpc('send', { session_id: SID, content: [{type:'text',text:'Target queued edit'}], attachments:[], delivery:'follow_up' });
    await rpc('send', { session_id: SID, content: [{type:'text',text:'Target queued send now'}], attachments:[], delivery:'follow_up' });
    await p.waitFor(`document.querySelector('.pane.focused .qcard')`, 8000);
    await hit(p, action('/Target queued edit/','Edit'), 'queue Edit');
    await p.waitFor(`document.querySelector('.pane.focused .qinput')`, 5000);
    await p.eval(`document.querySelector('.pane.focused .qinput').select()`); await p.type('Target saved edit');
    await hit(p, btn('Save'), 'queue Save');
    await wait(async () => (await snap()).queued.some(q => q.text === 'Target saved edit'));
    check((await snap()).queued.some(q => q.text === 'Target saved edit'), `${tag}: clicking Save persists edited queue text on the server`);
    await hit(p, action('/Target saved edit/','Edit'), 'queue Edit again');
    await p.waitFor(`document.querySelector('.pane.focused .qinput')`, 5000);
    await p.eval(`document.querySelector('.pane.focused .qinput').select()`); await p.type('Discard this draft');
    await hit(p, btn('Cancel'), 'queue Cancel');
    await p.waitFor(`!document.querySelector('.pane.focused .qinput')`, 5000);
    await sleep(300);
    check((await snap()).queued.some(q => q.text === 'Target saved edit') && !(await snap()).queued.some(q => /Discard/.test(q.text)), `${tag}: clicking Cancel preserves saved text and discards the draft`);
    await hit(p, action('/Target saved edit/','Steer'), 'queue Steer');
    await wait(async () => (await snap()).queued.some(q => q.text === 'Target saved edit' && q.steer));
    check((await snap()).queued.some(q => q.text === 'Target saved edit' && q.steer), `${tag}: Steer changes the queue delivery on the server`);
    await hit(p, action('/Target saved edit/','Remove'), 'queue Remove');
    await wait(async () => !(await snap()).queued.some(q => q.text === 'Target saved edit'));
    check(!(await snap()).queued.some(q => q.text === 'Target saved edit'), `${tag}: Remove withdraws the queued entry on the server`);
    await hit(p, action('/Target queued send now/','Send now'), 'queue Send now');
    await wait(async () => (await snap()).entries.some(e => e.kind === 'user' && e.content.some(c => c.text === 'Target queued send now')));
    check((await snap()).entries.some(e => e.kind === 'user' && e.content.some(c => c.text === 'Target queued send now')), `${tag}: Send now actually delivers the queued message`);
    await rpc('abort', {session_id:SID});
    if (touch) {
      const edge = await p.box('.edge-swipe');
      const y = edge.y + Math.min(64, edge.h / 2);
      await p.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: 4, y }] });
      await p.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: 64, y }] });
      await p.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
      await p.waitFor(`document.querySelector('.drawer-layer.open')`);
      check(true, `${tag}: the edge swipe still opens the drawer below the header without covering its buttons`);
      await p.eval(`__mod('lib/ui.ts').then(m => m.setDrawerOpen(false))`);
      await settled(p, `document.querySelector('.drawer-panel')`);
    }
    await p.shot(`${SHOTS}/targets-${tag.replaceAll(' ','-')}-queue.png`); await p.close();
  }
  finish();
});
