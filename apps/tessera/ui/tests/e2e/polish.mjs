// B27 polish: what the app walkthrough found. Wide tables scroll sideways on a phone; text and primary buttons
// keep 4.5:1 in both themes and every accent; a tooltip at the screen's edge stays on one line; a message sent
// while offline says so (and the phone keeps its header); Settings → Web & voice uses separate search and voice
// saves (the voice action appears only when changed); the pairing step lines up; search hits and note previews are plain words with the match in
// bold; a scheduled task is set in plain words (the cron expression sits under Custom); the freedom control is
// whole on a phone; the tab badge stays off its icon; a phone's header says when a chat waits for approval; a
// board question keeps its width in a narrow tile. Synthetic data only; desktop, phone cover screen and Fold.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const SHAPES = { desktop: [1440, 900, false, 1], phone: [344, 882, true, 2], fold: [884, 1104, true, 1.5] };
const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const ui = (p, js) => p.eval(`__mod('lib/ui.ts').then(m => { ${js} })`);
const openChat = async (p, id) => {
  await ws(p, `m.openSession(${JSON.stringify(id)}, { pane: m.workspace.focused() })`);
  await sleep(900);
};
const field = (p, sel, value) => p.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); Object.getOwnPropertyDescriptor(Object.getPrototypeOf(e), 'value').set.call(e, ${JSON.stringify(value)}); e.dispatchEvent(new Event('input', { bubbles: true })); })()`);
const say = async (p, text) => {
  await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
  await p.type(text);
  await sleep(150);
  await p.click(".pane.focused .composer .send-btn:not(.stop)");
  await sleep(300);
};
const net = (p, offline) => p.send("Network.emulateNetworkConditions", { offline, latency: 0, downloadThroughput: -1, uploadThroughput: -1 });
const setValue = (p, css, v, evt = "change") =>
  p.eval(`(() => { const e = document.querySelector(${JSON.stringify(css)}); e.value = ${JSON.stringify(v)}; e.dispatchEvent(new Event(${JSON.stringify(evt)}, { bubbles: true })); })()`);

/** WCAG contrast of every visible text node against its real background; what is below 4.5 (3 for large text). */
const contrastScan = (p) =>
  p.eval(`(() => {
    const parse = (s) => { const m = s.match(/rgba?\\(([^)]+)\\)/); if (!m) return null; const a = m[1].split(',').map(parseFloat); return { r: a[0], g: a[1], b: a[2], a: a.length > 3 ? a[3] : 1 }; };
    const lum = (c) => { const f = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); }; return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b); };
    const over = (t, b) => ({ r: t.r * t.a + b.r * (1 - t.a), g: t.g * t.a + b.g * (1 - t.a), b: t.b * t.a + b.b * (1 - t.a), a: 1 });
    const bgOf = (el) => { const st = []; for (let a = el; a; a = a.parentElement) { const c = parse(getComputedStyle(a).backgroundColor); if (c && c.a > 0) { st.push(c); if (c.a >= 0.99) break; } } let acc = parse(getComputedStyle(document.documentElement).backgroundColor) || { r: 255, g: 255, b: 255, a: 1 }; for (let i = st.length - 1; i >= 0; i--) acc = over(st[i], acc); return acc; };
    const out = [];
    const w = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const seen = new Set();
    while (w.nextNode()) {
      const n = w.currentNode, t = n.textContent.trim();
      if (t.length < 2 || /^[\\u2190-\\u2bff\\u{1f300}-\\u{1faff}]+$/u.test(t)) continue;
      const el = n.parentElement;
      if (!el || seen.has(el)) continue;
      seen.add(el);
      const r = el.getBoundingClientRect();
      if (r.width < 4 || r.height < 4 || r.bottom < 0 || r.top > innerHeight || r.right < 0 || r.left > innerWidth) continue;
      let hidden = false;
      for (let a = el; a && a !== document.body; a = a.parentElement) { const cs = getComputedStyle(a); if (cs.display === 'none' || cs.visibility === 'hidden' || Number(cs.opacity) < 0.05) { hidden = true; break; } }
      if (hidden) continue;
      const cs = getComputedStyle(el), fg = parse(cs.color);
      if (!fg) continue;
      let op = 1;
      for (let a = el; a && a !== document.body; a = a.parentElement) op *= Number(getComputedStyle(a).opacity);
      const bg = bgOf(el), f2 = over({ ...fg, a: fg.a * op }, bg);
      const L1 = lum(f2), L2 = lum(bg), ratio = (Math.max(L1, L2) + 0.05) / (Math.min(L1, L2) + 0.05);
      const px = parseFloat(cs.fontSize), large = px >= 24 || (px >= 18.66 && parseInt(cs.fontWeight) >= 700);
      if (ratio < (large ? 3 : 4.5)) out.push({ ratio: Math.round(ratio * 100) / 100, text: t.slice(0, 28), cls: String(el.className).slice(0, 40) });
    }
    return out.sort((a, b) => a.ratio - b.ratio).slice(0, 8);
  })()`);

const TABLE_MD =
  "| a | b | c | d | e | f | g | h | i | j |\n|---|---|---|---|---|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 |\n| one | two | three | four | five | six | seven | eight | nine | ten |";

// A fresh connection for each request: the mock's reset closes the open ones.
const rpc = async (method, params) => {
  const c = await wsClient("polish");
  try {
    const r = await c.rpc(method, params);
    if (r.error) throw new Error(`${method}: ${r.error.message}`);
    return r.result;
  } finally {
    c.close();
  }
};

run(async () => {
  for (const [shape, [w, h, mobile, dpr]] of Object.entries(SHAPES)) {
    for (const theme of ["light", "dark"]) {
      if (process.env.ONLY && !`${shape} ${theme}`.includes(process.env.ONLY)) continue;
      const tag = `${shape} ${theme}`;
      console.log(`— ${tag}`);
      await mock("/__mock/reset");
      await mock("/__mock/mosaics/reset");
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Polish ${tag}`);
      await p.theme(theme);
      await sleep(300);

      // ── new chat offers one set of choices, never tips plus another chip row ─────────────────────
      await p.eval(`__mod('lib/prefs.ts').then(m => m.setPrefs('tips', true))`);
      await ws(p, `m.newChat()`);
      await sleep(300);
      check(await p.eval(`!!document.querySelector('.pane.focused .hero-tips') && !document.querySelector('.pane.focused .quick-picks')`), `${tag}: a new chat has suggestions, without a second set of chips`);
      await p.shot(`${SHOTS}/polish-${shape}-${theme}-home.png`);
      await p.click('.pane.focused .tips-x');
      check(await p.eval(`!document.querySelector('.pane.focused .hero-tips') && !!document.querySelector('.pane.focused .quick-picks')`), `${tag}: hiding suggestions makes room for quick picks instead`);

      // ── contrast: the first screen, a chat, and the accents' button ink ──────────────────────────
      await openChat(p, "ses_8c1f2a9d4e70");
      if (mobile) await ui(p, `m.setDrawerOpen(true)`);
      await sleep(500);
      const low = await contrastScan(p);
      check(low.every((x) => /^[^\w]*$/.test(x.text)), `${tag}: no text on the first screen or in the chat is below 4.5:1`, low);
      if (mobile) await ui(p, `m.setDrawerOpen(false)`);
      const inks = await p.eval(`(async () => {
        const pr = await __mod('lib/prefs.ts');
        const lin = (v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); };
        const L = (c) => 0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2]);
        const rgb = (s) => (s.match(/\\d+(\\.\\d+)?/g) || []).slice(0, 3).map(Number);
        const hex = (s) => { s = s.replace('#', ''); return [0, 2, 4].map((i) => parseInt(s.slice(i, i + 2), 16)); };
        const out = {};
        for (const m of pr.MATERIALS) {
          pr.setPrefs('accent', m.id);
          await new Promise((r) => setTimeout(r, 60));
          const cs = getComputedStyle(document.documentElement);
          const bg = rgb(cs.getPropertyValue('--accent')), fg = hex(cs.getPropertyValue('--accent-fg').trim());
          const a = L(bg), b = L(fg);
          out[m.id] = Math.round(((Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05)) * 100) / 100;
        }
        pr.setPrefs('accent', 'smalti');
        return out;
      })()`);
      check(Object.values(inks).every((x) => x >= 4.5), `${tag}: the ink on every accent's button reads at 4.5:1 or better`, inks);

      // ── a wide table on a phone scrolls sideways, its words stay whole ───────────────────────────
      await openChat(p, "ses_e0b6d4a29c1f");
      await p.eval(`(async () => {
        const sm = await __mod('lib/mdstream.ts');
        const t = document.querySelector('.pane.focused .transcript');
        const host = [...t.querySelectorAll('.md')].pop();
        const clone = host.cloneNode(false); clone.id = 'polish-md';
        new sm.StreamRenderer(clone).update(${JSON.stringify(TABLE_MD)}, true);
        host.parentElement.appendChild(clone);
        clone.scrollIntoView({ block: 'center' });
      })()`);
      await sleep(400);
      const tbl = await p.eval(`(() => { const wrap = document.querySelector('#polish-md .table-wrap'); const td = [...wrap.querySelectorAll('td')].find((x) => x.textContent === 'seven'); const cs = getComputedStyle(td); return { wrap: wrap.clientWidth, scroll: wrap.scrollWidth, tdH: td.getBoundingClientRect().height, line: parseFloat(cs.lineHeight) || 20, pad: parseFloat(cs.paddingTop) * 2 }; })()`);
      check(tbl.tdH <= tbl.line + tbl.pad + 4, `${tag}: a table cell's word is not broken into letters`, tbl);
      if (w < 600) check(tbl.scroll > tbl.wrap, `${tag}: a table too wide for the phone scrolls sideways`, tbl);
      await p.eval(`document.getElementById('polish-md')?.remove()`);
      await p.eval(`(async () => {
        const sm = await __mod('lib/mdstream.ts');
        const host = [...document.querySelectorAll('.pane.focused .transcript .md')].pop();
        const clone = host.cloneNode(false); clone.id = 'polish-code';
        new sm.StreamRenderer(clone).update(${JSON.stringify('```js\n// Readable comment\nconst word = "hello";\n```')}, true);
        host.parentElement.appendChild(clone);
        clone.scrollIntoView({ block: 'center' });
      })()`);
      await p.waitFor(`document.querySelector('#polish-code .hljs-comment')`, 5000);
      const codeLow = await contrastScan(p);
      check(!codeLow.some(x => /hljs-comment|hljs-quote/.test(x.cls)), `${tag}: syntax-highlighted comments read at 4.5:1 or better`, codeLow);
      await p.shot(`${SHOTS}/polish-${shape}-${theme}-code.png`);
      await p.eval(`document.getElementById('polish-code')?.remove()`);

      // ── a message sent while offline says so, and the phone keeps its header ─────────────────────
      await openChat(p, "ses_8c1f2a9d4e70");
      await p.send("Network.enable");
      await net(p, true);
      // The socket outlives emulated "offline": the server drops it, and the app's first retry fails.
      for (let i = 0; i < 3 && !(await p.eval(`!!document.querySelector('.offline-banner')`)); i++) {
        await fetch(process.env.MOCK + "/__mock/drop", { method: "POST" });
        await p.waitFor(`document.querySelector('.offline-banner')`, 6000).catch(() => {});
      }
      await p.waitFor(`document.querySelector('.offline-banner')`, 3000);
      const bar = await p.eval(`(() => { const b = document.querySelector('.offline-banner').getBoundingClientRect(); const hd = document.querySelector('.pane.focused .pane-head')?.getBoundingClientRect(); return { top: b.top, text: document.querySelector('.offline-banner').textContent, headBottom: hd?.bottom ?? 0, cut: document.querySelector('.offline-banner .ellipsis').scrollWidth > document.querySelector('.offline-banner .ellipsis').clientWidth + 1 }; })()`);
      check(/^Offline/.test(bar.text) && !bar.cut, `${tag}: the offline pill says Offline and is not cut`, bar);
      if (w < 640) check(bar.top >= bar.headBottom - 1, `${tag}: the offline pill sits under the phone's header, not over it`, bar);
      await say(p, "please check the logs when you can");
      await p.waitFor(`document.querySelector('.msg-wait')`, 4000).catch(() => {});
      check(/Not sent yet/.test((await p.eval(`document.querySelector('.msg-wait')?.textContent ?? ''`))), `${tag}: a message typed offline says it was not sent yet`);
      check(/Offline/.test(await p.eval(`document.querySelector('.pane.focused .composer textarea').placeholder`)), `${tag}: the message box says it is offline`);
      await p.shot(`${SHOTS}/polish-${shape}-${theme}-offline.png`);
      await net(p, false);
      await p.waitFor(`!document.querySelector('.msg-wait') && !document.querySelector('.offline-banner')`, 15000).catch(() => {});
      check(await p.eval(`!document.querySelector('.msg-wait') && !document.querySelector('.offline-banner')`), `${tag}: back online the note and the pill go`);
      await sleep(300);

      // ── the approval: a tile header names it, one tag only ───────────────────────────────────────
      await mock("/__mock/reset");
      await p.goto(process.env.APP, 1500);
      await p.waitFor(`document.querySelector('.workspace')`, 10000);
      await p.theme(theme);
      await say(p, "delete the old build artifacts and deploy");
      await p.waitFor(`document.querySelector('.pane.focused .approval')`, 15000);
      await sleep(400);
      const ap = await p.eval(`(() => { const s = document.querySelector('.status-pill'); const r = s?.getBoundingClientRect(); const row = [...document.querySelectorAll('.pane.focused .tool .badge-warn')].map((b) => b.textContent); return { pill: s ? getComputedStyle(s).display : 'none', text: s?.textContent ?? '', w: r ? Math.round(r.width) : 0, rowTags: row.length, headX: document.querySelector('.pane.focused .pane-head')?.getBoundingClientRect().right }; })()`);
      check(ap.pill !== "none" && ap.text === "Needs approval", `${tag}: the header says the chat needs approval (a short "Approve" on a phone)`, ap);
      if (w < 640) check(ap.w < 70, `${tag}: …in a small pill that leaves the title room`, ap);
      check(ap.rowTags === 0, `${tag}: the tool row doesn't repeat "needs approval" above the approval card`, ap);
      if (shape === "desktop") {
        const bd = await p.eval(`(() => { const b = document.querySelector('.sb-tab-badge.need')?.getBoundingClientRect(), i = document.querySelector('.sb-tab-icon svg')?.getBoundingClientRect(), l = document.querySelector('.sb-tab.on .sb-tab-label')?.getBoundingClientRect(); if (!b || !i) return null; const over = (a, c) => Math.max(0, Math.min(a.right, c.right) - Math.max(a.left, c.left)) * Math.max(0, Math.min(a.bottom, c.bottom) - Math.max(a.top, c.top)); return { icon: Math.round(over(b, i)), label: l ? Math.round(over(b, l)) : 0, area: Math.round(b.width * b.height), busy: !!document.querySelector('.sb-tab.on .sb-tab-busy') && getComputedStyle(document.querySelector('.sb-tab.on .sb-tab-busy')).display !== 'none' }; })()`);
        check(!!bd && bd.icon <= bd.area * 0.2 && bd.label === 0 && !bd.busy, `${tag}: the tab's number sits at the icon's corner, off its glyph and the name, with no second marker`, bd);
        await p.shot(`${SHOTS}/polish-${shape}-${theme}-badge.png`, { x: 0, y: 80, width: 280, height: 60 });
      }
      await p.close();
    }
  }

  // ── desktop only: the tooltip at the right edge, the board question in a narrow tile ─────────────
  for (const theme of ["light", "dark"]) {
    await mock("/__mock/reset");
    const p = await device();
    await p.viewport(1440, 900, false, 1);
    await signIn(p, "Polish desktop edge");
    await p.theme(theme);
    await openChat(p, "ses_8c1f2a9d4e70");
    const b = await p.box(".topbar .tb-new-more");
    await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: b.cx, y: b.cy });
    await p.waitFor(`document.querySelector('.tooltip')`, 3000).catch(() => {});
    const tip = await p.eval(`(() => { const t = document.querySelector('.tooltip'); if (!t) return null; const r = t.getBoundingClientRect(); return { w: Math.round(r.width), h: Math.round(r.height), right: Math.round(r.right), vw: innerWidth }; })()`);
    check(!!tip && tip.h <= 40 && tip.w >= 100 && tip.right <= tip.vw, `${theme}: a tooltip at the right edge stays on one line inside the window`, tip);
    await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: 5, y: 500 });

    await say(p, "please sign the application");
    await p.waitFor(`document.querySelector('.chat-ask')`, 15000).catch(() => {});
    await sleep(500);
    await p.eval(`(() => { const a = document.querySelector('.chat-ask'); if (a) a.style.width = '250px'; })()`);
    await sleep(200);
    const ask = await p.eval(`(() => { const t = document.querySelector('.ask-pointer-text'); if (!t) return null; const l = [...document.querySelectorAll('.ask-pointer .link-btn')].map((e) => e.getBoundingClientRect().width); return { w: Math.round(t.getBoundingClientRect().width), links: l.length }; })()`);
    check(!!ask && ask.w >= 120 && ask.links === 2, `${theme}: a board question in a narrow tile keeps room for its words, the two links below`, ask);
    await p.close();
  }

  // ── a docked ask leaves a short board readable, with both areas independently scrollable ───────
  for (const [shape, [w, h, mobile, dpr]] of Object.entries(SHAPES)) {
    for (const theme of ["light", "dark"]) {
      await mock("/__mock/reset");
      await mock("/__mock/mosaics/reset");
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Polish short board ${shape} ${theme}`);
      await p.theme(theme);
      const b = await mock("/__mock/board", { session_id: "ses_8c1f2a9d4e70", kind: "confirm" });
      await p.waitFor(`document.querySelector('.board-ask')`, 8000);
      await ws(p, `const id = m.paneShowingBoard('${b.id}'); if (id) m.focusPane(id);`);
      await p.waitFor(`document.querySelector('.pane.focused .board-ask')`, 8000);
      await p.eval(`(() => { const pane = document.querySelector('.pane.focused'); pane.style.height = '280px'; })()`);
      await sleep(500);
      const g = await p.eval(`(() => {
        const board = document.querySelector('.pane.focused .board');
        const body = board.querySelector('.board-body'), ask = board.querySelector('.board-ask'), prose = board.querySelector('.board-prose');
        const br = board.getBoundingClientRect(), cr = body.getBoundingClientRect(), ar = ask.getBoundingClientRect();
        prose.scrollTop = prose.scrollHeight;
        const last = prose.querySelector('.md > :last-child').getBoundingClientRect();
        ask.scrollTop = ask.scrollHeight;
        const actions = ask.querySelector('.ask-actions').getBoundingClientRect();
        return { boardH: br.height, bodyH: cr.height, askH: ar.height, overlap: cr.bottom > ar.top + 1,
          contentReachable: last.bottom <= cr.bottom + 1, actionsReachable: actions.bottom <= ar.bottom + 1,
          scroll: getComputedStyle(prose).overflowY, askScroll: getComputedStyle(ask).overflowY };
      })()`);
      check(g.bodyH >= 40 && g.askH <= g.boardH * 0.61 && !g.overlap, `${shape} ${theme}: a short whiteboard keeps room above the docked question`, g);
      check(g.contentReachable && g.actionsReachable && g.scroll === 'auto' && g.askScroll === 'auto', `${shape} ${theme}: the board's last words and answer buttons can both be reached`, g);
      await p.shot(`${SHOTS}/polish-${shape}-${theme}-short-board.png`);
      await p.close();
    }
  }

  // ── Settings → Web & voice: separate per-card saves, including edits while voice is saving ───────
  for (const [shape, [w, h, mobile, dpr]] of Object.entries(SHAPES)) {
    for (const theme of ["light", "dark"]) {
    await mock("/__mock/reset");
    const p = await device();
    await p.viewport(w, h, mobile, dpr);
    await signIn(p, `Polish settings ${shape} ${theme}`);
    await p.theme(theme);
    await ui(p, `m.openSettings('web')`);
    await p.waitFor(`document.querySelector('.settings-tab.active[data-tab="web"]') && document.querySelector('.set-voice, .settings-body form, form.stack')`, 8000).catch(() => {});
    await sleep(700);
    // The setup dot is an older-server hint. Modern automatic speech has no
    // missing-service dot, so exercise its tooltip with that explicit UI fixture.
    await p.eval(`__mod('lib/store.ts').then(m => { m.setState('server', 'voice_engine', undefined); m.setState('server', 'voice', false); })`);
    check(!!(await p.eval(`document.querySelector('.settings-tab .tab-dot')?.getAttribute('data-tip')`)), "the dot on Web & voice has words (a tooltip)");
    check(await p.eval(`!document.querySelector('#set-voice .sticky-actions')`), `${shape} ${theme}: clean voice settings offer no Save`);
    await p.eval(`(() => { const e = document.querySelector('#set-voice [aria-label="Where speech is handled"]'); e.value = 'off'; e.dispatchEvent(new Event('change', {bubbles:true})); })()`);
    await p.waitFor(`document.querySelector('#set-voice .sticky-actions button')`, 5000);
    check(await p.eval(`document.querySelector('#set-voice .sticky-actions button').textContent.trim() === 'Save voice settings'`), `${shape} ${theme}: changing voice exposes its named Save`);
    await p.click('#set-voice .sticky-actions button');
    await p.waitFor(`!document.querySelector('#set-voice .sticky-actions')`, 6000);
    check((await rpc('get_voice')).enabled === false, `${shape} ${theme}: voice Save persists its card on the server`);

    await p.click('.search-choice button:nth-child(2)');
    await field(p, 'input[placeholder="https://search.example.com"]', 'https://search.example.com');
    await p.click('.search-choice + .stack + .form-actions button.btn-primary');
    await p.waitFor(`document.querySelector('.search-choice').parentElement.querySelector('.form-actions .btn-primary').disabled`, 6000);
    check((await rpc('get_web_search')).search === 'searxng' && (await rpc('get_voice')).enabled === false, `${shape} ${theme}: search's separate Save persists search, without changing voice`);

    await p.eval(`(() => { const e = document.querySelector('#set-voice [aria-label="Where speech is handled"]'); e.value = 'service'; e.dispatchEvent(new Event('change', {bubbles:true})); })()`);
    await p.waitFor(`document.querySelector('#set-voice [aria-label="Transcription service"]')`, 5000);
    await p.eval(`(() => { const e = document.querySelector('#set-voice [aria-label="Transcription service"]'); e.value = 'openai'; e.dispatchEvent(new Event('change', {bubbles:true})); })()`);
    await p.eval(`document.querySelector('#set-voice .disclosure[aria-expanded="false"]')?.click()`);
    await field(p, '#set-voice input[type="password"]', 'mock-submitted-key');
    await field(p, '#set-voice input[aria-label="Speech language"]', 'en');
    await mock('/__mock/voice-save-delay?ms=1500');
    await p.click('#set-voice .sticky-actions button');
    await p.waitFor(`document.querySelector('#set-voice .sticky-actions button').disabled`, 5000);
    await field(p, '#set-voice input[aria-label="Speech language"]', 'fr');
    await field(p, '#set-voice input[type="password"]', 'mock-replacement-key');
    await p.waitFor(`document.querySelector('#set-voice .sticky-actions button') && !document.querySelector('#set-voice .sticky-actions button').disabled`, 6000);
    const saved = await rpc('get_voice');
    const pending = await p.eval(`({ language: document.querySelector('#set-voice input[aria-label="Speech language"]').value, keyKept: document.querySelector('#set-voice input[type="password"]').value === 'mock-replacement-key' })`);
    check(saved.language === 'en' && pending.language === 'fr' && pending.keyKept, `${shape} ${theme}: a slow save stores only its submitted language and leaves a later language/key edit unsaved`, { serverLanguage: saved.language, ...pending });
    await mock('/__mock/voice-save-delay?ms=0');
    await p.click('#set-voice .sticky-actions button');
    await p.waitFor(`!document.querySelector('#set-voice .sticky-actions')`, 6000);
    check((await rpc('get_voice')).language === 'fr' && await p.eval(`document.querySelector('#set-voice input[type="password"]').value === ''`), `${shape} ${theme}: the second voice Save persists the pending edit and clears only that submitted key`);
    await p.shot(`${SHOTS}/polish-${shape}-${theme}-web-voice.png`);
    await p.close();
    }
  }

  // ── the pairing step ────────────────────────────────────────────────────────────────────────────
  for (const [shape, [w, h, mobile, dpr]] of Object.entries(SHAPES)) {
    for (const theme of ["light", "dark"]) {
    await mock("/__mock/reset?fresh=1");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(w, h, mobile, dpr);
    await signIn(p, `Polish pair ${shape}`).catch(() => {});
    await p.waitFor(`document.querySelector('.setup-wizard')`, 10000).catch(() => {});
    await p.theme(theme);
    await sleep(500);
    await p.eval(`document.querySelectorAll('.setup-step')[2]?.click()`);
    await p.waitFor(`document.querySelector('.code-big')`, 8000).catch(() => {});
    await sleep(500);
    const pr = await p.eval(`(() => { const c = document.querySelector('.code-big'), q = document.querySelector('.pair-qr'); if (!c || !q) return null; const cr = c.getBoundingClientRect(), qr = q.getBoundingClientRect(); return { codeH: Math.round(cr.height), size: parseFloat(getComputedStyle(c).fontSize), sideBySide: cr.left >= qr.right - 2, stacked: cr.top >= qr.bottom - 2, copy: [...document.querySelectorAll('.pair-copies .link-btn')].map((b) => b.textContent.trim()) }; })()`);
    check(!!pr && pr.codeH < pr.size * 1.6, `${shape}: the pairing code is on one line`, pr);
    check(!!pr && (pr.sideBySide || pr.stacked), `${shape}: the code sits beside or under the QR code, never over it`, pr);
    check(!!pr && pr.copy.includes("Copy code"), `${shape}: copying the code is a labelled button`, pr);
    if (mobile) {
      const copies = await p.eval(`[...document.querySelectorAll('.pair-copies .link-btn')].map(e => { const r = e.getBoundingClientRect(); return { name: e.textContent.trim(), w: r.width, h: r.height }; })`);
      check(copies.length === 2 && copies.every(b => b.w >= 44 && b.h >= 44), `${shape} ${theme}: both pairing copy links have real 44px touch targets`, copies);
    }
    await p.shot(`${SHOTS}/polish-${shape}-${theme}-pair.png`);
    await p.close();
    }
  }

  // ── search hits and note previews: plain words, the match in bold ────────────────────────────────
  for (const [shape, [w, h, mobile, dpr]] of Object.entries(SHAPES)) {
    for (const theme of ["light", "dark"]) {
    await mock("/__mock/reset");
    const p = await device();
    await p.viewport(w, h, mobile, dpr);
    await signIn(p, `Polish search ${shape}`);
    await p.theme(theme);
    if (mobile) await ui(p, `m.setDrawerOpen(true)`);
    await sleep(600);
    await p.eval(`document.querySelector('.sidebar input').focus()`);
    await p.type("reconnect");
    await p.waitFor(`document.querySelector('.hit-snippet mark')`, 6000).catch(() => {});
    const hits = await p.eval(`[...document.querySelectorAll('.hit-snippet')].map((e) => ({ text: e.textContent, marks: e.querySelectorAll('mark').length }))`);
    check(hits.length > 0 && hits.every((x) => x.marks >= 1 && !/\*\*|«|»|\`/.test(x.text)), `${shape}: message hits show the match in bold, with no markdown marks or « » left`, hits.slice(0, 3));
    const info = await rpc('create_session', { title: 'Fenced code search fixture' });
    await rpc('send', { session_id: info.id, content: [{ type: 'text', text: '```js\nconst fenceprobe = source;\n```' }], attachments: [] });
    await rpc('abort', { session_id: info.id });
    const raw = (await rpc('search', { query: 'fenceprobe', session_id: info.id, limit: 20 })).find(h => h.session_id === info.id && h.snippet.includes('const'));
    check(raw?.snippet.includes('```js\n') && raw.snippet.includes('«fenceprobe»'), `${shape} ${theme}: the mock search server returns source fences and its FTS-style code match`, raw);
    await p.eval(`document.querySelector('.sidebar input').select()`);
    await p.type('fenceprobe');
    await p.waitFor(`document.querySelector('.hit-snippet mark')?.textContent === 'fenceprobe'`, 6000);
    const codeHits = await p.eval(`[...document.querySelectorAll('.hit-snippet')].map(e => ({ text: e.textContent, marks: [...e.querySelectorAll('mark')].map(m => m.textContent) }))`);
    check(codeHits.some(h => h.text.includes('const fenceprobe = source;') && h.marks.includes('fenceprobe') && !/\x60\x60\x60|«|»/.test(h.text)), `${shape} ${theme}: server-backed search retains fenced code and its highlighted match, without fence lines`, codeHits);
    for (const source of ['if (a < angleprobe && b > 0)', 'std::vector<angleprobe>', '<angleprobe value={a < b} />']) {
      await rpc('send', { session_id: info.id, content: [{ type: 'text', text: '```cpp\n' + source + '\n```' }], attachments: [] });
      await rpc('abort', { session_id: info.id });
    }
    await p.eval(`document.querySelector('.sidebar input').select()`); await p.type('angleprobe');
    await p.waitFor(`document.querySelector('.hit-snippet mark')?.textContent === 'angleprobe'`, 6000);
    const angles = await p.eval(`[...document.querySelectorAll('.hit-snippet')].map(e => ({ text: e.textContent, marks: [...e.querySelectorAll('mark')].map(m => m.textContent), literal: !e.querySelector('angleprobe, script') }))`);
    check(['if (a < angleprobe && b > 0)', 'std::vector<angleprobe>', '<angleprobe value={a < b} />'].every(s => angles.some(h => h.text === s && h.marks.includes('angleprobe') && h.literal)), `${shape} ${theme}: server code comparisons, generics and JSX keep angle brackets and escaped highlighted text`, angles);
    await rpc("save_note", { title: "Camping trip", content: "# Camping trip\n\n# Day one\n\nPack the **tent**, the stove and `matches`. See [the list](https://example.com/list)." });
    await p.eval(`document.querySelector('.sidebar input').select()`);
    await p.type("tent");
    await sleep(800);
    const notes = await p.eval(`[...document.querySelectorAll('.nrow-preview')].map((e) => ({ text: e.textContent, marks: e.querySelectorAll('mark').length }))`);
    check(notes.length > 0 && notes.every((x) => !/^#|\*\*|\`|\]\(/.test(x.text)) && notes.some((x) => x.marks >= 1), `${shape}: a note's preview is plain words, the match in bold`, notes.slice(0, 3));
    await p.shot(`${SHOTS}/polish-${shape}-${theme}-search.png`);
    await p.close();
    }
  }

  // ── a scheduled task: plain words first, the expression under Custom ─────────────────────────────
  for (const [shape, [w, h, mobile, dpr]] of Object.entries(SHAPES)) {
    for (const theme of ["light", "dark"]) {
    await mock("/__mock/reset");
    const p = await device();
    await p.viewport(w, h, mobile, dpr);
    await signIn(p, `Polish job ${shape}`);
    await p.theme(theme);
    await ui(p, `m.setJobEditor({ job: null })`);
    await p.waitFor(`document.querySelector('.job-editor-view .sched')`, 8000);
    await sleep(300);
    const labels = await p.eval(`[...document.querySelectorAll('.job-editor-view .field-label')].map((e) => e.textContent.trim())`);
    check(labels.includes("How often") && !labels.includes("Schedule") && !(await p.eval(`!!document.querySelector('.job-editor-view .sched input.mono')`)), `${shape}: "How often" leads, with no cron expression in sight`, labels);
    check((await p.eval(`document.querySelector('.sched .field-help').textContent`)).includes("Every day at 09:00"), `${shape}: the default reads "Every day at 09:00"`);
    await setValue(p, ".sched > select", "weekdays");
    await setValue(p, '.sched input[type="time"]', "08:30", "input");
    await sleep(200);
    check((await p.eval(`document.querySelector('.sched .field-help').textContent`)).includes("Every weekday at 08:30"), `${shape}: weekdays at 08:30 reads in words`);
    await p.eval(`(() => { const set = (e, v) => { Object.getOwnPropertyDescriptor(Object.getPrototypeOf(e), 'value').set.call(e, v); e.dispatchEvent(new Event('input', { bubbles: true })); }; set(document.querySelector('.job-editor-view input.input:not(.mono)'), 'Morning news'); set(document.querySelector('.job-editor-view textarea'), 'Tell me the news.'); })()`);
    await p.shot(`${SHOTS}/polish-${shape}-${theme}-job.png`);
    await p.click(".job-editor-view button[type=submit]");
    await p.waitFor(`!document.querySelector('.job-editor-view')`, 6000).catch(() => {});
    const made = (await rpc("list_jobs")).find((j) => j.name === "Morning news");
    check(made?.schedule === "30 8 * * 1-5", `${shape}: saving stores the expression the server reads`, made?.schedule);
    // Opening it again finds the same words; Custom shows the expression and keeps it.
    await ui(p, `m.setJobEditor({ job: ${JSON.stringify(made ?? {})} })`);
    await p.waitFor(`document.querySelector('.job-editor-view .sched')`, 6000).catch(() => {});
    check((await p.eval(`document.querySelector('.sched > select')?.value`)) === "weekdays" && (await p.eval(`document.querySelector('.sched input[type="time"]')?.value`)) === "08:30", `${shape}: an existing task opens as weekdays at 08:30`);
    await setValue(p, ".sched > select", "custom");
    await sleep(150);
    check((await p.eval(`document.querySelector('.sched input.mono')?.value`)) === "30 8 * * 1-5", `${shape}: Custom shows the expression to edit`);
    await p.close();
    }
  }

  // ── chat freedom buttons: real 44px targets, and readable labels on cover/Fold ──────────────────
  for (const shape of ['phone', 'fold']) {
    for (const theme of ['light', 'dark']) {
      await mock('/__mock/reset');
      const [w, h, mobile, dpr] = SHAPES[shape];
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Polish freedom ${shape} ${theme}`);
      await p.theme(theme);
      await openChat(p, 'ses_8c1f2a9d4e70');
      await p.eval(`document.querySelector('.pane.focused .title-btn').click()`);
      await p.waitFor(`document.querySelector('.tile-opts .perm-seg')`, 5000);
      const seg = await p.eval(`(() => { const s = document.querySelector('.tile-opts .perm-seg'); return { cols: getComputedStyle(s).gridTemplateColumns.split(' ').length, cut: [...s.querySelectorAll('button span')].some(e => e.scrollWidth > e.clientWidth + 1), boxes: [...s.querySelectorAll('button')].map(b => { const r = b.getBoundingClientRect(); return { w: r.width, h: r.height }; }) }; })()`);
      check(seg.cols === (w < 420 ? 2 : 4) && !seg.cut, `${shape} ${theme}: freedom labels stay whole in ${w < 420 ? 'two rows' : 'one row'}`, seg);
      check(seg.boxes.length === 4 && seg.boxes.every(b => b.w >= 44 && b.h >= 44), `${shape} ${theme}: all freedom buttons have real 44px touch targets`, seg);
      await p.shot(`${SHOTS}/polish-${shape}-${theme}-freedom.png`);
      await p.close();
    }
  }

  finish();
});
