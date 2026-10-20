// Desktop: typing over a tile, focus following the mouse, the mosaic switcher's keys, dividers,
// fast mode, model roles, boards the user shows (and "Ask about this"), and the memory review
// never showing as a subagent.
import path from "node:path";
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient, menuPick } from "./cdp.mjs";

const here = path.dirname(new URL(import.meta.url).pathname);
const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const draft = (p, id) => ws(p, `return m.getDraft(${JSON.stringify(id)})`);
const focused = (p) => ws(p, `return m.workspace.focused()`);
const activePane = (p) => p.eval(`document.activeElement?.closest('[data-pane-id]')?.dataset.paneId ?? null`);
const activeTag = (p) => p.eval(`document.activeElement?.tagName`);
const moveTo = async (p, sel, dx = 0, dy = 0) => {
  const b = await p.box(sel);
  await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: b.cx + dx, y: b.cy + dy });
  await sleep(80);
  return b;
};
const keys = async (p, text) => {
  for (const ch of text) await p.key(ch, ch === " " ? "Space" : `Key${ch.toUpperCase()}`);
};
const ctrl = (p, type, key, code, vk, mods = 2) => p.send("Input.dispatchKeyEvent", { type, key, code, modifiers: mods, windowsVirtualKeyCode: vk });

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const probe = await wsClient("probe");
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Desktop");
  const A_SID = "ses_7d3a91e5c4b2";
  const B_SID = "ses_5a3e8f10c2d9";
  await ws(p, `m.openSession('${A_SID}', { pane: m.workspace.focused() })`);
  await sleep(400);
  await ws(p, `m.openSession('${B_SID}', { newPane: true })`);
  await sleep(900);
  const [A, B] = await ws(p, `return m.workspace.order()`);
  const inA = `[data-pane-id="${A}"] .transcript`;
  const inB = `[data-pane-id="${B}"] .transcript`;

  // ── typing over a tile ────────────────────────────────────────────────
  console.log("— typing over a tile");
  await p.eval(`document.activeElement?.blur?.()`);
  await ws(p, `m.focusPane('${A}')`);
  await moveTo(p, inB);
  await sleep(150);
  check((await focused(p)) === B, "the focus follows the mouse onto another chat", await focused(p));
  check((await activeTag(p)) === "BODY", "…without taking the keyboard from nowhere");
  await keys(p, "hello");
  check((await draft(p, B)) === "hello", "keys typed with nothing focused land in the hovered chat's box", await draft(p, B));
  check((await activePane(p)) === B, "…and that box now has the keyboard");
  await moveTo(p, inA);
  await sleep(150);
  check((await focused(p)) === A && (await activePane(p)) === A, "moving onto the other chat takes the keyboard along (like Hyprland)", [await focused(p), await activePane(p)]);
  await keys(p, "yo");
  check((await draft(p, A)) === "yo" && (await draft(p, B)) === "hello", "…so typing goes there", [await draft(p, A), await draft(p, B)]);
  // Another field keeps its keys.
  await p.click(`[data-pane-id="${A}"] .title-btn`);
  await sleep(200);
  check((await activeTag(p)) === "INPUT", "renaming a chat focuses its title");
  await moveTo(p, inB);
  await sleep(150);
  await keys(p, "zz");
  check((await activeTag(p)) === "INPUT" && (await draft(p, B)) === "hello", "…and the mouse moving elsewhere doesn't take its keys", await draft(p, B));
  await p.key("Escape");
  await sleep(150);
  // Shortcuts are not typing.
  await p.eval(`document.activeElement?.blur?.()`);
  await moveTo(p, inB);
  await ctrl(p, "keyDown", "k", "KeyK", 75);
  await ctrl(p, "keyUp", "k", "KeyK", 75);
  await sleep(300);
  check(await p.eval(`!!document.querySelector('.palette, .cmd-palette, [aria-label="Command palette"] input, .modal input')`), "Ctrl+K still opens the palette");
  await keys(p, "abc");
  check((await draft(p, B)) === "hello", "keys typed in the palette stay there", await draft(p, B));
  await p.key("Escape");
  await sleep(200);
  // Focus following the mouse can be turned off; typing over a tile still works.
  await p.eval(`__mod('lib/prefs.ts').then(m => m.setPrefs('focusFollowsMouse', false))`);
  await p.eval(`document.activeElement?.blur?.()`);
  await ws(p, `m.focusPane('${B}')`);
  await moveTo(p, inA);
  await sleep(150);
  check((await focused(p)) === B, "with it off, the mouse doesn't move the focus");
  await keys(p, "!");
  check((await draft(p, A)) === "yo!" && (await focused(p)) === A, "…typing over a tile still goes there (and focuses it)", [await draft(p, A), await focused(p)]);
  await p.eval(`__mod('lib/prefs.ts').then(m => m.setPrefs('focusFollowsMouse', true))`);
  // A board's frame keeps its keys.
  await ws(p, `m.openBoard('wb_seed7home', { newPane: true })`);
  await sleep(1500);
  const boardPane = await ws(p, `return m.paneShowingBoard('wb_seed7home')`);
  await p.eval(`document.querySelector('[data-pane-id="${boardPane}"] iframe')?.focus()`);
  await sleep(100);
  check((await activeTag(p)) === "IFRAME", "a board's frame can have the keyboard");
  await moveTo(p, inA);
  await keys(p, "q");
  check((await draft(p, A)) === "yo!", "…and typing then stays in the frame", await draft(p, A));
  await ws(p, `m.closePane('${boardPane}')`);
  await sleep(300);
  await ws(p, `m.setDraft('${A}', ''); m.setDraft('${B}', '')`);

  // ── dividers ──────────────────────────────────────────────────────────
  console.log("— dividers");
  const ratio = () => ws(p, `return m.workspace.geometry().dividers[0]?.ratio`);
  const d = await p.box(`.divider.h`);
  check(!!d && d.w === 12, "the divider is 12px to grab", d);
  await p.down(d.cx, d.cy);
  for (let k = 1; k <= 6; k++) await p.move(d.cx + k * 30, d.cy);
  await p.up(d.cx + 180, d.cy);
  await sleep(300);
  const r1 = await ratio();
  check(r1 > 0.55, "dragging the divider resizes the tiles", r1);
  await p.reload(2500);
  await sleep(800);
  check(Math.abs((await ratio()) - r1) < 0.01, "…and the size stays after a reload", await ratio());
  await sleep(800);
  const L = (await probe.rpc("get_mosaics", {})).result;
  const tree = L.mosaics[0]?.geometry?.tree;
  check(!!tree && Math.abs(tree.ratio - r1) < 0.01, "…and is shared with the layout", tree?.ratio);
  const d2 = await p.box(`.divider.h`);
  await p.send("Input.dispatchMouseEvent", { type: "mousePressed", x: d2.cx, y: d2.cy, button: "left", clickCount: 1 });
  await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: d2.cx, y: d2.cy, button: "left", clickCount: 1 });
  await p.send("Input.dispatchMouseEvent", { type: "mousePressed", x: d2.cx, y: d2.cy, button: "left", clickCount: 2 });
  await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: d2.cx, y: d2.cy, button: "left", clickCount: 2 });
  await sleep(300);
  check(Math.abs((await ratio()) - 0.5) < 0.01, "double-click evens it out", await ratio());

  // ── all mosaics: a page of its own ─────────────────────────────────────
  console.log("— all mosaics");
  await ws(p, `m.switchWorkspace(1); m.openSession('ses_1d9e4b7a0c52')`);
  await sleep(300);
  await ws(p, `m.switchWorkspace(2); m.openSession('ses_a6c3f0e82b19')`);
  await sleep(300);
  await ws(p, `m.switchWorkspace(0)`);
  await sleep(300);
  await p.eval(`document.activeElement?.blur?.()`);
  const active = () => ws(p, `return m.workspace.active()`);
  const pageOpen = () => p.eval(`!!document.querySelector('.mosaics-page')`);
  const selWs = () => p.eval(`document.querySelector('.mosaics-page .mz-card.sel')?.dataset.ws ?? null`);
  // Ctrl+Tab opens the page (and it stays: letting go of Ctrl does nothing).
  await ctrl(p, "keyDown", "Control", "ControlLeft", 17);
  await ctrl(p, "keyDown", "Tab", "Tab", 9);
  await ctrl(p, "keyUp", "Tab", "Tab", 9);
  await ctrl(p, "keyUp", "Control", "ControlLeft", 17, 0);
  await sleep(500);
  check(await pageOpen(), "Ctrl+Tab opens the all-mosaics page");
  check((await active()) === 0, "…letting go of Ctrl switches nothing");
  check(await p.eval(`(() => { const r = document.querySelector('.mosaics-page').getBoundingClientRect(); return r.width === innerWidth && r.height === innerHeight; })()`), "…it fills the window (a page, not a popup)");
  check((await p.eval(`document.querySelectorAll('.mosaics-page .mz-card[data-ws]').length`)) === 3, "…with all three mosaics");
  check((await selWs()) === "0", "…the one on screen selected", await selWs());
  await p.shot(`${SHOTS}/mosaics-page-desktop.png`);
  await p.key("ArrowRight", "ArrowRight");
  await sleep(100);
  check((await selWs()) === "1", "→ selects the next", await selWs());
  await keys(p, "x");
  await p.key("Enter", "Enter");
  await sleep(300);
  const drafts = await ws(p, `return Object.values(m.workspace.panes).map(x => m.getDraft(x.id)).join('')`);
  check((await active()) === 1 && !(await pageOpen()) && !drafts.includes("x"), "Enter goes into it; typed keys went nowhere", { active: await active(), drafts });
  // The hotkey again closes it; Alt+` opens it too; Escape goes back.
  await ctrl(p, "keyDown", "Tab", "Tab", 9);
  await sleep(300);
  check(await pageOpen(), "open again");
  await ctrl(p, "keyDown", "Tab", "Tab", 9);
  await ctrl(p, "keyUp", "Control", "ControlLeft", 17, 0);
  await sleep(300);
  check(!(await pageOpen()), "Ctrl+Tab on the page closes it");
  await ctrl(p, "keyDown", "`", "Backquote", 192, 1);
  await ctrl(p, "keyUp", "`", "Backquote", 192, 1);
  await ctrl(p, "keyUp", "Alt", "AltLeft", 18, 0);
  await sleep(300);
  check(await pageOpen(), "Alt+` opens it too");
  await p.key("Escape");
  await sleep(200);
  check(!(await pageOpen()) && (await active()) === 1, "Escape goes back where you were");
  // The top bar button, a number, a click.
  await p.click(`.topbar [aria-label="All mosaics"]`);
  await sleep(300);
  await p.key("3", "Digit3");
  await sleep(300);
  check((await active()) === 2 && !(await pageOpen()), "a number goes straight to that mosaic", await active());
  await p.click(`.topbar [aria-label="All mosaics"]`);
  await sleep(300);
  await p.click(`.mosaics-page .mz-card[data-ws="0"] .mz-preview`);
  await sleep(300);
  check((await active()) === 0 && !(await pageOpen()), "clicking a card goes into it");
  // Right-click a card: rename it.
  await p.click(`.topbar [aria-label="All mosaics"]`);
  await sleep(300);
  const card = await p.box(`.mosaics-page .mz-card[data-ws="1"]`);
  await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: card.cx, y: card.cy });
  await p.send("Input.dispatchMouseEvent", { type: "mousePressed", x: card.cx, y: card.cy, button: "right", clickCount: 1 });
  await p.send("Input.dispatchMouseEvent", { type: "mouseReleased", x: card.cx, y: card.cy, button: "right", clickCount: 1 });
  await sleep(300);
  const items = await p.eval(`[...document.querySelectorAll('.menu-item .menu-label')].map(e => e.textContent)`);
  check(["Open", "Rename…", "Only on this device", "Delete mosaic…"].every((x) => items.includes(x)), "right-click offers open, rename, this device only, delete", items);
  await p.clickText(".menu-item", "Rename…");
  await sleep(300);
  await p.eval(`(() => { const i = document.querySelector('.modal input'); i.select(); })()`);
  await p.type("Research");
  await p.key("Enter", "Enter");
  await sleep(1500);
  check((await p.eval(`document.querySelector('.mosaics-page .mz-card[data-ws="1"] .mz-name')?.textContent`)) === "Research", "the card shows the new name");
  check(await p.eval(`!!document.querySelector('.mosaics-page')`), "…and the page is still open");
  const named = (await probe.rpc("get_mosaics", {})).result.mosaics.map((m) => m.name);
  check(named.includes("Research"), "the name is shared", named);
  await p.shot(`${SHOTS}/mosaics-page-desktop-named.png`);
  await p.key("Escape");
  await sleep(200);
  check(await p.eval(`/Research/.test(document.querySelector('.ws-pill[data-ws="1"]')?.dataset.tip ?? '')`), "the top bar knows the name too");

  // ── fast mode ─────────────────────────────────────────────────────────
  console.log("— fast mode");
  await probe.rpc("complete_oauth", { provider: "openai-codex", input: "code" });
  await sleep(800);
  await p.eval(`(async () => { const a = await __mod('lib/actions.ts'); await a.setPaneModel('${A}', { provider: 'openai-codex', model: 'gpt-6' }); })()`);
  await sleep(500);
  await p.click(`[data-pane-id="${A}"] .opts-btn`);
  await sleep(300);
  check(await p.eval(`!!document.querySelector('.tile-opts [role="switch"][aria-label="Fast mode"]') && !document.querySelector('.tile-opts .opt-more')`), "a model with a fast tier offers its switch directly in flat options");
  await p.shot(`${SHOTS}/fast-options.png`, await p.eval(`(() => { const r = document.querySelector('.popover').getBoundingClientRect(); return { x: r.x - 8, y: r.y - 8, width: r.width + 16, height: Math.min(560, r.height + 16) }; })()`));
  await p.click(`.tile-opts [role="switch"][aria-label="Fast mode"]`);
  await sleep(500);
  let info = (await probe.rpc("get_session", { session_id: A_SID })).result.info;
  check(info.fast === true, "turning it on sets it on the chat", info.fast);
  await p.key("Escape");
  await sleep(200);
  check(await p.eval(`!!document.querySelector('[data-pane-id="${A}"] .model-chip .fast-mark')`), "the header shows a small mark");
  await p.shot(`${SHOTS}/fast-header.png`, await p.eval(`(() => { const r = document.querySelector('[data-pane-id="${A}"] .pane-head').getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; })()`));
  await ws(p, `m.focusComposer('${A}')`);
  await p.type("/fa");
  await sleep(200);
  check(await p.eval(`[...document.querySelectorAll('.slash-name')].some(e => e.textContent === '/fast')`), "/fast is offered for this model");
  await p.type("st off");
  await p.key("Enter", "Enter");
  await sleep(500);
  info = (await probe.rpc("get_session", { session_id: A_SID })).result.info;
  check(info.fast === false && !(await p.eval(`!!document.querySelector('[data-pane-id="${A}"] .fast-mark')`)), "/fast off turns it off", info.fast);
  await ws(p, `m.focusComposer('${B}')`);
  await p.type("/fa");
  await sleep(200);
  check(!(await p.eval(`[...document.querySelectorAll('.slash-name')].some(e => e.textContent === '/fast')`)), "…not offered where the model has no fast tier");
  await p.type("st");
  await p.key("Enter", "Enter");
  await sleep(400);
  check(await p.eval(`[...document.querySelectorAll('.toast')].some(t => /has no fast mode/.test(t.textContent))`), "typing it anyway explains");
  info = (await probe.rpc("get_session", { session_id: B_SID })).result.info;
  check(info.fast === false, "…and changes nothing");
  await p.eval(`(async () => { const a = await __mod('lib/actions.ts'); await a.setPaneModel('${A}', { provider: 'anthropic', model: 'claude-sonnet-5-5' }); })()`);

  // ── a goal is set with /goal only ─────────────────────────────────────
  console.log("— goal");
  await p.click(`[data-pane-id="${A}"] .opts-btn`);
  await sleep(300);
  check(!(await p.eval(`[...document.querySelectorAll('.tile-opts .opt-line-label')].some(e => e.textContent === 'Goal')`)), "the options have no Goal row");
  await p.key("Escape");
  await sleep(200);
  await ws(p, `m.focusComposer('${A}')`);
  await p.type("/go");
  await sleep(200);
  check(await p.eval(`[...document.querySelectorAll('.slash-name')].some(e => e.textContent === '/goal')`), "/goal is in the slash menu");
  await p.type("al Ship the healthz endpoint");
  await p.key("Enter", "Enter");
  await sleep(700);
  let task = (await probe.rpc("get_session", { session_id: A_SID })).result.info.task;
  check(task?.goal === "Ship the healthz endpoint", "/goal <text> sets it", task?.goal);
  check((await p.eval(`document.querySelector('[data-pane-id="${A}"] .goal .goal-text')?.textContent`)) === "Ship the healthz endpoint", "the goal bar shows it");
  await p.click(`[data-pane-id="${A}"] .goal .goal-text`);
  await sleep(200);
  check(!(await p.eval(`!!document.querySelector('[data-pane-id="${A}"] .goal-input')`)), "…and it isn't edited there");
  await ws(p, `m.focusComposer('${A}')`);
  await p.type("/goal");
  await p.key("Enter", "Enter");
  await p.key("Enter", "Enter");
  await sleep(400);
  check(await p.eval(`[...document.querySelectorAll('.toast')].some(t => /Ship the healthz endpoint/.test(t.textContent))`), "/goal alone shows it");
  await ws(p, `m.focusComposer('${A}')`);
  await p.type("/goal clear");
  await p.key("Enter", "Enter");
  await sleep(700);
  task = (await probe.rpc("get_session", { session_id: A_SID })).result.info.task;
  check(!task?.goal, "/goal clear clears it", task?.goal);
  check(!(await p.eval(`!!document.querySelector('[data-pane-id="${A}"] .goal')`)), "…and the goal bar goes");

  // ── model roles ───────────────────────────────────────────────────────
  console.log("— model roles");
  await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings('providers'))`);
  // One control for the model new chats use, on top; the others under More.
  await p.waitFor(`document.querySelector('.default-model .role-row[data-role="default"]')`, 6000).catch(() => {});
  check((await p.eval(`document.querySelectorAll('.role-row').length`)) === 1 && /Model for new chats/.test(await p.eval(`document.querySelector('.default-model .setting-title')?.textContent ?? ''`)), "Models: one control for the model new chats use, on top");
  check(!(await p.eval(`[...document.querySelectorAll('.settings-body button')].some((b) => /Make default/.test(b.textContent))`)), "…and no second way to set it on each model");
  await p.clickText(".settings-body .disclosure", "More: models for titles");
  await p.waitFor(`document.querySelectorAll('.role-row').length === 3`, 6000).catch(() => {});
  const rows = await p.eval(`[...document.querySelectorAll('.role-row')].map(r => [r.dataset.role, r.querySelector('.setting-title').textContent, r.querySelector('.role-pick-main').textContent])`);
  check(rows.length === 3 && rows[1][1] === "Titles and summaries" && rows[2][1] === "Memory", "More lists the models for titles and summaries, and for memory, in plain words", rows);
  check(rows[1][2] === "Same as each chat" && rows[2][2] === "Same as each chat", "unset roles say what they fall back to", rows);
  await p.shot(`${SHOTS}/roles-before.png`, await p.eval(`(() => { const r = document.querySelector('.roles').getBoundingClientRect(); return { x: r.x - 10, y: r.y - 40, width: r.width + 20, height: r.height + 50 }; })()`));
  await p.click(`.role-row[data-role="utility"] .role-pick`);
  await sleep(300);
  await p.clickText(".pop-model .model-item", "Claude Haiku 5");
  await sleep(600);
  const util = await p.eval(`document.querySelector('.role-row[data-role="utility"] .role-pick-main').textContent`);
  const mem = await p.eval(`[...document.querySelectorAll('.role-row[data-role="memory"] .role-pick span')].map(e => e.textContent)`);
  check(util === "Claude Haiku 5", "picking a model for Utility", util);
  check(mem[1] === "Same as titles and summaries" && mem[2] === "Claude Haiku 5", "Memory now follows the model for titles and summaries", mem);
  await p.click(`.role-row[data-role="memory"] .role-pick`);
  await sleep(300);
  await p.clickText(".pop-model .model-item", "Claude Opus 5.5");
  await sleep(600);
  let roles = (await probe.rpc("get_model_roles", {})).result.roles;
  check(roles.find((r) => r.name === "memory")?.value === "anthropic/claude-opus-5-5", "the memory role is saved on the server", roles.find((r) => r.name === "memory"));
  await p.shot(`${SHOTS}/roles-after.png`, await p.eval(`(() => { const r = document.querySelector('.roles').getBoundingClientRect(); return { x: r.x - 10, y: r.y - 40, width: r.width + 20, height: r.height + 50 }; })()`));
  await p.click(`.role-row[data-role="memory"] .role-pick`);
  await sleep(300);
  await p.clickText(".pop-model .model-item", "Same as titles and summaries");
  await sleep(600);
  roles = (await probe.rpc("get_model_roles", {})).result.roles;
  check(!roles.find((r) => r.name === "memory")?.value, "choosing “Same as titles and summaries” clears it again");
  await p.click(`.role-row[data-role="default"] .role-pick`);
  await sleep(300);
  check(!(await p.eval(`[...document.querySelectorAll('.pop-model .menu-label')].some(e => /^Same as|^Automatic/.test(e.textContent))`)), "the default role has no “Same as” (it's what the others fall back to)");
  await p.clickText(".pop-model .model-item", "Claude Opus 5.5");
  await sleep(600);
  const def = await p.eval(`__mod('lib/store.ts').then(m => JSON.parse(JSON.stringify(m.state.server.default_model)))`);
  check(def?.model === "claude-opus-5-5", "setting Default changes the default model for new chats", def);
  // Under load the picker may still be closing: Escape must reach Settings itself.
  await p.waitFor(`!document.querySelector('.pop-model')`, 4000);
  await p.key("Escape");
  await p.waitFor(`!document.querySelector('.modal-backdrop')`, 4000);
  await sleep(300);

  // ── boards the user shows ─────────────────────────────────────────────
  console.log("— boards the user shows");
  await ws(p, `m.switchWorkspace(0); m.focusPane('${A}')`);
  await sleep(300);
  // New ▾ spawns a tile at once; given an address it becomes a board in that very tile.
  const spawn = async (label) => {
    await p.click(`.topbar .tb-new-more`);
    await sleep(250);
    await menuPick(p, label);
    await sleep(500);
  };
  check(!(await p.eval(`!!document.querySelector('.layout-picker, [aria-label="Layout"]')`)), "the top bar has no layout picker");
  const before = (await ws(p, `return m.workspace.order()`)).length;
  await spawn("Web view");
  let order = await ws(p, `return m.workspace.order()`);
  const urlDraft = await ws(p, `return m.workspace.order().find(id => m.workspace.panes[id].content.type === 'draft')`);
  check(order.length === before + 1 && !!urlDraft, "“Web view” spawns a tile at once (no dialog)", { before, order });
  check(await p.eval(`document.activeElement?.classList.contains('wv-input')`), "…with its address bar ready to type in");
  check(!(await p.eval(`!!document.querySelector('.show-dialog, .modal-backdrop')`)), "…and no dialog");
  await p.type("127.0.0.1:7991/");
  await p.shot(`${SHOTS}/spawn-url-tile.png`);
  await p.key("Enter", "Enter");
  await sleep(1500);
  let boards = (await probe.rpc("list_boards", { session_id: A_SID })).result;
  const urlBoard = boards.find((b) => b.content.type === "url");
  check(!!urlBoard && urlBoard.by_user === true && urlBoard.content.url === "http://127.0.0.1:7991/", "a web page board was made, by the user, with the focused chat (a local address is http)", urlBoard);
  order = await ws(p, `return m.workspace.order()`);
  const urlPane = await ws(p, `return m.paneShowingBoard('${urlBoard?.id}')`);
  check(urlPane === urlDraft, "…in the very tile that was spawned", { urlPane, urlDraft });
  check(order.length === before + 1, "…and no second tile turns up for it", order);
  const tiles = (await probe.rpc("get_mosaics", {})).result.mosaics.flatMap((m) => JSON.stringify(m.root ?? m).match(/board_id":"[^"]+/g) ?? []);
  check(tiles.filter((t) => t.includes(urlBoard?.id)).length === 1, "the shared mosaic has its tile once", tiles);
  check(await p.eval(`document.querySelector('[data-pane-id="${urlPane}"] .sr-only')?.textContent.trim() === 'Shown by you'`), "it's known as shown by you");
  check(!(await p.eval(`[...document.querySelectorAll('.toast')].some(t => /put up a whiteboard/.test(t.textContent))`)), "no “put up a whiteboard” toast for your own");
  // An image the agent shows (images aren't spawned any more).
  const img = await mock("/__mock/board", { session_id: A_SID, kind: "image", title: "sketch", ask: false });
  await sleep(1200);
  check(!!(await ws(p, `return m.paneShowingBoard('${img?.id}')`)), "an image board gets a tile");
  // Bad input is explained in the tile.
  await ws(p, `m.focusPane('${A}')`);
  await spawn("Web view");
  const badDraft = await ws(p, `return m.workspace.order().find(id => m.workspace.panes[id].content.type === 'draft')`);
  await p.type("ftp://example.com/x");
  await p.key("Enter", "Enter");
  await sleep(300);
  check(await p.eval(`/Only web pages/.test(document.querySelector('[data-pane-id="${badDraft}"] .wv-error')?.textContent ?? '')`), "a non-web address is refused in the tile");
  // Empty spawned tiles stay on this device.
  const shared = JSON.stringify((await probe.rpc("get_mosaics", {})).result);
  check(!shared.includes(badDraft), "a spawned tile without content isn't shared", badDraft);
  await ws(p, `m.closePane('${badDraft}')`);
  await sleep(300);
  // Ask about this.
  const imgPane = await ws(p, `return m.paneShowingBoard('${img?.id}')`);
  // A narrow board tile keeps its actions in the ⋯ menu.
  const askBtn = await p.eval(`(() => { const b = document.querySelector('[data-pane-id="${imgPane}"] [aria-label="Ask about this"]'); return !!b && b.getClientRects().length > 0; })()`);
  if (askBtn) await p.click(`[data-pane-id="${imgPane}"] [aria-label="Ask about this"]`);
  else {
    await p.click(`[data-pane-id="${imgPane}"] [aria-label="Whiteboard options"]`);
    await sleep(250);
    await p.clickText(".menu-item", "Ask about this");
  }
  await sleep(400);
  const ref = await draft(p, A);
  check(ref.includes("“sketch”") && ref.includes(img?.id), "“Ask about this” puts a reference in the chat's box", ref);
  check((await activePane(p)) === A, "…and puts you there to finish it");
  await p.shot(`${SHOTS}/user-boards-desktop.png`);
  await ws(p, `m.setDraft('${A}', '')`);
  // /show with an address.
  await ws(p, `m.focusComposer('${A}')`);
  await p.type("/show 127.0.0.1:7991/docs");
  await p.key("Enter", "Enter");
  await sleep(1200);
  boards = (await probe.rpc("list_boards", { session_id: A_SID })).result;
  check(boards.some((b) => b.content.type === "url" && b.content.url.endsWith("/docs") && b.by_user), "/show <address> shows a page");

  // ── the memory review is never a subagent ─────────────────────────────
  console.log("— memory review");
  for (const b of boards) await probe.rpc("delete_board", { id: b.id });
  await sleep(500);
  await ws(p, `m.focusComposer('${B}')`);
  await p.type("Remember that I prefer tabs over spaces");
  await p.key("Enter", "Enter");
  let sawIndicator = false;
  let sawStrip = false;
  let sawCard = false;
  for (let k = 0; k < 60; k++) {
    await sleep(200);
    const st = await p.eval(`({ ind: !!document.querySelector('[data-pane-id="${B}"] .memory-review'), strip: !!document.querySelector('[data-pane-id="${B}"] .agents-bar'), card: !!document.querySelector('[data-pane-id="${B}"] .subagent'), learned: !!document.querySelector('[data-pane-id="${B}"] .learned') })`);
    sawIndicator ||= st.ind;
    sawStrip ||= st.strip;
    sawCard ||= st.card;
    if (st.ind && !(await p.eval(`!!document.querySelector('.shots-taken')`))) {
      await p.eval(`document.body.classList.add('shots-taken')`);
      await p.shot(`${SHOTS}/memory-review.png`, await p.eval(`(() => { const r = document.querySelector('[data-pane-id="${B}"] .composer-dock').getBoundingClientRect(); return { x: r.x, y: r.y - 40, width: r.width, height: r.height + 40 }; })()`));
    }
    if (st.learned && k > 5) break;
  }
  const internals = await p.eval(`__mod('lib/store.ts').then(m => Object.values(m.state.sessions).filter(s => s.info.internal).map(s => s.info.title))`);
  check(internals.length === 1, "the harness's memory review reached this device", internals);
  check(!sawStrip && !sawCard, "…but never shows as a subagent", { sawStrip, sawCard });
  check(sawIndicator, "a quiet line says it's looking for things to remember");
  check(await p.eval(`/Remembered 1 thing/.test(document.querySelector('[data-pane-id="${B}"] .learned')?.textContent ?? '')`), "the outcome still shows as “Remembered 1 thing”");
  check(!(await p.eval(`!!document.querySelector('[data-pane-id="${B}"] .memory-review')`)), "…and the quiet line is gone");

  console.log("errors:", p.errors);
  check(!p.errors.length, "no page errors");
  finish();
});
