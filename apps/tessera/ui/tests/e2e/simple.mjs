// Batch 16, "simple on the surface": one New (top right, on every device), one search, the sidebar
// tabs in use only, the empty mosaic with one New chat, three suggestions on a new chat, and a
// count of the controls a newcomer sees first (desktop and phone), so the first screen stays lean.
// Desktop, the folded phone (344 px) and the unfolded Fold, light and dark.
import { SHOTS, check, device, finish, menuLabels, menuPick, mock, run, signIn, sleep } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const store = (p, js) => p.eval(`__mod('lib/store.ts').then(m => { ${js} })`);
/** What a newcomer has: no chats, bots or scheduled tasks (cleared on this page only). */
const emptyAccount = (p) => store(p, `for (const k of Object.keys(m.state.sessions)) m.setState('sessions', k, undefined); m.setState('bots', []); m.setState('jobs', []); m.setState('archived', []);`);
/** Visible controls (buttons, links, fields, tabs, switches) inside `root`. */
const controls = (p, root = "body") =>
  p.eval(`(() => {
    const root = document.querySelector(${JSON.stringify(root)});
    if (!root) return null;
    const els = [...root.querySelectorAll('button, a[href], input:not([type=hidden]), select, textarea, [role=tab], [role=radio], [role=switch], [role=menuitem], [role=option]')]
      .filter((e) => e.getClientRects().length && getComputedStyle(e).visibility !== 'hidden')
      .filter((e) => { const b = e.getBoundingClientRect(); return b.bottom > 0 && b.right > 0 && b.top < innerHeight && b.left < innerWidth; });
    return [...new Set(els)].length;
  })()`);
const shown = (p, sel) => p.eval(`[...document.querySelectorAll(${JSON.stringify(sel)})].some((e) => { const r = e.getBoundingClientRect(); return r.width && r.left >= 0 && r.right <= innerWidth + 1 && getComputedStyle(e).visibility !== 'hidden'; })`);
const tabs = (p, side = ".sidebar") => p.eval(`[...document.querySelectorAll('${side} .sb-tab')].map((t) => t.getAttribute('aria-label').split(' ·')[0])`);
const vis = (p, sel) =>
  p.eval(`(() => { const e = [...document.querySelectorAll(${JSON.stringify(sel)})].find((e) => { const r = e.getBoundingClientRect(); return r.width && r.left >= 0 && r.right <= innerWidth + 1; }); if (!e) return null; const r = e.getBoundingClientRect(); return { x: r.x, y: r.y, r: r.right, b: r.bottom, w: r.width, h: r.height, cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);
const counts = {};

run(async () => {
  for (const theme of ["dark", "light"]) {
    // ── a computer ──────────────────────────────────────────────────────────
    console.log(`— desktop (${theme})`);
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, `Simple desktop ${theme}`);
    await p.theme(theme);
    await sleep(600);

    // A returning user (the mock's chats, bots, channels and scheduled tasks): every tab in use.
    check(JSON.stringify(await tabs(p)) === JSON.stringify(["Chats", "Bots", "Channels", "Scheduled", "Notes"]), "with bots, channels and scheduled tasks, all five tabs show", await tabs(p));
    const archived = await p.waitFor(`document.querySelector('.sidebar .sb-archived')`, 8000).then(
      () => p.eval(`document.querySelector('.sidebar .sb-archived').textContent`),
      () => null,
    );
    check(/^Archived chats · \d+$/.test(archived ?? ""), "archived chats are one line under the list, with their number", archived);
    check(!(await p.eval(`!!document.querySelector('.sidebar .sb-panel-link, .sidebar .sb-panel-add')`)), "…and the list's head has no Archived link or + New");
    counts[`desktop returning (${theme})`] = await controls(p);
    await p.shot(`${SHOTS}/simple-returning-desktop-${theme}.png`);

    // A newcomer.
    await emptyAccount(p);
    await sleep(600);
    check(JSON.stringify(await tabs(p)) === JSON.stringify(["Chats", "Notes"]), "a newcomer's sidebar: Chats and Notes only", await tabs(p));
    const first = await p.eval(`document.querySelector('.sidebar .sb-first')?.textContent ?? ''`);
    check(/No chats yet/.test(first) && /New chat at the top right/.test(first), "an empty chat list says where New is, without a button of its own", first);
    check(!(await p.eval(`!!document.querySelector('.sidebar .sb-archived')`)), "no Archived line when nothing is archived");
    // (A new chat tile's title reads "New chat" too: a name, not a button you can press.)
    const newChats = await p.eval(`[...document.querySelectorAll('button')].filter((b) => b.getClientRects().length && !b.disabled && /new chat/i.test(b.getAttribute('aria-label') ?? b.textContent)).map((b) => b.className + ': ' + (b.getAttribute('aria-label') ?? b.textContent.trim()).slice(0, 40))`);
    check(newChats.length === 1, "one New chat button on the first screen", newChats);
    check(!(await p.eval(`!!document.querySelector('.sb-new, .sb-private, .tb-search, .topbar [aria-label="Keyboard shortcuts"], [aria-label^="New mosaic"]')`)) && await shown(p, '.topbar .ws-pill.active') && await shown(p, '.topbar [aria-label="All mosaics"]'), "no duplicate New/Private/New mosaic icons, search or shortcuts; the requested mosaic tab and All mosaics are reachable");
    check((await p.eval(`document.querySelectorAll('input[type=search], .sb-search input').length`)) === 1, "one search box (the sidebar's)");
    const top = await p.eval(`[...document.querySelectorAll('.topbar button')].filter((b) => b.getClientRects().length).map((b) => b.getAttribute('aria-label'))`);
    check(JSON.stringify(top) === JSON.stringify(["Mosaic 1: 1 tile", "All mosaics", "New chat", "More new things"]), "the top bar holds the requested mosaic tab, All mosaics, New chat and its menu", top);
    const nb = await p.box(".topbar .tb-new");
    check(!!nb && 1440 - (nb.x + nb.w) < 16, "…at the top right", nb);
    const tips = await p.eval(`[...document.querySelectorAll('.pane .hero-tips .suggestion')].map((b) => b.textContent.trim())`);
    check(tips.length === 3 && !(await p.eval(`!!document.querySelector('.try-tips, .try-card')`)), "a new chat offers three one-line suggestions, no paragraph", tips);
    const n = await controls(p);
    counts[`desktop newcomer (${theme})`] = n;
    const added = await p.eval(`[...document.querySelectorAll('.topbar .ws-pill, .topbar .tb-mosaics, .composer .voice-call')].filter(e => e.getClientRects().length).map(e => e.className)`);
    check(added.length === 3 && n - added.length <= 20, `a newcomer's original controls stay within 20, plus exactly the requested mosaic tab, All mosaics and voice conversation`, { total: n, added });
    await p.shot(`${SHOTS}/simple-first-desktop-${theme}.png`);

    // The New menu: four everyday things, the rest behind More (in place).
    await p.click(".topbar .tb-new-more");
    await sleep(300);
    check(JSON.stringify(await menuLabels(p)) === JSON.stringify(["Chat", "Private chat", "Note", "Web view", "More"]), "New: Chat, Private chat, Note, Web view, More", await menuLabels(p));
    const hint = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-item')][0]?.querySelector('.menu-hint')?.textContent ?? null`);
    check(/N$/.test(hint ?? ""), "…Chat shows its shortcut", hint);
    const focusedBefore = await p.eval(`document.activeElement?.textContent ?? ''`);
    await p.eval(`document.querySelector('.menu-pop .menu-more').focus()`);
    await p.key("Enter", "Enter", 0, 13);
    await sleep(300);
    check(JSON.stringify(await menuLabels(p)) === JSON.stringify(["Chat", "Private chat", "Note", "Web view", "Bot", "Channel", "Scheduled task", "Terminal", "Mosaic"]), "More (by keyboard) opens in place: Bot, Channel, Scheduled task, Terminal, Mosaic", await menuLabels(p));
    check(/Bot/.test(await p.eval(`document.activeElement?.textContent ?? ''`)), "…and the keyboard goes on from its first row", { before: focusedBefore, now: await p.eval(`document.activeElement?.textContent ?? ''`) });
    check(await p.eval(`!document.querySelector('.menu-pop')?.classList.contains('as-sheet')`), "…in the same compact menu");
    await p.shot(`${SHOTS}/simple-new-menu-desktop-${theme}.png`);
    // Mosaic: a new screen of tiles; its pills and All mosaics appear once there are two.
    await menuPick(p, "Mosaic");
    await sleep(600);
    check((await ws(p, `return m.workspace.active()`)) === 1, "New → More → Mosaic opens a second mosaic");
    const launcher = await p.eval(`(() => { const l = document.querySelector('.launcher'); if (!l) return null; return { cards: [...l.querySelectorAll('.launch-card')].map((c) => c.querySelector('.launch-title').textContent), text: l.querySelector('.launcher-head p:not(.inscription)')?.textContent ?? '', tip: !!l.querySelector('.launcher-tip') }; })()`);
    check(JSON.stringify(launcher?.cards) === JSON.stringify(["New chat"]) && !launcher.tip, "an empty mosaic offers one New chat (no private, channel or bot cards, no shortcut tip)", launcher);
    check(/A mosaic is a screen of tiles/.test(launcher?.text ?? ""), "…and says in one line what a mosaic is", launcher?.text);
    check((await p.eval(`document.querySelectorAll('.topbar .ws-pill').length`)) === 2 && (await shown(p, '.topbar [aria-label="All mosaics"]')), "with two mosaics: their pills and All mosaics in the top bar");
    await p.shot(`${SHOTS}/simple-launcher-desktop-${theme}.png`);
    await ws(p, `m.switchWorkspace(0)`);
    await sleep(300);

    // A tab appears with its first item, and goes when the last one does (back to Chats).
    await store(p, `m.setState('bots', [{ id: 'bot_simple', name: 'Pip', handle: 'pip', role: 'Helps', avatar: '', color: '#7D9BE0', permission: 'default' }])`);
    await sleep(300);
    check(JSON.stringify(await tabs(p)) === JSON.stringify(["Chats", "Bots", "Notes"]), "the first bot brings the Bots tab", await tabs(p));
    await p.click(`.sidebar:not(.drawer) .sb-tab[id$="-bots"]`);
    await sleep(300);
    check(!(await p.eval(`!!document.querySelector('.sidebar .sb-panel-add')`)), "…its list has no + New (New → More → Bot)");
    await store(p, `m.setState('bots', [])`);
    await sleep(300);
    const on = await p.eval(`document.querySelector('.sidebar .sb-tab[aria-selected="true"]')?.id.split('-').pop()`);
    check(JSON.stringify(await tabs(p)) === JSON.stringify(["Chats", "Notes"]) && on === "chats", "…and when the last one goes, so does the tab (back to Chats)", { tabs: await tabs(p), on });

    // The collapsed sidebar: search, bots and Settings (New is in the top bar).
    await p.eval(`__mod('lib/prefs.ts').then(m => m.setPrefs('sidebarCollapsed', true))`);
    await sleep(400);
    const rail = await p.eval(`[...document.querySelectorAll('.rail button')].map((b) => b.getAttribute('aria-label'))`);
    check(rail.includes("Search") && rail.includes("Settings") && !rail.some((l) => /New/.test(l ?? "")), "the collapsed sidebar: Search and Settings, no New buttons", rail);
    await p.eval(`__mod('lib/prefs.ts').then(m => m.setPrefs('sidebarCollapsed', false))`);
    check(!p.errors.length, "no page errors (desktop)", p.errors);
    await p.close();

    // ── the folded phone (cover screen) ─────────────────────────────────────
    console.log(`— phone (${theme})`);
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const q = await device();
    await q.viewport(344, 882, true, 2.625);
    await signIn(q, `Simple phone ${theme}`);
    await q.theme(theme);
    await sleep(600);
    await emptyAccount(q);
    await sleep(600);
    const head = await q.eval(`(() => { const h = [...document.querySelectorAll('.pane-head')].find((e) => { const r = e.getBoundingClientRect(); return r.width && r.left >= 0 && r.right <= innerWidth + 1; }); if (!h) return null; return [...h.querySelectorAll('button')].filter((b) => b.getClientRects().length).map((b) => b.getAttribute('aria-label') ?? b.className); })()`);
    check(head?.length === 4 && head[0] === "Open sidebar" && /options/.test(head[1]) && head[2] === "Close tile" && head[3] === "New", "the phone's header: ≡ · title ▾ · × · New (no overview with one tile)", head);
    const qn = await vis(q, ".pane-head .head-new");
    check(!!qn && 344 - qn.r < 16, "…New at the top right", qn);
    counts[`phone newcomer (${theme})`] = await controls(q);
    const voice = await q.eval(`document.querySelectorAll('.composer .voice-call').length`);
    check(voice === 1 && counts[`phone newcomer (${theme})`] - voice <= 11, "a newcomer's original phone controls stay within 11, plus exactly one voice conversation action", counts[`phone newcomer (${theme})`]);
    await q.shot(`${SHOTS}/simple-first-phone-${theme}.png`);
    await q.tap(qn.cx, qn.cy);
    await sleep(400);
    check(JSON.stringify(await menuLabels(q)) === JSON.stringify(["Chat", "Private chat", "Note", "Web view", "More"]), "the phone's New menu is the computer's", await menuLabels(q));
    const pop = await vis(q, ".menu-pop");
    check(!!pop && pop.r <= 344 && pop.h < 400 && (await q.eval(`!document.querySelector('.menu-pop').classList.contains('as-sheet')`)), "…a compact menu under it, not a big sheet", pop);
    await q.shot(`${SHOTS}/simple-new-menu-phone-${theme}.png`);
    await q.eval(`history.back()`);
    await sleep(400);
    // The side menu: no New or Private chat of its own; the mosaics row only with two mosaics.
    const m = await vis(q, ".pane-head .head-menu");
    await q.tap(m.cx, m.cy);
    await sleep(500);
    check(!(await q.eval(`!!document.querySelector('.drawer .sb-new, .drawer .sb-private, .drawer .sb-ws')`)), "the side menu: no New, no Private chat, no mosaics row with one mosaic");
    check(JSON.stringify(await tabs(q, ".drawer")) === JSON.stringify(["Chats", "Bots", "Notes"]), "…tabs Chats, the requested always-reachable Bots, and Notes", await tabs(q, ".drawer"));
    counts[`phone side menu (${theme})`] = await controls(q, ".drawer-panel");
    await q.shot(`${SHOTS}/simple-drawer-phone-${theme}.png`);
    await q.eval(`history.back()`);
    await sleep(400);
    // An empty mosaic on a phone: ≡ and New at the top too.
    await ws(q, `m.newWorkspace()`);
    await sleep(700);
    check((await shown(q, ".launcher-menu")) && (await shown(q, ".launcher-new .head-new")), "an empty mosaic on a phone has ≡ and New at the top");
    await q.shot(`${SHOTS}/simple-launcher-phone-${theme}.png`);
    check(!q.errors.length, "no page errors (phone)", q.errors);
    await q.close();

    // ── the unfolded Fold ─────────────────────────────────────────────────
    console.log(`— fold (${theme})`);
    await mock("/__mock/mosaics/reset");
    const f = await device();
    await f.viewport(673, 841, true, 2.625);
    await signIn(f, `Simple fold ${theme}`);
    await f.theme(theme);
    await sleep(800);
    check((await ws(f, `return m.workspace.mode()`)) === "dual" || (await ws(f, `return m.workspace.mode()`)) === "tile", "the unfolded Fold shows tiles beside each other");
    check((await shown(f, ".topbar .tb-new")) && !(await shown(f, ".pane-head .head-new")), "the Fold has New in its top bar (not in the tiles)");
    check(!(await f.eval(`!!document.querySelector('.tb-search')`)), "…and no top bar search");
    await f.shot(`${SHOTS}/simple-fold-${theme}.png`);
    check(!f.errors.length, "no page errors (Fold)", f.errors);
    await f.close();
  }
  console.log("controls on screen:", JSON.stringify(counts));
  finish();
});
