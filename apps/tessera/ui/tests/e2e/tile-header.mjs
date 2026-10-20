// Tile headers: the running mark stays in view however long the title (plain chats, bot chats,
// rooms; tile headers and sidebar rows; narrow and wide tiles), the mark never goes dark for a
// beat, and on phones (the Fold folded and unfolded) the header is one calm line with nothing
// on top of anything: the title opens the rest, and the sticky bars never cover the chat.
import { SHOTS, check, device, finish, mock, readEarlier, run, signIn, sleep } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const PLAIN = "ses_2b7e91c04d5a";
const BOT = "ses_9e41b7d05c2a"; // Forge's chat (here renamed Orion, YOLO)
const ROOM = "ses_4b7d2e9a0c15";
const AUDIT = "ses_7a1c5e93b2d0"; // a chat that used subagents
const LONG = "okay please do a full sweep of my entire homelab and report anything that looks off to me before tonight so we can fix it together";

/** Set titles and mark the chats running (just before measuring: the app checks quiet "running" chats with the server). */
const running = (p, ids, len) =>
  p.eval(`(async () => {
    const s = await __mod('lib/store.ts');
    for (const id of ${JSON.stringify(ids)}) {
      const cur = s.state.sessions[id]?.info;
      if (!cur) continue;
      s.upsertSession({ ...JSON.parse(JSON.stringify(cur)), title: ${JSON.stringify(LONG)}.slice(0, ${len}), status: 'running' });
    }
    await new Promise((r) => setTimeout(r, 120));
  })()`);

/** Is the element seen: real size, inside every clipping ancestor, on top at its centre. */
const SEEN = `const seen = (el) => {
  if (!el) return 'absent';
  const r = el.getBoundingClientRect();
  if (r.width < 6 || r.height < 6) return 'squashed';
  for (let a = el.parentElement; a; a = a.parentElement) {
    const cs = getComputedStyle(a);
    if (cs.overflowX !== 'visible' || cs.overflowY !== 'visible') {
      const b = a.getBoundingClientRect();
      if (r.left < b.left - 0.5 || r.right > b.right + 0.5 || r.top < b.top - 0.5 || r.bottom > b.bottom + 0.5) return 'clipped';
    }
  }
  const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
  return hit && (hit === el || el.contains(hit)) ? 'ok' : 'covered';
};`;

const marks = (p) =>
  p.eval(`(() => { ${SEEN}
    const out = [];
    for (const pane of document.querySelectorAll('.pane')) {
      const r = pane.getBoundingClientRect();
      if (pane.style.display === 'none' || r.right < 2 || r.left > innerWidth - 2) continue;
      const h = pane.querySelector('.pane-head');
      if (!h || !pane.querySelector('.chat')) continue;
      const kind = h.classList.contains('has-bot') ? 'bot' : h.classList.contains('is-channel') ? 'room' : 'plain';
      const t = h.querySelector('.title-text');
      out.push({ where: kind + ' header ' + Math.round(r.width) + 'px', v: seen(h.querySelector('.tess')), cut: t ? t.scrollWidth > t.clientWidth + 1 : null });
    }
    for (const row of document.querySelectorAll('.sidebar .srow')) {
      const tess = row.querySelector('.srow-main .tess');
      if (tess) out.push({ where: 'sidebar row ' + (row.querySelector('.brow-av, .srow-lead .bot-av, .srow-lead img, .srow-lead [class*=av]') ? '(bot)' : ''), v: seen(tess) });
    }
    return out;
  })()`);

run(async () => {
  // ── the running mark on long titles (desktop) ──────────────────────────
  console.log("— desktop: the running mark on long titles");
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const desk = await device();
  await desk.viewport(1672, 900);
  await signIn(desk, "Desktop");
  await desk.eval(`(async () => { const s = await __mod('lib/store.ts'); s.setState('bots', (b) => b.id === 'bot_forge1', 'name', 'Orion'); s.setState('bots', (b) => b.id === 'bot_forge1', 'permission', 'auto'); })()`);
  await ws(desk, `m.openSession('${PLAIN}', { pane: m.workspace.focused() })`);
  await sleep(300);
  await ws(desk, `m.openSession('${BOT}', { newPane: true })`);
  await sleep(300);
  await ws(desk, `m.openSession('${ROOM}', { newPane: true })`);
  await sleep(1200);
  let bad = [];
  let cases = 0;
  for (const [w, layout] of [[1672, "three tiles"], [1180, "three narrow tiles"], [2400, "three wide tiles"]]) {
    await desk.viewport(w, 900);
    await sleep(500);
    for (const len of [24, 48, 72, 96, 130]) {
      await running(desk, [PLAIN, BOT, ROOM], len);
      for (const m of await marks(desk)) {
        cases++;
        if (m.v !== "ok") bad.push(`${layout}, ${len} chars: ${m.where} → ${m.v}`);
      }
    }
  }
  check(!bad.length && cases >= 60, `the running mark is in view for long titles (${cases} cases: plain, bot, room headers and sidebar rows)`, bad.slice(0, 8));
  await desk.viewport(1672, 900);
  await sleep(400);
  await running(desk, [PLAIN, BOT, ROOM], 130);
  const cut = (await marks(desk)).filter((m) => m.where.includes("header"));
  check(cut.length === 3 && cut.every((m) => m.cut && m.v === "ok"), "…the title gives way (cut short), the mark stays", cut);
  await desk.shot(`${SHOTS}/header-running-long-titles.png`);
  // One tile, alone and wide.
  await ws(desk, `m.focusPane('${await ws(desk, `return m.paneShowing('${BOT}')`)}'); m.toggleMaximize()`);
  await sleep(500);
  await running(desk, [BOT], 130);
  const solo = (await marks(desk)).find((m) => m.where.startsWith("bot header"));
  check(solo?.v === "ok", "a bot chat alone in a wide tile shows it too", solo);
  await ws(desk, `m.toggleMaximize()`);
  await sleep(300);

  // ── the mark itself: never all dark; still with reduced motion ─────────
  console.log("— the mark");
  await running(desk, [PLAIN], 30);
  const darkest = await desk.eval(`(async () => {
    const t = document.querySelector('.pane .pane-head .tess');
    let min = 1;
    for (let k = 0; k < 48; k++) {
      const top = Math.max(...[...t.querySelectorAll('i')].map((i) => Number(getComputedStyle(i).opacity)));
      min = Math.min(min, top);
      await new Promise((r) => setTimeout(r, 37));
    }
    return min;
  })()`);
  check(darkest > 0.9, "some tessera is always lit (no dark beat in the loop)", darkest);
  await desk.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "reduce" }] });
  await sleep(300);
  await running(desk, [PLAIN], 30);
  const slow = await desk.eval(`(async () => {
    const t = document.querySelector('.pane .pane-head .tess');
    let min = 1;
    for (let k = 0; k < 40; k++) {
      min = Math.min(min, Math.max(...[...t.querySelectorAll('i')].map((i) => Number(getComputedStyle(i).opacity))));
      await new Promise((r) => setTimeout(r, 80));
    }
    return { min, duration: getComputedStyle(t.querySelector('i')).animationDuration };
  })()`);
  check(slow.min > 0.9 && slow.duration === "3s", "with reduced motion it keeps going, slower, and stays lit", slow);
  await desk.send("Emulation.setEmulatedMedia", { features: [] });
  console.log("desktop errors:", desk.errors);
  check(!desk.errors.length, "no page errors (desktop)");
  await desk.close();

  // The app's CSS as the build minifies it keeps every animation (a nameless shorthand once
  // became "animation: none", and the mark never moved in the apps).
  const { transform } = await import("lightningcss");
  const fs = await import("node:fs");
  const dir = new URL("../../src/styles/", import.meta.url);
  const lost = [];
  for (const f of fs.readdirSync(dir).filter((x) => x.endsWith(".css"))) {
    const src = fs.readFileSync(new URL(f, dir), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");
    for (const m of src.matchAll(/animation:\s*([^;{}]+);/g)) {
      const v = m[1].trim();
      if (v === "none") continue;
      const out = transform({ filename: f, code: Buffer.from(`.x{animation:${v}}`), minify: true }).code.toString();
      if (/animation:none/.test(out)) lost.push(`${f}: ${v}`);
    }
  }
  const built = transform({ filename: "base.css", code: fs.readFileSync(new URL("base.css", dir)), minify: true }).code.toString();
  check(!lost.length && /\.tess i\{[^}]*animation-duration:1\.6s/.test(built), "the minified CSS keeps every animation, the running mark's included", lost);

  // ── phones: the Fold folded (344) and unfolded (673), dark and light ───
  for (const theme of ["dark", "light"]) {
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(344, 882, true, 2.625);
    await signIn(p, `Fold ${theme}`);
    await p.theme(theme);
    await p.eval(`(async () => { const s = await __mod('lib/store.ts'); s.setState('bots', (b) => b.id === 'bot_forge1', 'name', 'Orion'); s.setState('bots', (b) => b.id === 'bot_forge1', 'permission', 'auto'); })()`);
    await ws(p, `m.openSession('${AUDIT}', { pane: m.workspace.focused() })`);
    await sleep(300);
    await ws(p, `m.openSession('${BOT}', { newPane: true })`);
    await sleep(300);
    await ws(p, `m.openSession('${ROOM}', { newPane: true })`);
    await sleep(1000);
    await ws(p, `m.focusPane(m.workspace.order()[0])`);
    await sleep(500);
    for (const [fold, w, h] of [["folded", 344, 882], ["unfolded", 673, 841]]) {
      const tag = `${fold}-${theme}`;
      console.log(`— phone ${tag}`);
      await p.viewport(w, h, true, 2.625);
      await sleep(700);
      // Running, YOLO, a long title, a subagent at work.
      await p.eval(`(async () => {
        const s = await __mod('lib/store.ts');
        const a = await __mod('lib/actions.ts');
        const w = await __mod('lib/workspace.ts');
        await a.patchPane(w.paneShowing('${AUDIT}'), { permission: 'auto' });
        s.upsertSession({ ...JSON.parse(JSON.stringify(s.state.sessions['${AUDIT}'].info)), id: 'ses_kid0000001', kind: 'subagent', parent_id: '${AUDIT}', status: 'running', title: '↳ search-diagnosis', internal: false, steps: 3 });
      })()`);
      await running(p, [AUDIT, BOT, ROOM], 60);
      const heads = await p.eval(`(() => { ${SEEN}
        const box = (e) => { const r = e.getBoundingClientRect(); return { l: r.left, t: r.top, r: r.right, b: r.bottom }; };
        const meet = (a, b) => a.l < b.r - 0.5 && b.l < a.r - 0.5 && a.t < b.b - 0.5 && b.t < a.b - 0.5;
        const dots = document.querySelector('.workspace > .ws-dots');
        const out = [];
        for (const pane of document.querySelectorAll('.pane')) {
          const r = pane.getBoundingClientRect();
          if (pane.style.display === 'none' || r.right < 2 || r.left > innerWidth - 2) continue;
          const h = pane.querySelector('.pane-head');
          const kind = h.classList.contains('has-bot') ? 'bot' : h.classList.contains('is-channel') ? 'room' : 'plain';
          // What one sees: text, the marks, the buttons' icons.
          const parts = [...h.querySelectorAll('.title-text, .tess, .title-yolo, .title-chev, .icon-btn > svg, .head-av, .head-members, .ws-dots')].filter((e) => e.getClientRects().length);
          if (dots) parts.push(dots);
          const overlaps = [];
          for (let i = 0; i < parts.length; i++) for (let j = i + 1; j < parts.length; j++) {
            if (parts[i].contains(parts[j]) || parts[j].contains(parts[i])) continue;
            if (meet(box(parts[i]), box(parts[j]))) overlaps.push((parts[i].className.baseVal ?? parts[i].className) + ' × ' + (parts[j].className.baseVal ?? parts[j].className));
          }
          const lines = new Set([...h.querySelectorAll('.title-text, .model-sub, .head-sub, .title-static')].filter((e) => e.getClientRects().length).map((e) => Math.round(e.getBoundingClientRect().top / 8)));
          out.push({
            kind,
            mark: seen(h.querySelector('.title-menu .tess')),
            yolo: !!h.querySelector('.title-menu .title-yolo'),
            extras: [...h.querySelectorAll('.model-sub, .model-chip, .yolo-badge, .opts-btn, .head-count, .page-dots')].filter((e) => e.getClientRects().length).map((e) => e.className),
            buttons: [...h.querySelectorAll('button')].filter((e) => e.getClientRects().length && !e.closest('.popover')).length,
            lines: lines.size,
            overlaps,
          });
        }
        return { out, dots: dots ? { n: dots.children.length, on: dots.querySelectorAll('.on').length } : null };
      })()`);
      for (const h of heads.out) {
        check(h.extras.length === 0 && h.lines === 1, `${tag} ${h.kind}: one line, no model line, chip, YOLO badge or options button`, h);
        check(h.mark === "ok", `${tag} ${h.kind}: the running mark shows on the title`, h.mark);
        check(!h.overlaps.length, `${tag} ${h.kind}: nothing sits on anything (title, marks, dots, buttons)`, h.overlaps);
      }
      check(heads.out.some((h) => h.kind === "plain" && h.yolo), `${tag}: YOLO shows as a small mark on the title`);
      const visible = fold === "folded" ? 1 : 2;
      check(heads.dots?.n === 3 && heads.dots.on === visible, `${tag}: one row of page dots for the workspace (${visible} lit)`, heads.dots);
      // (Unfolded: options, the overview and the close X every tile has since batch 9.)
      check(heads.out.every((h) => h.buttons <= (fold === "folded" ? 5 : 3)), `${tag}: few buttons in a header (folded includes Close and New)`, heads.out.map((h) => h.buttons));
      // The subagents bar sits above the chat, never over it. Scroll like a reader first.
      const auditPane = await ws(p, `return m.paneShowing('${AUDIT}')`);
      await readEarlier(p, `[data-pane-id="${auditPane}"] .transcript`);
      const sticky = await p.eval(`(async () => {
        const w = await __mod('lib/workspace.ts');
        const pane = document.querySelector('[data-pane-id="' + w.paneShowing('${AUDIT}') + '"]');
        const b = pane.querySelector('.agents-bar'), t = pane.querySelector('.transcript');
        if (!b || !t) return { bar: !!b };
        t.scrollTop = 0;
        await new Promise((r) => setTimeout(r, 100));
        const br = b.getBoundingClientRect(), tr = t.getBoundingClientRect();
        const first = t.querySelector('.transcript-inner > *').getBoundingClientRect();
        const hit = document.elementFromPoint(tr.left + tr.width / 2, br.bottom + 4);
        return { bar: true, below: tr.top >= br.bottom - 0.5, firstShown: first.top >= br.bottom, hitInChat: !!hit && t.contains(hit) };
      })()`);
      check(sticky.bar && sticky.below && sticky.firstShown && sticky.hitInChat, `${tag}: the subagents bar sits above the chat, not over its first rows`, sticky);
      await p.shot(`${SHOTS}/phone-header-${tag}.png`);
      // The title opens the rest: the model (and its thinking level), permission, rename, close.
      await ws(p, `m.focusPane(m.paneShowing('${AUDIT}'))`);
      await sleep(300);
      await p.click(`.pane.focused .title-menu`);
      await p.waitFor(`document.querySelector('.tile-opts')`, 3000);
      const sheet = await p.eval(`(() => { const o = document.querySelector('.tile-opts'); return { model: o.querySelector('.opt-model .opt-line-value')?.textContent ?? null, perm: !!o.querySelector('.perm-seg'), rename: [...o.querySelectorAll('.act-btn')].some((b) => b.textContent.includes('Rename')), close: [...o.querySelectorAll('.act-btn')].some((b) => b.textContent.includes('Close')) }; })()`);
      check(!!sheet.model && sheet.perm && sheet.rename && sheet.close, `${tag}: tapping the title opens model, permission, rename and close`, sheet);
      await p.shot(`${SHOTS}/phone-header-sheet-${tag}.png`);
      await p.click(".tile-opts .opt-model");
      await p.waitFor(`document.querySelector('.pop-model .model-menu')`, 3000).catch(() => {});
      check(await p.eval(`!!document.querySelector('.pop-model .model-menu')`), `${tag}: …and the Model line opens the model menu`);
      await p.key("Escape", "Escape", 0, 27);
      await sleep(300);
    }
    console.log(`phone ${theme} errors:`, p.errors);
    check(!p.errors.length, `no page errors (phone, ${theme})`);
    await p.close();
  }
  finish();
});
