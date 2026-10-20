// The walkthrough's four freedom presets: one desktop row, two-by-two on phone/Fold,
// with their labels and controls fully inside the page. Synthetic settings only.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from './cdp.mjs';
const SHAPES = { desktop: [1440, 900, false, 1], phone: [344, 882, true, 2], fold: [884, 1104, true, 1.5] };
run(async () => {
  for (const [shape, [w, h, mobile, dpr]] of Object.entries(SHAPES)) {
    for (const theme of ['light', 'dark']) {
      await mock('/__mock/reset');
      await mock('/__mock/mosaics/reset');
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Safety polish ${shape} ${theme}`);
      await p.theme(theme);
      await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings('safety'))`);
      await p.waitFor(`document.querySelector('.perm-grid.wide')`, 8000);
      await sleep(250);
      const g = await p.eval(`(() => {
        const grid = document.querySelector('.perm-grid.wide'), r = grid.getBoundingClientRect();
        const cards = [...grid.querySelectorAll('.perm-card')];
        return { cols: getComputedStyle(grid).gridTemplateColumns.split(' ').length, count: cards.length,
          whole: cards.every(e => { const b = e.getBoundingClientRect(); return b.left >= r.left - 1 && b.right <= r.right + 1 && b.right <= innerWidth + 1; }),
          labels: cards.map(e => e.querySelector('.perm-name')?.textContent ?? e.querySelector('.perm-card-name')?.textContent ?? e.textContent),
          safe: !!document.querySelector('.sec-glance') && !document.querySelector('.form-error') };
      })()`);
      check(g.count === 4 && g.cols === (w > 960 ? 4 : 2), `${shape} ${theme}: four freedom presets in ${w > 960 ? 'one row' : 'two rows'}`, g);
      check(g.whole && g.labels.some(s => /Read-only/.test(s)), `${shape} ${theme}: all preset cards and Read-only stay inside the page`, g);
      check(g.safe, `${shape} ${theme}: the safety summary remains available, without form errors`, g);
      await p.shot(`${SHOTS}/polish-${shape}-${theme}-safety.png`);
      await p.close();
    }
  }
  finish();
});
