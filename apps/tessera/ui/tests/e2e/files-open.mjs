// Files in a chat, opened where the user wants them: a tap on a text file's
// name opens it in a tile by default (Settings → General → "Open files from
// chats in": a tile, a new window, or this device's app); each file's ⋯ menu
// offers every way, Download and "Save as note". The tile: the text (find,
// wrap, line numbers), a Markdown file drawn as a page, kept on this device
// only and back after a reload. A window: the app's own page showing just
// the file (it never starts the app). The desktop app (a stand-in shell):
// its own window and this computer's app (programs never), and a file
// window that doesn't connect. An Android stand-in: saved to Downloads, then
// opened. Desktop, a folded phone (344 px) and the unfolded Fold, light and dark.
import { APP, MOCK, SHOTS, browser, check, device, finish, mock, readEarlier, run, signIn, sleep, wsClient } from "./cdp.mjs";

const SID = "ses_7d3a91e5c4b2";
const LOG = Array.from({ length: 400 }, (_, i) => `2026-10-04T09:${String(Math.floor(i / 60)).padStart(2, "0")}:${String(i % 60).padStart(2, "0")}Z ${i % 9 === 0 ? "WARN" : "INFO"} worker[${i % 4}] handled /api/items/${i} in ${(i * 37) % 900}ms`).join("\n");
const README = "# Release notes\n\n- **Faster** start\n- Pictures in notes\n\n```sh\nnpm run build\n```\n";
const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const readChat = async (p) => {
  const pane = await ws(p, `return m.paneShowing('${SID}')`);
  await readEarlier(p, `[data-pane-id="${pane}"] .transcript`);
};
/** Settings → Appearance with its More opened ("Where files open" lives there). */
const openWhereFiles = async (pg) => {
  await pg.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab('appearance'))`);
  await pg.waitFor(`document.querySelector('.settings-body .disclosure')`, 8000);
  await pg.eval(`(() => { const d = document.querySelector('.settings-body .disclosure'); if (d.getAttribute('aria-expanded') !== 'true') d.click(); })()`);
  await sleep(300);
};
const waitFor = async (fn, ms = 6000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const v = await fn().catch(() => null);
    if (v) return v;
    await sleep(100);
  }
  return null;
};
const menuLabels = (p) => p.eval(`[...document.querySelectorAll('.menu-pop .menu-label')].map(e => e.textContent)`);
const closeMenus = (p) => p.eval(`(document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })), true)`);
/** The file tiles on screen: their names and what they show. */
const fileTiles = (p) =>
  p.eval(`[...document.querySelectorAll('.file-tile')].map((t) => ({ name: t.querySelector('.file-tile-name')?.textContent, sub: t.querySelector('.board-sub')?.textContent, text: t.querySelector('.cm-content')?.textContent?.slice(0, 200) ?? null, md: t.querySelector('.file-md')?.textContent?.slice(0, 200) ?? null, gutter: !!t.querySelector('.cm-lineNumbers'), err: t.querySelector('.file-error')?.textContent ?? null, pane: t.closest('[data-pane-id]')?.dataset.paneId }))`);

/** A stand-in for the apps' shell (as in paste.mjs), with the file commands recorded in `window.__calls`. */
function shellScript(token, label = "main") {
  return `(() => {
    const TOKEN = ${JSON.stringify(token)};
    let n = 0, ws = null, rid = 0;
    const cbs = {}, listeners = {}, pending = new Map();
    window.__calls = [];
    window.__windowFile = null;
    const emit = (event, payload) => (listeners[event] || []).forEach((id) => cbs[id] && cbs[id]({ event, id: 1, payload }));
    const status = (state) => emit("tessera://status", { state, serverId: "srv_mock", endpoint: location.origin, error: null, encrypted: true });
    function connect() {
      ws = new WebSocket(location.origin.replace(/^http/, "ws") + "/api/ws?token=" + encodeURIComponent(TOKEN));
      ws.onopen = () => status("connected");
      ws.onclose = () => status("disconnected");
      ws.onmessage = (m) => {
        const msg = JSON.parse(m.data);
        if (msg.event) emit("tessera://event", msg.event);
        else if (pending.has(msg.id)) { const p = pending.get(msg.id); pending.delete(msg.id); msg.error ? p.reject(msg.error) : p.resolve(msg.result ?? null); }
      };
    }
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
    window.__TAURI_INTERNALS__ = {
      metadata: { currentWindow: { label: ${JSON.stringify(label)} }, currentWebview: { label: ${JSON.stringify(label)}, windowLabel: ${JSON.stringify(label)} } },
      transformCallback(cb, once) { const id = ++n; cbs[id] = (x) => { if (once) delete cbs[id]; cb(x); }; return id; },
      async invoke(cmd, args = {}) {
        if (!cmd.startsWith("plugin:event")) window.__calls.push({ cmd, args: cmd === "rpc" ? args.request?.method : args });
        switch (cmd) {
          case "plugin:event|listen": (listeners[args.event] ||= []).push(args.handler); return args.handler;
          case "plugin:event|unlisten": return null;
          case "servers_get": return { servers: [{ id: "srv_mock", name: "Mock server", endpoints: [location.origin], headers: {}, token: "", serverKey: "", fingerprint: "1111 2222", deviceName: "App" }], active: "srv_mock" };
          case "servers_save": return null;
          case "server_connect": if (!ws) connect(); else status("connected"); return null;
          case "server_disconnect": return null;
          case "app_resumed": return null;
          case "rpc": return new Promise((resolve, reject) => { const id = ++rid; pending.set(id, { resolve, reject }); const r = args.request; ws.send(JSON.stringify({ id, method: r.method, ...(r.params !== undefined ? { params: r.params } : {}) })); });
          case "file_open_with": return { name: args.name, location: "/home/me/.cache/dev.tessera.app/open/x/" + args.name, place: "" };
          case "file_window": return true;
          case "file_window_take": return window.__windowFile;
          case "file_download": return { name: args.name, location: "content://media/external/downloads/42", place: "Downloads" };
          case "file_open": return null;
          case "file_save": return { name: args.name, location: "content://media/external/downloads/43", place: "Downloads" };
          case "notifications_get": return null;
          default: throw "command " + cmd + " not found";
        }
      },
    };
  })()`;
}

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const probe = await wsClient("probe");
  const log = (await probe.rpc("mock_share", { session_id: SID, name: "build.log", text: LOG })).result;
  const readme = (await probe.rpc("mock_share", { session_id: SID, name: "README.md", text: README })).result;
  const pdf = (await probe.rpc("mock_share", { session_id: SID, name: "Q3 report.pdf", size: 4096 })).result;
  const up = (await probe.rpc("mock_share", { session_id: SID, name: "todo.txt", source: "upload", text: "buy milk\ncall the bank\n" })).result;
  const card = (f) => `.shared-file[data-file="${f.id}"]`;

  // ── a browser on a computer ─────────────────────────────────────────────
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Desktop");
  await p.theme("dark");
  await ws(p, `m.openSession('${SID}', { pane: m.workspace.focused() })`);
  await waitFor(() => p.eval(`!!document.querySelector('${card(log)}')`), 8000);
  await readChat(p);
  await p.eval(`document.querySelector('${card(log)}').scrollIntoView({ block: 'center' })`);
  await sleep(300);
  console.log("— the default: a tile");
  check(await p.eval(`document.querySelector('${card(log)} .shared-file-main')?.tagName === 'BUTTON'`), "a text file's name is a button (it opens)");
  check(await p.eval(`document.querySelector('${card(pdf)} .shared-file-main')?.tagName === 'SPAN'`), "a PDF's name isn't (in a browser it can only be downloaded, as before)");
  check(await p.eval(`!!document.querySelector('${card(log)} .shared-file-get')`), "Download is where it was");
  await p.shot(`${SHOTS}/files-open-cards-desktop.png`);
  await p.click(`${card(log)} .shared-file-main`);
  let tiles = await waitFor(async () => {
    const t = await fileTiles(p);
    return t.find((x) => x.name === "build.log" && x.text) ? t : null;
  });
  const lt = tiles?.find((x) => x.name === "build.log");
  check(!!lt && lt.text.startsWith("2026-10-04T09:00:00Z WARN"), "a tap opens it in a tile beside the chat, its text read over the connection", lt);
  check(!!lt && /400 lines/.test(lt.sub) && lt.gutter, "…with its size and line count, and line numbers (a log)", lt?.sub);
  check(!(await p.eval(`window.__opened?.length`)), "nothing opened outside the app");
  await sleep(300);
  await p.shot(`${SHOTS}/files-open-tile-desktop.png`);
  // Find.
  const lsel = `[data-pane-id="${lt.pane}"]`;
  await p.click(`${lsel} [aria-label="Find in file"]`);
  await sleep(300);
  check(await p.eval(`!!document.querySelector('${lsel} .cm-search input[name=search]')`), "Find opens a find box in the tile");
  await p.eval(`(() => { const i = document.querySelector('${lsel} .cm-search input[name=search]'); i.value = 'WARN'; i.dispatchEvent(new Event('change', { bubbles: true })); i.dispatchEvent(new KeyboardEvent('keyup', { key: 'N', bubbles: true })); })()`);
  await sleep(300);
  check(await p.eval(`document.querySelectorAll('${lsel} .cm-searchMatch').length > 0`), "…and finds what's typed", await p.eval(`document.querySelectorAll('${lsel} .cm-searchMatch').length`));
  await p.shot(`${SHOTS}/files-open-find-desktop.png`);
  // Its menu.
  await p.click(`${lsel} [aria-label="File options"]`);
  await sleep(250);
  let ml = await menuLabels(p);
  check(["Find", "Wrap long lines", "Copy all", "Save as note", "Open in a new window", "Download", "Close tile"].every((x) => ml.includes(x)) && !ml.some((x) => /computer's app|phone/.test(x)), "the tile's menu: find, wrap, copy, save as note, a new window, download, close (no device app in a browser)", ml);
  await p.clickText(".menu-item", "Wrap long lines");
  await sleep(200);
  check(!(await p.eval(`!!document.querySelector('${lsel} .cm-lineWrapping')`)), "Wrap long lines turns wrapping off (and on again)");
  // Save as note.
  await p.click(`${lsel} [aria-label="File options"]`);
  await sleep(250);
  await p.clickText(".menu-item", "Save as note");
  const note = await waitFor(async () => (await probe.rpc("list_notes")).result?.find?.((n) => n.title === "build.log"));
  check(!!note, "Save as note makes a note of it", note?.title);
  const nfull = note && (await probe.rpc("get_note", { id: note.id })).result;
  check(!!nfull && nfull.content.startsWith("```\n2026-10-04T09:00:00Z") && nfull.content.trimEnd().endsWith("```"), "…the log in a code block", nfull?.content?.slice(0, 40));
  check(await waitFor(() => p.eval(`!!document.querySelector('.note-tile')`)), "…and opens it");

  // The ⋯ menu on a card, and a Markdown file drawn as a page.
  await readChat(p);
  await p.eval(`document.querySelector('${card(readme)}').scrollIntoView({ block: 'center' })`);
  await p.click(`${card(readme)} .shared-file-more`);
  await sleep(250);
  ml = await menuLabels(p);
  check(ml[0] === "Open in a tile" && ml.includes("Open in a new window") && ml.includes("Download") && ml.includes("Save as note"), "a file's ⋯ menu: the default way first, the others, Download, Save as note", ml);
  await p.clickText(".menu-item", "Open in a tile");
  tiles = await waitFor(async () => {
    const t = await fileTiles(p);
    return t.find((x) => x.name === "README.md" && x.md) ? t : null;
  });
  const mt = tiles?.find((x) => x.name === "README.md");
  check(!!mt && mt.md.includes("Release notes") && !mt.md.includes("# Release"), "a Markdown file opens drawn as a page", mt);
  await sleep(300);
  await p.shot(`${SHOTS}/files-open-markdown-desktop.png`);
  const msel = `[data-pane-id="${mt.pane}"]`;
  await p.click(`${msel} [aria-label="File options"]`);
  await sleep(250);
  await p.clickText(".menu-item", "Show the Markdown source");
  await sleep(500);
  check(await waitFor(() => p.eval(`(document.querySelector('${msel} .cm-content')?.textContent ?? '').includes('# Release notes')`)), "…and as its source from the menu");
  // A right-click on a card opens the same menu.
  await readChat(p);
  await p.eval(`(() => { const c = document.querySelector('${card(log)}'); c.scrollIntoView({ block: 'center' }); const r = c.getBoundingClientRect(); c.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: r.x + 40, clientY: r.y + 10 })); })()`);
  await sleep(250);
  check((await menuLabels(p)).includes("Open in a new window"), "a right-click on a file opens its menu");
  await closeMenus(p);
  // An attached text file: its ⋯ next to the chip (the chip still opens what was sent, as before).
  await readChat(p);
  const chipMore = await p.eval(`(() => { const a = [...document.querySelectorAll('.file-att')].find(x => x.textContent.includes('todo.txt')); a?.scrollIntoView({ block: 'center' }); return !!a?.querySelector('.file-more'); })()`);
  check(chipMore, "an attached text file has a ⋯ beside it");
  await p.eval(`[...document.querySelectorAll('.file-att')].find(x => x.textContent.includes('todo.txt')).querySelector('.file-chip').click()`);
  await sleep(200);
  check(await p.eval(`!![...document.querySelectorAll('.file-att')].find(x => x.textContent.includes('todo.txt'))?.querySelector('.file-att-text')`), "…and a tap on its chip still shows what was sent, as before");
  await p.eval(`[...document.querySelectorAll('.file-att')].find(x => x.textContent.includes('todo.txt')).querySelector('.file-more').click()`);
  await sleep(250);
  ml = await menuLabels(p);
  check(ml.includes("Open in a tile") && ml.includes("Download") && ml.includes("Save as note"), "its menu opens it, downloads it, makes a note", ml);
  await p.clickText(".menu-item", "Open in a tile");
  check(!!(await waitFor(async () => (await fileTiles(p)).find((x) => x.name === "todo.txt" && x.text?.includes("call the bank")))), "…Open in a tile shows its text");

  // A new window: the app's own page with just the file (no external page, no app started).
  console.log("— a new window");
  const c = await browser();
  const before = (await c.send("Target.getTargets")).targetInfos.map((t) => t.targetId);
  await readChat(p);
  await p.eval(`document.querySelector('${card(log)}').scrollIntoView({ block: 'center' })`);
  await p.click(`${card(log)} .shared-file-more`);
  await sleep(250);
  await p.clickText(".menu-item", "Open in a new window");
  const win = await waitFor(async () => (await c.send("Target.getTargets")).targetInfos.find((t) => t.type === "page" && !before.includes(t.targetId) && t.url.includes("?view=")));
  check(!!win && win.url.startsWith(APP.replace(/\/$/, "")), "Open in a new window opens the app's own page in a window (not another site)", win?.url);
  if (win) {
    const { sessionId } = await c.send("Target.attachToTarget", { targetId: win.targetId, flatten: true });
    const ev = (expr) => c.send("Runtime.evaluate", { expression: expr, returnByValue: true, awaitPromise: true }, sessionId).then((r) => r.result.value);
    const shown = await waitFor(() => ev(`document.querySelector('.file-window .cm-content')?.textContent?.slice(0, 40) ?? null`));
    check(shown?.startsWith("2026-10-04T09:00:00Z WARN"), "…which shows the file", shown);
    check((await ev(`document.title`)) === "build.log — Tessera" && !(await ev(`!!document.querySelector('.workspace')`)), "…titled by the file, and nothing of the app around it", await ev(`document.title`));
    const res = await ev(`performance.getEntriesByType('resource').filter(e => /\\/api\\//.test(e.name)).length`);
    check(res === 0, "…and it never talks to the server", res);
    await c.send("Page.captureScreenshot", { format: "png" }, sessionId).then((r) => import("node:fs").then((fs) => fs.writeFileSync(`${SHOTS}/files-open-window-browser.png`, Buffer.from(r.data, "base64"))));
    await c.send("Target.closeTarget", { targetId: win.targetId });
  }

  // Settings: where a tap opens files.
  console.log("— settings");
  await openWhereFiles(p);
  await waitFor(() => p.eval(`!!document.querySelector('.open-files-choice')`));
  const opts = await p.eval(`[...document.querySelectorAll('.open-files-choice button')].map(b => b.textContent.trim() + (b.getAttribute('aria-checked') === 'true' ? '*' : ''))`);
  check(opts.join("|") === "A tile*|A new window", "Settings → Appearance → More: Where files open — A tile (the default) or A new window, in a browser", opts);
  await p.shot(`${SHOTS}/files-open-settings-desktop.png`);
  await p.eval(`[...document.querySelectorAll('.open-files-choice button')].find(b => b.textContent.includes('A new window')).click()`);
  check((await p.eval(`JSON.parse(localStorage.getItem('tessera.prefs')).openFiles`)) === "window", "the choice is kept on this device");
  await p.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab(null))`);
  await sleep(300);
  const before2 = (await c.send("Target.getTargets")).targetInfos.map((t) => t.targetId);
  await readChat(p);
  await p.eval(`document.querySelector('${card(log)}').scrollIntoView({ block: 'center' })`);
  await p.click(`${card(log)} .shared-file-main`);
  const win2 = await waitFor(async () => (await c.send("Target.getTargets")).targetInfos.find((t) => t.type === "page" && !before2.includes(t.targetId) && t.url.includes("?view=")));
  check(!!win2, "with A new window, a tap on a text file opens it in a window", win2?.url);
  if (win2) await c.send("Target.closeTarget", { targetId: win2.targetId });
  await p.eval(`__mod('lib/prefs.ts').then(m => m.setPrefs('openFiles', 'tile'))`);

  // Kept on this device only, and back after a reload.
  const shared = (await probe.rpc("get_mosaics")).result;
  const sharedTiles = JSON.stringify(shared);
  check(!sharedTiles.includes("build.log") && !sharedTiles.includes('"file"'), "file tiles never go to the server's shared layout");
  await p.reload(1800);
  await p.waitFor(`document.querySelector('.workspace')`, 10000);
  check(!!(await waitFor(async () => (await fileTiles(p)).find((x) => x.name === "build.log" && x.text))), "after a reload the file's tile is back with its text");
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    await sleep(250);
    await p.shot(`${SHOTS}/files-open-desktop-${theme}.png`);
  }
  await p.theme("dark");
  check(!p.errors.length, "desktop: no page errors", p.errors);
  await p.close();

  // ── phones: a folded one (344 px) and the unfolded Fold ────────────────
  for (const [label, w, h] of [["phone", 344, 760], ["fold", 841, 701]]) {
    console.log(`— ${label}`);
    await mock("/__mock/mosaics/reset");
    const ph = await device();
    await ph.viewport(w, h, true, 2);
    await signIn(ph, label);
    await ws(ph, `m.openSession('${SID}', { pane: m.workspace.focused() })`);
    await waitFor(() => ph.eval(`!!document.querySelector('${card(log)}')`), 8000);
    await readChat(ph);
    const fit = await ph.eval(`(() => { const c = document.querySelector('${card(log)}'); c.scrollIntoView({ block: 'center' }); const r = c.getBoundingClientRect(); const m = c.querySelector('.shared-file-more').getBoundingClientRect(); return { left: r.left, right: r.right, more: m.right <= r.right && m.width >= 24, w: innerWidth }; })()`);
    check(fit.left >= 0 && fit.right <= fit.w && fit.more, `${label}: the file card fits, its ⋯ inside it`, fit);
    await sleep(200);
    await ph.shot(`${SHOTS}/files-open-cards-${label}.png`);
    await ph.click(`${card(log)} .shared-file-more`);
    await sleep(250);
    ml = await menuLabels(ph);
    check(ml.includes("Open in a tile") && !ml.includes("Open in a new window"), `${label}: no new windows in a phone's browser`, ml);
    const pop = await ph.eval(`(() => { const r = document.querySelector('.menu-pop').getBoundingClientRect(); return { h: r.height, w: r.width, vw: innerWidth, vh: innerHeight, sheet: !!document.querySelector('.menu-pop.sheet, .sheet .menu-pop') }; })()`);
    check(pop.h < pop.vh * 0.6 && pop.w <= pop.vw && !pop.sheet && !(await ph.eval(`!!document.querySelector('.popover.as-sheet')`)), `${label}: the menu is a small one beside the file (no sheet)`, pop);
    await ph.shot(`${SHOTS}/files-open-menu-${label}.png`);
    await ph.clickText(".menu-item", "Open in a tile");
    const t = await waitFor(async () => (await fileTiles(ph)).find((x) => x.name === "build.log" && x.text));
    check(!!t, `${label}: it opens in a tile`, t);
    const head = await ph.eval(`(() => { const h = document.querySelector('.file-tile .pane-head'); const r = h.getBoundingClientRect(); return { h: r.height, fits: h.scrollWidth <= h.clientWidth + 1 }; })()`);
    check(head.h <= 60 && head.fits, `${label}: its header is one line that fits`, head);
    check(await ph.eval(`document.querySelector('.file-tile .cm-content')?.getAttribute('contenteditable') === 'false'`), `${label}: reading doesn't bring up the keyboard`);
    for (const theme of ["dark", "light"]) {
      await ph.theme(theme);
      await sleep(250);
      await ph.shot(`${SHOTS}/files-open-tile-${label}-${theme}.png`);
    }
    await ph.theme("dark");
    // A phone's browser has one way only: no choice in Settings.
    await openWhereFiles(ph);
    await sleep(400);
    check(!(await ph.eval(`!!document.querySelector('.open-files-choice')`)), `${label}: in a phone's browser Settings has nothing to choose (files open in a tile)`);
    await ph.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab(null))`);
    check(!ph.errors.length, `${label}: no page errors`, ph.errors);
    await ph.close();
  }

  // ── the Linux desktop app (a stand-in shell) ───────────────────────────
  console.log("— the desktop app");
  await mock("/__mock/mosaics/reset");
  const pair = async (name) => (await (await fetch(MOCK + "/api/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "123456", device_name: name }) })).json()).token;
  const a = await device();
  await a.viewport(1280, 820);
  await a.send("Page.addScriptToEvaluateOnNewDocument", { source: shellScript(await pair("Desktop app")) });
  await a.goto(APP, 1800);
  await a.waitFor(`document.querySelector('.workspace')`, 12000);
  await ws(a, `m.openSession('${SID}', { pane: m.workspace.focused() })`);
  await waitFor(() => a.eval(`!!document.querySelector('${card(log)}')`), 8000);
  await readChat(a);
  const calls = (cmd) => a.eval(`window.__calls.filter(c => c.cmd === ${JSON.stringify(cmd)}).map(c => c.args)`);
  // A chat that just opened puts back where it was scrolled once it has drawn (a script's scroll
  // isn't the user's): scroll the card into view until it stays there, then click its ⋯.
  for (let i = 0; i < 8; i++) {
    await a.eval(`document.querySelector('${card(log)}').scrollIntoView({ block: 'center', behavior: 'instant' })`);
    await sleep(500);
    if (await a.eval(`(() => { const r = document.querySelector('${card(log)} .shared-file-more').getBoundingClientRect(); return r.bottom < innerHeight - 80 && r.top > 60; })()`)) break;
  }
  await a.click(`${card(log)} .shared-file-more`);
  await sleep(250);
  ml = await menuLabels(a);
  check(["Open in a tile", "Open in a new window", "Open with this computer's app", "Save on this computer", "Save as note"].every((x) => ml.includes(x)), "desktop app: the menu offers a tile, a window, this computer's app, saving and a note", ml);
  await a.clickText(".menu-item", "Open in a new window");
  const fw = await waitFor(async () => (await calls("file_window"))[0]);
  check(fw?.name === "build.log" && fw.text?.startsWith("2026-10-04T09:00:00Z") && fw.text.split("\n").length === 400, "desktop app: a new window gets the file's text (the app makes the window)", fw && { name: fw.name, lines: fw.text?.split("\n").length });
  await a.click(`${card(log)} .shared-file-more`);
  await sleep(250);
  await a.clickText(".menu-item", "Open with this computer's app");
  const ow = await waitFor(async () => (await calls("file_open_with"))[0]);
  check(ow?.id === log.id && ow.name === "build.log" && !ow.data, "desktop app: this computer's app gets the shared file by id (no save dialog)", ow);
  // A program is never offered to the computer.
  const exe = (await probe.rpc("mock_share", { session_id: SID, name: "setup.exe", size: 4096 })).result;
  await waitFor(() => a.eval(`!!document.querySelector('${card(exe)}')`));
  await readChat(a);
  await a.eval(`document.querySelector('${card(exe)}').scrollIntoView({ block: 'center' })`);
  await a.click(`${card(exe)} .shared-file-more`);
  await sleep(250);
  ml = await menuLabels(a);
  check(!ml.some((x) => /computer's app/.test(x)) && ml.includes("Save on this computer"), "desktop app: a program can only be saved, never opened from here", ml);
  await closeMenus(a);
  // The choice: this computer's app, then a tap opens it there.
  await openWhereFiles(a);
  await waitFor(() => a.eval(`!!document.querySelector('.open-files-choice')`));
  const aopts = await a.eval(`[...document.querySelectorAll('.open-files-choice button')].map(b => b.textContent.trim())`);
  check(aopts.join("|") === "A tile|A new window|This computer's app", "desktop app: Settings offers a tile, a new window and this computer's app", aopts);
  await a.shot(`${SHOTS}/files-open-settings-app.png`);
  await a.eval(`[...document.querySelectorAll('.open-files-choice button')].find(b => b.textContent.includes("computer's app")).click()`);
  await a.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab(null))`);
  await sleep(300);
  await a.eval(`window.__calls = []`);
  await readChat(a);
  await a.eval(`document.querySelector('${card(readme)}').scrollIntoView({ block: 'center' })`);
  await a.click(`${card(readme)} .shared-file-main`);
  const ow2 = await waitFor(async () => (await calls("file_open_with"))[0]);
  check(ow2?.id === readme.id, "desktop app: with This computer's app, a tap opens it there", ow2);
  // An attached file without a copy on the server (a private chat's): its text goes as data.
  await a.eval(`__mod('lib/fileOpen.ts').then(m => m.openWithApp({ name: 'notes.txt', text: 'hello' }))`);
  const ow3 = await waitFor(async () => (await calls("file_open_with")).find((x) => x.name === "notes.txt"));
  check(ow3 && atob(ow3.data) === "hello" && !ow3.id, "desktop app: text the chat has goes as data", ow3);
  check(!a.errors.length, "desktop app: no page errors", a.errors);
  await a.close();

  // The desktop app's file window: the page shows the file it was handed and never starts the app.
  const fwin = await device();
  await fwin.viewport(900, 720);
  await fwin.send("Page.addScriptToEvaluateOnNewDocument", { source: shellScript("unused", "file-0123456789abcdef") + `;window.__windowFile = { name: 'build.log', mime: 'text/plain', text: ${JSON.stringify(LOG.slice(0, 2000))} };` });
  await fwin.goto(APP, 1800);
  const fshown = await waitFor(() => fwin.eval(`document.querySelector('.file-window .cm-content')?.textContent?.slice(0, 30) ?? null`));
  check(fshown?.startsWith("2026-10-04T09:00:00Z"), "desktop app's file window: shows the file it was handed", fshown);
  const fcalls = await fwin.eval(`window.__calls.map(c => c.cmd)`);
  check(fcalls.join() === "file_window_take", "…and asks for nothing else (no server, no app)", fcalls);
  await fwin.shot(`${SHOTS}/files-open-window-app.png`);
  await fwin.theme("light");
  await sleep(200);
  await fwin.shot(`${SHOTS}/files-open-window-app-light.png`);
  check(!fwin.errors.length, "file window: no page errors", fwin.errors);
  await fwin.close();

  // ── an Android stand-in: saved to Downloads, then opened ────────────────
  console.log("— Android");
  const and = await device();
  await and.viewport(384, 832, true, 2);
  await and.send("Page.addScriptToEvaluateOnNewDocument", { source: shellScript(await pair("Phone app")) });
  await and.goto(APP, 1800);
  await and.waitFor(`document.querySelector('.workspace')`, 12000);
  await ws(and, `m.openSession('${SID}', { pane: m.workspace.focused() })`);
  await and.waitFor(`document.querySelector('.pane.focused .transcript')`);
  await readChat(and);
  await waitFor(() => and.eval(`!!document.querySelector('${card(log)}')`), 8000);
  // Clicked in place (the chat may still be following its end and move under a pointer).
  await sleep(800);
  await and.eval(`(() => { const b = document.querySelector('${card(log)} .shared-file-more'); b.scrollIntoView({ block: 'center' }); b.click(); })()`);
  await sleep(250);
  ml = await menuLabels(and);
  check(ml.includes("Open with an app on this phone") && ml.includes("Save to phone") && !ml.includes("Open in a new window"), "Android: the menu offers an app on this phone and Save to phone (no windows)", ml);
  await and.clickText(".menu-item", "Open with an app on this phone");
  const dl = await waitFor(async () => {
    const all = await and.eval(`window.__calls.map(c => c.cmd)`);
    return all.includes("file_open") ? all : null;
  });
  check(!!dl && dl.indexOf("file_download") >= 0 && dl.indexOf("file_download") < dl.indexOf("file_open"), "Android: it is saved to Downloads, then opened with the phone's app", dl?.filter((x) => /file_/.test(x)));
  const dlArgs = await and.eval(`window.__calls.filter(c => c.cmd === 'file_download' || c.cmd === 'file_open').map(c => c.args)`);
  check(dlArgs[0]?.id === log.id && dlArgs[0]?.name === "build.log" && dlArgs[1]?.location?.startsWith("content://"), "Android: …that file (its Downloads copy is what opens)", dlArgs);
  await openWhereFiles(and);
  await waitFor(() => and.eval(`!!document.querySelector('.open-files-choice')`));
  const andOpts = await and.eval(`[...document.querySelectorAll('.open-files-choice button')].map(b => b.textContent.trim())`);
  check(andOpts.join("|") === "A tile|An app on this phone", "Android: Settings offers a tile or an app on this phone", andOpts);
  await and.shot(`${SHOTS}/files-open-settings-android.png`);
  check(!and.errors.length, "Android: no page errors", and.errors);
  await and.close();
  probe.close();
  finish();
});
