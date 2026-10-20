// Tiles, batch 2 (desktop): the bolt and YOLO icons, the slimmer tile options with the
// subagents switch, "+" spawning tiles, note tiles (editing, autosave, combining and a conflict,
// one the agent shows), snapping a tile by dragging it to a side or corner, snap assist, tile
// headers (no layout picker; double-click maximizes; Restore; narrow tiles keep their close
// button), and keyboard tiling still working.
import { SHOTS, check, device, finish, menuLabels, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const A_SID = "ses_7d3a91e5c4b2";
const B_SID = "ses_5a3e8f10c2d9";
const C_SID = "ses_1d9e4b7a0c52";
const geom = (p, id) =>
  ws(p, `const r = m.workspace.geometry().panes.get('${id}'); return r ? [r.x, r.y, r.w, r.h].map(v => Math.round(v * 1000) / 1000) : null`);
const near = (g, want) => !!g && g.every((v, i) => Math.abs(v - want[i]) < 0.01);
const order = (p) => ws(p, `return m.workspace.order()`);
const kinds = (p) => ws(p, `return m.workspace.order().map(id => { const c = m.workspace.panes[id].content; return c.type === 'draft' ? 'draft:' + c.kind : c.type; })`);
/** An empty spot of a tile's header (left of its buttons), to grab it by. */
const grabPoint = (p, id) =>
  p.eval(`(() => { const h = document.querySelector('[data-pane-id="${id}"] .pane-head'); const c = h.querySelector('.head-controls'); const r = h.getBoundingClientRect(); return { x: c.getBoundingClientRect().left - 14, y: r.top + r.height / 2 }; })()`);
const area = (p) => p.eval(`(() => { const r = document.querySelector('.workspace').getBoundingClientRect(); return { l: r.left, t: r.top, r: r.right, b: r.bottom }; })()`);
/** Drag with the mouse; `during` runs while still held over the target. */
const mdrag = async (p, from, to, during) => {
  await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: from.x, y: from.y });
  await p.send("Input.dispatchMouseEvent", { type: "mousePressed", x: from.x, y: from.y, button: "left", buttons: 1, clickCount: 1 });
  const steps = 14;
  for (let k = 1; k <= steps; k++) {
    await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: from.x + ((to.x - from.x) * k) / steps, y: from.y + ((to.y - from.y) * k) / steps, button: "left", buttons: 1 });
    await sleep(18);
  }
  await sleep(250);
  const seen = during ? await during() : null;
  await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: to.x, y: to.y, button: "left", buttons: 0, clickCount: 1 });
  await sleep(450);
  return seen;
};
const preview = (p) => p.eval(`(() => { const d = document.querySelector('.drop-preview'); return d ? { snap: d.classList.contains('snap-zone'), label: document.querySelector('.drop-chip')?.textContent ?? '' } : null; })()`);

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const probe = await wsClient("probe");
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Desktop");
  await ws(p, `m.openSession('${A_SID}', { pane: m.workspace.focused() })`);
  await sleep(400);
  await ws(p, `m.openSession('${B_SID}', { newPane: true })`);
  await sleep(700);
  const [A, B] = await order(p);

  // ── icons: the bolt is fast mode, YOLO has its own ────────────────────
  console.log("— icons");
  await probe.rpc("complete_oauth", { provider: "openai-codex", input: "code" });
  await sleep(600);
  await p.eval(`(async () => { const a = await __mod('lib/actions.ts'); await a.setPaneModel('${A}', { provider: 'openai-codex', model: 'gpt-6' }); await a.patchPane('${A}', { permission: 'auto' }); })()`);
  await sleep(600);
  const yoloSvg = await p.eval(`document.querySelector('[data-pane-id="${A}"] .yolo-badge svg')?.innerHTML ?? ''`);
  check(yoloSvg.includes("l17 17") && !yoloSvg.includes("M13 3.5L5 13.5"), "the YOLO badge shows a shield with a slash, not the bolt", yoloSvg.slice(0, 120));
  await p.click(`[data-pane-id="${A}"] .opts-btn`);
  await sleep(350);
  const optsSvg = await p.eval(`document.querySelector('[data-pane-id="${A}"] .opts-btn svg')?.innerHTML ?? ''`);
  check((optsSvg.match(/<circle/g) ?? []).length === 3 && /cx="6"/.test(optsSvg), "the chat's options button is ⋯, like every tile's (not the sliders)", optsSvg.slice(0, 80));
  const gear = await p.eval(`document.querySelector('.sidebar .sb-foot [aria-label="Settings"] svg')?.innerHTML ?? ''`);
  check(/r="6.2"/.test(gear), "Settings is a gear", gear.slice(0, 80));
  // Fast mode and helpers are directly reachable, with no nested dropdown.
  const fastSvg = await p.eval(`[...document.querySelectorAll('.tile-opts .opt-line')].find(e => e.textContent.includes('Fast mode'))?.querySelector('svg')?.innerHTML ?? ''`);
  check(fastSvg.includes("M13 3.5L5 13.5"), "fast mode is the bolt", fastSvg.slice(0, 80));
  const yoloSeg = await p.eval(`[...document.querySelectorAll('.perm-seg button')].find(b => b.textContent.includes('YOLO'))?.querySelector('svg')?.innerHTML ?? ''`);
  check(yoloSeg.includes("l17 17"), "…and YOLO in the permission control uses the slashed shield", yoloSeg.slice(0, 80));

  // ── the tile's options, slimmer ───────────────────────────────────────
  console.log("— tile options");
  const text = await p.eval(`document.querySelector('.tile-opts').textContent`);
  check(!/New mosaic|Show beside|Split right|Split down/.test(text), "no New mosaic, “Show beside this chat” or split buttons in the options", text.slice(0, 200));
  const rows = await p.eval(`[...document.querySelectorAll('.tile-opts .acts-row')].map(r => [...r.querySelectorAll('.act-btn')].map(b => b.textContent.trim()))`);
  check(JSON.stringify(rows[0]) === JSON.stringify(["New chat here", "Rename", "Archive"]), "everyday chat actions in one tight row: a fresh chat here, rename, archive", rows[0]);
  check(JSON.stringify(rows[1]) === JSON.stringify(["Maximize", "Close"]), "…the tile's in another: maximize, close", rows[1]);
  const deeper = await p.eval(`[...document.querySelectorAll('.tile-opts .opt-more-body .opt-line-label')].map(e => e.textContent)`);
  check(["Fast mode", "Helpers", "Chat length", "Branch into a new chat", "Pin to the top"].every((x) => deeper.includes(x)) && !(await p.eval(`!!document.querySelector('.tile-opts .opt-more')`)), "flat options hold fast mode, helpers, chat length, branch and pin, in plain words", deeper);
  check((await p.eval(`document.querySelectorAll('.perm-seg [role="radio"]').length`)) === 4 && (await p.eval(`document.querySelector('.tile-opts .opt-head')?.textContent`)) === "How much freedom", "how much freedom is one segmented row, on top");
  const h = await p.eval(`document.querySelector('.pop-options').getBoundingClientRect().height`);
  check(h < 470, "the whole popover stays compact", h);
  await p.shot(`${SHOTS}/tile-options-desktop.png`, await p.eval(`(() => { const r = document.querySelector('.pop-options').getBoundingClientRect(); return { x: r.x - 8, y: r.y - 8, width: r.width + 16, height: r.height + 16 }; })()`));
  // Helpers (subagents).
  const sw = `.tile-opts [role="switch"][aria-label="Helpers"]`;
  check((await p.eval(`document.querySelector('${sw}')?.getAttribute('aria-checked')`)) === "true", "a Helpers switch, on by default");
  await p.click(sw);
  await sleep(500);
  let info = (await probe.rpc("get_session", { session_id: A_SID })).result.info;
  check(info.subagents === false, "turning it off is saved on the chat", info.subagents);
  check(await p.eval(`/Works alone/.test(document.querySelector('${sw}').closest('.opt-line').textContent)`), "…and says the chat works alone");
  await p.key("Escape");
  await sleep(200);
  await p.eval(`(async () => { const a = await __mod('lib/actions.ts'); await a.patchPane('${A}', { permission: 'default' }); })()`);
  await ws(p, `m.focusComposer('${A}')`);
  await p.type("research the options in parallel");
  await p.key("Enter", "Enter");
  await sleep(3500);
  const kids = (await probe.rpc("list_sessions", {})).result.filter((s) => s.parent_id === A_SID && s.kind === "subagent" && !s.internal);
  check(kids.length === 0, "with it off, the agent starts no helpers", kids.map((k) => k.title));
  check(await p.eval(`/subagents are off/.test(document.querySelector('[data-pane-id="${A}"] .transcript').textContent)`), "…and does it itself");
  await p.click(`[data-pane-id="${A}"] .opts-btn`);
  await sleep(300);
  await p.click(sw);
  await sleep(500);
  info = (await probe.rpc("get_session", { session_id: A_SID })).result.info;
  check(info.subagents !== false, "turning it back on", info.subagents);
  await p.key("Escape");
  await sleep(200);

  // ── "+": a note ───────────────────────────────────────────────────────
  console.log("— notes");
  await ws(p, `m.focusPane('${A}')`);
  await p.click(`.topbar .tb-new-more`);
  await sleep(250);
  const menu = await menuLabels(p);
  check(JSON.stringify(menu) === JSON.stringify(["Chat", "Private chat", "Note", "Web view", "More"]), "New ▾ offers a chat, a private chat, a note, a web view, then More", menu);
  check(await p.eval(`(() => { const m = document.querySelector('.menu-pop'); return !!m && !m.classList.contains('as-sheet') && m.getBoundingClientRect().height < 420; })()`), "…as a compact menu, not a sheet");
  await p.click(".menu-pop .menu-more");
  await sleep(250);
  const more = await menuLabels(p);
  check(JSON.stringify(more) === JSON.stringify(["Chat", "Private chat", "Note", "Web view", "Bot", "Channel", "Scheduled task", "Terminal", "Mosaic"]), "More opens in place: a bot, a channel, a scheduled task, a terminal, a mosaic", more);
  const mosaicLine = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-item')].find((e) => e.querySelector('.menu-label')?.textContent === 'Mosaic')?.querySelector('.menu-desc')?.textContent`);
  check(mosaicLine === "A new screen of tiles", "…“Mosaic” says in one line what it is", mosaicLine);
  await p.shot(`${SHOTS}/spawn-menu-desktop.png`, await p.eval(`(() => { const r = document.querySelector('.menu-pop').getBoundingClientRect(); return { x: r.x - 40, y: 0, width: r.width + 80, height: r.bottom + 10 }; })()`));
  await p.clickText(".menu-item", "Note");
  await sleep(1200);
  const N = await ws(p, `return m.workspace.order().find(id => m.workspace.panes[id].content.type === 'note')`);
  const noteId = await ws(p, `return m.workspace.panes['${N}'].content.noteId`);
  check(!!N && (await ws(p, `return m.workspace.focused()`)) === N, "a note tile opens at once, focused");
  let note = (await probe.rpc("get_note", { id: noteId })).result;
  check(note?.title === "Untitled note" && note.by_user === true, "…the note is made on the server (by you)", note && { title: note.title, by_user: note.by_user });
  check(await p.eval(`document.activeElement?.classList.contains('note-title')`), "…ready to be named");
  await p.key("a", "KeyA", 2);
  await p.type("Groceries");
  // Enter on the title goes on to the text (the note is open to write in: no Edit step).
  await p.key("Enter", "Enter", 0, 13);
  check(await p.eval(`document.activeElement?.matches('[data-pane-id="${N}"] .cm-content') ?? false`), "Enter on the title goes on to the text");
  await p.type("- milk\n- eggs");
  await sleep(1400);
  note = (await probe.rpc("get_note", { id: noteId })).result;
  check(note.title === "Groceries" && note.content === "- milk\n- eggs", "typing saves on its own (title and text)", { title: note.title, content: note.content });
  check((await p.eval(`document.querySelector('[data-pane-id="${N}"] .note-status')?.textContent ?? ''`)).includes("Saved"), "…and says so");
  // A change made elsewhere to another line while typing here: both kept.
  await p.type("\n- bread");
  await probe.rpc("save_note", { id: noteId, title: "Groceries", content: "- oat milk\n- eggs", base_version: note.version });
  await sleep(1800);
  note = (await probe.rpc("get_note", { id: noteId })).result;
  check(note.content === "- oat milk\n- eggs\n- bread" && !(await p.eval(`!!document.querySelector('[data-pane-id="${N}"] .note-bar.warn')`)), "a change elsewhere to another line is combined, no question", note.content);
  // The same line: asked, not lost.
  await p.type(" (brown)");
  await probe.rpc("save_note", { id: noteId, title: "Groceries", content: "- oat milk\n- eggs\n- rye bread", base_version: note.version });
  await sleep(1500);
  check(await p.eval(`!!document.querySelector('[data-pane-id="${N}"] .note-bar.warn')`), "a change elsewhere to the same line asks which to keep");
  check((await p.eval(`document.querySelector('[data-pane-id="${N}"] .cm-content')?.cmTile.root.view.state.doc.toString()`)) === "- oat milk\n- eggs\n- bread (brown)", "…and what was typed here is still there");
  await p.shot(`${SHOTS}/note-conflict-desktop.png`, await p.eval(`(() => { const r = document.querySelector('[data-pane-id="${N}"]').getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: Math.min(r.height, 360) }; })()`));
  await p.clickText(`[data-pane-id="${N}"] .note-bar button`, "Keep mine");
  await sleep(1200);
  note = (await probe.rpc("get_note", { id: noteId })).result;
  check(note.content === "- oat milk\n- eggs\n- bread (brown)" && !(await p.eval(`!!document.querySelector('[data-pane-id="${N}"] .note-bar.warn')`)), "“Keep mine” saves it over theirs", note.content);
  // Drawn as you write (no step to see it rendered).
  check((await p.eval(`document.querySelectorAll('[data-pane-id="${N}"] .cm-mark-bullet').length`)) === 3, "the list is drawn as a list while you write");
  await p.shot(`${SHOTS}/note-tile-desktop.png`);
  // A note of the user's own has no "mention it in the chat beside it" (a [[link]] in a message brings it in).
  await p.click(`[data-pane-id="${N}"] [aria-label="Note options"]`);
  await sleep(250);
  const noteMenu = await p.eval(`[...document.querySelectorAll('.menu .menu-item')].map(e => e.textContent)`);
  check(noteMenu.length > 3 && !noteMenu.some((t) => /Ask about this|beside it/.test(t)), "a note not made in a chat offers no “mention it in the chat beside it”", noteMenu);
  await p.key("Escape", "Escape", 0, 27);
  await sleep(200);
  check((await ws(p, `return m.getDraft('${A}')`)) === "", "…and the chat beside it is left alone");
  // openNote (for the notes list): focuses the tile already showing it.
  const before = (await order(p)).length;
  await p.eval(`__mod('lib/notes.ts').then(m => m.openNote('${noteId}'))`);
  await sleep(300);
  check((await order(p)).length === before && (await ws(p, `return m.workspace.focused()`)) === N, "openNote focuses the tile already showing it");
  // The agent shows a note of its own (asked to: a note it only writes is a card in the chat).
  await ws(p, `m.focusComposer('${B}')`);
  await p.type("take a note about the launch plan and show it");
  await p.key("Enter", "Enter");
  for (let i = 0; i < 60; i++) {
    if ((await ws(p, `return m.workspace.order().filter(id => m.workspace.panes[id].content.type === 'note').length`)) === 2) break;
    await sleep(250);
  }
  const agentNote = await ws(p, `const id = m.workspace.order().find(id => { const c = m.workspace.panes[id].content; return c.type === 'note' && c.noteId !== '${noteId}'; }); return id ? m.workspace.panes[id].content.noteId : null`);
  const an = agentNote ? (await probe.rpc("get_note", { id: agentNote })).result : null;
  check(!!an && an.by_user === false && /Launch plan/i.test(an.title), "a note the agent shows opens as a note tile", an && { title: an.title, by_user: an.by_user });
  // Deleting a note closes its tiles.
  await probe.rpc("delete_note", { id: agentNote });
  await sleep(700);
  check(!(await ws(p, `return m.workspace.order().some(id => m.workspace.panes[id].content.noteId === '${agentNote}')`)), "a deleted note's tile closes");

  // ── snapping by dragging to a side or corner ──────────────────────────
  console.log("— snap by dragging");
  await ws(p, `m.closePane('${N}')`);
  await sleep(300);
  await ws(p, `m.openSession('${C_SID}', { newPane: true })`);
  await sleep(700);
  const C = (await order(p)).find((id) => id !== A && id !== B);
  check((await order(p)).length === 3, "three tiles", await kinds(p));
  const ar = await area(p);
  let seen = await mdrag(p, await grabPoint(p, C), { x: ar.l + 6, y: (ar.t + ar.b) / 2 }, () => preview(p));
  check(seen?.snap && seen.label === "Snap to the left half", "dragging a tile to the mosaic's left side shows where it snaps", seen);
  check(near(await geom(p, C), [0, 0, 0.5, 1]), "dropped there it takes the left half", await geom(p, C));
  check(near(await geom(p, A), [0.5, 0, 0.5, 0.5]) && near(await geom(p, B), [0.5, 0.5, 0.5, 0.5]), "…the others share the right half", [await geom(p, A), await geom(p, B)]);
  // A drag that passes over a tile's own edge still moves next to that tile (the sides of the mosaic are a thin band).
  seen = await mdrag(p, await grabPoint(p, C), { x: ar.l + (ar.r - ar.l) * 0.72, y: ar.t + (ar.b - ar.t) * 0.25 }, () => preview(p));
  check(seen && !seen.snap && /Move|Swap/.test(seen.label), "over a tile it's the usual move or swap", seen);
  await ws(p, `m.snapToRegion('${C}', 'left')`);
  await sleep(300);
  // A corner: quarters; space left over offers what could fill it.
  seen = await mdrag(p, await grabPoint(p, A), { x: ar.r - 5, y: ar.t + 5 }, async () => {
    await p.shot(`${SHOTS}/snap-corner-preview.png`);
    return preview(p);
  });
  check(seen?.snap && seen.label === "Snap to the top-right quarter", "a corner snaps to a quarter", seen);
  check(near(await geom(p, A), [0.5, 0, 0.5, 0.5]), "…the tile takes the top-right quarter", await geom(p, A));
  let k = await kinds(p);
  check(k.filter((x) => x === "draft:assist").length === 1, "the fourth quarter offers what could fill it (snap assist)", k);
  const S1 = await ws(p, `return m.workspace.order().find(id => m.workspace.panes[id].content.kind === 'assist')`);
  const cards = await p.eval(`[...document.querySelectorAll('[data-pane-id="${S1}"] .assist-card .assist-card-title')].map(e => e.textContent)`);
  check(cards.length > 0, "…recent chats as cards", cards);
  await p.shot(`${SHOTS}/snap-assist-desktop.png`);
  const sharedNow = JSON.stringify((await probe.rpc("get_mosaics", {})).result);
  check(!sharedNow.includes(S1), "the empty space stays on this device");
  await p.clickText(`[data-pane-id="${S1}"] .assist-card`, cards[0]);
  await sleep(700);
  k = await kinds(p);
  check(!k.includes("draft:assist") && k.length === 4 && (await ws(p, `return m.workspace.panes['${S1}']?.content.type`)) === "chat", "picking one puts it right there", k);
  const D = S1;

  // ── tile headers: moved by dragging, maximized by double-click ─────────
  console.log("— tile headers");
  check(!(await p.eval(`!!document.querySelector('.pane .head-max')`)), "tile headers have no maximize / layout button");
  const spot = await p.eval(`(() => { const h = document.querySelector('[data-pane-id="${A}"] .pane-head'); const r = h.getBoundingClientRect(); const y = r.y + r.height / 2; for (let x = r.right - 120; x > r.x + 4; x -= 4) { const e = document.elementFromPoint(x, y); if (e && h.contains(e) && !e.closest('button, input, textarea, a, [role=button], [data-nodrag]')) return { x, y }; } return null; })()`);
  check(!!spot, "the header has room to double-click", spot);
  await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: spot.x, y: spot.y });
  await sleep(700);
  check(!(await p.eval(`!!document.querySelector('.snap-pop, [aria-label="Snap layouts"]')`)), "resting on the header opens no layout picker");
  for (const n of [1, 2]) {
    await p.send("Input.dispatchMouseEvent", { type: "mousePressed", x: spot.x, y: spot.y, button: "left", clickCount: n });
    await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: spot.x, y: spot.y, button: "left", clickCount: n });
  }
  await sleep(300);
  check((await ws(p, `return m.workspace.maximized()`)) === A, "double-clicking a tile's header maximizes it");
  const restore = await p.eval(`document.querySelector('[data-pane-id="${A}"] .head-max')?.getAttribute('aria-label') ?? null`);
  check(restore === "Restore tile", "a maximized tile shows Restore", restore);
  await p.shot(`${SHOTS}/header-maximized-desktop.png`, await p.eval(`(() => { const r = document.querySelector('[data-pane-id="${A}"] .pane-head').getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; })()`));
  await p.click(`[data-pane-id="${A}"] .head-max`);
  await sleep(300);
  check(!(await ws(p, `return m.workspace.maximized()`)) && !(await p.eval(`!!document.querySelector('.pane .head-max')`)), "Restore puts it back, and the button goes");
  // A narrow tile (four columns) still shows its close button.
  await ws(p, `m.focusPane('${A}')`);
  const narrow = await p.eval(`new Promise((done) => { const el = document.querySelector('[data-pane-id="${A}"]'); window.__w = el.style.width; el.style.width = '330px'; requestAnimationFrame(() => requestAnimationFrame(() => { const x = el.querySelector('.head-close'); const b = x?.getBoundingClientRect(); const pr = el.getBoundingClientRect(); done({ pane: Math.round(pr.width), close: !!b && getComputedStyle(x).display !== 'none' && getComputedStyle(x).opacity === '1' && b.width > 0 && b.left >= pr.left && b.right <= pr.right + 1 }); })); })`);
  check(narrow.pane <= 340 && narrow.close, "a narrow tile still shows its close button", narrow);
  await p.shot(`${SHOTS}/header-narrow-desktop.png`, await p.eval(`(() => { const r = document.querySelector('[data-pane-id="${A}"] .pane-head').getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; })()`));
  await p.eval(`document.querySelector('[data-pane-id="${A}"]').style.width = window.__w`);
  await ws(p, `m.closePane('${C}'); m.closePane('${D}')`);
  await sleep(400);

  // ── keyboard tiling still works ───────────────────────────────────────
  console.log("— keyboard tiling");
  await ws(p, `m.focusPane('${A}')`);
  const g0 = await geom(p, A);
  const dir = await ws(p, `return ['left', 'right', 'up', 'down'].find(d => m.neighbor('${A}', d))`);
  const arrow = { left: "ArrowLeft", right: "ArrowRight", up: "ArrowUp", down: "ArrowDown" }[dir];
  await p.key(arrow, arrow, 9);
  await sleep(300);
  const g1 = await geom(p, A);
  check(JSON.stringify(g0) !== JSON.stringify(g1), `Alt+Shift+${arrow} still swaps tiles`, [g0, g1]);
  await p.key("f", "KeyF", 1);
  await sleep(200);
  check((await ws(p, `return m.workspace.maximized()`)) === A, "Alt+F still maximizes");
  await p.key("f", "KeyF", 1);
  await sleep(200);

  // ── "New chat here": a fresh chat in the same tile ─────────────────────
  console.log("— new chat here");
  const was = await ws(p, `return m.workspace.panes['${A}'].content.sessionId`);
  await ws(p, `m.focusPane('${A}')`);
  await p.click(`[data-pane-id="${A}"] .opts-btn`);
  await sleep(300);
  await p.clickText(".tile-opts .act-btn", "New chat here");
  await sleep(500);
  const now = await ws(p, `const c = m.workspace.panes['${A}']?.content; return { type: c?.type, sid: c?.sessionId ?? null, focused: m.workspace.focused() }`);
  check(!!was && now.type === "chat" && now.sid === null && now.focused === A, "“New chat here” puts a fresh chat in the same tile", { was, now });
  check(!(await p.eval(`!!document.querySelector('.pop-options')`)) && (await p.eval(`!!document.activeElement?.closest('[data-pane-id="${A}"] .composer')`)), "…closes the menu and is ready to type");
  check(await p.eval(`__mod('lib/store.ts').then(m => !!m.state.sessions['${was}'])`), "the old chat is kept (it's only out of the tile)");

  console.log("errors:", p.errors);
  check(!p.errors.length, "no page errors");
  probe.close();
  await p.close();
  finish();
});
