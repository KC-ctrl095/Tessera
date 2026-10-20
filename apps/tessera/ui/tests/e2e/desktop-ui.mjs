// Fixes-note 1 and 12: visible mosaic tabs (like browser tabs) and Bots → Add new bot. (Following the
// live end of a chat has its own suites: follow, follow-resize, follow-layout.)
// Synthetic data only. Fine/coarse desktop, cover screen and unfolded Fold; both themes.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const visible = (p, sel) => p.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) return false; const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0 && r.left >= 0 && r.right <= innerWidth + 1; })()`);
const hit = async (p, sel, touch) => { const b = await p.box(sel); if (b) await (touch ? p.tap(b.cx, b.cy) : p.mouse(b.cx, b.cy)); return !!b; };
const hittable = (p, sel) => p.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) return false; const r = e.getBoundingClientRect(); return e.contains(document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2)); })()`);
const drawer = async (p) => {
  await p.eval(`__mod('lib/ui.ts').then(u => u.setDrawerOpen(true))`);
  await p.waitFor(`document.querySelector('.drawer-layer.open') && new DOMMatrixReadOnly(getComputedStyle(document.querySelector('.drawer-panel')).transform).m41 === 0`);
};

run(async () => {
  for (const [label, width, height, touch] of [
    ['desktop-fine', 1440, 900, false], ['desktop-coarse', 1440, 900, true],
    ['folded', 344, 882, true], ['fold', 884, 1104, true],
  ]) {
    if (process.env.ONLY && !label.includes(process.env.ONLY)) continue;
    await mock('/__mock/reset');
    await mock('/__mock/mosaics/reset');
    const c = await wsClient(`Desktop UI ${label}`);
    const p = await device();
    await p.viewport(width, height, width < 1100, touch ? 2 : 1);
    // Coarse touchscreen desktop keeps a desktop UA and non-mobile viewport.
    if (touch) await p.send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
    await signIn(p, label);
    const detection = await ws(p, `return { mode: m.workspace.mode(), width: innerWidth, coarse: matchMedia('(pointer: coarse)').matches, fine: matchMedia('(pointer: fine)').matches }`);
    console.log('DETECTION', label, JSON.stringify(detection));
    check(detection.coarse === touch && detection.mode === (width >= 1100 ? 'tile' : width >= 640 ? 'dual' : 'mobile'), `${label}: width controls layout, pointer controls touch targets`, detection);
    if (width >= 640) {
      check(await visible(p, '.topbar .ws-pill'), `${label}: the first mosaic tab is visible even with only one mosaic`);
      check(await visible(p, '.topbar [aria-label="All mosaics"]') && await hittable(p, '.topbar [aria-label="All mosaics"]'), `${label}: All mosaics is obvious before making a second mosaic`);
    }
    const named = [];
    if (width >= 640 && await hit(p, '.topbar [aria-label="All mosaics"]', touch)) {
      await p.waitFor(`document.querySelector('.mosaics-page .mz-card[role="option"]')`);
      await p.shot(`${SHOTS}/desktop-ui-${label}-picker.png`);
      check(await p.eval(`document.querySelectorAll('.mosaics-page .mz-card[role="option"]').length === 1`), `${label}: All mosaics opens the existing picker with the first mosaic`);
      await hit(p, '.mosaics-page .overview-head .btn', touch);
      await p.waitFor(`document.querySelectorAll('.topbar .ws-pill').length === 2`);
      check((await ws(p, `return m.workspace.active()`)) === 1, `${label}: the picker's Mosaic button creates and opens another mosaic`);
      for (const [i, name] of ['Planning', 'Research', 'Build'].entries()) {
        const info = (await c.rpc('create_session', { title: `${name} content` })).result;
        const marker = `Unique ${name} mosaic content`;
        await c.rpc('send', { session_id: info.id, content: [{ type: 'text', text: marker }], attachments: [] });
        await c.rpc('abort', { session_id: info.id });
        named.push({ i, name, id: info.id, marker });
      }
      await ws(p, `const items = ${JSON.stringify(named)}; for (const n of items) { if (n.i === 2) m.newWorkspace(); m.switchWorkspace(n.i); m.openSession(n.id, { pane: m.workspace.focused() }); m.setMosaicName(n.i, n.name); } for (let i = 3; i < 9; i++) { m.newWorkspace(); m.newChat(); m.setMosaicName(i, 'Synthetic workspace ' + (i + 1)); }`);
      await sleep(300);
      check(await p.eval(`['Planning', 'Research', 'Build'].every((name, i) => document.querySelector('.topbar .ws-pill[data-ws="' + i + '"]')?.textContent.includes(name))`), `${label}: three tabs show their distinct mosaic names`);
      check(await visible(p, '.topbar [aria-label="All mosaics"]') && await visible(p, '.topbar .tb-new'), `${label}: nine mosaic tabs never push All mosaics or New offscreen`);
    }
    if (width < 640) {
      await ws(p, `for (let i = 1; i < 9; i++) { m.newWorkspace(); m.newChat(); } m.switchWorkspace(0);`);
    }
    for (const theme of ['light', 'dark']) {
      await p.eval(`__mod('lib/ui.ts').then(u => u.toasts().forEach(t => u.dismissToast(t.id)))`);
      await p.theme(theme);
      await sleep(200);
      for (const n of named) {
        const sel = `.topbar .ws-pill[data-ws="${n.i}"]`;
        await p.eval(`document.querySelector(${JSON.stringify(sel)}).scrollIntoView({ inline: 'nearest', block: 'nearest' })`);
        await sleep(150);
        await hit(p, sel, touch);
        await p.waitFor(`document.querySelector('.pane.focused .transcript')?.textContent.includes(${JSON.stringify(n.marker)})`, 8000);
        check((await ws(p, `return m.workspace.active()`)) === n.i && await p.eval(`document.querySelector('.pane.focused .transcript').textContent.includes(${JSON.stringify(n.marker)})`), `${label} ${theme}: clicking ${n.name} shows its own chat content`);
      }
      await p.shot(`${SHOTS}/desktop-ui-${label}-${theme}.png`);
      if (width < 1100) {
        await drawer(p);
        check(await visible(p, '.drawer-layer.open .sb-ws-all') && await hittable(p, '.drawer-layer.open .sb-ws-all'), `${label} ${theme}: the drawer's All mosaics stays reachable with nine mosaics`);
        const side = '.drawer-layer.open';
        await hit(p, `${side} .sb-tab[id$="-bots"]`, touch);
        await sleep(200);
        const add = `${side} [aria-label="Add new bot"]`;
        check(await visible(p, add), `${label} ${theme}: Bots has a simple Add new bot button`);
        if (touch) { const g = await p.box(add); check(g.w >= 44 && g.h >= 44, `${label} ${theme}: Add new bot has a real 44px touch target`, g); }
        await p.shot(`${SHOTS}/desktop-ui-${label}-bots-${theme}.png`);
        if (await hit(p, add, touch)) {
          await p.waitFor(`document.querySelector('.bot-editor .be-templates')`);
          check(await p.eval(`__mod('lib/ui.ts').then(u => u.botEditor()?.id === null && !u.drawerOpen())`), `${label} ${theme}: Add new bot opens the existing new editor and closes the drawer`);
          await p.shot(`${SHOTS}/desktop-ui-${label}-editor-${theme}.png`);
          if (label === 'folded' && theme === 'light') {
            // Existing form validation, server refusal, and secure defaults remain on this path.
            await p.clickText('.be-actions button', 'Create and keep editing');
            check(await p.eval(`/Give your bot a name/.test(document.querySelector('.bot-editor').textContent)`), 'new bot still requires a name');
            await p.eval(`(() => { const e = document.querySelector('.bot-editor input.input:not(.mono)'); Object.getOwnPropertyDescriptor(Object.getPrototypeOf(e), 'value').set.call(e, 'UI test bot'); e.dispatchEvent(new Event('input', { bubbles: true })); })()`);
            await p.eval(`__mod('lib/store.ts').then(m => { window.__saveBot = m.api.saveBot; m.api.saveBot = async () => { throw new Error('This device is not allowed to manage bots.'); }; })`);
            const count = (await c.rpc('list_bots')).result.length;
            await p.clickText('.be-actions button', 'Create and keep editing');
            check(await p.eval(`/not allowed to manage bots/.test(document.querySelector('.bot-editor').textContent)`) && (await c.rpc('list_bots')).result.length === count, 'existing editor shows a refused save and creates nothing');
            await p.eval(`__mod('lib/store.ts').then(m => { m.api.saveBot = window.__saveBot; delete window.__saveBot; })`);
            await p.clickText('.be-actions button', 'Create and keep editing');
            await p.waitFor(`document.querySelector('.be-actions')?.textContent.includes('Save changes')`);
            const made = (await c.rpc('list_bots')).result.find(b => b.name === 'UI test bot');
            check(made?.permission === 'default' && made.tools === 'assistant' && made.machines?.default === 'no_access' && made.secrets?.default === false, 'new button uses the existing safe permissions/tools/no-machine-access defaults', made && { permission: made.permission, tools: made.tools, machines: made.machines, secrets: made.secrets });
            await c.rpc('delete_bot', { id: made.id });
          }
          await p.eval(`__mod('lib/ui.ts').then(u => u.setBotEditor(null))`);
        }
        await p.eval(`__mod('lib/ui.ts').then(u => u.setDrawerOpen(false))`);
        if (label === 'folded' || label === 'fold') {
          // The server is genuinely empty, not just the client store.
          for (const b of (await c.rpc('list_bots')).result) await c.rpc('delete_bot', { id: b.id });
          await drawer(p);
          await hit(p, '.drawer-layer.open .sb-tab[id$="-bots"]', touch);
          await p.waitFor(`document.querySelector('.drawer-layer.open .sb-panel-head')?.textContent.includes('0 bots')`, 8000);
          const add = '.drawer-layer.open [aria-label="Add new bot"]';
          check((await c.rpc('list_bots')).result.length === 0 && await visible(p, add), `${label} ${theme}: a genuinely empty Bots list offers Add new bot`);
          const g = await p.box(add); check(g.w >= 44 && g.h >= 44, `${label} ${theme}: empty-state Add new bot has a real 44px touch target`, g);
          await p.shot(`${SHOTS}/desktop-ui-${label}-no-bots-${theme}.png`);
          await hit(p, add, touch);
          await p.waitFor(`document.querySelector('.bot-editor .be-templates')`, 8000);
          const name = `First ${label} ${theme} bot`;
          await p.eval(`(() => { const e = document.querySelector('.bot-editor input.input:not(.mono)'); Object.getOwnPropertyDescriptor(Object.getPrototypeOf(e), 'value').set.call(e, ${JSON.stringify(name)}); e.dispatchEvent(new Event('input', { bubbles: true })); })()`);
          await p.clickText('.be-actions button', 'Create and keep editing');
          await p.waitFor(`document.querySelector('.be-actions')?.textContent.includes('Save changes')`, 8000);
          const made = (await c.rpc('list_bots')).result;
          check(made.length === 1 && made[0].name === name && made[0].permission === 'default' && made[0].tools === 'assistant', `${label} ${theme}: empty state → Add new bot → editor → create reaches the server with safe defaults`, made.map(b => ({ name: b.name, permission: b.permission, tools: b.tools })));
          await p.shot(`${SHOTS}/desktop-ui-${label}-first-bot-${theme}.png`);
          await p.eval(`__mod('lib/ui.ts').then(u => u.setBotEditor(null))`);
        }
      }
    }
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    await p.close();
    c.close();
  }
  finish();
});
