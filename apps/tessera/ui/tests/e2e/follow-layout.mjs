// Intent/engine-timing regressions plus real mock streaming, with native anchoring disabled.
// Desktop, Fold cover (344px), unfolded Fold; both themes. No real server/profile/model.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';

// Synthetic 600x340 PNG. Markdown deliberately allows only inline images, never remote fetches.
const IMAGE = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAlgAAAFUAQMAAADCgtB5AAAABlBMVEVUe2r////oXxOFAAAAMElEQVR42u3BgQAAAADDoPlTX+EAVQEAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB8BmTwAAGkYMQDAAAAAElFTkSuQmCC';

const T = '.pane.focused .transcript';
const open = (p, sid) => p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${sid}', { pane: m.workspace.focused() }))`);
const gap = `(() => { const t = document.querySelector('${T}'); return t.scrollHeight - t.clientHeight - t.scrollTop; })()`;
const away = `!!document.querySelector('.pane.focused .activity-jump')`;
const live = `!!document.querySelector('.pane.focused .send-btn.stop')`;
const frame = p => p.eval(`new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))`);

async function timing(p) {
  const results = await p.eval(`(async () => {
    const { Follow } = await __mod('lib/follow.ts');
    const el = document.createElement('div'), body = document.createElement('div');
    el.style.cssText = 'position:fixed;left:0;top:0;height:100px;width:200px;overflow:auto;overflow-anchor:none;scroll-behavior:auto';
    body.style.height = '1000px'; el.append(body); document.body.append(el);
    const f = new Follow(el), out = {};
    const scroll = () => el.dispatchEvent(new Event('scroll'));
    const wheel = dy => el.dispatchEvent(new WheelEvent('wheel', { deltaY: dy, bubbles: true }));
    const wait = () => new Promise(r => setTimeout(r, 220));
    try {
      f.attach(); scroll(); scroll();
      // WebKit-like clamping event delivered AFTER the next content burst, not at its old end.
      body.style.height = '200px'; void el.scrollTop; body.style.height = '1200px'; scroll();
      out.unowned = f.following && f.gap() <= 1;
      el.scrollTo({ top: 0, behavior: 'smooth' }); f.attach(); await wait();
      out.smooth = f.following && f.gap() <= 1;
      wheel(-10); el.scrollTop -= 10; body.style.height = '1400px'; f.stick(); scroll(); await wait();
      out.threshold = f.following && f.gap() <= 1;
      const hold = e => e.stopImmediatePropagation();
      el.addEventListener('scroll', hold, { capture: true });
      wheel(-200); el.scrollTop -= 200; await wait();
      out.delayed = !f.following && f.gap() >= 200;
      el.removeEventListener('scroll', hold, { capture: true });
      f.attach();
      wheel(-200); el.scrollTop -= 200; scroll(); await wait();
      out.user = !f.following;
      // Native scroll has reached the old painted end. RO runs before the delayed scroll event.
      wheel(200); el.scrollTop = el.scrollHeight; body.style.height = '1600px'; f.stick(); scroll(); await wait();
      out.burst = f.following && f.gap() <= 1;
      wheel(-100); el.scrollTop -= 100; scroll(); await wait();
      body.style.height = '300px'; f.stick(); scroll(); await wait();
      body.style.height = '1000px'; f.stick(); scroll(); await wait();
      out.clamp = !f.following && f.gap() > 500;
      // A card's programmatic position correction does not masquerade as a return to the end.
      f.scrollTo(el.scrollHeight); scroll(); scroll(); await wait();
      body.style.height = '1400px'; f.stick();
      out.owned = !f.following && f.gap() > 300;
      f.attach();
      const nested = document.createElement('div');
      nested.style.cssText = 'height:50px;overflow:auto'; nested.innerHTML = '<div style="height:500px">tool output</div>';
      body.append(nested); nested.scrollTop = 300;
      nested.dispatchEvent(new WheelEvent('wheel', { deltaY: -100, bubbles: true }));
      body.style.height = '1800px'; f.stick();
      out.nested = f.following && f.gap() <= 1;
      return out;
    } finally { f.dispose(); el.remove(); }
  })()`);
  for (const [key, name] of Object.entries({
    unowned: 'delayed layout/programmatic scrolls and duplicate events never detach',
    smooth: 'attach cancels a smooth-scroll target racing the end',
    threshold: 'a small gesture within 24px remains attached',
    delayed: 'a scroll event delayed past gesture settlement never snaps the reader back',
    user: 'a genuine gesture leaving the end detaches',
    burst: 'return to the painted end repins through RO-before-scroll burst timing',
    clamp: 'layout clamping then growth never repins a detached reader',
    owned: 'owned reading corrections cannot resume following',
    nested: 'wheel inside a scrollable tool does not pause the transcript',
  })) check(results[key], `engine timing: ${name}`, results);
}

// Test observer runs after the transcript's observer, before paint (not a timer sampling old layout).
const watch = `(() => {
  const t = document.querySelector('${T}');
  const w = window.__layout = { rows: [], on: true };
  w.ro = new ResizeObserver(() => {
    if (w.on) w.rows.push({ gap: t.scrollHeight - t.scrollTop - t.clientHeight, sh: t.scrollHeight, h: t.clientHeight });
  });
  w.ro.observe(t); w.ro.observe(t.querySelector('.transcript-inner'));
})()`;
async function pinned(p, label, scenario, action) {
  await p.eval(watch);
  const before = await p.eval(gap);
  await action();
  await sleep(300);
  const r = await p.eval(`(() => { const w = window.__layout; w.on = false; w.ro.disconnect(); return { rows: w.rows, gap: ${gap}, away: ${away}, live: ${live} }; })()`);
  const worst = Math.max(0, ...r.rows.map(x => x.gap));
  check(before <= 2 && r.gap <= 2 && !r.away && r.live && r.rows.length > 0 && worst <= 2,
    `${label}: ${scenario} stays pinned before paint (${r.rows.length} layouts, worst ${worst.toFixed(1)}px)`, r);
}
async function wheel(p, dy) {
  const g = await p.eval(`(() => { const r = document.querySelector('${T}').getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()`);
  await p.send('Input.dispatchMouseEvent', { type: 'mouseWheel', ...g, deltaX: 0, deltaY: dy });
}
async function drag(p, dy) {
  const g = await p.eval(`(() => { const r = document.querySelector('${T}').getBoundingClientRect(); return { x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2 - ${dy > 0 ? 80 : -80}) }; })()`);
  await p.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [g] });
  for (let k = 1; k <= 20; k++) {
    await p.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: g.x, y: g.y + Math.round(dy * k / 20) }] });
    await sleep(16);
  }
  await sleep(160); // Stop the finger first: distinguish drag from uncontrolled fixture momentum.
  await p.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
}
async function reader(p, label, mobile) {
  if (mobile) await drag(p, 180); else await wheel(p, -480);
  await p.waitFor(away);
  await sleep(500);
  const before = await p.eval(`(() => { const t = document.querySelector('${T}'); return { top: t.scrollTop, sh: t.scrollHeight }; })()`);
  await sleep(1100);
  const after = await p.eval(`(() => { const t = document.querySelector('${T}'); return { top: t.scrollTop, sh: t.scrollHeight, live: ${live}, away: ${away} }; })()`);
  check(after.live && after.away && after.sh > before.sh + (mobile ? 20 : 100) && Math.abs(after.top - before.top) <= 1,
    `${label}: real ${mobile ? 'touch drag' : 'wheel up'} mid-stream stays put through bursts`, { before, after });
  // Native scroll back to the end, never a programmatic scrollTop assignment.
  for (let k = 0; k < 35 && await p.eval(away); k++) {
    if (mobile) await drag(p, -300); else await wheel(p, 3000);
    await sleep(220);
  }
  await p.waitFor(`!${away} && ${gap} <= 2`, 8000);
  await pinned(p, label, 'user scroll back to bottom resumes streaming follow', () => sleep(700));
  if (mobile) await drag(p, 100); else await wheel(p, -300);
  await p.waitFor(away);
  await p.click('.pane.focused .activity-jump');
  await pinned(p, label, 'Jump to live resumes follow', () => sleep(500));
}

run(async () => {
  const ws = await wsClient('Follow layout streaming');
  for (const [name, w, h, mobile, dpr] of [
    ['desktop', 1440, 900, false, 1], ['cover', 344, 820, true, 2.6], ['fold', 768, 900, true, 2.6],
  ]) for (const theme of ['light', 'dark']) {
    if (process.env.ONLY && !`${name}-${theme}`.includes(process.env.ONLY)) continue;
    const label = `${name}-${theme}`;
    await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
    const p = await device();
    try {
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Follow ${label}`); await p.theme(theme);
      await p.eval(`(() => { const s = document.createElement('style'); s.textContent = '* { overflow-anchor: none !important; }'; document.head.append(s); })()`);
      if (name === 'desktop' && theme === 'light') await timing(p);
      if (process.env.TIMING_ONLY) continue;
      const fixture = (await ws.rpc('mock_phone_lag', { chats: 1, calls: 12, helpers: 3 })).result;
      const sid = fixture.parents[0].id;
      await open(p, sid);
      await p.waitFor(`document.querySelector('${T} .steps-head') && document.querySelector('.pane.focused .agents-head')`);
      await frame(p);
      check(await p.eval(gap) <= 2, `${label}: opening a populated chat starts at bottom without overflow-anchor`);
      const code = '```ts\n' + Array.from({ length: 80 }, (_, i) => `const value${i} = await request(${i});`).join('\n') + '\n```\n\n';
      const image = `![Late diagram](${IMAGE})\n\n`;
      const text = image + code + Array.from({ length: 350 }, (_, i) => `## Burst ${i}\n\n${'A growing **markdown** block with code and text. '.repeat(16)}\n\n`).join('');
      await ws.rpc('mock_stream', { session_id: sid, text, thinking: 1500,
        chunk: mobile ? [12, 60] : [200, 1400], delay: mobile ? [80, 250] : [40, 180], seed: 9 });
      await p.waitFor(`document.querySelector('${T} .thinking.live .thinking-head')`, 10000);
      await pinned(p, label, 'thinking expansion while generating', () => p.click(`${T} .thinking.live .thinking-head`));
      await p.waitFor(`document.querySelector('${T} .md.streaming code')?.textContent.includes('value79')`, 30000);
      // Defer the accepted inline image's source, then let the browser actually load/decode it late.
      // Remote-image interception would test a feature Tessera intentionally refuses for privacy.
      await p.eval(`(() => { const img = document.querySelector('${T} .md.streaming img');
        window.__lateImage = img; window.__lateSource = img.getAttribute('src'); img.removeAttribute('src'); })()`);
      await p.waitFor(`window.__lateImage.naturalHeight === 0`);
      check(await p.eval(`!!window.__lateSource?.startsWith('data:image/png;base64,')`), `${label}: real streamed inline image is deferred for a late load`);
      await pinned(p, label, 'long reply/code blocks grow in irregular server bursts', () => sleep(700));
      await pinned(p, label, 'late image decoding increases content height', async () => {
        await p.eval(`window.__lateImage.src = window.__lateSource`);
        await p.waitFor(`window.__lateImage.naturalHeight === 340`);
      });
      check(await p.eval(`document.querySelector('${T} img').getBoundingClientRect().height`) > 100, `${label}: the late image really occupies layout space`);
      await pinned(p, label, 'steps strip expansion/folding is not scroll-away', async () => {
        await p.eval(`document.querySelector('${T} .steps-head').click()`); await sleep(220);
        await p.eval(`document.querySelector('${T} .steps-head').click()`);
      });
      await pinned(p, label, 'tool card expanding/collapsing is not scroll-away', async () => {
        await p.eval(`(() => { const s = document.querySelector('${T} .steps-head'); if (s.getAttribute('aria-expanded') !== 'true') s.click(); })()`);
        await p.eval(`document.querySelector('${T} .tool-head').click()`); await sleep(220);
        await p.eval(`document.querySelector('${T} .tool-head').click()`);
      });
      await pinned(p, label, 'helper bar expansion/folding resizes the transcript', async () => {
        await p.eval(`document.querySelector('.pane.focused .agents-head').click()`); await sleep(220);
        await p.eval(`document.querySelector('.pane.focused .agents-head').click()`);
      });
      await pinned(p, label, 'a font loading/reflowing after first mount', () => p.eval(`(async () => {
        const f = new FontFace('FollowFixture', 'url(/src/assets/fonts/marcellus-latin-400-normal.woff2)');
        await f.load(); document.fonts.add(f); document.querySelector('${T} .transcript-inner').style.fontFamily = 'FollowFixture';
      })()`));
      await pinned(p, label, 'composer growing through multiple lines', async () => {
        await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
        await p.type('A draft\nwith several lines\nthat grows\nwithout sending\nthe message.');
      });
      await pinned(p, label, 'keyboard-sized viewport reduction while composer focused', () => p.viewport(w, h - 260, mobile, dpr));
      await pinned(p, label, 'window/unfold resize restores the larger viewport', () => p.viewport(w, h, mobile, dpr));
      await p.eval(`document.querySelector('.pane.focused .composer textarea').blur()`);
      await reader(p, label, mobile);
      await p.shot(`${SHOTS}/follow-layout-${label}.png`);
      const other = (await ws.rpc('mock_session', { title: 'Other populated tile', entries: [{ kind: 'assistant', text: 'Other history.\n\n'.repeat(100) }] })).result;
      const originalPane = await p.eval(`__mod('lib/workspace.ts').then(m => m.workspace.focused())`);
      if (!mobile) {
        await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${other.id}', { zone: 'right' }))`);
        await p.waitFor(`document.querySelectorAll('.pane .transcript').length === 2`);
        await sleep(400);
        const all = await p.eval(`[...document.querySelectorAll('.pane .transcript')].map(t => t.scrollHeight - t.scrollTop - t.clientHeight)`);
        check(all.every(x => x <= 2), `${label}: splitting a tile pins both independent transcripts`, all);
      }
      await p.eval(`__mod('lib/workspace.ts').then(m => m.focusPane('${originalPane}'))`);
      if (mobile) {
        await drag(p, 150); await p.waitFor(away);
        await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${other.id}', { newPane: true }))`);
        // An unfolded Fold shows two tiles at once; a third actually hides the first.
        if (w >= 600) await p.eval(`__mod('lib/workspace.ts').then(m => m.newChat({ newPane: true }))`);
        await sleep(350);
        await p.eval(`__mod('lib/workspace.ts').then(m => m.focusPane('${originalPane}'))`);
      } else {
        await wheel(p, -350); await p.waitFor(away);
        await p.eval(`__mod('lib/workspace.ts').then(m => { const id = m.paneShowing('${other.id}'); m.focusPane(id); m.toggleMaximize(id); })`);
        await sleep(350);
        await p.eval(`__mod('lib/workspace.ts').then(m => { m.toggleMaximize(m.workspace.focused()); m.focusPane('${originalPane}'); })`);
      }
      await sleep(350);
      check(await p.eval(gap) <= 2 && !await p.eval(away), `${label}: reopening a retained hidden tile starts at bottom`);
      // Real scroll away, then replace and reopen that pane: fresh transcript must start attached.
      if (mobile) await drag(p, 150); else await wheel(p, -350);
      await p.waitFor(away);
      await open(p, other.id); await sleep(300); await open(p, sid); await sleep(400);
      check(await p.eval(gap) <= 2 && !await p.eval(away), `${label}: switching chats and reopening starts at bottom`);
      // Finished helper reports in a parent that is generating its next reply.
      const audit = 'ses_7a1c5e93b2d0';
      await open(p, audit);
      await p.waitFor(`document.querySelector('${T} .report-head')`);
      await ws.rpc('mock_stream', { session_id: audit, text, chunk: [200, 1400], delay: [40, 180] });
      await p.waitFor(live);
      await frame(p);
      await pinned(p, label, 'helper report card expands during a live reply without detaching', async () => {
        await p.eval(`document.querySelector('${T} .report-head').click()`);
        await p.waitFor(`document.querySelector('${T} .report.open .report-body')`);
      });
      await pinned(p, label, 'helper report card folds during a live reply without detaching', () => p.eval(`document.querySelector('${T} .report-head').click()`));
      check(!p.errors.length, `${label}: no page errors`, p.errors);
    } finally { await p.close(); }
  }
  ws.close(); finish();
});
