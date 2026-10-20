// Detached desktop readers keep their paragraph through width/height changes.
// Also cover a resize that clamps scrollTop to the end: layout is not a request to follow.
import { check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';

const T = '.pane.focused .transcript';
const detached = `!!document.querySelector('.pane.focused .activity-jump')`;
const geometry = p => p.eval(`(() => {
  const t = document.querySelector('${T}');
  const probe = window.__resizeProbe;
  return { top: t.scrollTop, gap: t.scrollHeight - t.scrollTop - t.clientHeight,
    height: t.clientHeight, content: t.scrollHeight,
    probe: probe?.isConnected ? probe.getBoundingClientRect().top - t.getBoundingClientRect().top : null,
    detached: ${detached}, live: !!document.querySelector('.pane.focused .send-btn.stop') };
})()`);

run(async () => {
  for (const touch of [false, true]) {
    await mock('/__mock/reset');
    await mock('/__mock/mosaics/reset');
    const c = await wsClient('Detached resize fixture');
    const p = await device();
    const viewport = async (w, h) => {
      await p.viewport(w, h, false, 1);
      if (touch) await p.send('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
    };
    try {
      await viewport(1440, 900);
      await signIn(p, `resize-${touch ? 'coarse' : 'fine'}`);
      await p.eval(`(() => { const style = document.createElement('style'); style.textContent = '* { overflow-anchor: none !important; }'; document.head.append(style); })()`);
      const text = Array.from({ length: 60 }, (_, i) => `Paragraph ${i + 1}: ${'Synthetic text for reading while the window changes size. '.repeat(12)}\n\n`).join('');
      const s = (await c.rpc('mock_session', { title: 'Detached resize', entries: [
        { kind: 'user', text: 'Keep this paragraph visible.' }, { kind: 'assistant', text },
      ] })).result;
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${s.id}', { pane: m.workspace.focused() }))`);
      await p.waitFor(`document.querySelectorAll('${T} .md p').length >= 60`);
      await c.rpc('mock_stream', { session_id: s.id, size: 260000, chunk: [6, 14], delay: [20, 30], seed: 3 });
      await p.waitFor(`document.querySelector('.pane.focused .send-btn.stop')`);
      for (const theme of ['light', 'dark']) {
        const label = `${touch ? 'coarse' : 'fine'} ${theme}`;
        await p.theme(theme);
        await viewport(1440, 900);
        await sleep(300);
        const wheel = await p.eval(`(() => {
          const t = document.querySelector('${T}'), r = t.getBoundingClientRect();
          const probe = window.__resizeProbe = t.querySelectorAll('.md p')[25];
          return { x: r.left + r.width / 2, y: r.top + r.height / 2,
            deltaY: probe.getBoundingClientRect().top - r.top + 10, deltaX: 0, type: 'mouseWheel' };
        })()`);
        await p.send('Input.dispatchMouseEvent', wheel);
        await p.waitFor(detached);
        await sleep(250);
        let before = await geometry(p);
        check(before.probe !== null && before.probe < 0 && before.probe > -20 && before.gap > 100, `${label}: detached paragraph fixture is in view`, before);
        for (const [name, w, h] of [['narrower wrapping', 1120, 900], ['wider wrapping', 1440, 900], ['shorter window', 1440, 650], ['taller window', 1440, 1050]]) {
          await viewport(w, h);
          await sleep(450);
          const after = await geometry(p);
          check(after.live && after.detached && after.gap > 100, `${label}: ${name} does not reattach a reader`, after);
          check(after.probe !== null && Math.abs(after.probe - before.probe) <= 2, `${label}: ${name} preserves the reading paragraph`, { before, after });
          before = after;
        }
        await viewport(1440, 900);
        await sleep(250);
        const nearEnd = await p.eval(`(() => { const t = document.querySelector('${T}'), r = t.getBoundingClientRect();
          return { x: r.left + r.width / 2, y: r.top + r.height / 2,
            deltaY: t.scrollHeight - t.clientHeight - 100 - t.scrollTop, deltaX: 0, type: 'mouseWheel' }; })()`);
        await p.send('Input.dispatchMouseEvent', nearEnd);
        await p.waitFor(detached);
        await sleep(250);
        const preClamp = await geometry(p);
        await viewport(1440, 1600);
        await sleep(450);
        const clamped = await geometry(p);
        check(clamped.top < preClamp.top - 200, `${label}: height resize actually clamps scrollTop`, { preClamp, clamped });
        check(clamped.detached && clamped.live, `${label}: layout clamping alone does not resume following`, clamped);
        await p.eval(`(() => { const d = document.createElement('div'); d.className = 'resize-output-burst'; d.style.height = '400px'; document.querySelector('${T} .transcript-inner').append(d); })()`);
        await sleep(450);
        const grew = await geometry(p);
        check(grew.detached && grew.gap >= 300 && Math.abs(grew.top - clamped.top) <= 2, `${label}: later output cannot turn resize clamping into a reader gesture`, { clamped, grew });
        await p.eval(`document.querySelector('.resize-output-burst')?.remove()`);
      }
      check(!p.errors.length, `${touch ? 'coarse' : 'fine'}: no page errors`, p.errors);
    } finally {
      await p.close();
      c.close();
    }
  }
  finish();
});
