// Long replies stream without lag (contract 8, STREAM): only the block being
// written is rendered again (lib/mdstream.ts), so the cost of a frame doesn't
// grow with the reply; the committed message takes the streamed DOM as it is
// (the same DOM as rendering the whole text at once, no second render); a
// selection survives the stream and the commit; the transcript follows the
// tail until the reader scrolls up, and then nothing moves under them; a
// running tool's output grows in place. Desktop and phone, light and dark.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const T = ".pane.focused .transcript";
const until = (p, expr, ms = 8000) => p.waitFor(expr, ms).then(() => true, () => false);
const open = (p, sid) => p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${sid}', { pane: m.workspace.focused() }))`);

/** Markdown of every kind the renderer splits on, for the equality checks. */
const EDGE = [
  "Setext heading\n===\n\nPara with **bold**, `code` and [a link](https://example.com/x).",
  "- a\n- b\n\n- loose c\n\n1. one\n2. two\n   - nested **x**\n3. three",
  "> quote\n> more\n\nlazy\ncontinuation",
  "```js\nconst a = 1;\n```\n~~~\nplain tilde\n~~~\n\n    indented code\n\nafter",
  "| a | b |\n|:-|-:|\n| 1 | 2 |\n| 3 |\n\ntext after the table",
  "Ref [link][r] here.\n\n[r]: https://example.com/ref",
  "<div>raw html</div>\n\n<b>inline</b> html <i>x</i>",
  "Term with ***triple*** and ~~strike~~ and a\\*literal\\*.\n\n- [ ] task\n- [x] done",
  "Line one  \nhard break\n\n***\n\n# H1\n## H2\n\nEnd.",
  "1) paren list\n2) two\n\n10. ten\n11. eleven",
  "- item with code:\n\n  ```sh\n  echo **not bold**\n  ```\n- next",
  "```\nunclosed fence at the end\nmore",
].join("\n\n");

/** The renderer alone, in the page (no network, no frames): cost, equality, selection. */
async function unit(p) {
  const r = await p.eval(`(async () => {
    const md = await __mod('lib/markdown.ts');
    const ms = await __mod('lib/mdstream.ts');
    // highlight.js loads with the first code block: have it here before timing anything.
    const warm = document.createElement('div');
    warm.innerHTML = '<div class="code-block"><pre><code class="language-js">x</code></pre></div>';
    md.highlightCodes([warm.querySelector('code')]);
    await new Promise((r) => setTimeout(r, 600));
    let seed = 7;
    const rnd = () => (seed = (seed * 1103515245 + 12345) % 2147483648) / 2147483648;
    // Code colours come in idle moments, a big block over a few frames: wait for them.
    const idle = async (box) => {
      for (let k = 0; k < 80 && box.querySelector('.code-block code[class*="language-"]:not(.hljs)'); k++) await new Promise((r) => setTimeout(r, 50));
    };
    /** Stream \`text\` into a fresh renderer in steps of 1…\`step\` characters; per-update costs. */
    const stream = async (text, step, keep) => {
      const box = document.createElement('div');
      box.className = 'md';
      document.body.append(box);
      const r = new ms.StreamRenderer(box);
      const costs = [];
      for (let pos = 0; pos < text.length; ) {
        pos = Math.min(text.length, pos + 1 + Math.floor(rnd() * step));
        const t0 = performance.now();
        r.update(text.slice(0, pos), false);
        costs.push([pos, performance.now() - t0]);
      }
      r.update(text, true);
      await idle(box);
      if (!keep) box.remove();
      return { box, r, costs };
    };
    const whole = (text) => { const d = document.createElement('div'); d.append(md.renderMarkdownFragment(text)); md.enhance(d, true); return d.innerHTML; };
    const out = {};
    // Empty history blocks cannot hold a selection. Reading the live browser
    // selection here used to flush layout after each preceding reply mounted.
    const getSelection = document.getSelection;
    let selectionReads = 0;
    document.getSelection = () => { selectionReads++; return getSelection.call(document); };
    try { ms.saveSelection([]); } finally { document.getSelection = getSelection; }
    out.emptySelectionReads = selectionReads;
    // 1. Same DOM as a whole render, whatever the chunking.
    const edge = ${JSON.stringify(EDGE)};
    out.same = [];
    for (const step of [1, 7, 40]) { const { box } = await stream(edge, step); out.same.push(box.innerHTML === whole(edge)); }
    // 2. Cost per update doesn't grow with the reply: twice the text takes about twice the time.
    const section = (i) => '## Section ' + i + '\\n\\n' + 'Some prose with **bold**, \`code\` and a [link](https://example.com/' + i + '). '.repeat(6) +
      '\\n\\n' + Array.from({ length: 6 }, (_, k) => '- item ' + k + ' of section ' + i + ' with \`x\`').join('\\n') +
      '\\n\\n\\u0060\\u0060\\u0060rust\\n' + Array.from({ length: 8 }, (_, k) => '    let v' + k + ' = f(' + i + ');').join('\\n') + '\\n\\u0060\\u0060\\u0060\\n\\n' +
      '| a | b |\\n|---|---|\\n' + Array.from({ length: 4 }, (_, k) => '| ' + k + ' | ' + i + ' |').join('\\n') + '\\n\\n';
    let doc = '';
    for (let i = 0; doc.length < 64000; i++) doc += section(i);
    const total = (c) => c.reduce((a, [, d]) => a + d, 0);
    const half = await stream(doc.slice(0, 32000), 40);
    const full = await stream(doc, 40);
    out.t32 = total(half.costs); out.t64 = total(full.costs);
    const mean = (c) => c.reduce((a, [, d]) => a + d, 0) / Math.max(1, c.length);
    out.first = mean(full.costs.filter(([pos]) => pos < doc.length * 0.15));
    out.last = mean(full.costs.filter(([pos]) => pos > doc.length * 0.85));
    out.updates = full.costs.length;
    // 3. One block that keeps growing (600 lines of code): appended to, not rebuilt.
    const fence = '\\u0060\\u0060\\u0060ts\\n' + Array.from({ length: 600 }, (_, k) => '  const r' + k + ' = await client.request("x", { since: ' + k + ' });').join('\\n') + '\\n\\u0060\\u0060\\u0060\\n';
    const code = await stream(fence, 40);
    out.fenceFirst = mean(code.costs.filter(([pos]) => pos < fence.length * 0.1));
    out.fenceLast = mean(code.costs.filter(([pos]) => pos > fence.length * 0.9 && pos < fence.length - 10));
    // 3b. One very long paragraph (a long string of text): its settled part is kept, so the cost stays flat,
    // and what shows matches a whole render of the same text at every point where nothing is left open.
    const words = ['snake_case', 'a * b', '**bold words**', '\u0060code span\u0060', '[a link](https://example.com/p)', '~~struck~~', '5 * 3 = 15', '_em_', 'plain', 'text,', 'and', 'more.'];
    let long = '';
    while (long.length < 16000) long += words[Math.floor(rnd() * words.length)] + ' ';
    long = long.trim();
    const pbox = document.createElement('div'); pbox.className = 'md'; document.body.append(pbox);
    const pr = new ms.StreamRenderer(pbox);
    const pc = []; let mismatch = null, compared = 0;
    for (let pos = 0; pos < long.length; ) {
      pos = Math.min(long.length, pos + 1 + Math.floor(rnd() * 40));
      const src = long.slice(0, pos);
      const t0 = performance.now(); pr.update(src, false); pc.push([pos, performance.now() - t0]);
      if (src.endsWith(' ') && ms.closeLine(src) === src && compared < 60 && pos % 7 === 0) {
        compared++;
        const w = document.createElement('div'); w.append(md.renderMarkdownFragment(src));
        const a = pbox.querySelector('p'), b = w.querySelector('p');
        const shape = (e) => [e.textContent.trim(), e.querySelectorAll('strong').length, e.querySelectorAll('em').length, e.querySelectorAll('code').length, e.querySelectorAll('a').length, e.querySelectorAll('del').length].join('|');
        if (!mismatch && shape(a) !== shape(b)) mismatch = { pos, a: shape(a).slice(-160), b: shape(b).slice(-160) };
      }
    }
    pr.update(long, true);
    out.paraFirst = mean(pc.filter(([pos]) => pos < long.length * 0.15));
    out.paraLast = mean(pc.filter(([pos]) => pos > long.length * 0.85));
    out.paraCompared = compared; out.paraMismatch = mismatch;
    out.paraSame = pbox.innerHTML === whole(long);
    pbox.remove();
    // 4. A selection in the block being written survives it being redone, the end, and the hand-over.
    const box = document.createElement('div');
    box.className = 'md';
    document.body.append(box);
    const r = new ms.StreamRenderer(box);
    const para = 'Settled block first.\\n\\nThe paragraph being written has **bold words** and more text that keeps coming';
    r.update(para, false);
    const p = box.querySelector('p:last-of-type');
    const walker = document.createTreeWalker(p, NodeFilter.SHOW_TEXT);
    const t1 = walker.nextNode();
    document.getSelection().setBaseAndExtent(t1, 4, t1, 13);
    const picked = document.getSelection().toString();
    for (const more of [' in', ' small', ' pieces', ' with \`code', ' spans\` too.']) r.update((r.src || para) + more, false);
    out.selTail = document.getSelection().toString() === picked;
    r.update(r.src, true);
    out.selFinal = document.getSelection().toString() === picked;
    ms.park(r, r.src);
    const box2 = document.createElement('div');
    document.body.append(box2);
    box.remove();
    const adopted = ms.adopt(r.src, box2);
    out.adopted = adopted === r && box2.childNodes.length > 0;
    out.selAdopted = document.getSelection().toString() === picked;
    out.picked = picked;
    box2.remove();
    // 4b. …and one inside a big code block survives its colours coming in, a few frames at a time.
    const cbox = document.createElement('div'); cbox.className = 'md'; document.body.append(cbox);
    const cr = new ms.StreamRenderer(cbox);
    const cfence = '\\u0060\\u0060\\u0060ts\\n' + Array.from({ length: 400 }, (_, k) => '  const r' + k + ' = await client.request("x", { since: ' + k + ' });').join('\\n') + '\\n\\u0060\\u0060\\u0060\\n\\nAfter the code.';
    cr.update(cfence.slice(0, 9000), false);
    const ct = cbox.querySelector('code').firstChild;
    document.getSelection().setBaseAndExtent(ct, 5000, ct, 5040);
    const cpicked = document.getSelection().toString();
    cr.update(cfence, false);
    await idle(cbox);
    const ccode = cbox.querySelector('code');
    out.codeSel = { same: document.getSelection().toString() === cpicked, hl: ccode.classList.contains('hljs'), spans: ccode.querySelectorAll('span').length, picked: cpicked };
    cr.update(cfence, true);
    out.codeSame = cbox.innerHTML === whole(cfence);
    cbox.remove();
    // 5. Unfinished markup is closed for display, never shown raw.
    out.closed = [ms.closeLine('a **bold'), ms.closeLine('run \`cargo te'), ms.closeLine('see [docs](https://exa'), ms.closeLine('dangling **'), ms.closeLine('\`a\` and **b \`c')];
    return out;
  })()`);
  check(r.emptySelectionReads === 0, "mounting an empty markdown block never asks for a layout-flushing selection", r.emptySelectionReads);
  check(r.same.every(Boolean), "streamed in pieces of 1, 7 and 40 characters, every kind of block ends up as the same DOM as a whole render", r.same);
  const ratio = r.t64 / r.t32;
  check(ratio < 2.8, `twice the text costs about twice the time, not four times (${r.t32.toFixed(0)} ms for 32k, ${r.t64.toFixed(0)} ms for 64k: ×${ratio.toFixed(2)})`, r);
  check(r.last < r.first * 2.5 + 0.3, `an update at the end of a 64k reply costs what one at the start does (${r.first.toFixed(2)} → ${r.last.toFixed(2)} ms over ${r.updates} updates)`, r);
  check(r.fenceLast < 1.5, `a 600-line code block grows by appending (last updates ${r.fenceLast.toFixed(2)} ms, first ${r.fenceFirst.toFixed(2)} ms)`, r);
  check(r.paraLast < r.paraFirst * 2.5 + 0.3, `one 16k-character paragraph costs as much per update at its end as at its start (${r.paraFirst.toFixed(2)} → ${r.paraLast.toFixed(2)} ms)`, r);
  check(r.paraCompared > 10 && !r.paraMismatch, `…and shows what a whole render of the same text does (${r.paraCompared} points compared)`, r.paraMismatch);
  check(r.paraSame, "…and ends up as the same DOM", r.paraSame);
  check(r.selTail && r.selFinal, `a selection in the block being written survives it being redone and the end ("${r.picked}")`, r);
  check(r.adopted && r.selAdopted, "the committed copy takes the streamed DOM, selection and all", r);
  check(r.codeSel.hl && r.codeSel.spans > 100 && r.codeSel.same, `a selection inside a 400-line code block survives its colours coming in over several frames ("${r.codeSel.picked.trim().slice(0, 30)}…")`, r.codeSel);
  check(r.codeSame, "…and the block ends up as the same DOM as a whole render", r.codeSame);
  check(
    JSON.stringify(r.closed) === JSON.stringify(["a **bold**", "run `cargo te`", "see [docs](https://exa)", "dangling ", "`a` and **b `c`**"]),
    "unfinished bold, code and links are closed for display (a lone marker is left out)",
    r.closed,
  );
}

/** Every frame while on: how far from the bottom the transcript is, and long tasks. */
const MONITOR = `(() => {
  const M = (window.__mon = { gaps: [], long: [], on: true });
  new PerformanceObserver((l) => { for (const e of l.getEntries()) if (M.on) M.long.push([Math.round(e.startTime), Math.round(e.duration)]); }).observe({ type: 'longtask' });
  // Between frames (deltas only change the DOM inside one), a pinned transcript is at its end.
  M.timer = setInterval(() => {
    const t = document.querySelector(${JSON.stringify(T)});
    if (M.on && t && M.pinned) M.gaps.push(Math.round(t.scrollHeight - t.scrollTop - t.clientHeight));
  }, 40);
  M.pinned = false;
  return 1;
})()`;

/** A block around the middle of the transcript (not the one being written) and where it is. */
const probe = (p) =>
  p.eval(`(() => {
    const t = document.querySelector(${JSON.stringify(T)});
    const r = t.getBoundingClientRect();
    let el = null;
    for (let y = r.top + r.height / 2; y > r.top + 20 && !el; y -= 80) {
      const hit = document.elementFromPoint(r.left + r.width / 2, y);
      const md = hit?.closest('.md');
      const block = md ? [...md.children].find((c) => c.contains(hit)) : hit?.closest('.item');
      if (block && !(md?.classList.contains('streaming') && block === md.lastElementChild)) el = block;
    }
    window.__probe = el;
    return { top: Math.round(el.getBoundingClientRect().top), scroll: Math.round(t.scrollTop) };
  })()`);
const probeAgain = (p) => p.eval(`(() => { const t = document.querySelector(${JSON.stringify(T)}); return { top: Math.round(window.__probe.getBoundingClientRect().top), scroll: Math.round(t.scrollTop), on: window.__probe.isConnected }; })()`);

async function streamRun(p, ws, label, mobile, throttle) {
  const info = (await ws.rpc("create_session", { title: "Long write-up" })).result;
  await open(p, info.id);
  await sleep(1200);
  await p.eval(MONITOR);
  if (throttle) await p.send("Emulation.setCPUThrottlingRate", { rate: throttle });
  const started = (await ws.rpc("mock_stream", { session_id: info.id, size: 30000, chunk: [4, 16], delay: [3, 6], thinking: 3000 })).result;
  const text = started.text;

  // The reasoning streams first: open it while it does.
  await until(p, `document.querySelector('${T} .thinking.live .thinking-head')`, 15000);
  await p.eval(`document.querySelector('${T} .thinking.live .thinking-head').click()`);
  await until(p, `document.querySelector('${T} .thinking.live .thinking-body .md')`, 4000);
  await until(p, `document.querySelectorAll('${T} .thinking-body .md > *').length > 3`, 8000);
  const thinkBlocks = await p.eval(`document.querySelectorAll('${T} .thinking-body .md > *').length`);
  check(thinkBlocks > 3, `${label}: the reasoning renders as it streams (${thinkBlocks} blocks so far)`);
  await p.eval(`(() => { const b = document.querySelector('${T} .thinking-body .md').firstElementChild; b.__mark = 'think'; return 1; })()`);

  // The long paragraph is being written: select its first words, mark the reply's first block.
  const tailP = `(() => { const md = document.querySelector('${T} .msg-assistant .md.streaming'); const h = md && [...md.querySelectorAll('h2')].find((e) => e.textContent.includes('A long stretch of prose')); const p = h?.nextElementSibling; return p && p.tagName === 'P' && p === md.lastElementChild && p.textContent.length > 80 ? p : null; })()`;
  const caught = await until(p, `${tailP}`, 20000);
  check(caught, `${label}: caught the long paragraph while it is being written`);
  const picked = await p.eval(`(() => {
    const p = ${tailP};
    const w = document.createTreeWalker(p, NodeFilter.SHOW_TEXT);
    const t = w.nextNode();
    document.getSelection().setBaseAndExtent(t, 0, t, Math.min(40, t.data.length));
    const md = document.querySelector('${T} .msg-assistant .md');
    md.firstElementChild.__mark = 'reply';
    md.closest('.item').__mark = 'row';
    return document.getSelection().toString();
  })()`);
  await p.shot(`${SHOTS}/stream-midway-${label}.png`);
  // A while later the paragraph has moved on (it was redone many times): the selection is the same.
  await until(p, `!(${tailP})`, 15000);
  const sel1 = await p.eval(`document.getSelection().toString()`);
  check(picked.length > 20 && sel1 === picked, `${label}: the selection in the paragraph being written stays put ("${picked.slice(0, 30)}…")`, { picked, sel1 });

  // Following: the transcript stays at its end while the reply grows (opening the reasoning above
  // may have let go of the tail: come back to it first).
  if (await p.eval(`!!document.querySelector('.pane.focused .activity-jump')`)) await p.click(".pane.focused .activity-jump");
  await sleep(300);
  await p.eval(`(window.__mon.pinned = true, 1)`);
  await sleep(2500);
  const gaps = await p.eval(`window.__mon.gaps`);
  const off = gaps.filter((g) => g > 2);
  check(gaps.length > 20 && off.length <= Math.max(1, gaps.length * 0.02), `${label}: pinned to the bottom while it streams (${off.length} of ${gaps.length} samples off the end)`, off.slice(0, 10));

  // Scroll up: from then on nothing moves under the reader, not even when the reply is committed.
  await p.eval(`(window.__mon.pinned = false, 1)`);
  const box = await p.eval(`(() => { const r = document.querySelector('${T}').getBoundingClientRect(); return { x: r.left + r.width / 2, y: r.top + r.height / 2 }; })()`);
  if (mobile) {
    // A finger dragging the content down.
    await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: box.x, y: box.y - 150 }] });
    for (let k = 1; k <= 10; k++) {
      await p.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x: box.x, y: box.y - 150 + k * 40 }] });
      await sleep(16);
    }
    // Stop the finger before lifting, as in follow-layout: this check is a
    // stationary reader, not an active native fling that is still scrolling.
    await sleep(160);
    await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
  } else for (let i = 0; i < 4; i++) await p.send("Input.dispatchMouseEvent", { type: "mouseWheel", x: box.x, y: box.y, deltaX: 0, deltaY: -150 });
  await sleep(500);
  const a = await probe(p);
  const detached = await until(p, `document.querySelector('.pane.focused .activity-jump')`, 3000);
  check(detached, `${label}: scrolling up lets go of the tail ("Jump to live" shows)`);
  const moves = [];
  for (let i = 0; i < 12; i++) {
    await sleep(150);
    const b = await probeAgain(p);
    if (Math.abs(b.top - a.top) > 1 || Math.abs(b.scroll - a.scroll) > 1 || !b.on) moves.push(b);
  }
  check(moves.length === 0, `${label}: while it keeps streaming, what the reader looks at doesn't move`, { a, moves: moves.slice(0, 4) });
  await p.shot(`${SHOTS}/stream-scrolled-up-${label}.png`);

  // The end: committed while the reader is up there.
  const ended = await until(p, `!document.querySelector('${T} .md.streaming') && !document.querySelector('.pane.focused .send-btn.stop')`, 60000);
  check(ended, `${label}: the reply is committed and the turn ends`);
  if (throttle) await p.send("Emulation.setCPUThrottlingRate", { rate: 1 });
  const c = await probeAgain(p);
  check(c.on && Math.abs(c.top - a.top) <= 1 && Math.abs(c.scroll - a.scroll) <= 1, `${label}: committing the reply moves nothing on screen`, { a, c });
  const mon = await p.eval(`(window.__mon.on = false, clearInterval(window.__mon.timer), window.__mon)`);

  // The committed message is the streamed DOM (not rendered again), and the same as a whole render.
  // Code colours come in idle moments.
  await until(p, `!document.querySelector('${T} .msg-assistant .code-block code[class*="language-"]:not(.hljs)')`, 5000);
  const r = await p.eval(`(async () => {
    const md = await __mod('lib/markdown.ts');
    const msgs = [...document.querySelectorAll('${T} .msg-assistant .md')];
    const el = msgs[msgs.length - 1];
    const whole = document.createElement('div');
    whole.append(md.renderMarkdownFragment(${JSON.stringify(text)}));
    md.enhance(whole, true);
    const think = document.querySelector('${T} .thinking.open .thinking-body .md');
    return {
      same: el.innerHTML === whole.innerHTML,
      len: [el.innerHTML.length, whole.innerHTML.length],
      kept: el.firstElementChild.__mark === 'reply',
      inItem: !!el.closest('.item'),
      sameRow: el.closest('.item').__mark === 'row',
      thinkOpen: !!think,
      thinkKept: think?.firstElementChild?.__mark === 'think',
      sel: document.getSelection().toString(),
    };
  })()`);
  check(r.inItem && r.kept && r.sameRow, `${label}: the committed message is the streamed one, the same row and DOM (not rendered again, not moved)`, r);
  check(r.same, `${label}: …and it is exactly what rendering the whole ${text.length}-character text at once gives`, r.len);
  check(r.thinkOpen && r.thinkKept, `${label}: the reasoning the reader opened stays open when committed, same DOM`, r);
  check(r.sel === picked, `${label}: the selection survived the commit`, { sel: r.sel.slice(0, 60), picked });

  // Back down.
  const jump = await until(p, `document.querySelector('.pane.focused .jump-latest')`, 3000);
  check(jump, `${label}: "Jump to latest" is offered once the turn is over`);
  if (jump) await p.click(".pane.focused .jump-latest");
  await until(p, `(() => { const t = document.querySelector('${T}'); return t.scrollHeight - t.scrollTop - t.clientHeight <= 2; })()`, 4000);
  const gap = await p.eval(`(() => { const t = document.querySelector('${T}'); return Math.round(t.scrollHeight - t.scrollTop - t.clientHeight); })()`);
  check(gap <= 2, `${label}: "Jump to latest" goes back to the end (${gap}px off)`);
  return { mon, info };
}

/** A running tool's output grows in place (appended, not replaced) and its collapsed tail is its last lines. */
async function toolRun(p, ws, label) {
  const info = (await ws.rpc("create_session", { title: "Tool output" })).result;
  await open(p, info.id);
  await sleep(1000);
  await ws.rpc("mock_stream", { session_id: info.id, size: 400, tool_lines: 1600, chunk: [6, 20], delay: [3, 6] });
  const tailShown = await until(p, `document.querySelector('${T} .tool-live.tail')?.textContent.includes('case_')`, 15000);
  check(tailShown, `${label}: the running tool's card shows its output as it comes`);
  const lines = await p.eval(`document.querySelector('${T} .tool-live.tail').textContent.split('\\n').length`);
  check(lines <= 6, `${label}: the collapsed card shows the last lines of the output (${lines})`);
  // Open it: the full output, appended to one text node.
  await p.eval(`(() => { const h = document.querySelector('${T} .tool-live.tail').closest('.tool').querySelector('[aria-expanded]'); h.click(); return 1; })()`);
  await until(p, `document.querySelector('${T} .tool-live:not(.tail)')`, 4000);
  const n1 = await p.eval(`(() => { const pre = document.querySelector('${T} .tool-live:not(.tail)'); pre.querySelector('.tool-live-text').firstChild.__mark = 1; return pre.textContent.length; })()`);
  await sleep(700);
  const s2 = await p.eval(`(() => { const pre = document.querySelector('${T} .tool-live:not(.tail)'); const body = pre?.querySelector('.tool-live-text'); return body ? { same: body.firstChild.__mark === 1, len: pre.textContent.length, nodes: body.childNodes.length, end: Math.round(pre.scrollHeight - pre.scrollTop - pre.clientHeight) } : null; })()`);
  check(!!s2 && s2.same && s2.len > n1 && s2.nodes === 1, `${label}: the open output grows in its one text node (${n1} → ${s2?.len} characters)`, s2);
  check(!!s2 && s2.end <= 2, `${label}: …and follows its end`, s2);
  await until(p, `!document.querySelector('.pane.focused .send-btn.stop')`, 30000);
  const done = await p.eval(`[...document.querySelectorAll('${T} .tool .tool-pre')].some((e) => e.textContent.includes('case_00001'))`);
  check(done, `${label}: the finished tool shows its output`);
}

run(async () => {
  await mock("/__mock/reset");
  const ws = await wsClient("stream test");
  for (const [label, w, h, mobile, dpr, theme, throttle] of [
    ["desktop", 1440, 900, false, 1, "light", 4],
    ["phone", 375, 812, true, 2, "dark", 4],
  ]) {
    if (process.env.ONLY && !label.includes(process.env.ONLY)) continue;
    console.log(`— ${label}`);
    const p = await device();
    await p.viewport(w, h, mobile, dpr);
    await signIn(p, `Stream ${label}`);
    await p.theme(theme);
    if (label === "desktop") await unit(p);
    const { mon } = await streamRun(p, ws, label, mobile, throttle);
    // Long tasks while it streamed, at 4× CPU throttling (the dev build, so a little slower than the app).
    const worst = Math.max(0, ...mon.long.map(([, d]) => d));
    console.log(`  ${label} long tasks during the stream (4× throttle):`, JSON.stringify(mon.long.slice(0, 12)));
    check(mon.long.filter(([, d]) => d > 50).length <= 3 && worst < 150, `${label}: at 4× CPU throttling, at most a few tasks over 50 ms in a 30k stream (${mon.long.length}, worst ${worst} ms)`, mon.long);
    for (const t of ["light", "dark"]) {
      await p.theme(t);
      await sleep(300);
      await p.shot(`${SHOTS}/stream-done-${label}-${t}.png`);
    }
    await toolRun(p, ws, label);
    console.log("errors", p.errors);
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    await p.close();
  }
  ws.close();
  finish();
});
