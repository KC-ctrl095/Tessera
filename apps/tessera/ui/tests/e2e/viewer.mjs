// The picture viewer (components/Lightbox.tsx): a chat's pictures open full screen, centred and
// fitted with their shape kept, on a computer (wheel, double click, drag, arrows, Escape, a click
// beside the picture) and on a phone (pinch, double tap, swipes, the back gesture). Pictures in
// old messages (which skip painting while off screen) open the same way; big ones (a 12 MP photo,
// a long screenshot) don't hold anything up; and afterwards the chat, the tiles and the pager are
// exactly as they were.
import { APP, SHOTS, check, device, finish, mock, readEarlier, run, signIn, sleep } from "./cdp.mjs";

const touch = (p, type, pts) => p.send("Input.dispatchTouchEvent", { type, touchPoints: pts.map(([x, y], id) => ({ x, y, id })) });
const stroke = async (p, x0, y0, x1, y1, steps = 10, ms = 16) => {
  await touch(p, "touchStart", [[x0, y0]]);
  for (let k = 1; k <= steps; k++) {
    await touch(p, "touchMove", [[x0 + ((x1 - x0) * k) / steps, y0 + ((y1 - y0) * k) / steps]]);
    await sleep(ms);
  }
  await touch(p, "touchEnd", []);
  await sleep(450);
};
const pinch = async (p, cx, cy, from, to) => {
  const at = (d) => [[cx - d, cy - d], [cx + d, cy + d]];
  await touch(p, "touchStart", at(from));
  for (let k = 1; k <= 10; k++) {
    await touch(p, "touchMove", at(from + ((to - from) * k) / 10));
    await sleep(16);
  }
  await touch(p, "touchEnd", []);
  await sleep(450);
};
/** Two quick taps (well inside a phone's double-tap time). */
const dtap = async (p, x, y) => {
  for (const k of [0, 1]) {
    await touch(p, "touchStart", [[x, y]]);
    await sleep(30);
    await touch(p, "touchEnd", []);
    if (!k) await sleep(70);
  }
  await sleep(500);
};

/** A test picture drawn in the page: a gradient with a yellow frame and TOP/BOTTOM marks. */
const picture = (w, h, label, mime = "image/jpeg") => `(() => { const c = document.createElement('canvas'); c.width = ${w}; c.height = ${h}; const g = c.getContext('2d'); const gr = g.createLinearGradient(0, 0, ${w}, ${h}); gr.addColorStop(0, '#c43'); gr.addColorStop(1, '#34c'); g.fillStyle = gr; g.fillRect(0, 0, ${w}, ${h}); g.fillStyle = '#fff'; g.font = 'bold ' + Math.round(Math.min(${w}, ${h}) / 9) + 'px sans-serif'; g.fillText('TOP ${label}', 20, Math.min(${w}, ${h}) / 7); g.fillText('BOTTOM', 20, ${h} - 24); g.strokeStyle = '#ff0'; g.lineWidth = Math.max(6, ${w} / 90); g.strokeRect(0, 0, ${w}, ${h}); return c.toDataURL('${mime}', 0.85).split(',')[1]; })()`;

/** A new chat in the focused tile whose first message has these pictures; returns its id. */
async function chatWith(p, pics, text = "What is in these pictures?") {
  const parts = pics.map(([w, h, label]) => `{ type: 'image', mime: 'image/jpeg', data: ${picture(w, h, label)} }`).join(", ");
  const id = await p.eval(`(async () => { const st = await __mod('lib/store.ts'); const w = await __mod('lib/workspace.ts'); const i = await st.api.createSession({}); st.upsertSession(i); st.setState('sessions', i.id, 'loaded', true); w.openSession(i.id, { pane: w.workspace.focused() }); await st.sendMessage(i.id, [${parts}, { type: 'text', text: ${JSON.stringify(text)} }], 'auto'); return i.id; })()`);
  await p.waitFor(`document.querySelectorAll('.pane.focused .msg-user .bubble-img').length === ${pics.length}`, 15000);
  await p.waitFor(`!document.querySelector('.pane.focused .composer.busy')`, 20000).catch(() => {});
  await sleep(400);
  return id;
}

/** The viewer's state: which picture, zoom, where the picture is on screen. */
const view = (p) =>
  p.eval(`(() => { const l = document.querySelector('.lightbox-view'); if (!l) return null; const r = l.getBoundingClientRect(); const i = l.querySelector('.lb-img'); const b = i?.getBoundingClientRect(); return { box: [r.x, r.y, r.width, r.height], img: b ? { x: b.x, y: b.y, w: b.width, h: b.height, cx: b.x + b.width / 2, cy: b.y + b.height / 2, nw: i.naturalWidth, nh: i.naturalHeight, ready: i.classList.contains('ready') } : null, index: +l.dataset.index, scale: +l.dataset.scale, count: l.querySelector('.lb-count').textContent, vw: innerWidth, vh: innerHeight, zoom: visualViewport.scale }; })()`);
const near = (a, b, tol = 1.5) => Math.abs(a - b) <= tol;
/** Centred in the window, and the whole picture on screen with its own shape. */
const fitted = (v, pad = { x: 0, y: 0 }) =>
  !!v?.img &&
  near(v.img.cx, v.vw / 2) &&
  near(v.img.cy, v.vh / 2) &&
  v.img.w <= v.vw - pad.x * 2 + 1 &&
  v.img.h <= v.vh - pad.y * 2 + 1 &&
  near(v.img.w / v.img.h, v.img.nw / v.img.nh, 0.01) &&
  (near(v.img.w, Math.min(v.vw - pad.x * 2, v.img.nw)) || near(v.img.h, Math.min(v.vh - pad.y * 2, v.img.nh)));
/** What else is on screen: the chat's scroll position and the tiles' places, to compare after. */
const layout = (p) =>
  p.eval(`(() => ({ scroll: Math.round(document.querySelector('.pane.focused .transcript')?.scrollTop ?? -1), panes: [...document.querySelectorAll('.pane')].map((e) => { const r = e.getBoundingClientRect(); return [e.dataset.paneId, Math.round(r.x), Math.round(r.y), Math.round(r.width), Math.round(r.height)].join(':'); }).join(' '), body: document.body.scrollTop + ':' + document.documentElement.scrollTop, cls: document.documentElement.className, zoom: visualViewport.scale }))()`);
/** The i-th picture of the chat on screen (scrolled into view first). */
const thumb = async (p, i) => {
  await readEarlier(p);
  return p.eval(`(() => { const e = [...document.querySelectorAll('.msg-user .bubble-img')].filter((e) => { const r = e.closest('.pane').getBoundingClientRect(); return r.left >= -1 && r.right <= innerWidth + 1; })[${i}]; if (!e) return null; e.scrollIntoView({ block: 'center' }); const r = e.getBoundingClientRect(); return { cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);
};
const waitReady = (p) => p.waitFor(`document.querySelector('.lightbox-view .lb-img.ready')`, 10000);

run(async () => {
  await mock("/__mock/reset");

  // ── the fitting maths ──
  {
    const p = await device();
    await p.goto(APP, 800);
    const r = await p.eval(`__mod('lib/viewer.ts').then((m) => [m.fitSize(1600, 2400, 1296, 780), m.fitSize(300, 200, 1296, 780), m.fitSize(4000, 3000, 344, 882), m.fitSize(1080, 9000, 1296, 780), m.fitSize(0, 0, 100, 100)])`);
    check(near(r[0].h, 780) && near(r[0].w, 520) && near(r[0].k, 0.325, 0.001), "a tall picture fits the height", r[0]);
    check(r[1].w === 300 && r[1].h === 200 && r[1].k === 1, "a small one is never enlarged", r[1]);
    check(near(r[2].w, 344) && near(r[2].h, 258), "a 12 MP photo fits a phone's width", r[2]);
    check(near(r[3].h, 780) && near(r[3].w, 93.6), "a long screenshot fits the height", r[3]);
    check(r[4].w === 0, "no size, no picture (no NaN)", r[4]);
    await p.close();
  }

  // ── a computer ──
  for (const theme of ["dark", "light"]) {
    console.log(`— desktop (${theme})`);
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, `Viewer desk ${theme}`);
    await p.theme(theme);
    await chatWith(p, [[1600, 2400, "TALL"], [2400, 1200, "WIDE"], [300, 200, "SMALL"]]);
    let b = await thumb(p, 0);
    await sleep(200);
    const before = await layout(p);
    await p.mouse(b.cx, b.cy);
    await waitReady(p);
    await sleep(250);
    let v = await view(p);
    check(v && v.box[0] === 0 && v.box[1] === 0 && v.box[2] === 1440 && v.box[3] === 900, "it covers the whole window", v?.box);
    check(fitted(v, { x: 72, y: 60 }) && near(v.img.h, 780), "a tall picture: centred, all of it on screen, its shape kept", v?.img);
    check(v.count === "1 / 3", "it says which of the chat's pictures it is", v.count);
    await p.shot(`${SHOTS}/viewer-desk-${theme}.png`);
    // the wheel zooms at the pointer: the point under it stays put
    const px = v.img.x + v.img.w * 0.25;
    const py = v.img.y + v.img.h * 0.3;
    const u = (px - v.img.x) / v.img.w;
    const w0 = (py - v.img.y) / v.img.h;
    await p.send("Input.dispatchMouseEvent", { type: "mouseWheel", x: px, y: py, deltaX: 0, deltaY: -800 });
    await sleep(300);
    v = await view(p);
    const u1 = (px - v.img.x) / v.img.w;
    const w1 = (py - v.img.y) / v.img.h;
    check(v.scale > 3 && near(u, u1, 0.01) && near(w0, w1, 0.01), "the wheel zooms in where the pointer is", { scale: v.scale, u, u1, w0, w1 });
    // drag to look around; never past the picture's edge
    const x0 = v.img.x;
    await p.down(720, 450);
    for (let k = 1; k <= 8; k++) await p.move(720 + k * 40, 450 + k * 30);
    await p.up(1040, 690);
    await sleep(200);
    v = await view(p);
    check(v.img.x > x0 + 100 && v.img.x <= 1 && v.img.y <= 1, "dragging pans the zoomed picture, and stops at its edge", { x0, x: v.img.x, y: v.img.y });
    // "0" fits again; double click zooms in, double click again fits
    await p.key("0", "Digit0");
    await sleep(400);
    v = await view(p);
    check(v.scale === 1 && fitted(v, { x: 72, y: 60 }), "0 puts it back to fit", v.scale);
    await p.send("Input.dispatchMouseEvent", { type: "mousePressed", x: 720, y: 450, button: "left", clickCount: 1 });
    await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: 720, y: 450, button: "left", clickCount: 1 });
    await sleep(60);
    await p.send("Input.dispatchMouseEvent", { type: "mousePressed", x: 720, y: 450, button: "left", clickCount: 2 });
    await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: 720, y: 450, button: "left", clickCount: 2 });
    await sleep(450);
    v = await view(p);
    check(v.scale >= 2.5 && near(v.img.cx, 720, 2) && near(v.img.cy, 450, 2), "a double click zooms in at that point", { scale: v.scale, cx: v.img.cx, cy: v.img.cy });
    await p.shot(`${SHOTS}/viewer-desk-zoomed-${theme}.png`);
    await p.key("+", "Equal");
    await sleep(350);
    const z = (await view(p)).scale;
    await p.key("-", "Minus");
    await sleep(350);
    check(z > v.scale && (await view(p)).scale < z, "+ and − zoom in and out");
    await p.key("0", "Digit0");
    await sleep(350);
    // the arrows go through the chat's pictures
    await p.key("ArrowRight", "ArrowRight", 0, 39);
    await waitReady(p);
    await sleep(400);
    v = await view(p);
    check(v.index === 1 && v.count === "2 / 3" && fitted(v, { x: 72, y: 60 }) && near(v.img.w, 1296), "→ shows the next picture, fitted to the width", v);
    await p.click(".lb-next");
    await waitReady(p);
    await sleep(400);
    v = await view(p);
    check(v.index === 2 && v.img.w === 300 && v.img.h === 200 && fitted(v), "a small picture shows at its own size, centred", v.img);
    check(await p.eval(`document.querySelector('.lb-next').disabled`), "no next after the last");
    await p.key("ArrowRight", "ArrowRight", 0, 39);
    await sleep(300);
    check((await view(p)).index === 2, "→ at the last stays there");
    await p.key("ArrowLeft", "ArrowLeft", 0, 37);
    await sleep(400);
    check((await view(p)).index === 1, "← goes back");
    // the app under it gets no keys
    await p.key("n", "KeyN", 2);
    await sleep(200);
    check((await p.eval(`document.querySelectorAll('.pane').length`)) === before.panes.split(" ").length, "Ctrl+N doesn't reach the app while it's open");
    // Escape closes; nothing moved
    await p.key("Escape", "Escape", 0, 27);
    await sleep(300);
    check(!(await view(p)), "Escape closes it");
    const after = await layout(p);
    check(JSON.stringify(after) === JSON.stringify(before), "the chat, the tiles and the page are as they were", { before, after });
    // a click beside the picture closes it; a click on it doesn't
    b = await thumb(p, 1);
    await p.mouse(b.cx, b.cy);
    await waitReady(p);
    await sleep(250);
    v = await view(p);
    await p.mouse(v.img.cx, v.img.cy);
    await sleep(400);
    check(!!(await view(p)), "a click on the picture keeps it open");
    await p.mouse(720, v.img.y + v.img.h + 30);
    await sleep(300);
    check(!(await view(p)), "a click beside it closes it");
    // the ✕
    b = await thumb(p, 2);
    await p.mouse(b.cx, b.cy);
    await waitReady(p);
    await p.click(".lb-close");
    await sleep(300);
    check(!(await view(p)), "✕ closes it");
    // Save: a download of the picture itself
    b = await thumb(p, 0);
    await p.mouse(b.cx, b.cy);
    await waitReady(p);
    const saved = await p.eval(`new Promise((res) => { let blob = null; const make = URL.createObjectURL; URL.createObjectURL = (b) => { blob = b; URL.createObjectURL = make; return make.call(URL, b); }; const click = HTMLAnchorElement.prototype.click; HTMLAnchorElement.prototype.click = function () { HTMLAnchorElement.prototype.click = click; res({ name: this.download, type: blob?.type, size: blob?.size }); }; document.querySelector('.lb-bar [aria-label="Save picture"]').click(); setTimeout(() => res(null), 4000); })`);
    check(saved && /^image-\d{4}-\d\d-\d\d-\d{6}\.jpg$/.test(saved.name) && saved.type === "image/jpeg" && saved.size > 10000, "Save downloads the picture as a .jpg", saved);
    await p.key("Escape", "Escape", 0, 27);
    await sleep(300);
    check(p.errors.length === 0, `desktop (${theme}): no errors`, p.errors);
    await p.close();
  }

  // ── an older message (it skips painting while off screen): the viewer still covers the window ──
  {
    console.log("— older messages");
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, "Viewer old");
    const sid = await chatWith(p, [[1600, 2400, "OLD"], [1080, 9000, "LONG"]]);
    for (let k = 0; k < 9; k++) {
      await p.eval(`__mod('lib/store.ts').then((st) => st.sendMessage(${JSON.stringify(sid)}, [{ type: 'text', text: 'one more message ${k}' }], 'auto'))`);
      await p.waitFor(`!document.querySelector('.pane.focused .composer.busy')`, 20000).catch(() => {});
      await sleep(250);
    }
    await sleep(600);
    const settled = await p.eval(`(() => { const t = document.querySelector('.pane.focused .transcript'); t.scrollTop = 0; t.dispatchEvent(new WheelEvent('wheel', { deltaY: -200, bubbles: true })); const it = document.querySelector('.pane.focused .msg-user .bubble-img').closest('.item'); return it.classList.contains('settled') && getComputedStyle(it).contentVisibility === 'auto'; })()`);
    await sleep(500);
    check(settled, "the pictures' message is an old one (content-visibility: auto)");
    const b = await thumb(p, 0);
    await p.mouse(b.cx, b.cy);
    await waitReady(p);
    await sleep(250);
    let v = await view(p);
    check(v && v.box.join() === "0,0,1440,900" && fitted(v, { x: 72, y: 60 }), "it covers the window, centred and whole (not boxed in the message)", v);
    await p.shot(`${SHOTS}/viewer-desk-old-message.png`);
    // a long screenshot: fits the height; a double click shows it at full width, from the top
    await p.key("ArrowRight", "ArrowRight", 0, 39);
    await waitReady(p);
    await sleep(400);
    v = await view(p);
    check(fitted(v, { x: 72, y: 60 }) && near(v.img.h, 780), "a long screenshot first shows whole", v.img);
    const t0 = Date.now();
    await p.send("Input.dispatchMouseEvent", { type: "mousePressed", x: 720, y: 80, button: "left", clickCount: 1 });
    await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: 720, y: 80, button: "left", clickCount: 1 });
    await sleep(60);
    await p.send("Input.dispatchMouseEvent", { type: "mousePressed", x: 720, y: 80, button: "left", clickCount: 2 });
    await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: 720, y: 80, button: "left", clickCount: 2 });
    await sleep(500);
    v = await view(p);
    check(near(v.img.w, 1440, 2) && v.img.y <= 1, "a double click shows it across the whole width", v.img);
    // wheel-scroll down the long screenshot by dragging
    await p.down(720, 700);
    for (let k = 1; k <= 6; k++) await p.move(720, 700 - k * 100);
    await p.up(720, 100);
    await sleep(200);
    check((await view(p)).img.y < v.img.y - 400, "dragging moves down it");
    check(Date.now() - t0 < 4000, "no stall", Date.now() - t0);
    await p.key("Escape", "Escape", 0, 27);
    check(p.errors.length === 0, "older messages: no errors", p.errors);
    await p.close();
  }

  // ── pictures in the agent's reply and on a tool card ──
  {
    console.log("— replies and tool cards");
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, "Viewer tools");
    const sid = await chatWith(p, [[800, 600, "MINE"]]);
    const png = await p.eval(picture(1280, 800, "SHOT", "image/png"));
    const md = await p.eval(picture(900, 500, "CHART", "image/png"));
    await p.eval(`(async () => { const st = await __mod('lib/store.ts'); const s = st.state.sessions[${JSON.stringify(sid)}]; let seq = s.entries[s.entries.length - 1].seq; const now = Date.now(); const usage = { input: 0, output: 0, cache_read: 0, cache_write: 0 };
      st.applyEvent({ type: 'entry_added', session_id: ${JSON.stringify(sid)}, entry: { seq: ++seq, ts: now, kind: 'assistant', content: [{ type: 'text', text: 'Let me look at the page.' }, { type: 'tool_call', id: 'call_shot', name: 'browser', arguments: { action: 'screenshot' } }], model: s.info.model, usage, stop: 'tool_use' } });
      st.applyEvent({ type: 'entry_added', session_id: ${JSON.stringify(sid)}, entry: { seq: ++seq, ts: now, kind: 'tool_result', call_id: 'call_shot', tool: 'browser', content: [{ type: 'text', text: 'Took a screenshot.' }, { type: 'image', mime: 'image/png', data: '${png}' }], is_error: false } });
      st.applyEvent({ type: 'entry_added', session_id: ${JSON.stringify(sid)}, entry: { seq: ++seq, ts: now, kind: 'assistant', content: [{ type: 'text', text: 'Here is the chart:\\n\\n![Latency chart](data:image/png;base64,${md})' }], model: s.info.model, usage, stop: 'stop' } });
    })()`);
    await p.waitFor(`document.querySelector('.pane.focused .msg-assistant img')`, 6000);
    await sleep(400);
    // the tool card: open it, then its screenshot
    await p.eval(`(() => { const h = [...document.querySelectorAll('.pane.focused .tool-head')].pop(); h.scrollIntoView({ block: 'center' }); h.click(); })()`);
    await p.waitFor(`document.querySelector('.pane.focused .tool-images img')`, 6000);
    await sleep(300);
    let b = await p.box(".pane.focused .tool-images img");
    await p.mouse(b.cx, b.cy);
    await waitReady(p);
    await sleep(250);
    let v = await view(p);
    check(v && v.img.nw === 1280 && fitted(v, { x: 72, y: 60 }), "a tool card's screenshot opens in the viewer", v?.img);
    check(v.count === "2 / 3", "…as one of the chat's pictures, in order", v.count);
    await p.key("ArrowRight", "ArrowRight", 0, 39);
    await waitReady(p);
    await sleep(300);
    v = await view(p);
    check(v.index === 2 && v.img.nw === 900 && v.count === "3 / 3", "→ goes on to the picture in the reply", v);
    await p.key("Escape", "Escape", 0, 27);
    await sleep(300);
    b = await p.box(".pane.focused .msg-assistant img");
    await p.mouse(b.cx, b.cy);
    await waitReady(p);
    v = await view(p);
    check(v && v.img.nw === 900 && v.index === 2, "the reply's picture opens on its own click too", v?.img);
    await p.key("Escape", "Escape", 0, 27);
    check(p.errors.length === 0, "replies and tools: no errors", p.errors);
    await p.close();
  }

  // ── phones: the folded Fold's cover screen, and an unfolded Fold ──
  for (const [name, w, h, dpr] of [
    ["cover", 344, 882, 2.625],
    ["fold", 673, 841, 2.625],
  ]) {
    for (const theme of name === "cover" ? ["dark", "light"] : ["dark"]) {
      console.log(`— ${name} (${theme})`);
      const p = await device();
      await p.viewport(w, h, true, dpr);
      await signIn(p, `Viewer ${name} ${theme}`);
      await p.theme(theme);
      // two tiles, so there's a page to swipe to afterwards
      await p.eval(`__mod('lib/workspace.ts').then((m) => m.openSession('ses_7d3a91e5c4b2', { newPane: true }))`);
      await sleep(500);
      await p.eval(`__mod('lib/workspace.ts').then((m) => m.focusPane(m.workspace.order()[0]))`);
      await sleep(600);
      await chatWith(p, [[4000, 3000, "12MP"], [1536, 2048, "TALL"], [1080, 9000, "LONG"]]);
      const before = await layout(p);
      const histBefore = await p.eval(`history.length`);
      let b = await thumb(p, 0);
      const t0 = Date.now();
      await p.tap(b.cx, b.cy);
      await waitReady(p);
      const opened = Date.now() - t0;
      await sleep(300);
      let v = await view(p);
      check(v && v.box.join() === `0,0,${w},${h}`, `${name}: it opens, covering the screen`, v?.box);
      check(fitted(v) && near(v.img.w, w), `${name}: a 12 MP photo fits the width, centred`, v?.img);
      check(opened < 3000, `${name}: a 12 MP photo shows within moments`, opened);
      await p.shot(`${SHOTS}/viewer-${name}-${theme}.png`);
      // a pinch zooms the picture, never the page
      await pinch(p, w / 2, h / 2, 30, 110);
      v = await view(p);
      check(v.scale > 2 && v.zoom === 1, `${name}: a pinch zooms the picture, not the app`, { scale: v.scale, page: v.zoom });
      await p.shot(`${SHOTS}/viewer-${name}-pinched-${theme}.png`);
      // one finger pans the zoomed picture (no page turn)
      const x0 = v.img.x;
      await stroke(p, w / 2, h / 2, w / 2 + 80, h / 2);
      v = await view(p);
      check(v.index === 0 && near(v.img.x, x0 + 80, 3), `${name}: one finger moves the zoomed picture`, { x0, x: v.img.x, i: v.index });
      // double tap: back to fit; again: zoomed in
      await dtap(p, w / 2, h / 2);
      v = await view(p);
      check(v.scale === 1 && fitted(v), `${name}: a double tap fits it again`, v.scale);
      await dtap(p, w / 2, h / 2);
      v = await view(p);
      check(v.scale >= 2.5, `${name}: another zooms in`, v.scale);
      await dtap(p, w / 2, h / 2);
      // a sideways swipe: the next picture (the tiles stay put)
      await stroke(p, w * 0.8, h / 2, w * 0.15, h / 2, 8);
      await waitReady(p);
      await sleep(350);
      v = await view(p);
      check(v.index === 1 && v.count === "2 / 3" && fitted(v), `${name}: a swipe shows the next picture`, v);
      await stroke(p, w * 0.15, h / 2, w * 0.85, h / 2, 8);
      await waitReady(p);
      await sleep(300);
      check((await view(p)).index === 0, `${name}: and back`);
      // a tap on the picture hides the controls, another brings them back
      await p.tap(w / 2, h / 2);
      await sleep(450);
      check(await p.eval(`document.querySelector('.lightbox-view').classList.contains('lb-bare')`), `${name}: a tap on it hides the controls`);
      await p.tap(w / 2, h / 2);
      await sleep(450);
      check(!(await p.eval(`document.querySelector('.lightbox-view').classList.contains('lb-bare')`)), `${name}: …and brings them back`);
      // the back gesture closes it, and only it
      await p.eval(`history.back()`);
      await sleep(500);
      check(!(await view(p)), `${name}: back closes it`);
      check((await p.eval(`!!document.querySelector('.workspace') && location.href.startsWith('http')`)) && (await p.eval(`history.length`)) >= histBefore, `${name}: …and stays in the app`);
      // a swipe down closes it too
      b = await thumb(p, 1);
      await p.tap(b.cx, b.cy);
      await waitReady(p);
      await sleep(250);
      await stroke(p, w / 2, h * 0.4, w / 2 + 6, h * 0.4 + 260, 10);
      await sleep(300);
      check(!(await view(p)), `${name}: a swipe down closes it`);
      // the long screenshot: a double tap shows its full width from where you tapped
      b = await thumb(p, 2);
      await p.tap(b.cx, b.cy);
      await waitReady(p);
      await sleep(250);
      v = await view(p);
      check(fitted(v), `${name}: a long screenshot shows whole at first`, v.img);
      await dtap(p, w / 2, h * 0.3);
      v = await view(p);
      check(near(v.img.w, w, 2), `${name}: a double tap shows it full width`, v.img);
      await p.click(".lb-close");
      await sleep(400);
      check(!(await view(p)), `${name}: ✕ closes it`);
      // nothing else changed, and the pages still turn
      const after = await layout(p);
      check(after.panes === before.panes && after.zoom === 1 && !/viewer-open/.test(after.cls), `${name}: the tiles are where they were, nothing zoomed`, { before, after });
      if (name === "cover") {
        const i0 = await p.eval(`__mod('lib/workspace.ts').then((m) => m.workspace.order().indexOf(m.workspace.focused()))`);
        const y = h * 0.45;
        await stroke(p, w * 0.85, y, w * 0.1, y, 10);
        await sleep(400);
        const i1 = await p.eval(`__mod('lib/workspace.ts').then((m) => m.workspace.order().indexOf(m.workspace.focused()))`);
        check(i1 === i0 + 1, `${name}: afterwards a swipe still turns the page`, { i0, i1 });
      }
      check(p.errors.length === 0, `${name} (${theme}): no errors`, p.errors);
      await p.close();
    }
  }
  finish();
});
