// APP-PANELS: sidebar tabs, the Notes tab, Settings → Usage, answering board
// snapshot requests (web UI: an error, never a picture), the YOLO icon.
import { MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const tabSel = (id) => `.sidebar:not(.drawer) .sb-tab[id$="-${id}"]`;
const onTab = (p) => p.eval(`document.querySelector('.sidebar .sb-tab[aria-selected="true"]')?.id.split('-').pop()`);

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const ws = await wsClient("panels script");

  // ── sidebar tabs ──
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Panels desktop");
  await sleep(600);
  check((await onTab(p)) === "chats", "the sidebar opens on Chats");
  const tabs = await p.eval(`[...document.querySelectorAll('.sidebar .sb-tab')].map(t => [t.getAttribute('role'), t.getAttribute('aria-label').split(' ·')[0]])`);
  check(JSON.stringify(tabs.map((t) => t[1])) === JSON.stringify(["Chats", "Bots", "Channels", "Scheduled", "Notes"]), "tabs: Chats · Bots · Channels · Scheduled · Notes", tabs);
  check(tabs.every((t) => t[0] === "tab") && (await p.eval(`document.querySelector('.sidebar .sb-tabs').getAttribute('role')`)) === "tablist", "the tabs are an ARIA tablist");
  const rows = await p.eval(`document.querySelectorAll('.sidebar .sb-scroll .srow').length`);
  const total = await p.eval(`__mod('lib/store.ts').then(m => Object.values(m.state.sessions).filter(s => s.info.kind === 'chat' && !s.info.archived && !s.info.private).length)`);
  check(rows === total && rows > 12, "Chats lists every chat (no cap of 12)", { rows, total });
  const geo = await p.eval(`(() => { const s = document.querySelector('.sidebar .sb-scroll').getBoundingClientRect(); const f = document.querySelector('.sidebar .sb-foot').getBoundingClientRect(); const t = document.querySelector('.sidebar .sb-tabs').getBoundingClientRect(); return { top: s.top, bottom: s.bottom, foot: f.top, tabs: t.bottom }; })()`);
  check(Math.abs(geo.bottom - geo.foot) < 2 && geo.top - geo.tabs <= 8, "the section fills the sidebar from the tabs to the footer", geo);
  check(!(await p.eval(`!!document.querySelector('.sidebar .sb-scroll .brow')`)), "bots are not under the chats");
  await p.click(tabSel("bots"));
  await sleep(200);
  check((await onTab(p)) === "bots" && (await p.eval(`document.querySelectorAll('.sidebar .sb-scroll .brow').length`)) === 3, "Bots shows the bots");
  check(await p.eval(`(() => { const r = [...document.querySelectorAll('.sidebar .brow')].find(r => r.textContent.includes('Forge')); return !!r.querySelector('.brow-yolo') && r.querySelector('.brow-yolo').innerHTML.includes('M3.5 3.5l17 17'); })()`), "a YOLO bot shows the struck-through shield, not a bolt");
  await p.shot(`${SHOTS}/panels-bots-desktop.png`, { x: 0, y: 0, width: 270, height: 900 });
  // keyboard
  await p.eval(`document.getElementById(document.querySelector('.sidebar .sb-tab[aria-selected="true"]').id).focus()`);
  await p.key("ArrowRight", "ArrowRight", 0, 39);
  check((await onTab(p)) === "channels" && (await p.eval(`document.activeElement.id.endsWith('channels')`)), "→ moves to Channels and focuses it");
  await p.key("End", "End", 0, 35);
  check((await onTab(p)) === "notes", "End: Notes");
  await p.key("ArrowRight", "ArrowRight", 0, 39);
  check((await onTab(p)) === "chats", "→ from the last tab wraps to Chats");
  await p.key("ArrowLeft", "ArrowLeft", 0, 37);
  await p.key("Home", "Home", 0, 36);
  check((await onTab(p)) === "chats", "Home: Chats");
  const tabStops = await p.eval(`[...document.querySelectorAll('.sidebar .sb-tab')].filter(t => t.tabIndex === 0).length`);
  check(tabStops === 1, "one tab stop for the whole row (roving tabindex)", tabStops);
  await p.click(tabSel("jobs"));
  await p.reload(1500);
  await p.waitFor(`document.querySelector('.sidebar .sb-tab')`);
  check((await onTab(p)) === "jobs", "the tab is remembered on this device after a reload");
  check(await p.eval(`document.querySelectorAll('.sidebar .sb-scroll .srow').length === 4 && document.querySelector('.sidebar .sb-panel-count').textContent.includes('next')`), "Scheduled lists the jobs and when the next one runs");

  // badges: due / failed job, needs-you, working
  const dot = await p.eval(`(() => { const d = document.querySelector('${tabSel("jobs")} .sb-tab-dot'); return d ? d.className : null; })()`);
  check(!!dot, "Scheduled shows a dot for a job due soon or a failed run", dot);
  await ws.rpc("send", { session_id: "ses_2b7e91c04d5a", content: [{ type: "text", text: "delete the build cache please" }], delivery: "auto" });
  await p.waitFor(`document.querySelector('${tabSel("chats")} .sb-tab-badge.need')`, 15000).catch(() => {});
  const badge = await p.eval(`document.querySelector('${tabSel("chats")} .sb-tab-badge.need')?.textContent ?? null`);
  check(badge === "1", "a chat waiting for approval puts a 1 on the Chats tab", badge);
  check(/1 needs you/.test(await p.eval(`document.querySelector('${tabSel("chats")}').getAttribute('aria-label')`)), "…and says so to screen readers");
  await p.shot(`${SHOTS}/panels-badges-desktop.png`, { x: 0, y: 0, width: 270, height: 260 });
  await ws.rpc("abort", { session_id: "ses_2b7e91c04d5a" });

  // search works across the tabs
  await p.eval(`document.querySelector('.sidebar .sb-search input').focus()`);
  await p.type("nova");
  await sleep(700);
  const groups = await p.eval(`[...document.querySelectorAll('.sidebar .sb-group')].map(g => g.dataset.group)`);
  check(groups.includes("bots"), "searching from Scheduled still finds a bot", groups);
  const counts = await p.eval(`[...document.querySelectorAll('.sidebar .sb-tab .sb-tab-n')].map(n => n.textContent)`);
  check(counts.length === 5 && counts[1] !== "0", "each tab shows how many matches it has", counts);
  await p.click(tabSel("bots"));
  await sleep(500);
  check((await onTab(p)) === "bots", "clicking a tab while searching selects it (and jumps to its results)");
  await p.shot(`${SHOTS}/panels-search-desktop.png`, { x: 0, y: 0, width: 270, height: 900 });
  await p.eval(`document.querySelector('.sidebar .sb-search input').focus()`);
  await p.key("Escape", "Escape", 0, 27);
  await sleep(200);
  check((await p.eval(`!document.querySelector('.sidebar .sb-group')`)) && (await onTab(p)) === "bots", "clearing the search goes back to the tab");

  // ── notes ──
  await p.click(tabSel("notes"));
  await sleep(200);
  check(
    await p.eval(`/New → Note at the top right/.test(document.querySelector('.sidebar .sb-first')?.textContent ?? '') && document.querySelector('.sidebar .sb-panel-count').textContent === '0 notes' && !document.querySelector('.sidebar .sb-panel-add, .sidebar .sb-add-row')`),
    "an empty library says where New is (no second New button)",
  );
  await p.click(".topbar .tb-new-more");
  await sleep(250);
  await p.clickText(".menu-pop .menu-item", "Note");
  await sleep(1200);
  const notePane = await p.eval(`__mod('lib/workspace.ts').then(m => { const f = m.workspace.focused(); return JSON.parse(JSON.stringify(m.workspace.panes[f]?.content ?? null)); })`);
  check(notePane?.type === "note", "New opens the new note in a tile", notePane);
  const mine = await p.eval(`[...document.querySelectorAll('.sidebar .nrow')].map(r => [r.querySelector('.nrow-title').textContent, r.querySelector('.nrow-by-label').textContent, r.classList.contains('active')])`);
  check(mine.length === 1 && mine[0][1] === "You" && mine[0][2], "it is listed as yours, marked as the focused tile", mine);
  // an agent writes one: live, with the chat that wrote it
  await p.click(tabSel("chats"));
  const agentNote = (await ws.rpc("mock_agent_note", { session_id: "ses_91d0c3e7a8b2", title: "RFC 9110 caching: key points", content: "## Freshness\n\nmax-age wins over Expires." })).result;
  await sleep(400);
  const fresh = await p.eval(`document.querySelector('${tabSel("notes")} .sb-tab-badge.fresh')?.textContent ?? null`);
  check(fresh === "1", "a note from an agent puts a 1 on the Notes tab", fresh);
  await p.click(tabSel("notes"));
  await sleep(300);
  const listed = await p.eval(`[...document.querySelectorAll('.sidebar .nrow')].map(r => ({ t: r.querySelector('.nrow-title').textContent, by: r.querySelector('.nrow-by-label').textContent, prev: r.querySelector('.nrow-preview')?.textContent ?? '', time: r.querySelector('.nrow-time').textContent }))`);
  check(listed[0]?.t === "RFC 9110 caching: key points" && listed[0].by === "Summarize RFC 9110 caching" && listed[0].prev.includes("Expires"), "the agent's note comes first, with its preview and the chat that wrote it", listed);
  check(listed[0]?.time === "just now", "…and when it changed", listed[0]?.time);
  check(!(await p.eval(`!!document.querySelector('${tabSel("notes")} .sb-tab-badge.fresh')`)), "looking at the list clears the badge");
  // filter
  await p.clickText(".sidebar .nl-chip", "Yours");
  check((await p.eval(`document.querySelectorAll('.sidebar .nrow').length`)) === 1, "Yours: only your notes");
  await p.clickText(".sidebar .nl-chip", "From chats");
  check((await p.eval(`[...document.querySelectorAll('.sidebar .nrow .nrow-title')].map(e => e.textContent).join()`)) === "RFC 9110 caching: key points", "From chats: only agents' notes");
  await p.clickText(".sidebar .nl-chip", "All");
  await p.shot(`${SHOTS}/panels-notes-desktop.png`, { x: 0, y: 0, width: 270, height: 900 });
  await p.theme("light");
  await sleep(200);
  await p.shot(`${SHOTS}/panels-notes-desktop-light.png`, { x: 0, y: 0, width: 270, height: 900 });
  await p.theme("dark");
  // search finds notes
  await p.eval(`document.querySelector('.sidebar .sb-search input').focus()`);
  await p.type("expires");
  await sleep(500);
  check(await p.eval(`!!document.querySelector('.sidebar .sb-group[data-group="notes"] .nrow')`), "search finds a note by its text");
  await p.key("Escape", "Escape", 0, 27);
  // open from the list
  await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_2b7e91c04d5a', { pane: m.workspace.focused() }))`);
  await sleep(300);
  await p.clickText(".sidebar .nrow .nrow-title", "RFC 9110");
  await sleep(600);
  const opened = await p.eval(`__mod('lib/workspace.ts').then(m => { const f = m.workspace.focused(); return JSON.parse(JSON.stringify(m.workspace.panes[f]?.content ?? null)); })`);
  check(opened?.type === "note" && opened.noteId === agentNote.id, "clicking a note opens it as a tile", opened);
  // rename (menu → prompt)
  await p.eval(`(() => { const r = [...document.querySelectorAll('.sidebar .nrow')].find(r => r.textContent.includes('RFC 9110')); r.querySelector('.srow-more').click(); })()`);
  await sleep(250);
  await p.clickText(".menu [role=menuitem], .menu button", "Rename");
  await sleep(250);
  await p.eval(`(() => { const i = document.querySelector('.modal input'); i.select(); })()`);
  await p.type("Caching cheat sheet");
  await p.key("Enter", "Enter", 0, 13);
  await sleep(600);
  const renamed = (await ws.rpc("get_note", { id: agentNote.id })).result;
  check(renamed.title === "Caching cheat sheet" && renamed.content.includes("max-age") && renamed.version === 2, "Rename keeps the content and bumps the version", renamed);
  check(await p.eval(`[...document.querySelectorAll('.sidebar .nrow-title')].some(e => e.textContent === 'Caching cheat sheet')`), "the list shows the new title");
  // someone else renames it at the same time: a conflict is retried on the latest version
  await ws.rpc("save_note", { id: agentNote.id, title: "Caching cheat sheet", content: renamed.content + "\n\nMore.", base_version: 2 });
  // delete (menu → confirm)
  await p.eval(`(() => { const r = [...document.querySelectorAll('.sidebar .nrow')].find(r => r.textContent.includes('Caching')); r.querySelector('.srow-more').click(); })()`);
  await sleep(250);
  await p.clickText(".menu [role=menuitem], .menu button", "Delete");
  await sleep(250);
  const confirmText = await p.eval(`document.querySelector('.modal')?.textContent ?? ''`);
  check(/Delete note\?/.test(confirmText) && /for good/.test(confirmText), "Delete asks first", confirmText.slice(0, 120));
  await p.clickText(".modal button", "Delete");
  await sleep(600);
  check((await ws.rpc("get_note", { id: agentNote.id })).error?.code === "not_found", "…and deletes it on the server");
  check(!(await p.eval(`[...document.querySelectorAll('.sidebar .nrow-title')].some(e => e.textContent.includes('Caching'))`)), "…and from the list");
  // removed elsewhere: live
  const other = (await ws.rpc("save_note", { title: "Shopping list", content: "- milk" })).result;
  await sleep(300);
  check(await p.eval(`[...document.querySelectorAll('.sidebar .nrow-title')].some(e => e.textContent === 'Shopping list')`), "a note made on another device shows up live");
  await ws.rpc("delete_note", { id: other.id });
  await sleep(300);
  check(!(await p.eval(`[...document.querySelectorAll('.sidebar .nrow-title')].some(e => e.textContent === 'Shopping list')`)), "…and goes when it is deleted there");

  // ── usage ──
  await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings('usage'))`);
  await p.waitFor(`document.querySelector('.usage .kpi')`, 8000);
  await sleep(300);
  const want = (await ws.rpc("get_usage", { from: await p.eval(`(() => { const d = new Date(); d.setHours(0,0,0,0); d.setDate(d.getDate() - 29); return d.getTime(); })()`) })).result;
  const kpis = await p.eval(`[...document.querySelectorAll('.usage .kpi')].map(k => [k.querySelector('.kpi-label').textContent, k.querySelector('.kpi-value').textContent])`);
  check(kpis[0][1] === want.total.requests.toLocaleString("en-US") || kpis[0][1].replace(/\D/g, "") === String(want.total.requests), "30 days: the request total matches the server", { kpis, want: want.total.requests });
  // APP-SETTINGS: the cost KPI is "At API prices" (an estimate; the toggle offers "Subscription").
  const costKpi = kpis[kpis.length - 1];
  check(costKpi[0] === "At API prices" && costKpi[1].startsWith("$"), "the cost says which prices it uses", costKpi);
  const bars = await p.eval(`document.querySelectorAll('.usage .usage-hit').length`);
  check(bars === 30, "30 days: a bar per day", bars);
  const legend = await p.eval(`[...document.querySelectorAll('.usage-legend li')].map(l => l.textContent)`);
  check(legend.length === 6, "a legend names each model (with ChatGPT and an OpenRouter model, added by APP-SETTINGS)", legend);
  const modelRows = await p.eval(`document.querySelectorAll('.usage-table tbody tr').length`);
  check(modelRows >= 4, "a row per model", modelRows);
  const unknownCost = await p.eval(`[...document.querySelectorAll('.usage-table tbody tr')].find(r => r.textContent.includes('Mistral'))?.lastElementChild.textContent`);
  check(unknownCost === "—", "a model without a known price shows a dash, not $0", unknownCost);
  const localCost = await p.eval(`[...document.querySelectorAll('.usage-table tbody tr')].find(r => r.textContent.includes('Qwen'))?.lastElementChild.textContent`);
  check(localCost === "Free", "a local model nobody priced is Free (APP-SETTINGS)", localCost);
  const kinds = await p.eval(`[...document.querySelectorAll('.usage-kind-name')].map(e => e.textContent)`);
  check(["Chats", "Helpers", "Memory & learning", "Chat titles", "Summaries", "Scheduled tasks"].every((k) => kinds.includes(k)), "a line for each kind of request", kinds);
  check(/estimates/.test(await p.eval(`document.querySelector('.usage-note').textContent`)), "plain words: costs are estimates");
  // hover + keyboard
  const hit = await p.box(".usage .usage-hit:nth-of-type(20)");
  await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: hit.cx, y: hit.y + hit.h - 10 });
  await sleep(200);
  check(await p.eval(`!!document.querySelector('.usage-tip') && document.querySelectorAll('.usage-col.dim').length === 29`), "hovering a day shows its numbers and quiets the others");
  await p.shot(`${SHOTS}/panels-usage-hover-desktop.png`);
  await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: 5, y: 5 });
  await p.eval(`document.querySelector('.usage-plot svg').focus()`);
  await p.key("ArrowLeft", "ArrowLeft", 0, 37);
  const tipDay = await p.eval(`document.querySelector('.usage-detail-day')?.textContent`);
  check(!!tipDay, "the chart can be read with the arrow keys", tipDay);
  await p.key("Escape", "Escape", 0, 27);
  await p.eval(`document.querySelector('.usage-plot svg').blur()`);
  // stack off: one series
  await p.click(".usage-stack-toggle .toggle");
  await sleep(200);
  check((await p.eval(`document.querySelectorAll('.usage-legend li').length`)) === 0, "By model off: one series, no legend");
  await p.click(".usage-stack-toggle .toggle");
  // periods
  await p.clickText(".usage-bar .segmented button", "7 days");
  await sleep(600);
  check((await p.eval(`document.querySelectorAll('.usage .usage-hit').length`)) === 7, "7 days: seven bars");
  await p.clickText(".usage-bar .segmented button", "Today");
  await sleep(600);
  check(!(await p.eval(`!!document.querySelector('.usage .usage-plot')`)) && (await p.eval(`!!document.querySelector('.usage .kpi')`)), "Today: totals without a one-bar chart");
  await p.clickText(".usage-bar .segmented button", "All");
  await sleep(600);
  const allBars = await p.eval(`document.querySelectorAll('.usage .usage-hit').length`);
  check(allBars > 60, "All: the whole history", allBars);
  // colours follow the model, not its rank
  const colorOf = (name) => p.eval(`(() => { const l = [...document.querySelectorAll('.usage-legend li')].find(l => l.textContent.includes(${JSON.stringify(name)})); return l?.querySelector('.usage-swatch').style.background; })()`);
  const haikuAll = await colorOf("Haiku");
  await p.clickText(".usage-segmented, .usage-card-head .segmented button", "Requests");
  await sleep(300);
  check((await colorOf("Haiku")) === haikuAll, "a model keeps its colour when the measure re-ranks the models", [haikuAll, await colorOf("Haiku")]);
  await p.clickText(".usage-card-head .segmented button", "Tokens");
  await p.clickText(".usage-bar .segmented button", "30 days");
  await sleep(600);
  // Usage is a section of its own (batch 17): Escape closes Settings.
  await p.key("Escape", "Escape", 0, 27);
  await sleep(300);
  check(!(await p.eval(`!!document.querySelector('.settings-layout')`)), "Escape closes Settings from Usage (a section of its own)");

  // ── board snapshots: the web UI never sends a picture ──
  const board = await mock("/__mock/board", { session_id: "ses_2b7e91c04d5a", kind: "markdown", title: "Snapshot check", ask: false });
  await sleep(400);
  await p.eval(`__mod('lib/workspace.ts').then(m => m.openBoard(${JSON.stringify(board.id)}))`);
  await sleep(800);
  const paneId = await p.eval(`__mod('lib/workspace.ts').then(m => m.paneShowingBoard(${JSON.stringify(board.id)}))`);
  const vis = await p.eval(`__mod('lib/snapshot.ts').then(m => { const r = m.visibleTileRect(${JSON.stringify(paneId)}); return 'rect' in r ? { x: r.rect.x, y: r.rect.y, w: r.rect.width, h: r.rect.height } : r; })`);
  const body = await p.box(`section.pane[data-pane-id="${paneId}"] .board-body`);
  check(vis.w > 100 && Math.abs(vis.x - body.x) < 1 && Math.abs(vis.y - body.y) < 1 && Math.abs(vis.h - body.h) < 1, "the capture rect is exactly the board's content area (not its header)", { vis, body });
  await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings('safety'))`);
  await p.waitFor(`document.querySelector('.sec-more .disclosure')`, 8000).catch(() => {});
  await p.eval(`document.querySelector('.sec-more .disclosure')?.click()`);
  await sleep(400);
  const covered = await p.eval(`__mod('lib/snapshot.ts').then(m => m.visibleTileRect(${JSON.stringify(paneId)}))`);
  check(!!covered.error, "a dialog over the tile counts as not visible", covered);
  const lookToggle = await p.eval(`[...document.querySelectorAll('.setting-title')].some(e => e.textContent.includes('look at whiteboards'))`);
  check(!lookToggle, "the browser UI doesn't offer the look switch (it can't take pictures)");
  await p.key("Escape", "Escape", 0, 27);
  await sleep(300);
  // another mosaic
  await p.eval(`__mod('lib/workspace.ts').then(m => m.switchWorkspace(1))`);
  await sleep(400);
  const away = await p.eval(`__mod('lib/snapshot.ts').then(m => m.visibleTileRect(${JSON.stringify(paneId)}))`);
  check(!!away.error, "a tile on another mosaic isn't visible", away);
  await p.eval(`__mod('lib/workspace.ts').then(m => m.switchWorkspace(0))`);
  await sleep(400);
  // the server asks: the browser answers with an error, never an image
  const before = (await (await fetch(MOCK + "/__mock/snapshots")).json()).length;
  await ws.rpc("send", { session_id: "ses_2b7e91c04d5a", content: [{ type: "text", text: "look at the board" }], delivery: "auto" });
  await sleep(2500);
  const snaps = (await (await fetch(MOCK + "/__mock/snapshots")).json()).slice(before);
  check(snaps.length >= 1 && snaps.every((s) => !s.image && s.error === "can't capture on this device"), "asked to look, the web UI answers “can't capture on this device”", snaps);
  check(!(await p.eval(`__mod('lib/snapshot.ts').then(m => m.lookedAt(${JSON.stringify(board.id)}))`)), "no “looked at this” mark without a picture");
  check(p.errors.length === 0, "no errors in the page", p.errors);
  await p.close();

  // ── phone and fold: the drawer ──
  for (const [name, w, h] of [["phone", 412, 915], ["fold", 884, 1104]]) {
    const m = await device();
    await m.viewport(w, h, true, 2);
    await signIn(m, `Panels ${name}`);
    await m.eval(`__mod('lib/ui.ts').then(u => u.setDrawerOpen(true))`);
    await sleep(500);
    const t = await m.eval(`(() => { const b = document.querySelector('.sidebar.drawer .sb-tab').getBoundingClientRect(); return { h: b.height, n: document.querySelectorAll('.sidebar.drawer .sb-tab').length }; })()`);
    check(t.n === 5 && t.h >= 34, `${name}: the drawer has the tabs, with taller touch targets`, t);
    await m.tap(...(await m.eval(`(() => { const b = document.querySelector('.sidebar.drawer .sb-tab[id$="-bots"]').getBoundingClientRect(); return [b.x + b.width / 2, b.y + b.height / 2]; })()`)));
    await sleep(300);
    check((await m.eval(`document.querySelector('.sidebar.drawer .sb-tab[aria-selected="true"]').id.endsWith('bots')`)), `${name}: tapping a tab switches`);
    await m.shot(`${SHOTS}/panels-drawer-bots-${name}.png`);
    await m.eval(`__mod('components/Sidebar.tsx').then(x => x.showSidebarTab('notes'))`);
    await sleep(300);
    await m.shot(`${SHOTS}/panels-drawer-notes-${name}.png`);
    await m.eval(`__mod('lib/ui.ts').then(u => { u.setDrawerOpen(false); u.openSettings('usage'); })`);
    await sleep(1200);
    await m.shot(`${SHOTS}/panels-usage-${name}.png`);
    const overflow = await m.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth - b.clientWidth; })()`);
    check(overflow <= 1, `${name}: Usage fits the width (tables scroll inside their card)`, overflow);
    check(m.errors.length === 0, `${name}: no errors`, m.errors);
    await m.close();
  }
  ws.close();
  finish();
});
