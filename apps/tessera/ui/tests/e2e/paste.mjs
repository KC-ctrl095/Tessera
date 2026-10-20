// Pasting into the composer: images become attachments, big text becomes a
// "Pasted text" attachment the agent reads in full, small text stays inline.
// Real clipboard pastes (Chromium's own clipboard, Ctrl+V), a paste whose
// image is only listed under `items`, an Android keyboard's commit (one big
// insertText), and an Android keyboard's image (the `tessera:content` event
// MainActivity sends).
// The Linux desktop app (a stand-in shell): pastes shaped as WebKitGTK hands
// them over (files copied in a file manager, a browser's "Copy Image", a
// picture with its link) where the shell reads what the page can't see; the
// image or file wins over the text beside it, plain text stays text; files
// dropped from a file manager; a clipboard read that never answers.
import { APP, MOCK, SHOTS, browser, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const SID = "ses_2b7e91c04d5a";
const TA = ".pane.focused .composer textarea";
const LINES = 300;
const BIG = Array.from({ length: LINES }, (_, i) => `${String(i + 1).padStart(4, "0")} INFO worker[${i % 7}] handled request /api/items/${i} in ${(i * 37) % 900}ms`).join("\n");

async function paste(p) {
  await p.eval(`document.querySelector('${TA}').focus()`);
  await p.send("Input.dispatchKeyEvent", { type: "keyDown", key: "v", code: "KeyV", modifiers: 2, windowsVirtualKeyCode: 86, commands: ["paste"] });
  await p.send("Input.dispatchKeyEvent", { type: "keyUp", key: "v", code: "KeyV", modifiers: 2, windowsVirtualKeyCode: 86 });
  await sleep(500);
}
const putText = (p, t) => p.eval(`navigator.clipboard.writeText(${JSON.stringify(t)}).then(() => true)`);
const putImage = (p) =>
  p.eval(`(async () => { const c = document.createElement('canvas'); c.width = 120; c.height = 80; const x = c.getContext('2d'); x.fillStyle = '#c33'; x.fillRect(0, 0, 120, 80); x.fillStyle = '#fff'; x.fillRect(20, 20, 80, 40); const b = await new Promise((r) => c.toBlob(r, 'image/png')); await navigator.clipboard.write([new ClipboardItem({ 'image/png': b })]); return b.size; })()`);
/** A paste whose image is listed only under `items` (no `files`), as some engines hand it over. */
const pasteItemsOnly = (p) =>
  p.eval(`(async () => {
    const c = document.createElement('canvas'); c.width = 60; c.height = 40; c.getContext('2d').fillRect(0, 0, 60, 40);
    const blob = await new Promise((r) => c.toBlob(r, 'image/png'));
    const file = new File([blob], 'image.png', { type: 'image/png' });
    const dt = { files: [], items: [{ kind: 'file', type: 'image/png', getAsFile: () => file }], types: ['Files'], getData: () => '' };
    const e = new Event('paste', { bubbles: true, cancelable: true });
    Object.defineProperty(e, 'clipboardData', { value: dt });
    const ta = document.querySelector('${TA}'); ta.focus(); ta.dispatchEvent(e);
    return e.defaultPrevented;
  })()`);
const ui = (p) =>
  p.eval(`(() => { const c = document.querySelector('.pane.focused .composer'); const ta = c.querySelector('textarea'); return {
    value: ta.value,
    images: c.querySelectorAll('.attachment img').length,
    names: [...c.querySelectorAll('.attachment img')].map((i) => i.alt),
    pasted: [...c.querySelectorAll('.pasted-chip')].map((e) => e.querySelector('.file-chip-name').textContent + ' · ' + e.querySelector('.file-chip-size').textContent.replace(/\\s+/g, ' ').trim()),
    files: [...c.querySelectorAll('.file-chip:not(.pasted-chip) .file-chip-name')].map((e) => e.textContent),
    peek: c.querySelector('.pasted-peek-text')?.textContent ?? null,
    box: Math.round(c.getBoundingClientRect().height),
  }; })()`);
const clearComposer = (p) =>
  p.eval(`(() => { for (const b of document.querySelectorAll('.pane.focused .composer .file-chip-x, .pane.focused .composer .attachment-x')) b.click(); const ta = document.querySelector('${TA}'); ta.focus(); ta.select(); document.execCommand('delete'); return true; })()`);
const grant = async (p) => {
  const c = await browser();
  await c.send("Browser.grantPermissions", { origin: new URL(APP).origin, permissions: ["clipboardReadWrite", "clipboardSanitizedWrite"], browserContextId: p.contextId });
};

/**
 * A stand-in for the Linux desktop app's shell (Tauri): the app's own commands
 * go to the server over a WebSocket (as the Rust side would); the clipboard
 * commands answer from `window.__clip`, which each case sets to what the real
 * clipboard would hold. The web view's own clipboard read never answers (it
 * mustn't be needed), and every command is logged in `window.__calls`.
 */
function shellScript(token) {
  return `(() => {
    const TOKEN = ${JSON.stringify(token)};
    let n = 0, ws = null, rid = 0;
    const cbs = {}, listeners = {}, pending = new Map();
    window.__calls = [];
    window.__clip = {};
    window.__webReads = 0;
    if (navigator.clipboard) navigator.clipboard.read = () => { window.__webReads++; return new Promise(() => {}); };
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

/** A paste event shaped as WebKitGTK hands it over: these types and strings, no files. */
const pasteAs = (p, types, data = {}) =>
  p.eval(`(() => {
    const data = ${JSON.stringify(data)};
    const dt = { types: ${JSON.stringify(types)}, files: [], items: [], getData: (t) => data[t] ?? '' };
    const e = new Event('paste', { bubbles: true, cancelable: true });
    Object.defineProperty(e, 'clipboardData', { value: dt });
    const ta = document.querySelector('${TA}'); ta.focus(); ta.dispatchEvent(e);
    return e.defaultPrevented;
  })()`);
/** Files dragged from a file manager onto the composer, as WebKitGTK hands them over: links only. */
const dropAs = (p, types, data = {}) =>
  p.eval(`(() => {
    const data = ${JSON.stringify(data)};
    const dt = { types: ${JSON.stringify(types)}, files: [], items: [], getData: (t) => data[t] ?? '', dropEffect: 'none', effectAllowed: 'all' };
    const target = document.querySelector('${TA}');
    const fire = (type) => { const e = new Event(type, { bubbles: true, cancelable: true }); Object.defineProperty(e, 'dataTransfer', { value: dt }); target.dispatchEvent(e); return e.defaultPrevented; };
    const over = fire('dragover');
    return { over, drop: fire('drop') };
  })()`);
const b64 = (t) => Buffer.from(t).toString("base64");

run(async () => {
  for (const [label, w, h, mobile] of [["desktop", 1440, 900, false], ["phone", 375, 812, true], ["fold", 841, 701, true]]) {
    await mock("/__mock/reset");
    const p = await device();
    await p.viewport(w, h, mobile, mobile ? 2 : 1);
    await grant(p);
    await signIn(p, `Paste ${label}`);
    await p.theme("dark");
    await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${SID}', { pane: m.workspace.focused() }))`);
    await sleep(1400);
    const rest = (await ui(p)).box;

    // An image on the clipboard → an attachment (the paste carries it as a file).
    await putImage(p);
    await paste(p);
    let u = await ui(p);
    check(u.images === 1 && u.value === "", `${label}: pasting an image adds it as an attachment`, u);
    check(u.names[0] === "Pasted image.png", `${label}: …named "Pasted image.png"`, u.names);
    await p.shot(`${SHOTS}/paste-image-${label}.png`);
    await clearComposer(p);
    // Only under `items`.
    const prevented = await pasteItemsOnly(p);
    await sleep(400);
    u = await ui(p);
    check(prevented && u.images === 1, `${label}: an image listed only under items is attached too`, u);
    await clearComposer(p);

    // A little text stays text.
    await putText(p, "npm run build -- --watch");
    await paste(p);
    u = await ui(p);
    check(u.value === "npm run build -- --watch" && !u.pasted.length, `${label}: a small paste stays in the box`, u);
    await clearComposer(p);

    // A big paste → "Pasted text" (lines · size), the box stays slim.
    await putText(p, BIG);
    await paste(p);
    u = await ui(p);
    check(u.value === "" && u.pasted.length === 1 && /^Pasted text · 300 lines · \d+(\.\d)? KB$/.test(u.pasted[0]), `${label}: a big paste becomes "Pasted text" with its size`, u);
    check(u.box <= rest + 80, `${label}: …and the box doesn't grow with it (${rest} → ${u.box}px)`);
    for (const theme of ["dark", "light"]) {
      await p.theme(theme);
      await sleep(200);
      await p.shot(`${SHOTS}/paste-chip-${label}-${theme}.png`);
    }
    await p.theme("dark");
    // Tap to see it, then put it back into the message.
    await p.click(".pane.focused .pasted-open");
    await sleep(300);
    u = await ui(p);
    check(!!u.peek?.startsWith("0001 INFO worker[0]") && u.peek.includes(`0${LINES} INFO`), `${label}: tapping it shows the text`);
    for (const theme of ["dark", "light"]) {
      await p.theme(theme);
      await sleep(200);
      await p.shot(`${SHOTS}/paste-peek-${label}-${theme}.png`);
    }
    await p.theme("dark");
    await p.clickText(".pane.focused .pasted-peek button", "Put in message");
    await sleep(300);
    u = await ui(p);
    check(u.value === BIG && !u.pasted.length && u.peek === null, `${label}: "Put in message" puts all of it back in the box`, { len: u.value.length, pasted: u.pasted });
    await clearComposer(p);

    // Remove it (the chip's x, and the preview's Remove).
    await paste(p);
    await p.click(".pane.focused .pasted-chip .file-chip-x");
    await sleep(200);
    u = await ui(p);
    check(!u.pasted.length && u.value === "", `${label}: the chip's x removes it`, u);
    await paste(p);
    await p.click(".pane.focused .pasted-open");
    await sleep(200);
    await p.clickText(".pane.focused .pasted-peek button", "Remove");
    await sleep(200);
    u = await ui(p);
    check(!u.pasted.length && u.peek === null && u.value === "", `${label}: the preview's Remove removes it`, u);

    // An Android keyboard commits clipboard text as typing (no paste event).
    await p.eval(`document.querySelector('${TA}').focus()`);
    await p.type("See: ");
    await p.send("Input.insertText", { text: BIG });
    await sleep(300);
    u = await ui(p);
    check(u.value === "See: " && u.pasted.length === 1, `${label}: a keyboard's big commit becomes "Pasted text" too`, { value: u.value.slice(0, 40), pasted: u.pasted });
    await clearComposer(p);

    // An Android keyboard's image (its clipboard, a GIF): the activity hands it over as `tessera:content`.
    const png = await p.eval(`(() => { const c = document.createElement('canvas'); c.width = 30; c.height = 30; c.getContext('2d').fillRect(0, 0, 30, 30); const u = c.toDataURL('image/png'); return u.slice(u.indexOf(',') + 1); })()`);
    const fire = `window.dispatchEvent(new CustomEvent('tessera:content', { detail: { items: [{ mime: 'image/png', data: ${JSON.stringify(png)} }] } }))`;
    await p.eval(`document.querySelector('${TA}').blur()`);
    await p.eval(fire);
    await sleep(400);
    check((await ui(p)).images === 0, `${label}: a keyboard image goes nowhere when no composer is being typed in`);
    await p.eval(`document.querySelector('${TA}').focus()`);
    await p.eval(fire);
    await sleep(400);
    check((await ui(p)).images === 1, `${label}: a keyboard image lands in the composer being typed in`);
    await clearComposer(p);

    // Two big pastes are numbered; sent, the agent gets every line.
    await putText(p, BIG);
    await paste(p);
    await putText(p, BIG.replaceAll("INFO", "WARN"));
    await paste(p);
    u = await ui(p);
    check(u.pasted.length === 2 && u.pasted[1].startsWith("Pasted text 2 · "), `${label}: a second big paste is "Pasted text 2"`, u.pasted);
    await p.eval(`(() => { [...document.querySelectorAll('.pane.focused .pasted-chip')].pop()?.querySelector('.file-chip-x')?.click(); return 1; })()`);
    await sleep(200);
    await p.eval(`document.querySelector('${TA}').focus()`);
    await p.type("Summarize this log please");
    if (mobile) await p.click(".pane.focused .send-btn");
    else await p.key("Enter", "Enter", 0, 13);
    await p.eval(`__mod('lib/store.ts').then((m) => { window.__st = m.state; return 1; })`);
    await p.waitFor(`window.__st.sessions['${SID}'].entries.some((e) => e.kind === 'user' && JSON.stringify(e.content).includes('Pasted text.txt'))`, 8000);
    const sent = await p.eval(`(() => { const e = window.__st.sessions['${SID}'].entries.findLast((e) => e.kind === 'user'); return e.content.map((c) => c.type === 'text' ? c.text : '[' + c.type + ']'); })()`);
    const filePart = sent.find((t) => t.startsWith("[Attached file: Pasted text.txt"));
    check(sent.includes("Summarize this log please"), `${label}: the typed message goes as typed`, sent.map((t) => t.slice(0, 60)));
    check(!!filePart && filePart.includes("\n```\n" + BIG + "\n```"), `${label}: the server gets the pasted text in full (${LINES} lines) for the agent`, filePart?.slice(0, 120));
    await p.waitFor(`[...document.querySelectorAll('.pane.focused .transcript .bubble-files .file-chip')].some((c) => c.textContent.includes('Pasted text.txt'))`, 6000);
    await p.eval(`[...document.querySelectorAll('.pane.focused .transcript .bubble-files .file-chip')].filter((c) => c.textContent.includes('Pasted text.txt')).pop().scrollIntoView({ block: 'center' })`);
    await sleep(200);
    await p.eval(`[...document.querySelectorAll('.pane.focused .transcript .bubble-files .file-chip')].filter((c) => c.textContent.includes('Pasted text.txt')).pop().click()`);
    await sleep(300);
    const shown = await p.eval(`[...document.querySelectorAll('.pane.focused .transcript .file-att.open')].pop()?.querySelector('.file-att-text')?.textContent ?? ''`);
    check(shown === BIG, `${label}: in the chat, "Pasted text.txt" opens to the full text`);
    await p.waitFor(`window.__st.sessions['${SID}'].entries.some((e) => e.kind === 'assistant' && JSON.stringify(e.content).includes('Pasted text.txt'))`, 20000).catch(() => {});
    const ack = await p.eval(`window.__st.sessions['${SID}'].entries.findLast((e) => e.kind === 'assistant')?.content.map((c) => c.text ?? '').join(' ') ?? ''`);
    check(/Pasted text\.txt.*300 lines/.test(ack), `${label}: the (mock) agent read all ${LINES} lines`, ack.slice(0, 160));
    await sleep(500);
    await p.shot(`${SHOTS}/paste-sent-${label}.png`);
    // Rewinding that message brings the paste back as "Pasted text".
    const back = await p.eval(`__mod('lib/images.ts').then((m) => { const e = window.__st.sessions['${SID}'].entries.findLast((e) => e.kind === 'user'); const r = m.messageAttachments(e.content); return r.attachments.map((a) => ({ name: a.name, pasted: (a.pasted ?? '').length })); })`);
    check(back.length === 1 && back[0].pasted === BIG.length, `${label}: rewinding restores it as "Pasted text"`, back);

    console.log("errors", p.errors);
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    await p.close();
  }

  // A private chat takes big pastes too (kept in memory, sent in full).
  await mock("/__mock/reset");
  const p = await device();
  await p.viewport(1440, 900);
  await grant(p);
  await signIn(p, "Paste private");
  await p.eval(`__mod('lib/actions.ts').then((m) => m.newPrivateChat())`);
  await sleep(800);
  await putText(p, BIG);
  await paste(p);
  const u = await ui(p);
  check(u.pasted.length === 1 && u.value === "", 'private chat: a big paste becomes "Pasted text"', u);
  await p.eval(`document.querySelector('${TA}').focus()`);
  await p.type("Read this");
  await p.key("Enter", "Enter", 0, 13);
  await p.eval(`__mod('lib/store.ts').then((m) => { window.__st = m.state; return 1; })`);
  await p.waitFor(`Object.values(window.__st.sessions).some((s) => s.info.private && s.entries.some((e) => e.kind === 'user' && JSON.stringify(e.content).includes('Pasted text.txt')))`, 8000);
  const full = await p.eval(`(() => { const s = Object.values(window.__st.sessions).find((s) => s.info.private); const e = s.entries.findLast((e) => e.kind === 'user'); return e.content.some((c) => c.type === 'text' && c.text.includes(${JSON.stringify(BIG)})); })()`);
  check(full, "private chat: the agent receives it in full");
  check(!p.errors.length, "private chat: no page errors", p.errors);
  await p.close();

  // ── The Linux desktop app (a stand-in shell) ──
  const pr = await fetch(MOCK + "/api/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "123456", device_name: "Desktop app" }) });
  const { token } = await pr.json();
  const a = await device();
  await a.viewport(1280, 820);
  await grant(a);
  await a.send("Page.addScriptToEvaluateOnNewDocument", { source: shellScript(token) });
  await a.goto(APP, 1800);
  await a.waitFor(`document.querySelector('.workspace')`, 12000);
  await a.theme("dark");
  await a.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${SID}', { pane: m.workspace.focused() }))`);
  await sleep(1400);
  const calls = () => a.eval(`window.__calls.filter((c) => /clipboard|dropped/.test(c))`);
  const resetCalls = () => a.eval(`(window.__calls = [], window.__clip = {}, true)`);
  const toasts = () => a.eval(`[...document.querySelectorAll('.toast')].map((t) => (t.querySelector('.toast-title')?.textContent ?? '') + ': ' + t.querySelector('.toast-body').textContent)`);
  const dismissToasts = () => a.eval(`(document.querySelectorAll('.toast button').forEach((b) => b.click()), true)`);
  const shot = await a.eval(`(() => { const c = document.createElement('canvas'); c.width = 64; c.height = 40; const x = c.getContext('2d'); x.fillStyle = '#2a7'; x.fillRect(0, 0, 64, 40); const v = c.toDataURL('image/png'); return v.slice(v.indexOf(',') + 1); })()`);
  const png = { name: "Screenshot from 2026-10-01.png", mime: "image/png", data: shot };
  const notes = { name: "notes.txt", mime: "text/plain", data: b64("Quarterly notes\n") };
  const setClip = (clip) => a.eval(`(window.__clip = ${JSON.stringify(clip)}, true)`);
  let v;

  // Files copied in Nautilus or Dolphin: the paste event lists an empty uri-list; the shell reads the files.
  await setClip({ files: { files: [png, notes], skipped: [] } });
  let prevented = await pasteAs(a, ["text/uri-list"]);
  await sleep(500);
  v = await ui(a);
  check(prevented && v.images === 1 && v.names[0] === png.name && v.files.join() === "notes.txt" && v.value === "", "desktop app: files copied in a file manager attach as files (an image and a text file)", v);
  await a.shot(`${SHOTS}/paste-desktop-files.png`);
  await clearComposer(a);
  // Thunar: the file's path comes as plain text, the files only through the shell.
  await resetCalls();
  await setClip({ files: { files: [notes], skipped: [] } });
  prevented = await pasteAs(a, ["text/plain"], { "text/plain": "/home/me/notes.txt" });
  await sleep(500);
  v = await ui(a);
  check(prevented && v.files.join() === "notes.txt" && v.value === "", "desktop app: a copied file whose path comes as text attaches the file, not the path", v);
  await clearComposer(a);

  // A browser's "Copy Image": the picture's markup (and sometimes its link as text) beside the image.
  await resetCalls();
  await setClip({ image: { mime: "image/png", data: shot, width: 64, height: 40 } });
  await pasteAs(a, ["text/html"], { "text/html": '<img src="https://example.org/cat.png" alt="cat">' });
  await sleep(500);
  v = await ui(a);
  check(v.images === 1 && v.names[0] === "Pasted image.png" && v.value === "", 'desktop app: a browser\'s "Copy Image" attaches the image, not its markup', v);
  check((await calls()).join() === "clipboard_files,clipboard_image", "…asking the shell for copied files first, then the image", await calls());
  await clearComposer(a);
  await resetCalls();
  await setClip({ image: { mime: "image/png", data: shot, width: 64, height: 40 } });
  await pasteAs(a, ["text/plain", "text/html"], { "text/plain": "https://example.org/cat.png", "text/html": '<img src="https://example.org/cat.png">' });
  await sleep(500);
  v = await ui(a);
  check(v.images === 1 && v.value === "", "desktop app: an image wins over its link pasted beside it", v);
  await clearComposer(a);

  // Plain text stays text: prose without asking the shell, a link or path after asking it.
  await resetCalls();
  await putText(a, "npm run build -- --watch");
  await paste(a);
  v = await ui(a);
  check(v.value === "npm run build -- --watch" && !(await calls()).length, "desktop app: plain text pastes as typed (the shell isn't asked)", { v, calls: await calls() });
  await clearComposer(a);
  for (const t of ["https://example.org/docs/getting-started", "/etc/nginx/nginx.conf"]) {
    await resetCalls();
    await putText(a, t);
    await paste(a);
    await sleep(300);
    v = await ui(a);
    check(v.value === t && !v.images && !v.files.length && (await calls()).join() === "clipboard_files,clipboard_image", `desktop app: "${t}" with nothing behind it pastes as text`, { v, calls: await calls() });
    await clearComposer(a);
  }
  await resetCalls();
  await putText(a, BIG);
  await paste(a);
  v = await ui(a);
  check(v.value === "" && v.pasted.length === 1 && !(await calls()).length, 'desktop app: a big paste still becomes "Pasted text"', { v, calls: await calls() });
  await clearComposer(a);

  // Files the shell can't attach are named, and nothing else is pasted.
  await resetCalls();
  await setClip({ files: { files: [], skipped: [{ name: "Projects", reason: "folder" }, { name: "backup.iso", reason: "too large" }] } });
  await pasteAs(a, ["text/plain"], { "text/plain": "/home/me/Projects\n/home/me/backup.iso" });
  await sleep(500);
  v = await ui(a);
  const said = await toasts();
  check(v.value === "" && !v.files.length && said.some((t) => t.startsWith("Projects:") && /Folders can't be attached/.test(t)) && said.some((t) => t.startsWith("backup.iso:") && /Larger than 25 MB/.test(t)), "desktop app: a copied folder or too-large file is named in a notice, nothing pasted", { v, said });
  await a.shot(`${SHOTS}/paste-desktop-skipped.png`);
  await dismissToasts();

  // The web view's own clipboard read never answers: the Linux app doesn't wait for it.
  check((await a.eval(`window.__webReads`)) === 0, "desktop app: the web view's clipboard read is never used on Linux (it may ask the user, or hang)");

  // Dropped from a file manager: WebKitGTK gives links only, the shell the files.
  await resetCalls();
  await setClip({ dropped: { files: [png, notes], skipped: [] } });
  let d = await dropAs(a, ["text/uri-list"]);
  await sleep(500);
  v = await ui(a);
  check(d.over && d.drop && v.images === 1 && v.files.join() === "notes.txt" && v.value === "", "desktop app: files dropped from a file manager attach", { d, v });
  await clearComposer(a);
  // A web link dropped (no files behind it) goes in as text.
  await resetCalls();
  d = await dropAs(a, ["text/uri-list"], { "text/uri-list": "https://example.org/page" });
  await sleep(400);
  v = await ui(a);
  check(d.drop && v.value === "https://example.org/page" && !v.files.length, "desktop app: a dropped web link goes in as text", { d, v });
  await clearComposer(a);

  // A second composer (split view, a new chat's): the paste lands in the one being typed in.
  await a.eval(`__mod('lib/workspace.ts').then((m) => m.splitPane('right'))`);
  await sleep(900);
  const hero = await a.eval(`!!document.querySelector('.pane.focused .composer-wrap.in-hero') && document.querySelectorAll('.composer textarea').length`);
  await resetCalls();
  await setClip({ files: { files: [notes], skipped: [] } });
  await pasteAs(a, ["text/uri-list"]);
  await sleep(500);
  const both = await a.eval(`[...document.querySelectorAll('.pane')].map((p) => ({ focused: p.classList.contains('focused'), files: p.querySelectorAll('.composer .file-chip').length }))`);
  check(hero === 2 && both.length === 2 && both.every((p) => p.files === (p.focused ? 1 : 0)), "desktop app: in a split view (a new chat's composer) the file goes to the composer pasted into", { hero, both });
  await a.shot(`${SHOTS}/paste-desktop-split.png`);
  await clearComposer(a);

  // Narrow (the Fold's inner screen): the same.
  await a.viewport(841, 701, true, 2);
  await sleep(600);
  await resetCalls();
  await setClip({ files: { files: [png], skipped: [] } });
  await pasteAs(a, ["text/uri-list"]);
  await sleep(500);
  v = await ui(a);
  check(v.images === 1 && v.names[0] === png.name, "desktop app, narrow: a copied file attaches", v);
  await clearComposer(a);
  console.log("desktop app errors", a.errors);
  check(!a.errors.length, "desktop app: no page errors", a.errors);
  await a.close();

  // Not on Linux (the web view hands pasted images over itself): an empty paste
  // asks the web view's clipboard API, and gives up when that never answers.
  const w = await device();
  await w.viewport(1280, 820);
  await grant(w);
  await w.send("Emulation.setUserAgentOverride", { userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36 Edg/140.0.0.0" });
  await w.send("Page.addScriptToEvaluateOnNewDocument", { source: shellScript(token) });
  await w.goto(APP, 1800);
  await w.waitFor(`document.querySelector('.workspace')`, 12000);
  await w.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${SID}', { pane: m.workspace.focused() }))`);
  await sleep(1400);
  const t0 = Date.now();
  prevented = await pasteAs(w, []);
  check(prevented && (await w.eval(`window.__webReads`)) === 1 && Date.now() - t0 < 1000, "other desktops: an empty paste asks the web view's clipboard API at once", { prevented, reads: await w.eval(`window.__webReads`) });
  await sleep(1900);
  await putText(w, "still pastes");
  await paste(w);
  v = await ui(w);
  check(v.value === "still pastes" && !v.images && !(await w.eval(`window.__calls.filter((c) => /clipboard/.test(c))`)).length, "…a read that never answers is given up; the next paste works", v);
  check(!w.errors.length, "other desktops: no page errors", w.errors);
  await w.close();
  finish();
});
