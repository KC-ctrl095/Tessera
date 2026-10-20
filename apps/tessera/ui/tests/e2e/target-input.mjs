// A real pointer action: measure the actual button, scroll it into view, hit-test its
// center and inset corners, then dispatch CDP input (never HTMLElement.click()).
import { check, sleep } from './cdp.mjs';
export async function hitTarget(p, expression, touch, label, minimum = 40) {
  if (minimum < 40) throw new Error('Target checks cannot exempt controls from the 40px minimum');
  await p.eval(`(${expression}).scrollIntoView({ block: 'nearest', inline: 'nearest' })`);
  await sleep(180);
  const g = await p.eval(`(() => {
    const e = (${expression}), r = e.getBoundingClientRect();
    const points = [[r.x+r.width/2,r.y+r.height/2],[r.left+3,r.top+3],[r.right-3,r.top+3],[r.left+3,r.bottom-3],[r.right-3,r.bottom-3]];
    return { w:r.width, h:r.height, cx:points[0][0], cy:points[0][1], hit:points.every(([x,y]) => e.contains(document.elementFromPoint(x,y))) };
  })()`);
  const ok = g.w >= minimum && g.h >= minimum && g.hit;
  check(ok, `${label}: ${minimum}px+ rect and center/corner hit tests`, g);
  if (!ok) throw new Error(`Cannot safely hit ${label}: ${JSON.stringify(g)}`);
  if (touch) await p.tap(g.cx, g.cy); else await p.mouse(g.cx, g.cy);
}
