// Moving a web view by its top bar. The address fills most of the bar, so until it is being edited
// it is part of what you grab: a press and move there lifts the tile, a click (a tap) edits it.
// Desktop tiles also keep a strip of empty bar beside the address. While the address is edited,
// pressing and moving in it selects text and moves nothing. Phones: a long press lifts, a tap edits.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const CHAT = "ses_7d3a91e5c4b2"; // "Add a /healthz endpoint"
const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const box = (p, sel) => p.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) return null; const r = e.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height, r: r.right, cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);
const lifted = (p) => p.eval(`document.documentElement.classList.contains('dragging-now')`);
const editing = (p) => p.eval(`!!document.querySelector('.wv-addr.editing') && document.activeElement?.classList.contains('wv-input')`);
const touch = (p, type, pts) => p.send("Input.dispatchTouchEvent", { type, touchPoints: pts.map(([x, y], id) => ({ x, y, id })) });

/** A chat and its web view, side by side; returns their tile ids. */
async function setup(p) {
  const C = await ws(p, `return m.openSession('${CHAT}', { pane: m.workspace.focused() })`);
  await sleep(500);
  const board = await mock("/__mock/board", { session_id: CHAT, kind: "url", ask: false });
  await p.waitFor(`document.querySelector('.web-view .wv-input')?.value`, 8000).catch(() => {});
  await sleep(600);
  const W = await ws(p, `return m.paneShowingBoard('${board.id}')`);
  return { C, W };
}

run(async () => {
  // ── desktop ──
  {
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, "Grab desktop");
    const { C, W } = await setup(p);
    check(!!C && !!W && C !== W, "a chat and its web view in two tiles", { C, W });
    const addr = `[data-pane-id="${W}"] .wv-addr`;
    const look = await p.eval(`(() => { const a = document.querySelector('${addr}'); const i = a.querySelector('.wv-input'); const bar = a.closest('.wv-bar').getBoundingClientRect(); const ctl = a.closest('.wv-bar').querySelector('.head-controls').getBoundingClientRect(); return { cursor: getComputedStyle(a).cursor, input: getComputedStyle(i).pointerEvents, gap: Math.round(ctl.left - a.getBoundingClientRect().right), bar: Math.round(bar.width) }; })()`);
    check(look.input === "none" && look.cursor === "default", "not being edited, the address is part of the bar (no text cursor, presses go to the bar)", look);
    check(look.gap >= 14, "an empty strip of bar beside the address to take hold of", look);
    await p.shot(`${SHOTS}/webview-grab-bar-desktop.png`);

    // A press and move on the address lifts the tile; dropped on the chat's left side, it goes there.
    let a = await box(p, addr);
    const c = await box(p, `[data-pane-id="${C}"]`);
    await p.down(a.cx, a.cy);
    const tx = c.x + c.w * 0.18, ty = c.cy;
    let wasLifted = false;
    for (let i = 1; i <= 16; i++) {
      await p.move(a.cx + ((tx - a.cx) * i) / 16, a.cy + ((ty - a.cy) * i) / 16);
      await sleep(16);
      if (i === 8) wasLifted = await lifted(p);
    }
    await sleep(150);
    await p.shot(`${SHOTS}/webview-grab-lifted-desktop.png`);
    await p.up(tx, ty);
    await sleep(800);
    check(wasLifted, "a press and move on the address lifts the tile");
    const moved = await p.eval(`document.querySelector('[data-pane-id="${W}"]').getBoundingClientRect().left < document.querySelector('[data-pane-id="${C}"]').getBoundingClientRect().left`);
    check(moved, "…and dropping it on the chat's left side moves it there");
    check(!(await editing(p)), "…without starting to edit the address");

    // A click edits: the field has the keyboard, its address selected.
    a = await box(p, addr);
    await p.mouse(a.cx, a.cy);
    await sleep(250);
    const sel = await p.eval(`(() => { const i = document.querySelector('${addr} .wv-input'); return { all: i.selectionStart === 0 && i.selectionEnd === i.value.length && i.value.length > 0, v: i.value }; })()`);
    check((await editing(p)) && sel.all, "a click on the address edits it (all of it selected)", sel);
    // Editing: a press and move there selects text, it doesn't move the tile.
    const order0 = await ws(p, `return m.workspace.order().join()`);
    a = await box(p, addr);
    await p.down(a.x + 40, a.cy);
    for (let i = 1; i <= 10; i++) {
      await p.move(a.x + 40 + i * 40, a.cy + i * 12);
      await sleep(16);
    }
    const liftedWhileEditing = await lifted(p);
    await p.up(a.x + 440, a.cy + 120);
    await sleep(400);
    check(!liftedWhileEditing && (await ws(p, `return m.workspace.order().join()`)) === order0, "while it is edited, dragging in the address moves nothing");
    await p.key("Escape", "Escape", 0, 27);
    await sleep(200);
    check(!(await p.eval(`!!document.querySelector('${addr}.editing')`)), "Esc stops editing");

    // The empty strip beside the address lifts the tile too (then Esc puts it back).
    a = await box(p, addr);
    const sx = a.r + 7;
    const under = await p.eval(`(() => { const e = document.elementFromPoint(${sx}, ${a.cy}); return e?.closest('button, input, form') ? 'control' : e?.closest('.wv-bar') ? 'bar' : 'other'; })()`);
    check(under === "bar", "beside the address is bare bar", under);
    await p.down(sx, a.cy);
    for (let i = 1; i <= 8; i++) {
      await p.move(sx + i * 12, a.cy + i * 20);
      await sleep(16);
    }
    check(await lifted(p), "a press and move on the strip lifts the tile");
    await p.key("Escape", "Escape", 0, 27);
    await p.up(sx + 96, a.cy + 160);
    await sleep(500);
    check(!(await lifted(p)), "…and Esc puts it back");

    // A lone web view: nothing to move, a click edits.
    await ws(p, `m.closePane('${C}')`);
    await sleep(500);
    a = await box(p, addr);
    await p.mouse(a.cx, a.cy);
    await sleep(250);
    check(await editing(p), "a web view alone in its mosaic: a click on the address edits it");
    await p.key("Escape", "Escape", 0, 27);
    check(p.errors.length === 0, "desktop: no script errors", p.errors.slice(0, 3));
    await p.close();
  }

  // ── the unfolded Fold (two tiles side by side, touch) and the folded phone ──
  for (const [label, w, h] of [["unfolded", 690, 829], ["folded", 344, 882]]) {
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(w, h, true, 2.625);
    await signIn(p, `Grab ${label}`);
    const { C, W } = await setup(p);
    await ws(p, `m.focusPane('${W}')`);
    await sleep(800);
    const addr = `[data-pane-id="${W}"] .wv-addr`;
    let a = await box(p, addr);
    await p.tap(a.cx, a.cy);
    await sleep(300);
    check(await editing(p), `${label}: a tap on the address edits it`, await p.eval(`(() => { const e = document.elementFromPoint(${a.cx}, ${a.cy}); return e ? e.tagName + '.' + e.className + ' in ' + (e.closest('[class]')?.parentElement?.className ?? '') : null; })()`));
    await p.eval(`document.activeElement?.blur()`);
    await sleep(300);
    if (label === "unfolded") {
      // A long press on the address lifts the tile; dropped on the chat's left side, it goes before it.
      const before = await ws(p, `return m.workspace.order().indexOf('${W}') > m.workspace.order().indexOf('${C}')`);
      a = await box(p, addr);
      const c = await box(p, `[data-pane-id="${C}"]`);
      await touch(p, "touchStart", [[a.cx, a.cy]]);
      await sleep(480);
      const tx = c.x + c.w * 0.12, ty = c.cy;
      for (let k = 1; k <= 14; k++) {
        await touch(p, "touchMove", [[a.cx + ((tx - a.cx) * k) / 14, a.cy + ((ty - a.cy) * k) / 14]]);
        await sleep(20);
      }
      const up = await lifted(p);
      await sleep(200);
      await touch(p, "touchEnd", []);
      await sleep(900);
      const after = await ws(p, `return m.workspace.order().indexOf('${W}') < m.workspace.order().indexOf('${C}')`);
      check(up, `${label}: a long press on the address lifts the tile`);
      check(before && after, `${label}: …and it moves before the chat`, { before, after });
      check(!(await editing(p)), `${label}: …without editing the address`);
    }
    await p.shot(`${SHOTS}/webview-grab-${label}.png`);
    check(p.errors.length === 0, `${label}: no script errors`, p.errors.slice(0, 3));
    await p.close();
  }
  finish();
});
