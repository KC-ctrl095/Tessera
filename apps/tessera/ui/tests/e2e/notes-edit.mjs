// Notes, editing like a document: pictures pasted on every platform (a
// browser's paste, one listed only under `items`, an Android keyboard's
// picture, and in the Linux desktop app the picture its shell reads from the
// system clipboard, a browser's "Copy Image", files copied or dragged from a
// file manager), a picture made wider or narrower with its handle, dragged
// to another place, moved and sized from its menu, the header's picture
// button; and the selection's format bubble, which never covers what's
// selected and stays inside the tile (first line, last visible line, a
// scrolled note, a narrow window), on computers and phones, light and dark.
import { APP, MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

/** A picture made here: `w`×`h`, one colour (PNG), as a File. */
const makePng = (w, h, color, name = "beach.png") =>
  `await new Promise((res) => { const c = document.createElement('canvas'); c.width = ${w}; c.height = ${h}; const g = c.getContext('2d'); g.fillStyle = '${color}'; g.fillRect(0, 0, ${w}, ${h}); c.toBlob((b) => res(new File([b], '${name}', { type: 'image/png' })), 'image/png'); })`;
/** The same picture as base64 (what the shell and Android's activity hand over). */
const pngB64 = (w, h, color) =>
  `(() => { const c = document.createElement('canvas'); c.width = ${w}; c.height = ${h}; const g = c.getContext('2d'); g.fillStyle = '${color}'; g.fillRect(0, 0, ${w}, ${h}); const v = c.toDataURL('image/png'); return v.slice(v.indexOf(',') + 1); })()`;

/**
 * A stand-in for the Linux desktop app's shell (Tauri), as in paste.mjs: the
 * app's commands go to the mock over a WebSocket; the clipboard commands
 * answer from `window.__clip`; every command is logged in `window.__calls`.
 */
function shellScript(token) {
  return `(() => {
    const TOKEN = ${JSON.stringify(token)};
    let n = 0, ws = null, rid = 0;
    const cbs = {}, listeners = {}, pending = new Map();
    window.__calls = [];
    window.__clip = {};
    if (navigator.clipboard) navigator.clipboard.read = () => new Promise(() => {});
    const none = { files: [], skipped: [] };
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
      metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main", windowLabel: "main" } },
      transformCallback(cb, once) { const id = ++n; cbs[id] = (x) => { if (once) delete cbs[id]; cb(x); }; return id; },
      async invoke(cmd, args = {}) {
        if (!cmd.startsWith("plugin:") && cmd !== "rpc") window.__calls.push(cmd);
        switch (cmd) {
          case "plugin:event|listen": (listeners[args.event] ||= []).push(args.handler); return args.handler;
          case "plugin:event|unlisten": return null;
          case "servers_get": return { servers: [{ id: "srv_mock", name: "Mock server", endpoints: [location.origin], headers: {}, token: "", serverKey: "", fingerprint: "1111 2222", deviceName: "Desktop app" }], active: "srv_mock" };
          case "servers_save": return null;
          case "server_connect": if (!ws) connect(); else status("connected"); return null;
          case "server_disconnect": return null;
          case "app_resumed": return null;
          case "rpc": return new Promise((resolve, reject) => { const id = ++rid; pending.set(id, { resolve, reject }); const r = args.request; ws.send(JSON.stringify({ id, method: r.method, ...(r.params !== undefined ? { params: r.params } : {}) })); });
          case "clipboard_files": return window.__clip.files ?? none;
          case "clipboard_image": return window.__clip.image ?? null;
          case "dropped_files": return window.__clip.dropped ?? none;
          default: throw "command " + cmd + " not found";
        }
      },
    };
  })()`;
}

const intersects = (a, b) => a.left < b.right - 0.5 && a.right > b.left + 0.5 && a.top < b.bottom - 0.5 && a.bottom > b.top + 0.5;
const inside = (a, b) => a.left >= b.left - 0.5 && a.right <= b.right + 0.5 && a.top >= b.top - 0.5 && a.bottom <= b.bottom + 0.5;

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const probe = await wsClient("probe");
  const server = async (id) => (await probe.rpc("get_note", { id })).result;
  const until = async (fn, ms = 8000) => {
    const t0 = Date.now();
    while (Date.now() - t0 < ms) {
      if (await fn()) return true;
      await sleep(120);
    }
    return false;
  };

  /** Open a new note with `content` in a tile of its own on page `pg`. */
  const show = async (pg, title, content) => {
    const n = (await probe.rpc("save_note", { title, content })).result;
    await pg.eval(`__mod('lib/notes.ts').then(m => m.openNote('${n.id}', { newPane: true }))`);
    let pane = null;
    await until(async () => (pane = await pg.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().find(id => m.workspace.panes[id].content.noteId === '${n.id}') ?? null)`)) && (await pg.eval(`!!document.querySelector('[data-pane-id="${pane}"] .note-ed .cm-content')`)), 10000);
    await sleep(300);
    return { id: n.id, pane, sel: `[data-pane-id="${pane}"]` };
  };
  const V = (t) => `document.querySelector('${t.sel} .cm-content').cmTile.root.view`;
  const doc = (pg, t) => pg.eval(`${V(t)}.state.doc.toString()`);
  const pics = (pg, t) => pg.eval(`${V(t)}.state.doc.toString().match(/!\\[[^\\]]*\\]\\(assets\\/[0-9a-f]{16}\\.\\w+\\)/g) ?? []`);
  const caretEnd = (pg, t) => pg.eval(`(() => { const v = ${V(t)}; v.focus(); v.dispatch({ selection: { anchor: v.state.doc.length } }); })()`);
  const closeMenus = (pg) => pg.eval(`(document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })), true)`);

  // ── a browser on a computer ─────────────────────────────────────────────
  const p = await device();
  await p.viewport(1280, 860);
  await signIn(p, "Desktop");
  await p.theme("dark");

  console.log("— pictures pasted in a browser");
  const t1 = await show(p, "Album", "Intro line.\n\nMiddle paragraph with a few words.\n\nLast paragraph here.");
  await caretEnd(p, t1);
  // A paste whose picture is listed only under `items` (some engines hand it over that way).
  const itemsOnly = await p.eval(`(async () => {
    const f = ${makePng(320, 200, "#3a7", "image.png")};
    const dt = { files: [], items: [{ kind: 'file', type: 'image/png', getAsFile: () => f }], types: ['Files'], getData: () => '' };
    const e = new Event('paste', { bubbles: true, cancelable: true });
    Object.defineProperty(e, 'clipboardData', { value: dt });
    ${V(t1)}.contentDOM.dispatchEvent(e);
    return e.defaultPrevented;
  })()`);
  await until(async () => (await pics(p, t1)).length === 1);
  check(itemsOnly && (await pics(p, t1)).length === 1, "a picture listed only under the paste's items goes into the note", await doc(p, t1));
  // An Android keyboard's picture (MainActivity hands it over as `tessera:content`): for the note being written in.
  await caretEnd(p, t1);
  const b64 = await p.eval(pngB64(200, 120, "#c63"));
  await p.eval(`window.dispatchEvent(new CustomEvent('tessera:content', { detail: { items: [{ mime: 'image/png', data: ${JSON.stringify(b64)} }] } }))`);
  await until(async () => (await pics(p, t1)).length === 2);
  check((await pics(p, t1)).length === 2, "a picture from an Android keyboard goes into the note being written in", await doc(p, t1));
  // …but not when the note isn't being written in (it's the message box's then, or no one's).
  await p.eval(`document.activeElement?.blur()`);
  await p.eval(`window.dispatchEvent(new CustomEvent('tessera:content', { detail: { items: [{ mime: 'image/png', data: ${JSON.stringify(b64)} }] } }))`);
  await sleep(800);
  check((await pics(p, t1)).length === 2, "…and not into a note nobody is writing in");
  await until(async () => ((await server(t1.id)).content.match(/assets\//g) ?? []).length === 2);
  check(((await server(t1.id)).content.match(/assets\//g) ?? []).length === 2, "both are saved with the note");

  console.log("— a picture's handle, menu and dragging");
  // The first picture: click it (its menu opens), close the menu, then drag its handle.
  const picBox = (i = 0) => p.eval(`(() => { const e = document.querySelectorAll('${t1.sel} .cm-pic')[${i}]; e.scrollIntoView({ block: 'center' }); const r = e.getBoundingClientRect(); const h = e.querySelector('.cm-pic-handle').getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height, hx: h.x + h.width / 2, hy: h.y + h.height / 2, hv: getComputedStyle(e.querySelector('.cm-pic-handle')).display !== 'none' }; })()`);
  let b = await picBox(0);
  check(!b.hv, "the handle shows only on a selected picture");
  await p.mouse(b.x + b.w / 2, b.y + b.h / 2);
  await sleep(300);
  const pm = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-label')].map(e => e.textContent)`);
  check(["View", "Small", "Medium", "Large", "Full width", "Move up", "Move down", "Description…", "Remove"].every((x) => pm.includes(x)), "a picture's menu: view, Small/Medium/Large/Full width, move up/down, description, remove", pm);
  await closeMenus(p);
  await sleep(250);
  b = await picBox(0);
  check(b.hv, "a selected picture shows its handle");
  await p.shot(`${SHOTS}/notes-edit-handle.png`, { x: b.x - 20, y: b.y - 20, width: Math.min(600, b.w + 80), height: b.h + 40 });
  await p.down(b.hx, b.hy);
  for (let k = 1; k <= 8; k++) await p.move(b.hx - (120 * k) / 8, b.hy);
  await p.up(b.hx - 120, b.hy);
  await sleep(300);
  let d = await doc(p, t1);
  const w1 = Number(/!\[[^\]|]*\|(\d+)\]\(assets\//.exec(d)?.[1]);
  check(w1 === 200, "dragging the handle 120 px to the left makes the picture 200 px wide, written as alt|200", d.match(/!\[[^\]]*\]/g));
  b = await picBox(0);
  check(Math.abs(b.w - 200) <= 1, "…and it is drawn that wide", b.w);
  check(!(await p.eval(`!!document.querySelector('.menu-pop')`)), "…without opening its menu");
  // Dragged below the last paragraph.
  const before = await doc(p, t1);
  const last = await p.eval(`(() => { const w = document.createTreeWalker(document.querySelector('${t1.sel} .cm-content'), NodeFilter.SHOW_TEXT); let n; while ((n = w.nextNode()) && !n.data.includes('Last paragraph')); const r = n.parentElement.getBoundingClientRect(); return { x: r.x + 40, y: r.y + r.height * 0.8 }; })()`);
  b = await picBox(0);
  await p.down(b.x + b.w / 2, b.y + b.h / 2);
  await p.move(b.x + b.w / 2 + 4, b.y + b.h / 2 + 10);
  for (let k = 1; k <= 8; k++) await p.move(last.x, b.y + b.h / 2 + ((last.y - b.y - b.h / 2) * k) / 8);
  const marker = await p.eval(`!!document.querySelector('${t1.sel} .cm-pic-drop')`);
  await p.up(last.x, last.y);
  await sleep(300);
  d = await doc(p, t1);
  check(marker, "while dragging, a line shows where it will go");
  check(d.indexOf("|200](assets/") > d.indexOf("Last paragraph"), "a picture dragged below the last paragraph moves there", d);
  check(!/\n{3,}/.test(d), "…leaving no gap behind", JSON.stringify(d));
  await p.eval(`(() => { const v = ${V(t1)}; v.focus(); })()`);
  await p.key("z", "KeyZ", 2, 90);
  await sleep(200);
  check((await doc(p, t1)) === before, "one Undo puts it back", await doc(p, t1));
  // From the menu: Large, then Move down.
  b = await picBox(0);
  await p.mouse(b.x + b.w / 2, b.y + b.h / 2);
  await sleep(300);
  await p.clickText(".menu-item", "Large");
  await sleep(250);
  check(/!\[[^\]|]*\|720\]\(assets\//.test(await doc(p, t1)), "Large writes alt|720", (await doc(p, t1)).match(/!\[[^\]]*\]/g));
  b = await picBox(0);
  await p.mouse(b.x + b.w / 2, b.y + b.h / 2);
  await sleep(300);
  await p.clickText(".menu-item", "Move down");
  await sleep(250);
  d = await doc(p, t1);
  check(d.indexOf("|720](assets/") > d.indexOf("Middle paragraph"), "Move down moves it past the next paragraph", d);
  // The header's picture button (computers).
  await p.eval(`(() => { const i = document.querySelector('${t1.sel} input[type=file]:not([capture])'); i.click = () => { window.__picked = true; }; })()`);
  check(await p.eval(`!!document.querySelector('${t1.sel} .note-add-pic')`), "the note's header has a picture button on computers");
  await p.click(`${t1.sel} .note-add-pic`);
  await sleep(200);
  check(await p.eval(`window.__picked === true`), "…which opens the picker");
  await p.shot(`${SHOTS}/notes-edit-pictures-desktop.png`);
  await p.theme("light");
  await sleep(200);
  await p.shot(`${SHOTS}/notes-edit-pictures-desktop-light.png`);
  await p.theme("dark");

  // ── the format bubble over a selection ─────────────────────────────────
  console.log("— the format bubble");
  const long = Array.from({ length: 60 }, (_, i) => `${i + 1}. line number ${i + 1} of a long list that keeps going`).join("\n");
  const t2 = await show(p, "Tessera fixes", long);
  /** Select `from`–`to` and measure the bubble against the selection and the tile. */
  const bubbleAt = (pg, t, from, to) =>
    pg.eval(`(async () => {
      const v = ${V(t)};
      v.focus();
      v.dispatch({ selection: { anchor: ${from}, head: ${to} } });
      await new Promise((r) => setTimeout(r, 350));
      const tip = document.querySelector('${t.sel} .cm-bubble')?.closest('.cm-tooltip');
      const s = getSelection();
      const sel = s.rangeCount ? s.getRangeAt(0).getBoundingClientRect().toJSON() : null;
      const tile = document.querySelector('${t.sel}').getBoundingClientRect().toJSON();
      const head = document.querySelector('${t.sel} .pane-head').getBoundingClientRect().toJSON();
      const r = tip?.getBoundingClientRect().toJSON();
      return { tip: r && r.top > -1000 ? r : null, sel, tile, head };
    })()`);
  const lineAt = (pg, t, n) => pg.eval(`(() => { const l = ${V(t)}.state.doc.line(${n}); return [l.from + 3, l.to]; })()`);
  const expectBubble = (r, what) => {
    check(!!r.tip && !!r.sel && !intersects(r.tip, r.sel), `${what}: the bubble doesn't cover the selection`, r);
    check(!!r.tip && inside(r.tip, r.tile) && r.tip.top >= r.head.bottom - 0.5, `${what}: …and is inside the tile, under its header`, r.tip && { tip: r.tip, tile: r.tile, head: r.head });
  };
  let [f, t] = await lineAt(p, t2, 1);
  let r = await bubbleAt(p, t2, f, t);
  expectBubble(r, "first line");
  check(!!r.tip && r.tip.top >= r.sel.bottom - 0.5, "first line: …it goes below the line (no room above it)", r);
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    await sleep(200);
    await p.shot(`${SHOTS}/notes-edit-bubble-first-${theme}.png`, { x: r.tile.x, y: r.tile.y, width: r.tile.width, height: 220 });
  }
  await p.theme("dark");
  [f] = await lineAt(p, t2, 1);
  [, t] = await lineAt(p, t2, 2);
  expectBubble(await bubbleAt(p, t2, f, t), "first two lines");
  // A line in the middle: above it, as before.
  [f, t] = await lineAt(p, t2, 8);
  r = await bubbleAt(p, t2, f, t);
  expectBubble(r, "a middle line");
  check(!!r.tip && r.tip.bottom <= r.sel.top + 0.5, "a middle line: …above it", r);
  // The last line in view.
  const lastVisible = await p.eval(`(() => { const v = ${V(t2)}; const b = v.scrollDOM.getBoundingClientRect(); const blk = v.lineBlockAtHeight(b.bottom - 30 - v.documentTop); return v.state.doc.lineAt(blk.from).number; })()`);
  [f, t] = await lineAt(p, t2, lastVisible);
  expectBubble(await bubbleAt(p, t2, f, t), "the last line in view");
  // A scrolled note: the line now at the top of the page.
  await p.eval(`(() => { const v = ${V(t2)}; v.scrollDOM.scrollTop = 600; })()`);
  await sleep(300);
  const topLine = await p.eval(`(() => { const v = ${V(t2)}; const b = v.scrollDOM.getBoundingClientRect(); const blk = v.lineBlockAtHeight(b.top + 4 - v.documentTop); return v.state.doc.lineAt(blk.from).number + 1; })()`);
  await p.eval(`(() => { const v = ${V(t2)}; const l = v.state.doc.line(${topLine}); v.scrollDOM.scrollTop += v.coordsAtPos(l.from).top - v.scrollDOM.getBoundingClientRect().top - 2; })()`);
  await sleep(300);
  [f, t] = await lineAt(p, t2, topLine);
  r = await bubbleAt(p, t2, f, t);
  expectBubble(r, "a scrolled note, the line at the top");
  await p.shot(`${SHOTS}/notes-edit-bubble-scrolled.png`, { x: r.tile.x, y: r.tile.y, width: r.tile.width, height: 260 });
  check(!p.errors.length, "desktop: no page errors", p.errors);

  // A narrow window with a mouse: the bubble stays inside the tile sideways too.
  const n = await device();
  await n.viewport(344, 760);
  await signIn(n, "Narrow");
  const t3 = await show(n, "Narrow", long);
  [f, t] = await lineAt(n, t3, 1);
  r = await bubbleAt(n, t3, f + 20, t);
  expectBubble(r, "a 344 px window");
  await n.shot(`${SHOTS}/notes-edit-bubble-narrow.png`);
  await n.close();

  // Phones (folded and unfolded): no bubble; the format bar sits under the page, never over it.
  for (const [label, w, h] of [["phone", 344, 760], ["fold", 841, 701]]) {
    const ph = await device();
    await ph.viewport(w, h, true, 2);
    await signIn(ph, label);
    const tp = await show(ph, "Phone note", long);
    [f, t] = await lineAt(ph, tp, 1);
    r = await bubbleAt(ph, tp, f, t);
    const barOk = await ph.eval(`(() => { const bar = document.querySelector('${tp.sel} .nb-tools'); const sc = ${V(tp)}.scrollDOM.getBoundingClientRect(); return !bar || bar.getBoundingClientRect().top >= sc.bottom - 1; })()`);
    check(!r.tip && barOk, `${label}: no bubble over the selection (phones have the format bar, under the page)`, r);
    check(!(await ph.eval(`!!document.querySelector('${tp.sel} .note-add-pic')`)), `${label}: no header picture button (the format bar has one)`);
    for (const theme of ["dark", "light"]) {
      await ph.theme(theme);
      await sleep(200);
      await ph.shot(`${SHOTS}/notes-edit-${label}-${theme}.png`);
    }
    check(!ph.errors.length, `${label}: no page errors`, ph.errors);
    await ph.close();
  }

  // ── the Linux desktop app (a stand-in shell) ───────────────────────────
  console.log("— the Linux desktop app");
  const pr = await fetch(MOCK + "/api/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "123456", device_name: "Desktop app" }) });
  const { token } = await pr.json();
  const a = await device();
  await a.viewport(1280, 820);
  await a.send("Page.addScriptToEvaluateOnNewDocument", { source: shellScript(token) });
  await a.goto(APP, 1800);
  await a.waitFor(`document.querySelector('.workspace')`, 12000);
  const ta = await show(a, "Desktop app note", "First line.");
  const shot = await a.eval(pngB64(64, 40, "#2a7"));
  const setClip = (clip) => a.eval(`(window.__clip = ${JSON.stringify(clip)}, window.__calls = [], true)`);
  const calls = () => a.eval(`window.__calls.filter((c) => /clipboard|dropped/.test(c))`);
  /** A paste shaped as WebKitGTK hands it over: these types and strings, no files. */
  const pasteAs = (types, data = {}) =>
    a.eval(`(() => {
      const data = ${JSON.stringify(data)};
      const dt = { types: ${JSON.stringify(types)}, files: [], items: [], getData: (t) => data[t] ?? '' };
      const e = new Event('paste', { bubbles: true, cancelable: true });
      Object.defineProperty(e, 'clipboardData', { value: dt });
      ${V(ta)}.contentDOM.dispatchEvent(e);
      return e.defaultPrevented;
    })()`);
  // A screenshot copied (the paste event is empty there): the shell reads it.
  await caretEnd(a, ta);
  await setClip({ image: { mime: "image/png", data: shot, width: 64, height: 40 } });
  let prevented = await pasteAs([]);
  await until(async () => (await pics(a, ta)).length === 1);
  check(prevented && (await pics(a, ta)).length === 1, "desktop app: a copied screenshot (an empty paste event) goes into the note, read by the app", await doc(a, ta));
  check((await calls()).join() === "clipboard_files,clipboard_image", "…asking the app for copied files first, then the picture", await calls());
  // A browser's "Copy Image": the picture, not a placeholder for its address.
  await caretEnd(a, ta);
  await setClip({ image: { mime: "image/png", data: shot, width: 64, height: 40 } });
  await pasteAs(["text/html"], { "text/html": '<img src="https://example.org/cat.png" alt="cat">' });
  await until(async () => (await pics(a, ta)).length === 2);
  d = await doc(a, ta);
  check((await pics(a, ta)).length === 2 && !d.includes("example.org"), 'desktop app: a browser\'s "Copy Image" puts the picture in, not its address', d);
  // Files copied in a file manager: the picture goes in, a text file doesn't.
  await caretEnd(a, ta);
  await setClip({ files: { files: [{ name: "Screenshot.png", mime: "image/png", data: shot }, { name: "notes.txt", mime: "text/plain", data: Buffer.from("hi").toString("base64") }], skipped: [] } });
  await pasteAs(["text/uri-list"]);
  await until(async () => (await pics(a, ta)).length === 3);
  check((await pics(a, ta)).length === 3, "desktop app: a picture copied in a file manager goes in", await doc(a, ta));
  // Plain prose: pasted as typed, without asking the app.
  await caretEnd(a, ta);
  await setClip({});
  await pasteAs(["text/plain"], { "text/plain": "plain words" });
  await sleep(300);
  check((await doc(a, ta)).endsWith("plain words") && !(await calls()).length, "desktop app: plain words paste as usual (the app isn't asked)", { calls: await calls(), end: (await doc(a, ta)).slice(-20) });
  // A link: the app is asked (it may be a picture's address beside the picture); none there, so the link goes in as text.
  await caretEnd(a, ta);
  const len0 = (await doc(a, ta)).length;
  await setClip({});
  prevented = await pasteAs(["text/plain"], { "text/plain": "https://example.org/docs" });
  await until(async () => (await doc(a, ta)).length > len0, 3000);
  check(prevented && (await doc(a, ta)).endsWith("https://example.org/docs") && (await calls()).length === 2, "desktop app: a link is checked for a picture first, then pasted as text", { calls: await calls(), end: (await doc(a, ta)).slice(-30) });
  // Dropped from a file manager (WebKitGTK gives only links): the app reads the files.
  await setClip({ dropped: { files: [{ name: "Photo.png", mime: "image/png", data: shot }], skipped: [] } });
  const dropped = await a.eval(`(() => {
    const dt = { types: ['text/uri-list'], files: [], items: [], getData: (t) => t === 'text/uri-list' ? 'file:///home/me/Photo.png' : '', dropEffect: 'none', effectAllowed: 'all' };
    const r = document.querySelector('${ta.sel} .cm-content').getBoundingClientRect();
    const fire = (type) => { const e = new Event(type, { bubbles: true, cancelable: true }); Object.defineProperty(e, 'dataTransfer', { value: dt }); Object.defineProperty(e, 'clientX', { value: r.x + 30 }); Object.defineProperty(e, 'clientY', { value: r.y + 8 }); ${V(ta)}.contentDOM.dispatchEvent(e); return e.defaultPrevented; };
    return { over: fire('dragover'), drop: fire('drop') };
  })()`);
  await until(async () => (await pics(a, ta)).length === 4);
  check(dropped.over && dropped.drop && (await pics(a, ta)).length === 4, "desktop app: a picture dragged from a file manager goes in where it was dropped", { dropped, pics: (await pics(a, ta)).length });
  await until(async () => ((await server(ta.id)).content.match(/assets\//g) ?? []).length === 4);
  check(((await server(ta.id)).content.match(/assets\//g) ?? []).length === 4, "desktop app: all four are saved with the note");
  await a.shot(`${SHOTS}/notes-edit-desktop-app.png`);
  check(!a.errors.length, "desktop app: no page errors", a.errors);
  await a.close();
  await p.close();
  finish();
});
