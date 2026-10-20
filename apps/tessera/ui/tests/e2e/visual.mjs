// A visual sweep of the main screens in every shape the app runs in: the folded Fold's cover screen
// (344 px), the unfolded Fold (two tiles side by side) and a desktop (one wide tile, then three), light
// and dark. Every screen is kept as a picture in E2E_OUT/shots/visual (look at them) and checked:
//   · nothing visible in a tile (or the side menu, or Settings) reaches past its right edge, unless it
//     sits in something that scrolls sideways or in a narrower box that clips it;
//   · a run of steps keeps its pieces apart: the strip, "N steps" (whole) and the failed badge inside
//     the row's head, then the page's picture and Watch, none over another, nothing past the row;
//   · tile headers: their buttons, title and address field don't overlap and stay in the header.
// The chat with long runs of browser steps comes from POST /__mock/steps-chat.
import path from "node:path";
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const OUT = path.join(SHOTS, "visual");
const SQLITE = "ses_8c1f2a9d4e70"; // "Refactor session store to SQLite": tool cards, diffs, a plan
const AUDIT = "ses_7a1c5e93b2d0"; // "Audit the reconnect logic": subagents and their reports
const HEALTHZ = "ses_7d3a91e5c4b2"; // "Add a /healthz endpoint": the chat that gets a queue
const MODES = [
  { name: "folded", w: 344, h: 882, mobile: true, dpr: 2.625 },
  { name: "unfolded", w: 690, h: 829, mobile: true, dpr: 2.625 },
  { name: "desktop", w: 1440, h: 900, mobile: false, dpr: 1 },
];
// General last: Settings opens on it, so asking for it first changes nothing on a phone (the list stays).
const SECTIONS = ["appearance", "providers", "devices", "secrets", "general"];

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const ui = (p, js) => p.eval(`__mod('lib/ui.ts').then(u => { ${js} })`);

// ── the checks (run in the page) ───────────────────────────────────────────
const PAGE_LIB = String.raw`
window.__vis = (() => {
  const onScreen = (r) => r.width > 0 && r.height > 0 && r.right > 0 && r.left < innerWidth && r.bottom > 0 && r.top < innerHeight;
  const shown = (e) => { for (let a = e; a && a !== document.body; a = a.parentElement) { const cs = getComputedStyle(a); if (cs.display === 'none' || cs.visibility === 'hidden' || Number(cs.opacity) < 0.05) return false; } return true; };
  const name = (e) => e.tagName.toLowerCase() + (typeof e.className === 'string' && e.className ? '.' + e.className.trim().split(/\s+/).slice(0, 3).join('.') : '') + (e.textContent ? ' "' + e.textContent.trim().slice(0, 30) + '"' : '');
  /** Elements past the right (or left) edge of their root: tiles on screen, the side menu, Settings. */
  function overflow(rootSel) {
    const out = [];
    for (const root of document.querySelectorAll(rootSel)) {
      const R = root.getBoundingClientRect();
      // Pages to the side (phones) and closed drawers aren't on screen.
      if (!onScreen(R) || R.left < -1 || R.right > innerWidth + 1 || !shown(root)) continue;
      const clip = new Map();
      /** Is this box something that scrolls sideways, or a narrower box that clips what's in it? */
      const clips = (a) => {
        if (clip.has(a)) return clip.get(a);
        const cs = getComputedStyle(a);
        const ox = cs.overflowX;
        const r = a.getBoundingClientRect();
        const v = ox === 'auto' || ox === 'scroll' || ((ox === 'hidden' || ox === 'clip') && r.right <= R.right + 1 && r.width < R.width - 8);
        clip.set(a, v);
        return v;
      };
      for (const e of root.querySelectorAll('*')) {
        const r = e.getBoundingClientRect();
        if (r.width < 2 || r.height < 2 || (r.right <= R.right + 1 && r.left >= R.left - 1)) continue;
        if (r.bottom < R.top || r.top > R.bottom) continue; // scrolled out of view
        let ok = false;
        for (let a = e.parentElement; a && a !== root; a = a.parentElement) if (clips(a)) { ok = true; break; }
        if (ok || !shown(e)) continue;
        out.push({ el: name(e), left: Math.round(r.left), right: Math.round(r.right), edge: [Math.round(R.left), Math.round(R.right)] });
        if (out.length > 6) return out;
      }
    }
    return out;
  }
  /** Runs of steps: their pieces in order, apart and inside the row. */
  function steps() {
    const out = [];
    let rows = 0;
    for (const row of document.querySelectorAll('.steps-row')) {
      const rr = row.getBoundingClientRect();
      if (!onScreen(rr) || rr.left < -1 || rr.right > innerWidth + 1 || !shown(row)) continue;
      rows++;
      const headEl = row.querySelector('.steps-head');
      const head = headEl.getBoundingClientRect();
      const hs = getComputedStyle(headEl);
      const inL = head.left + parseFloat(hs.paddingLeft) - 0.5;
      const inR = head.right - parseFloat(hs.paddingRight) + 0.5;
      const bad = (what, extra) => out.push({ row: row.querySelector('.steps-sum')?.textContent.trim(), what, ...extra });
      let x = inL;
      for (const sel of ['.steps-strip', '.steps-sum', '.badge']) {
        const e = headEl.querySelector(sel);
        if (!e || !e.getClientRects().length) continue;
        const b = e.getBoundingClientRect();
        if (b.left < x - 0.5 || b.right > inR) bad(sel + ' outside the head or over the piece before it', { l: Math.round(b.left), r: Math.round(b.right), head: [Math.round(inL), Math.round(inR)] });
        x = b.right;
      }
      // "N steps" whole: the count (the sum's text before its time) fits in the sum.
      const sum = headEl.querySelector('.steps-sum');
      const range = document.createRange();
      range.setStart(sum, 0);
      const time = sum.querySelector('.steps-time');
      if (time) range.setEndBefore(time);
      else range.setEnd(sum, sum.childNodes.length);
      const cr = range.getBoundingClientRect();
      if (cr.right > sum.getBoundingClientRect().right + 0.5 || cr.right > inR) bad('"N steps" cut off', { count: Math.round(cr.right), sum: Math.round(sum.getBoundingClientRect().right) });
      // The picture, Watch, Show tile: after the head, in order, apart, inside the row.
      let edge = head.right;
      for (const c of row.children) {
        if (c === headEl || !c.getClientRects().length || !shown(c)) continue;
        const b = c.getBoundingClientRect();
        if (b.left < edge - 0.5) bad(name(c) + ' overlaps what is before it', { l: Math.round(b.left), before: Math.round(edge) });
        edge = b.right;
      }
      if (edge > rr.right + 0.5) bad('past the row', { right: Math.round(edge), row: Math.round(rr.right) });
      // The detail shows readably or not at all (no sliver of its "…").
      const det = headEl.querySelector('.steps-detail');
      const dw = det && det.getClientRects().length ? det.getBoundingClientRect().width : 0;
      if (dw > 0.05 && dw < 20) bad('a sliver of the detail', { width: dw });
    }
    return { rows, bad: out };
  }
  /** Tile headers: buttons, the title and the address apart, inside the header. */
  function headers() {
    const out = [];
    let n = 0;
    for (const head of document.querySelectorAll('.pane-head')) {
      const H = head.getBoundingClientRect();
      if (!onScreen(H) || H.left < -1 || H.right > innerWidth + 1 || !shown(head)) continue;
      n++;
      // Every tile shows its close X; on phones (one tile on screen, batch 16) New sits there instead,
      // and Close is in the tile's sheet or ⋯ menu.
      const phoneNew = head.querySelector('.head-new');
      const x = phoneNew ?? head.querySelector('.head-close');
      if (!x || !x.getClientRects().length || Number(getComputedStyle(x).opacity) < 0.5) out.push({ what: phoneNew ? 'no New in sight' : 'no close X in sight', head: name(head) });
      const items = [...head.querySelectorAll('button, .head-title, .wv-addr, .badge, .status-pill')].filter((e) => e.getClientRects().length && shown(e));
      const box = (e) => e.getBoundingClientRect();
      for (const e of items) {
        const b = box(e);
        if (b.left < H.left - 1 || b.right > H.right + 1) out.push({ what: 'outside the header', el: name(e), l: Math.round(b.left), r: Math.round(b.right), head: [Math.round(H.left), Math.round(H.right)] });
      }
      for (let i = 0; i < items.length; i++)
        for (let j = i + 1; j < items.length; j++) {
          const a = items[i], b = items[j];
          if (a.contains(b) || b.contains(a)) continue;
          const A = box(a), B = box(b);
          const w = Math.min(A.right, B.right) - Math.max(A.left, B.left);
          const h = Math.min(A.bottom, B.bottom) - Math.max(A.top, B.top);
          if (w > 1 && h > 1) out.push({ what: 'overlap', a: name(a), b: name(b), w: Math.round(w) });
        }
    }
    return { n, bad: out };
  }
  return { overflow, steps, headers, sideways: () => document.documentElement.scrollWidth <= innerWidth + 1 };
})();`;

/** Every check on what is on screen now; `what` names the screen in the messages. */
async function look(p, what, opts = {}) {
  const t0 = Date.now();
  await p.eval(PAGE_LIB);
  const roots = opts.roots ?? "[data-pane-id]";
  const over = await p.eval(`__vis.overflow(${JSON.stringify(roots)})`);
  check(over.length === 0, `${what}: nothing reaches past the edge`, over);
  if (opts.steps) {
    const s = await p.eval(`__vis.steps()`);
    check(s.rows >= opts.steps && s.bad.length === 0, `${what}: runs of steps keep their pieces apart (${s.rows} on screen)`, s);
  }
  if (opts.headers !== false && roots === "[data-pane-id]") {
    const h = await p.eval(`__vis.headers()`);
    check(h.n >= 1 && h.bad.length === 0, `${what}: tile headers don't overlap (${h.n})`, h.bad);
  }
  check(await p.eval(`__vis.sideways()`), `${what}: the page doesn't scroll sideways`);
  if (process.env.VIS_TIME) console.log(`  (${what}: ${Date.now() - t0} ms)`);
}

async function shot(p, file) {
  await sleep(250);
  await p.shot(path.join(OUT, `${file}.png`));
}

/** Scroll a tile's transcript so `sel` is in view (centred, or `block`). */
const scrollTo = (p, pane, sel, block = "center") =>
  p.eval(`(() => { const e = document.querySelector('[data-pane-id="${pane}"] ${sel}'); if (!e) return false; e.scrollIntoView({ block: '${block}' }); return true; })()`);

run(async () => {
  // VIS_ONLY=folded-dark (or folded, dark…): only those runs.
  const only = process.env.VIS_ONLY?.split(",") ?? null;
  for (const mode of MODES) {
    for (const theme of ["dark", "light"]) {
      if (only && !only.some((o) => `${mode.name}-${theme}`.includes(o))) continue;
      const tag = `${mode.name}-${theme}`;
      console.log(`— ${mode.name} (${mode.w}×${mode.h}), ${theme}`);
      await mock("/__mock/reset");
      await mock("/__mock/mosaics/reset");
      const steps = await mock("/__mock/steps-chat");
      const p = await device();
      await p.viewport(mode.w, mode.h, mode.mobile, mode.dpr);
      await signIn(p, `Visual ${tag}`);
      await p.theme(theme);
      const want = mode.w < 640 ? "mobile" : mode.w < 1100 ? "dual" : "tile";
      check((await ws(p, `return m.workspace.mode()`)) === want, `${tag}: the app is in ${want} mode`);

      // ── a bot chat with long runs of browser steps (the cover-screen bug) ──
      const S = await ws(p, `return m.openSession('${steps.id}', { pane: m.workspace.focused() })`);
      if (want === "dual") await ws(p, `m.openSession('${SQLITE}', { newPane: true }); m.focusPane('${S}')`); // two tiles side by side
      await sleep(1400);
      await p.waitFor(`document.querySelectorAll('[data-pane-id="${S}"] .steps-row').length >= 2`, 8000).catch(() => {});
      await scrollTo(p, S, ".steps-row");
      await sleep(300);
      await look(p, `${tag} steps`, { steps: 2 });
      const watch = await p.eval(`(() => { const w = document.querySelector('[data-pane-id="${S}"] .steps-row .tool-watch'); if (!w) return null; const l = w.querySelector('.tool-show-label'); return { label: !!l && getComputedStyle(l).display !== 'none', name: w.getAttribute('aria-label') }; })()`);
      const rowW = await p.eval(`Math.round(document.querySelector('[data-pane-id="${S}"] .steps-row').getBoundingClientRect().width)`);
      check(!!watch && watch.name === "Watch" && watch.label === rowW > 420, `${tag}: Watch ${rowW > 420 ? "with its word" : "as an eye only"} (row ${rowW}px), named for screen readers`, { watch, rowW });
      await shot(p, `steps-${tag}`);
      // The second run opened: its steps as rows (the folded picture and Watch go away).
      await p.eval(`[...document.querySelectorAll('[data-pane-id="${S}"] .steps-head')].pop().click()`);
      await sleep(500);
      await scrollTo(p, S, ".steps.open .timeline", "start");
      await sleep(300);
      await look(p, `${tag} steps opened`);
      // Its steps' times line up, with a picture beside them or not (the row with Watch aside).
      const times = await p.eval(`[...document.querySelectorAll('[data-pane-id="${S}"] .steps.open .timeline .tool-row')].filter((r) => !r.querySelector('.tool-watch') && r.querySelector('.tool-time')).map((r) => Math.round(r.querySelector('.tool-time').getBoundingClientRect().right))`);
      check(times.length >= 20 && Math.max(...times) - Math.min(...times) <= 1, `${tag}: an opened run's step times line up (${times.length} rows)`, [...new Set(times)]);
      await shot(p, `steps-open-${tag}`);
      await p.eval(`document.querySelector('[data-pane-id="${S}"] .steps.open .steps-head')?.click()`);

      // ── a chat with tool cards, a diff, a plan ──
      const T = await ws(p, `return m.openSession('${SQLITE}', { pane: '${S}' })`);
      await sleep(900);
      await scrollTo(p, T, ".tool, .steps", "start");
      await sleep(300);
      await look(p, `${tag} tool cards`);
      await shot(p, `tools-${tag}`);

      // ── subagents and their reports ──
      await ws(p, `return m.openSession('${AUDIT}', { pane: '${S}' })`);
      await sleep(1000);
      await scrollTo(p, S, ".agents-bar, .report", "center");
      await sleep(300);
      await look(p, `${tag} subagents`);
      await shot(p, `subagents-${tag}`);

      // ── a running chat with queued messages and the composer ──
      const c = await wsClient(`visual ${tag}`);
      await ws(p, `return m.openSession('${HEALTHZ}', { pane: '${S}' })`);
      await sleep(600);
      const send = (text, delivery) => c.rpc("send", { session_id: HEALTHZ, content: [{ type: "text", text }], delivery, attachments: [] });
      await send("stream the answer slowly please", "auto");
      await sleep(700);
      await send("Also check the docs folder when you're done, and summarise what changed in the README over the last two releases.", "follow_up");
      await send("Then run the linter.", "follow_up");
      await send("skip the benchmarks, they take too long", "steer");
      await p.waitFor(`document.querySelectorAll('[data-pane-id="${S}"] .qcards .qcard').length >= 2`, 6000).catch(() => {});
      await p.eval(`document.querySelector('[data-pane-id="${S}"] .composer textarea')?.focus()`);
      // Once the cards have glided into place, every one is there, in full.
      await p.waitFor(`[...document.querySelectorAll('[data-pane-id="${S}"] .qcard')].every((e) => !e.getAnimations().length)`, 4000).catch(() => {});
      await sleep(300);
      const qc = await p.eval(`[...document.querySelectorAll('[data-pane-id="${S}"] .qcards .qcard')].map((e) => ({ op: getComputedStyle(e).opacity, h: Math.round(e.getBoundingClientRect().height), text: e.textContent.trim().slice(0, 40) }))`);
      check(qc.length === 3 && qc.every((c) => c.op === "1" && c.h > 20), `${tag}: the three queued messages show in full`, qc);
      await look(p, `${tag} queue`);
      await shot(p, `queue-${tag}`);
      await c.rpc("abort", { session_id: HEALTHZ });
      c.close();
      await sleep(400);
      await p.eval(`document.activeElement?.blur()`);

      // ── the side menu ──
      if (mode.mobile) {
        await ui(p, `u.setDrawerOpen(true)`);
        await sleep(600);
        await look(p, `${tag} side menu`, { roots: ".drawer-layer.open .sidebar" });
        await shot(p, `sidemenu-${tag}`);
        // its bots
        await p.eval(`document.querySelector('.drawer-layer.open .sb-tab[aria-controls]:nth-child(2)')?.click()`);
        await sleep(300);
        await look(p, `${tag} side menu, bots`, { roots: ".drawer-layer.open .sidebar" });
        await shot(p, `sidemenu-bots-${tag}`);
        await p.eval(`document.querySelector('.drawer-layer.open .sb-tab')?.click()`);
        await ui(p, `u.setDrawerOpen(false)`);
        await sleep(500);
      } else {
        await look(p, `${tag} side menu`, { roots: ".sidebar:not(.drawer)" });
      }

      // ── a web view and a note beside the chat ──
      const board = await mock("/__mock/board", { session_id: HEALTHZ, kind: "url", ask: false });
      await sleep(600);
      const W = await ws(p, `return m.openBoard('${board.id}')`);
      await sleep(1500);
      const probe = await wsClient(`visual note ${tag}`);
      const note = (await probe.rpc("save_note", { title: "Packing list for the trip to Lisbon", content: "# Packing\n\n- passport\n- charger\n- **sun cream**\n\n| Day | Plan |\n|---|---|\n| Fri | Alfama |\n| Sat | Belém |" })).result;
      probe.close();
      await p.eval(`__mod('lib/notes.ts').then(m => m.openNote('${note.id}', { newPane: true }))`);
      await sleep(1500);
      if (mode.mobile) {
        // One tile at a time (folded) or two (unfolded): look at the web view, then the note.
        await ws(p, `m.focusPane('${W}')`);
        await sleep(900);
        await look(p, `${tag} web view`);
        await shot(p, `webview-${tag}`);
        const N = await ws(p, `return m.paneShowingNote('${note.id}')`);
        await ws(p, `m.focusPane('${N}')`);
        await sleep(900);
        await look(p, `${tag} note`);
        await shot(p, `note-${tag}`);
      } else {
        // Desktop: the chat, the web view and the note as three tiles, then the steps chat among them.
        await look(p, `${tag} three tiles`);
        await shot(p, `tiles-${tag}`);
        await ws(p, `m.openSession('${steps.id}', { pane: m.paneShowing('${HEALTHZ}') })`);
        await sleep(1000);
        await scrollTo(p, await ws(p, `return m.paneShowing('${steps.id}')`), ".steps-row");
        await sleep(300);
        await look(p, `${tag} three tiles, steps`, { steps: 1 });
        await shot(p, `tiles-steps-${tag}`);
      }

      // ── Settings: the list (phones) and a few sections ──
      await ui(p, `u.openSettings()`);
      const opened = await p.waitFor(`document.querySelector('.settings-layout')`, 20000).catch(() => false);
      const why = opened ? null : { ...(await p.eval(`__mod('lib/ui.ts').then(u => ({ tab: u.settingsTab(), modal: document.querySelector('.modal, [role=dialog]')?.className ?? null }))`)), errors: p.errors.slice(0, 3) };
      if (!check(opened, `${tag}: Settings opens`, why)) {
        await p.close();
        continue;
      }
      await sleep(500);
      await look(p, `${tag} settings`, { roots: ".settings-layout" });
      await shot(p, `settings-${tag}`);
      for (const id of SECTIONS) {
        await ui(p, `u.openSettings('${id}')`);
        await sleep(400);
        // Sections that load something show a spinner first.
        await p.waitFor(`!document.querySelector('.settings-layout .settings-main .tess')`, 5000).catch(() => {});
        await sleep(300);
        await look(p, `${tag} settings ${id}`, { roots: ".settings-layout" });
        await shot(p, `settings-${id}-${tag}`);
      }
      await p.eval(`history.back()`);
      await sleep(300);
      check(p.errors.length === 0, `${tag}: no script errors`, p.errors.slice(0, 3));
      await p.close();
    }
  }
  console.log(`pictures: ${OUT}`);
  finish();
});
