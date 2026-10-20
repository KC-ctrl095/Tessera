// Settings → Web & voice → the web view browser (its More): how sharp pictures are while pages move
// (Auto / Sharp / Smooth), saved on the server, the plain note on a server
// that draws pages slowly, and on one with little memory (its own browser off
// until turned on). Desktop and phone, light and dark.
import { SHOTS, check, device, finish, run, signIn, sleep, wsClient } from "./cdp.mjs";

run(async () => {
  const ws = await wsClient("webview settings script");
  const info = async () => (await ws.rpc("get_browser")).result;
  /** The browser card's More (its finer choices live there since batch 16). */
  const more = `[...document.querySelectorAll('.disclosure')].find(d => /picture while pages move/.test(d.textContent))`;
  const open = async (p) => {
    await p.eval(`__mod('lib/ui.ts').then(u => { u.setDrawerOpen?.(false); u.openSettings('search'); })`);
    await p.waitFor(`!!(${more})`, 8000);
    await p.eval(`(() => { const d = ${more}; if (d.getAttribute('aria-expanded') !== 'true') d.click(); })()`);
    await p.waitFor(`[...document.querySelectorAll('.setting-title')].some(t => t.textContent === 'Picture while pages move')`, 8000);
    await p.eval(`[...document.querySelectorAll('.setting-title')].find(t => t.textContent === 'Picture while pages move').scrollIntoView({ block: 'center' })`);
    await sleep(300);
  };
  const row = `[...document.querySelectorAll('.setting-row')].find(r => r.querySelector('.setting-title')?.textContent === 'Picture while pages move')`;
  const state = (p) =>
    p.eval(`(() => { const r = ${row}; if (!r) return null; const b = [...r.querySelectorAll('.segmented button')]; return { options: b.map(x => x.textContent.trim()), on: b.find(x => x.getAttribute('aria-checked') === 'true')?.textContent.trim(), warn: b.filter(x => x.classList.contains('warn')).map(x => x.textContent.trim()), help: r.querySelector('.setting-help')?.textContent ?? '', slow: !!r.querySelector('.webview-slow') }; })()`);
  const choose = async (p, label) => {
    const at = await p.eval(`(() => { const b = [...(${row}).querySelectorAll('.segmented button')].find(x => x.textContent.trim() === ${JSON.stringify(label)}); const r = b.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; })()`);
    await p.mouse(at[0], at[1]);
    await sleep(500);
  };

  // A fast server: Auto, no note.
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Web view settings desktop");
  await open(p);
  let s = await state(p);
  check(JSON.stringify(s?.options) === JSON.stringify(["Auto", "Sharp", "Smooth"]), "three choices: Auto, Sharp, Smooth", s);
  check(s?.on === "Auto" && /as sharp as this server can draw smoothly/.test(s.help), "Auto is the default, explained in a line", s);
  check(!s?.slow && s?.warn.length === 0, "no note on a fast server", s);
  await p.shot(`${SHOTS}/webview-quality-desk-fast.png`);

  // Smooth is saved on the server and explained.
  await choose(p, "Smooth");
  s = await state(p);
  check(s?.on === "Smooth" && /least work for the server/.test(s.help), "Smooth: chosen, with what it means", s);
  check((await info()).quality === "smooth", "saved on the server");
  await choose(p, "Auto");
  check((await info()).quality === "auto", "back to Auto");

  // A slow server (a virtual machine's generic CPU): a plain note.
  await ws.rpc("mock_browser_speed", { speed: 14.6 });
  await p.eval(`__mod('lib/ui.ts').then(u => u.openSettings('general'))`);
  await sleep(300);
  await open(p);
  s = await state(p);
  check(s?.slow && /draws web pages slowly \(about 15 sharp frames a second\)/.test(s.help) && /Auto keeps them smooth/.test(s.help), "a slow server gets a plain note", s?.help);
  for (const theme of ["light", "dark"]) {
    await p.theme(theme);
    await sleep(250);
    const box = await p.eval(`(() => { const r = (${row}).closest('.card').getBoundingClientRect(); return { x: r.x - 8, y: r.y - 8, width: r.width + 16, height: r.height + 16 }; })()`);
    await p.shot(`${SHOTS}/webview-quality-desk-slow-${theme}.png`, box);
  }

  // A server with little memory: its own browser is off by default, said
  // plainly, and one switch turns it on.
  const low = (pg) =>
    pg.eval(`(() => { const n = document.querySelector('.webview-low-memory'); const r = n?.closest('.setting-row'); const t = r?.querySelector('[role=switch]'); const b = t?.getBoundingClientRect(); return n ? { text: n.textContent, on: t?.getAttribute('aria-checked'), quality: !!(${row}) || !!(${more}), at: b && [b.x + b.width / 2, b.y + b.height / 2], fits: document.documentElement.scrollWidth <= innerWidth + 1 && r.getBoundingClientRect().right <= innerWidth + 1 } : null; })()`);
  const openLow = async (pg) => {
    await pg.eval(`__mod('lib/ui.ts').then(u => u.openSettings('general'))`);
    await sleep(300);
    await pg.eval(`__mod('lib/ui.ts').then(u => { u.setDrawerOpen?.(false); u.openSettings('search'); })`);
    await pg.waitFor(`!!document.querySelector('.webview-low-memory')`, 8000);
    await pg.eval(`document.querySelector('.webview-low-memory').scrollIntoView({ block: 'center' })`);
    await sleep(300);
  };
  await ws.rpc("mock_browser_memory", { ram: 1073741824 });
  await openLow(p);
  let l = await low(p);
  check(/This server has 1\.0 GB of memory/.test(l?.text) && /one of your computers/.test(l?.text), "a small server says plainly why its web views are off", l?.text);
  check(l?.on === "false" && l.quality === false, "off, and the browser's other settings wait until it is on", l);
  for (const theme of ["light", "dark"]) {
    await p.theme(theme);
    await sleep(250);
    await p.shot(`${SHOTS}/webview-low-memory-desk-${theme}.png`);
  }
  await p.mouse(l.at[0], l.at[1]);
  await sleep(500);
  check((await info()).enabled !== false, "one switch turns web views on for this server");
  l = await low(p);
  check(l?.on === "true" && l.quality === true, "on: the switch shows it and the browser's settings are back", l);
  await p.mouse(l.at[0], l.at[1]);
  await sleep(500);
  check((await info()).enabled === false, "and off again");
  await p.close();

  // The note on a folded phone's cover screen.
  const c = await device();
  await c.viewport(344, 882, true, 2.625);
  await signIn(c, "Web view settings cover");
  for (const theme of ["dark", "light"]) {
    await c.theme(theme);
    await openLow(c);
    l = await low(c);
    check(l?.fits === true && /1\.0 GB/.test(l.text), `cover screen ${theme}: the note and its switch fit`, l);
    await c.shot(`${SHOTS}/webview-low-memory-cover-${theme}.png`);
  }
  await c.close();
  await ws.rpc("mock_browser_memory", { ram: null });

  // A phone: the row fits, and works by touch.
  const m = await device();
  await m.viewport(412, 915, true, 2.625);
  await signIn(m, "Web view settings phone");
  for (const theme of ["dark", "light"]) {
    await m.theme(theme);
    await open(m);
    const fits = await m.eval(`(() => { const r = ${row}; const seg = r.querySelector('.segmented').getBoundingClientRect(); return seg.right <= innerWidth + 1 && seg.left >= -1 && document.documentElement.scrollWidth <= innerWidth + 1; })()`);
    check(fits === true, `phone ${theme}: the choices fit the screen`, fits);
    await m.shot(`${SHOTS}/webview-quality-phone-${theme}.png`);
  }
  await choose(m, "Sharp");
  check((await info()).quality === "sharp", "a tap on the phone chooses Sharp");
  await choose(m, "Auto");
  await m.close();
  ws.close?.();
  finish();
});
