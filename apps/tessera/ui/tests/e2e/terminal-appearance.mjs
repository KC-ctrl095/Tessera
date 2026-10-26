// Matched terminal previews at desktop, cover-phone and Fold density.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';
run(async () => {
  await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
  const probe = await wsClient('appearance probe');
  await probe.rpc('set_terminal_settings', {settings:{access:'sandboxed',idle_minutes:60}});
  for (const [label,w,h,touch,dpr] of [['desktop',1440,900,false,1],['phone',344,882,true,3],['fold',884,1104,true,2]]) {
    const p = await device(); await p.viewport(w,h,touch,dpr); await signIn(p,'Appearance '+label);
    await p.eval(`__mod('lib/workspace.ts').then(m=>m.openPane({type:'terminal',machine:null}))`);
    await p.waitFor(`document.querySelector('.xterm-rows')?.textContent.includes('Welcome')`);
    const palette = '\x1b[2J\x1b[H\x1b]2;Terminal preview\x07' +
      'Native terminal preview\r\n\r\n' +
      Array.from({length:16},(_,i)=>`\x1b[${i<8?30+i:90+i-8}m${String(i).padStart(2,'0')} \x1b[0m`).join('') + '\r\n\r\n' +
      '\x1b[1mBold text\x1b[0m  regular text\r\n' +
      '┌──────────────┐\r\n│ box drawing  │\r\n└──────────────┘\r\n' +
      '░▒▓█ ▀▄▌▐ ⠁⠃⠇⠏⠟⠿⣿\r\n\ue0a0 \ue0a1 \ue0a2 \ue0b0\ue0b1\ue0b2\ue0b3\r\n' +
      '\r\nhttps://example.com/docs\r\n\x1b[32mme@host\x1b[0m:~$ ';
    await p.eval(`__mod('lib/backend.ts').then(({backend})=>backend.events.emit({type:'terminal_output',id:document.querySelector('.term-screen').dataset.termId,seq:0,data:btoa(unescape(encodeURIComponent(${JSON.stringify(palette)})))}))`);
    await p.waitFor(`document.querySelector('.title-static')?.textContent === 'Terminal preview'`);
    for (const theme of ['dark','light']) {
      await p.theme(theme);
      await p.eval(`__mod('lib/terminal.ts').then(m=>m.setTerminalTheme('match'))`);
      await sleep(200);
      const appearance = await p.eval(`__mod('lib/terminal.ts').then(m => {
        const body = document.querySelector('.term-body');
        const rows = document.querySelector('.xterm-rows');
        const names = ['black','red','green','yellow','blue','magenta','cyan','white','brightBlack','brightRed','brightGreen','brightYellow','brightBlue','brightMagenta','brightCyan','brightWhite'];
        const rgb = hex => 'rgb(' + hex.slice(1).match(/../g).map(s=>parseInt(s,16)).join(', ') + ')';
        const colours = names.every((name,i) => getComputedStyle(rows.querySelector('.xterm-fg-'+i)).color === rgb(m.TERMINAL_THEMES['${theme}'][name]));
        const canvas = document.createElement('canvas'), ctx = canvas.getContext('2d');
        const widths = ['400','700'].flatMap(weight => {
          ctx.font = weight+' 14px "Tessera Terminal"';
          return [...'MWil─│┌┐└┘░▒▓█▀▄▌▐⠁⠟⣿\\ue0a0\\ue0a1\\ue0a2\\ue0b0\\ue0b1\\ue0b2\\ue0b3'].map(ch=>ctx.measureText(ch).width);
        });
        const grid = document.querySelector('.xterm-screen').getBoundingClientRect();
        const host = document.querySelector('.term-screen').getBoundingClientRect();
        return { colours, widths, font:getComputedStyle(rows).fontFamily, loaded:document.querySelector('.term-screen').dataset.font,
          background:getComputedStyle(body).backgroundColor === rgb(m.TERMINAL_THEMES['${theme}'].background),
          fits:grid.right <= host.right + 1 && grid.bottom <= host.bottom + 1,
          grid:{right:grid.right,bottom:grid.bottom},host:{right:host.right,bottom:host.bottom},
          keys:!document.querySelector('.term-keys') || getComputedStyle(document.querySelector('.term-keys')).backgroundColor === getComputedStyle(body).backgroundColor };
      })`);
      check(appearance.colours, `${label} ${theme}: all 16 ANSI colours match the named palette`);
      check(appearance.loaded === 'loaded' && appearance.font.includes('Tessera Terminal'), `${label} ${theme}: bundled font loaded and used`);
      check(Math.max(...appearance.widths) - Math.min(...appearance.widths) < 0.05, `${label} ${theme}: regular, bold, box, block, braille and Powerline have identical cell widths`, appearance.widths);
      check(appearance.background && appearance.keys && appearance.fits, `${label} ${theme}: fitted grid and surrounding body have one background`, {background:appearance.background,keys:appearance.keys,fits:appearance.fits,grid:appearance.grid,host:appearance.host});
      await p.shot(`${SHOTS}/appearance-${label}-${theme}.png`);
    }
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    await p.close();
  }
  const fallback = await device(); await fallback.viewport(344,882,true,3); await signIn(fallback,'Font fallback');
  await fallback.eval(`document.fonts.load = async () => { throw new Error('Font unavailable'); }`);
  await fallback.eval(`__mod('lib/workspace.ts').then(m=>m.openPane({type:'terminal',machine:null}))`);
  await fallback.waitFor(`document.querySelector('.xterm-rows')?.textContent.includes('Welcome')`);
  check(await fallback.eval(`document.querySelector('.term-screen').dataset.font === 'fallback' && !getComputedStyle(document.querySelector('.xterm-rows')).fontFamily.includes('Tessera Terminal')`), 'failed font load uses a stable fallback without late cell reflow');
  check(!fallback.errors.length, 'font failure does not break the terminal', fallback.errors);
  await fallback.close(); probe.close(); finish();
});
