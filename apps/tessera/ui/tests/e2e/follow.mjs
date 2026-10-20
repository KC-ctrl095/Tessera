// Only a reader's gesture stops bottom-follow. Programmatic/layout scrolls are not intent.
// Wheel, paging keys, scrollbar, slow/fast touch and momentum all leave the reader undisturbed.
// Content above the view keeps its place; returning to the end resumes following.
// Desktop (1440 px) and the folded phone (344 px), light and dark.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

process.env.E2E_SCROLLBARS = '1';

const T = ".pane.focused .transcript";
const until = (p, expr, ms = 8000) => p.waitFor(expr, ms).then(() => true, () => false);
const open = (p, sid) => p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${sid}', { pane: m.workspace.focused() }))`);
const live = (p) => p.eval(`!!document.querySelector('.pane.focused .send-btn.stop')`);
const geometry = (p) =>
  p.eval(`(() => { const t = document.querySelector('${T}'); const r = t.getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2, top: r.top, bottom: r.bottom, right: r.right, h: t.clientHeight, sh: t.scrollHeight, st: t.scrollTop }; })()`);

/** Every frame from now on: scroll position, gap to the end, where the watched element is. */
const WATCH = `(() => {
  const t = document.querySelector('${T}');
  const W = (window.__w = { rows: [], ats: [], on: true, gaps: [], el: null, input: Infinity, released: Infinity, host: t, writes: [] });
  if (!t.__watchScroll) {
    // Native scrollbar motion bypasses these JS setters; app writes do not.
    const descriptor = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollTop');
    Object.defineProperty(t, 'scrollTop', { configurable: true,
      get() { return descriptor.get.call(this); },
      set(value) {
        const w = window.__w;
        if (w?.on && w.host === this) w.writes.push({ t: performance.now(), from: this.scrollTop, to: value });
        descriptor.set.call(this, value);
      },
    });
    const scrollTo = t.scrollTo;
    t.scrollTo = function(...args) {
      const w = window.__w;
      if (w?.on && w.host === this) w.writes.push({ t: performance.now(), from: this.scrollTop, to: args[0]?.top ?? args[1] });
      return scrollTo.apply(this, args);
    };
    t.__watchScroll = true;
  }
  // Where the watched block is once the transcript has made up for a change (what gets painted):
  // a ResizeObserver made after the transcript's runs after it, in the same frame, before paint.
  W.ro = new ResizeObserver(() => {
    if (!W.on) return;
    W.gaps.push(t.scrollHeight - t.scrollTop - t.clientHeight);
    if (W.el?.isConnected) W.ats.push(W.el.getBoundingClientRect().top);
  });
  W.ro.observe(t.querySelector('.transcript-inner'));
  // When the reader's first input arrived (a finger counts once it has moved past the slop).
  const mark = () => { if (W.input === Infinity) W.input = performance.now(); };
  if (!window.__wInputs) {
    window.__wInputs = true;
    window.addEventListener('pointerup', () => { if (window.__w?.on) window.__w.released = performance.now(); }, { capture: true, passive: true });
    let y0 = 0;
    const opt = { capture: true, passive: true };
    for (const type of ['wheel', 'keydown', 'pointerdown']) window.addEventListener(type, () => window.__w?.on && window.__w.mark(), opt);
    window.addEventListener('touchstart', (e) => (y0 = e.touches[0]?.clientY ?? 0), opt);
    window.addEventListener('touchmove', (e) => { if (Math.abs((e.touches[0]?.clientY ?? y0) - y0) > 4 && window.__w?.on) window.__w.mark(); }, opt);
  }
  W.mark = mark;
  const tick = () => {
    if (!W.on) return;
    W.rows.push({ t: performance.now(), top: t.scrollTop, sh: t.scrollHeight, gap: t.scrollHeight - t.scrollTop - t.clientHeight, at: W.el && W.el.isConnected ? W.el.getBoundingClientRect().top : null });
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  return performance.now();
})()`;
const stopWatch = (p) => p.eval(`(window.__w.on = false, window.__w.ro.disconnect(), window.__w.rows)`);

/** Watch a block in the middle of the view (not the one being written). */
const pickProbe = (p) =>
  p.eval(`(() => {
    const t = document.querySelector('${T}');
    const r = t.getBoundingClientRect();
    let el = null;
    for (let y = r.top + r.height / 2; y > r.top + 20 && !el; y -= 60) {
      const hit = document.elementFromPoint(r.left + r.width / 2, y);
      const md = hit?.closest('.md');
      const block = md ? [...md.children].find((c) => c.contains(hit)) : hit?.closest('.item');
      if (block && !(md?.classList.contains('streaming') && block === md.lastElementChild)) el = block;
    }
    window.__w.el = el;
    return !!el;
  })()`);

let session = null;
/** A long reply streaming in the focused pane (a new one when the last has ended). */
/** Pace: `fast` (~3,000 characters a second, the worst case) or `model` (~400, a model writing quickly). */
const PACE = { fast: { chunk: [10, 28], delay: [4, 8] }, model: { chunk: [6, 14], delay: [20, 30] } };
let pace = "fast";
async function streaming(p, ws, size, want = "fast") {
  if (session && pace === want && (await live(p))) return;
  pace = want;
  session = (await ws.rpc("create_session", { title: "Scrolling while it streams" })).result;
  await open(p, session.id);
  await sleep(900);
  await ws.rpc("mock_stream", { session_id: session.id, size, ...PACE[want], seed: 3 });
  await until(p, `(() => { const t = document.querySelector('${T}'); return t && t.scrollHeight > t.clientHeight * 4; })()`, 30000);
}

/** Back at the end and following. */
async function follow(p) {
  if (await p.eval(`!!document.querySelector('.pane.focused .activity-jump')`)) await p.click(".pane.focused .activity-jump");
  await until(p, `(() => { const t = document.querySelector('${T}'); return !document.querySelector('.pane.focused .activity-jump') && t.scrollHeight - t.scrollTop - t.clientHeight <= 2; })()`, 4000);
  await sleep(250);
}

/** While it streams for `ms`: the share of frames at the very end. */
async function following(p, ms) {
  await p.eval(WATCH);
  await sleep(ms);
  const rows = await stopWatch(p);
  const off = rows.filter((r) => r.gap > 2);
  // How much the reply grew meanwhile (following is only tested while it does).
  const grew = rows.length ? Math.round(rows.at(-1).sh - rows[0].sh) : 0;
  const paintOff = await p.eval(`window.__w.gaps.filter(g => g > 2).length`);
  return { n: rows.length, off: off.length, paintOff, worst: Math.round(Math.max(0, ...rows.map((r) => r.gap))), grew };
}

/**
 * Follow, do `act` (the reader scrolling up), let any coasting end, then watch a block in view: nothing
 * may move toward the end at any frame after the first input, and the watched block stays put.
 */
async function gesture(p, ws, label, name, act, { coast = 500, watch = 1600, size = 260000 } = {}) {
  await streaming(p, ws, size, "fast");
  await follow(p);
  const before = await geometry(p);
  const t0 = await p.eval(WATCH);
  await act(before);
  await sleep(coast);
  await pickProbe(p);
  await sleep(watch);
  const stillLive = await live(p);
  const rows = await stopWatch(p);
  const input = await p.eval(`window.__w.input`);
  const released = name === 'scrollbar drag' ? await p.eval(`window.__w.released`) : input;
  // A grabbed native thumb tracks the growing scroll range proportionally. It can move a few
  // pixels even while the mouse pauses; only JS writes could fight that. Once released, no movement.
  if (name === 'scrollbar drag') {
    const writes = await p.eval(`window.__w.writes`);
    check(writes.filter(w => w.t > input && w.t < released).length === 0,
      `${label} ${name}: no programmatic writes during the native drag`, writes);
  }
  const back = [];
  for (let i = 1; i < rows.length; i++) if (rows[i - 1].t > released && rows[i].top > rows[i - 1].top + 1) back.push({ i, from: Math.round(rows[i - 1].top), to: Math.round(rows[i].top) });
  check(input < Infinity && input > t0, `${label} ${name}: the input arrived`, { input, t0 });
  const last = rows.at(-1);
  const probed = rows.filter((r) => r.at !== null);
  const moved = probed.length ? Math.max(...probed.map((r) => Math.abs(r.at - probed[0].at))) : Infinity;
  const away = await p.eval(`!!document.querySelector('.pane.focused .activity-jump')`);
  check(stillLive, `${label} ${name}: the reply was still streaming the whole time`);
  check(back.length === 0, `${label} ${name}: never pulled back toward the end (${rows.length} frames)`, back.slice(0, 4));
  check(last.gap > 40, `${label} ${name}: stays where the reader went (${Math.round(last.gap)}px above the end)`, last);
  check(probed.length > 10 && moved <= 1, `${label} ${name}: what the reader looks at doesn't move while it streams (${probed.length} frames, moved ${moved === Infinity ? "?" : moved.toFixed(1)}px)`);
  check(away, `${label} ${name}: "Jump to live" is offered`);
  return rows;
}

/** Content growing (and shrinking) above the view moves nothing the reader sees. */
async function growthAbove(p, label) {
  await p.eval(WATCH);
  await pickProbe(p);
  const r = await p.eval(`(async () => {
    const t = document.querySelector('${T}');
    const top = t.getBoundingClientRect().top;
    const frame = () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    // A row wholly above the view, and a block inside it.
    const rows = [...t.querySelectorAll('.transcript-inner > .item')].filter((e) => e.getBoundingClientRect().bottom < top - 40);
    const row = rows.at(-1);
    if (!row) return null;
    const at0 = window.__w.el.getBoundingClientRect().top;
    const grow = document.createElement('div');
    grow.style.height = '320px';
    grow.className = 'e2e-grow';
    row.prepend(grow);
    await frame();
    const at1 = window.__w.el.getBoundingClientRect().top;
    grow.style.height = '40px';
    await frame();
    const at2 = window.__w.el.getBoundingClientRect().top;
    grow.remove();
    await frame();
    const at3 = window.__w.el.getBoundingClientRect().top;
    // An earlier row taller by a picture loading.
    const img = rows[Math.max(0, rows.length - 3)];
    const pic = document.createElement('div');
    pic.style.height = '0px';
    img.append(pic);
    await frame();
    pic.style.height = '260px';
    await frame();
    const at4 = window.__w.el.getBoundingClientRect().top;
    pic.remove();
    await frame();
    return { at0, at1, at2, at3, at4, at5: window.__w.el.getBoundingClientRect().top };
  })()`);
  const ats = await p.eval(`window.__w.ats`);
  await stopWatch(p);
  check(!!r, `${label}: there is a row above the view to grow`);
  if (!r) return;
  const worst = Math.max(...["at1", "at2", "at3", "at4", "at5"].map((k) => Math.abs(r[k] - r.at0)));
  const flick = ats.length ? Math.max(...ats.map((x) => Math.abs(x - r.at0))) : Infinity;
  check(worst <= 1 && flick <= 1, `${label}: content growing and shrinking above the view doesn't move what the reader looks at (${worst.toFixed(1)}px, ${flick.toFixed(1)}px in any frame)`, r);
}

/** Back to the end by the reader's own scroll: following again. */
async function backDown(p, label, name, act) {
  await act();
  await sleep(700);
  const f = await following(p, 1500);
  const away = await p.eval(`!!document.querySelector('.pane.focused .activity-jump')`);
  check(!away && f.n > 20 && f.grew > 50 && f.off <= Math.max(1, f.n * 0.03), `${label} ${name}: following the end again (${f.off} of ${f.n} frames off the end while it grew ${f.grew}px, worst ${f.worst}px)`, f);
}

const wheel = (p, x, y, dy, n = 1) =>
  (async () => {
    for (let i = 0; i < n; i++) {
      await p.send("Input.dispatchMouseEvent", { type: "mouseWheel", x, y, deltaX: 0, deltaY: dy });
      await sleep(30);
    }
  })();

/** A quick finger flick (dy > 0: the finger moves down, the content scrolls up), then lift: it coasts. */
async function flick(p, x, y, dy, steps = 6) {
  x = Math.round(x);
  y = Math.round(y);
  await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x, y }] });
  for (let k = 1; k <= steps; k++) {
    await p.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x, y: Math.round(y + (dy * k) / steps) }] });
    await sleep(8);
  }
  await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
}

async function desktop(p, ws, label) {
  await gesture(p, ws, label, "wheel", (g) => wheel(p, g.x, g.y, -120, 3));
  await p.shot(`${SHOTS}/follow-${label}-wheel.png`);
  await growthAbove(p, label);
  await backDown(p, label, "wheel down to the end", async () => {
    const g = await geometry(p);
    await wheel(p, g.x, g.y, 2000, 6);
  });

  await gesture(p, ws, label, "keyboard (PageUp)", async () => {
    await p.eval(`document.querySelector('${T}').focus()`);
    await p.key("PageUp", "PageUp", 0, 33);
  });
  await backDown(p, label, "End key", () => p.key("End", "End", 0, 35));

  // The scrollbar: shown for this test (the suites hide scrollbars), dragged up and held.
  await p.send("Emulation.setScrollbarsHidden", { hidden: false });
  await sleep(200);
  await gesture(
    p,
    ws,
    label,
    "scrollbar drag",
    async (g) => {
      const x = g.right - 5;
      const y = g.bottom - 24;
      await p.down(x, y);
      for (let k = 1; k <= 12; k++) {
        await p.move(x, y - k * 12);
        await sleep(25);
      }
      // Held still a while (the reply keeps growing under the thumb), then let go.
      await sleep(600);
      await p.up(x, y - 144);
    },
    { coast: 300 },
  );
  await p.send("Emulation.setScrollbarsHidden", { hidden: true });

  // Layout and other programmatic scrolls must not masquerade as user input, even when upward.
  await follow(p);
  await p.eval(`document.querySelector('${T}').scrollBy(0, -500)`);
  await sleep(300);
  const f = await following(p, 900);
  check(!await p.eval(`!!document.querySelector('.pane.focused .activity-jump')`) && f.paintOff === 0 && await p.eval(`(() => { const t = document.querySelector('${T}'); return t.scrollHeight - t.clientHeight - t.scrollTop <= 1; })()`),
    `${label}: a programmatic upward scroll cannot detach bottom-follow`, f);
}

async function phone(p, ws, label) {
  // A quick flick: the content keeps coasting after the finger lifts.
  const rows = await gesture(
    p,
    ws,
    label,
    "touch fling",
    (g) => flick(p, g.x, g.top + 60, 300),
    { coast: 1500 },
  );
  // It did move up over many frames (a fling coasting after the finger lifted).
  const moving = rows.filter((r, i) => i && r.top < rows[i - 1].top - 0.5).length;
  const went = Math.round(rows[0].top - Math.min(...rows.map((r) => r.top)));
  check(moving >= 4 && went > 150, `${label}: the flick moved the transcript up over ${moving} frames (${went}px)`);
  await p.shot(`${SHOTS}/follow-${label}-fling.png`);
  await growthAbove(p, label);
  // Back to the end with a finger: a short look back, then strokes down until the end (synthetic
  // touches coast little here; at the pace a model writes, a few strokes catch up).
  await streaming(p, ws, 60000, "model");
  await follow(p);
  {
    const g = await geometry(p);
    // Eased down 80px and held before lifting.
    const x = Math.round(g.x);
    let y = Math.round(g.y - 100);
    await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x, y }] });
    for (let k = 0; k < 10; k++) {
      y += 8;
      await p.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x, y }] });
      await sleep(16);
    }
    await sleep(150);
    await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
    const away = await until(p, `document.querySelector('.pane.focused .activity-jump')`, 2000);
    check(away, `${label}: a short look back lets go of the end`);
  }
  await backDown(p, label, "strokes back to the end", async () => {
    const g = await geometry(p);
    // Stroke after stroke, a moment apart (following comes back once a stroke ends at the end).
    for (let k = 0; k < 30; k++) {
      await flick(p, g.x, g.y + 200, -600);
      await sleep(250);
      const c = await geometry(p);
      const away = await p.eval(`!!document.querySelector('.pane.focused .activity-jump')`);
      console.log(`  stroke ${k}: ${Math.round(c.sh - c.st - c.h)}px from the end${away ? "" : ", following"}`);
      if (!away) break;
    }
  });

  // A slow drag: a couple of pixels per move, as a finger easing the text down to reread a line.
  await gesture(p, ws, label, "slow finger drag", async (g) => {
    const x = Math.round(g.x);
    let y = Math.round(g.top + 60);
    await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x, y }] });
    for (let k = 0; k < 70; k++) {
      y += 2;
      await p.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x, y }] });
      await sleep(12);
    }
    await sleep(400);
    await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  });

  // A finger resting on the text without moving (to select a word, or just holding the phone): the
  // text holds still under it, and following goes on once it lifts.
  await follow(p);
  const g = await geometry(p);
  await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: Math.round(g.x), y: Math.round(g.y) }] });
  await p.eval(WATCH);
  await sleep(900);
  const held = await stopWatch(p);
  await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  await sleep(300);
  const after = await following(p, 900);
  const still = held.every((r) => Math.abs(r.top - held[0].top) <= 0.5);
  check(still && held.at(-1).gap > held[0].gap, `${label}: under a resting finger the text holds still while the reply grows below`, { first: held[0], last: held.at(-1) });
  check(after.grew > 50 && after.off <= Math.max(1, after.n * 0.05) && !(await p.eval(`!!document.querySelector('.pane.focused .activity-jump')`)), `${label}: lifted without scrolling, it follows the end again (${after.off}/${after.n} frames off the end while it grew ${after.grew}px)`, after);
  await redrawnUnderFinger(p, ws, label);
}

/**
 * A finger on a block that is redrawn while it rests there (the block being written is): its touch
 * events no longer reach the transcript, only the block they started on. Lifting it still counts, and so
 * does dragging.
 */
async function redrawnUnderFinger(p, ws, label) {
  // A chat of its own (a fresh reply streaming).
  session = null;
  await streaming(p, ws, 260000, "fast");
  const press = async () => {
    await follow(p);
    // A block of the reply's text near the middle of the view, and a point on it.
    const at = await p.eval(`(() => {
      const t = document.querySelector('${T}');
      const r = t.getBoundingClientRect();
      for (let y = r.top + r.height / 2; y > r.top + 40; y -= 30) {
        const el = document.elementFromPoint(r.left + r.width / 2, y);
        if (el && el.closest('.md') && el !== el.closest('.md') && el.textContent.trim()) {
          window.__under = el;
          return { x: Math.round(r.left + r.width / 2), y: Math.round(y), tag: el.tagName };
        }
      }
      return null;
    })()`);
    check(!!at, `${label}: found a block of text to put a finger on`);
    await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: at.x, y: at.y }] });
    // Redrawn: the node the touch started on leaves the page.
    await p.eval(`(() => { const el = window.__under; el.replaceWith(el.cloneNode(true)); return el.isConnected; })()`);
    return at;
  };
  // Held, then lifted: following goes on.
  await press();
  await sleep(500);
  await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  await sleep(300);
  const after = await following(p, 900);
  check(after.grew > 100 && after.off <= Math.max(1, after.n * 0.05), `${label}: a finger lifted from a block that was redrawn under it doesn't leave the chat stuck (${after.off}/${after.n} frames off the end while it grew ${after.grew}px)`, after);
  // Dragged slowly: it lets go of the end, as anywhere else.
  const at = await press();
  let y = at.y;
  for (let k = 0; k < 30; k++) {
    y += 2;
    await p.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x: at.x, y }] });
    await sleep(12);
  }
  await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  const away = await until(p, `document.querySelector('.pane.focused .activity-jump')`, 2000);
  check(away, `${label}: dragging a block that was redrawn under the finger lets go of the end`);
}

/** A running tool's output box: the reader scrolls inside it, it stays; back at its end, it follows. */
async function toolBox(p, ws, label, mobile) {
  const info = (await ws.rpc("create_session", { title: "Tool output scroll" })).result;
  await open(p, info.id);
  await sleep(900);
  await ws.rpc("mock_stream", { session_id: info.id, size: 300, tool_lines: 6000, chunk: [6, 20], delay: [3, 6] });
  await until(p, `document.querySelector('${T} .tool-live.tail')?.textContent.includes('case_')`, 15000);
  await p.eval(`(() => { const h = document.querySelector('${T} .tool-live.tail').closest('.tool').querySelector('[aria-expanded]'); h.click(); return 1; })()`);
  await until(p, `(() => { const pre = document.querySelector('${T} .tool-live:not(.tail)'); return pre && pre.scrollHeight > pre.clientHeight * 3; })()`, 10000);
  const PRE = `document.querySelector('${T} .tool-live:not(.tail)')`;
  const box = await p.eval(`(() => { const r = ${PRE}.getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2, top: r.top }; })()`);
  if (mobile) {
    const x = Math.round(box.x);
    let y = Math.round(box.top + 10);
    await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x, y }] });
    for (let k = 0; k < 25; k++) {
      y += 4;
      await p.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x, y }] });
      await sleep(16);
    }
    await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  } else await wheel(p, box.x, box.y, -100, 2);
  await sleep(300);
  const s = [];
  for (let i = 0; i < 10; i++) {
    s.push(await p.eval(`(() => { const pre = ${PRE}; return { top: Math.round(pre.scrollTop), gap: Math.round(pre.scrollHeight - pre.scrollTop - pre.clientHeight), len: pre.textContent.length }; })()`));
    await sleep(120);
  }
  const grew = s.at(-1).len > s[0].len;
  const steady = s.every((x) => Math.abs(x.top - s[0].top) <= 1);
  check(grew && steady && s[0].gap > 30, `${label}: scrolled up inside a running tool's output, it stays put while lines keep coming (${s[0].gap}px above its end)`, s.slice(0, 3).concat(s.slice(-2)));
  // Back to its end: it follows again. The output grows by thousands of lines a second here: on the
  // desktop, the wheel spun until it gets there; on the phone (no finger catches that), closing and
  // opening the card again.
  if (mobile) {
    await p.eval(`(() => { ${PRE}.closest('.tool').querySelector('[aria-expanded]').click(); return 1; })()`);
    await until(p, `document.querySelector('${T} .tool-live.tail')`, 4000);
    await p.eval(`(() => { document.querySelector('${T} .tool-live.tail').closest('.tool').querySelector('[aria-expanded]').click(); return 1; })()`);
    await until(p, `${PRE}`, 4000);
  } else
    for (let k = 0; k < 60; k++) {
      await wheel(p, box.x, box.y, 3000);
      if ((await p.eval(`(() => { const pre = ${PRE}; return pre.scrollHeight - pre.scrollTop - pre.clientHeight; })()`)) < 20) break;
    }
  await sleep(700);
  const e = [];
  for (let i = 0; i < 8; i++) {
    e.push(await p.eval(`(() => { const pre = ${PRE}; return pre ? Math.round(pre.scrollHeight - pre.scrollTop - pre.clientHeight) : -1; })()`));
    await sleep(100);
  }
  check(e.filter((g) => g > 2).length <= 1, `${label}: back at its end, the output box follows the new lines again`, e);
  await until(p, `!document.querySelector('.pane.focused .send-btn.stop')`, 40000);
}

run(async () => {
  await mock("/__mock/reset");
  const ws = await wsClient("follow test");
  for (const [label, w, h, mobile, dpr, theme] of [
    ["desktop", 1440, 900, false, 1, "light"],
    ["phone", 344, 820, true, 2, "dark"],
  ]) {
    if (process.env.ONLY && !label.includes(process.env.ONLY)) continue;
    console.log(`— ${label}`);
    session = null;
    const p = await device();
    await p.viewport(w, h, mobile, dpr);
    await signIn(p, `Follow ${label}`);
    await p.theme(theme);
    if (mobile) await phone(p, ws, label);
    else await desktop(p, ws, label);
    await toolBox(p, ws, label, mobile);
    for (const t of ["light", "dark"]) {
      await p.theme(t);
      await sleep(250);
      await p.shot(`${SHOTS}/follow-${label}-${t}.png`);
    }
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    await p.close();
  }
  ws.close();
  finish();
});
