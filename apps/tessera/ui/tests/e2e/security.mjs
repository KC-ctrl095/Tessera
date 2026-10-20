// Whiteboard isolation: a board that tries to escape must fail at every step,
// and the app must ignore anything a frame posts to it.
import { MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";
import { createServer } from "node:http";

const SID = "ses_wb7c1e5a9f20";

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const p = await device();
  await p.viewport(1440, 900);
  const requests = [];
  const responses = [];
  await p.send("Network.enable");
  p.on("Network.requestWillBeSent", (e) => requests.push(e.request.url));
  p.on("Network.responseReceived", (e) => responses.push(e.response.url));
  const consoleMsgs = [];
  await p.send("Log.enable");
  p.on("Log.entryAdded", (e) => consoleMsgs.push(e.entry.text));
  // Sandboxed frames run out of process: attach to them as they appear.
  const oopifs = [];
  const frameReq = [];
  const frameRes = [];
  const frameLog = [];
  p.on("Target.attachedToTarget", async (e) => {
    oopifs.push(e);
    const sid = e.sessionId;
    // Watch its network from its very first request, then let it run.
    p.conn.on(sid, "Network.requestWillBeSent", (x) => frameReq.push(x.request.url));
    p.conn.on(sid, "Network.responseReceived", (x) => frameRes.push(x.response.url));
    p.conn.on(sid, "Network.loadingFailed", (x) => frameLog.push(`failed ${x.blockedReason ?? ""} ${x.errorText}`));
    p.conn.on(sid, "Log.entryAdded", (x) => frameLog.push(x.entry.text));
    await p.conn.send("Network.enable", {}, sid).catch(() => {});
    await p.conn.send("Log.enable", {}, sid).catch(() => {});
    await p.conn.send("Runtime.runIfWaitingForDebugger", {}, sid).catch(() => {});
  });
  await p.send("Target.setAutoAttach", { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
  await signIn(p, "Security");
  await p.theme("dark");
  await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${SID}', { pane: m.workspace.focused() }))`);
  await sleep(900);
  const before = await p.eval(`({ ls: Object.keys(localStorage).sort(), cookie: document.cookie, href: location.href, title: document.title })`);
  // the escaping board, with a question pending on it (the fake answer targets it)
  const board = await mock("/__mock/board", { session_id: SID, kind: "evil", ask: { prompt: "Deploy to production?", input: { type: "confirm" } } });
  check(!!board.ask?.id, "evil board is up with a pending ask", board.ask);
  await p.waitFor(`document.querySelector('.board-iframe')`, 10000);
  await sleep(3500);
  const iframe = await p.eval(`(() => { const f = document.querySelector('.board-iframe'); return { sandbox: f.getAttribute('sandbox'), srcdoc: f.hasAttribute('srcdoc'), referrer: f.getAttribute('referrerpolicy'), allow: f.getAttribute('allow'), src: f.getAttribute('src') }; })()`);
  console.log("iframe:", JSON.stringify(iframe));
  check(iframe.sandbox === "allow-scripts", "HTML board frame: sandbox is exactly allow-scripts");
  check(!iframe.srcdoc, "no srcdoc (never inherits the app's origin or policy)");
  check(iframe.referrer === "no-referrer" && iframe.allow === "", "no referrer, no permissions (allow=\"\")");
  check(/\/board\/[^/]+\/[^/]+\/\?theme=dark/.test(iframe.src ?? ""), "web UI frames the server's board_link page", iframe.src);

  // Read what the board saw, from inside its frame (its own process).
  // (attached before it navigated, so its URL is read from inside)
  let bf = null;
  for (const e of oopifs.filter((x) => x.targetInfo.type === "iframe")) {
    const r = await p.conn.send("Runtime.evaluate", { expression: "location.href", returnByValue: true }, e.sessionId).catch(() => null);
    if (/\/board\//.test(r?.result?.value ?? "")) bf = e;
  }
  check(!!bf, "board frame found (out of process)", oopifs.map((e) => e.targetInfo.url));
  let lines = [];
  let frameOrigin = null;
  if (bf) {
    const r = await p.conn.send("Runtime.evaluate", { expression: `JSON.stringify({ lines: [...document.querySelectorAll('#out li')].map(li => li.className + ' | ' + li.textContent), origin: self.origin, tauri: typeof window.__TAURI_INTERNALS__ })`, returnByValue: true }, bf.sessionId);
    const v = JSON.parse(r.result.value);
    lines = v.lines;
    frameOrigin = v.origin;
  }
  console.log("board origin:", frameOrigin);
  for (const l of lines) console.log("   ", l);
  check(frameOrigin === "null", "the board runs on an opaque origin", frameOrigin);
  const verdict = (name) => lines.find((l) => l.includes(`| ${name} →`))?.split(" | ")[0] ?? "missing";
  check(verdict("parent.document") === "blocked", "parent.document is blocked");
  check(verdict("window.__TAURI_INTERNALS__") === "blocked", "no __TAURI_INTERNALS__ in the frame");
  check(verdict("localStorage") === "blocked", "localStorage is blocked");
  check(verdict("document.cookie") === "blocked", "document.cookie is blocked");
  check(verdict("open a popup") === "blocked", "popups are blocked");
  check(verdict("fetch(example.com)") === "blocked", "fetch to example.com is blocked (CSP connect-src 'none')");
  check(verdict("new Image() → example.com") === "blocked", "an image beacon to example.com is blocked (CSP img-src)");
  const after = await p.eval(`({ ls: Object.keys(localStorage).sort(), cookie: document.cookie, href: location.href, title: document.title })`);
  check(after.href === before.href, "top.location = … did not navigate the app", { before: before.href, after: after.href });
  check(JSON.stringify(after.ls) === JSON.stringify(before.ls) && after.cookie === before.cookie, "the app's storage and cookies are untouched");
  const leaked = [...responses, ...frameRes].filter((u) => /example\.com/.test(u));
  check(!leaked.length, "no response ever came from example.com (app page and the board's own process)", leaked);
  console.log("   board frame requests:", frameReq.map((u) => u.replace(/^https?:\/\/[^/]+/, "").slice(0, 60)));
  console.log("   board frame requests to example.com:", frameReq.filter((u) => /example\.com/.test(u)).length);
  for (const m of frameLog.slice(0, 10)) console.log("     frame log:", m.slice(0, 180));
  // The fake answer the frame posted must have been ignored.
  const state = await (await fetch(MOCK + "/__mock/boards")).json();
  const still = state.boards.find((b) => b.id === board.id);
  check(!!still?.ask && state.answers.length === 0, "the frame's postMessage 'answer' was ignored: the ask is still pending, no answer_board sent", { ask: still?.ask, answers: state.answers });
  const listeners = await (async () => {
    const { result } = await p.send("Runtime.evaluate", { expression: "window" });
    const l = await p.send("DOMDebugger.getEventListeners", { objectId: result.objectId });
    return l.listeners.filter((x) => x.type === "message").map((x) => x.lineNumber + ":" + x.scriptId);
  })();
  console.log("   window 'message' listeners in the app page:", listeners.length, listeners);
  check(listeners.length === 0, "the app has no window message listener at all");
  const csp = consoleMsgs.filter((m) => /Content Security Policy|sandbox|Blocked/i.test(m));
  console.log("   console reports:", csp.length);
  for (const m of csp.slice(0, 12)) console.log("     -", m.slice(0, 170));
  await p.shot(`${SHOTS}/wb-security-desktop-dark.png`);
  for (const [label, width, height] of [["phone", 344, 780], ["fold", 800, 1000], ["desktop", 1440, 900]]) {
    await p.viewport(width, height);
    for (const theme of ["light", "dark"]) {
      await p.theme(theme);
      await sleep(500);
      await p.shot(`${SHOTS}/wb-security-${label}-${theme}.png`);
    }
  }

  // URL boards the app refuses outright.
  const refusals = await p.eval(`__mod('lib/boards.ts').then(m => ['javascript:alert(1)', 'file:///etc/passwd', 'data:text/html,<b>x</b>', location.origin + '/', '${MOCK}/api/health', 'https://example.com/'].map(u => [u, m.checkBoardUrl(u).ok]))`);
  console.log("   url checks:", JSON.stringify(refusals));
  check(refusals.slice(0, 4).every(([, ok]) => !ok) && refusals[5][1], "non-http(s) and the app's own origin are refused; normal sites pass");
  // Render a url board of the app's own address: a refusal card, no frame.
  const webViewImports = [];
  p.on('Fetch.requestPaused', e => webViewImports.push(e));
  await p.send('Fetch.enable', { patterns: [{ urlPattern: '*/src/components/board/WebView.tsx*', requestStage: 'Request' }] });
  await p.eval(`__mod('lib/store.ts').then(m => m.setState('boards', 'wb_selftest', { id: 'wb_selftest', session_id: '${SID}', title: 'Self', content: { type: 'url', url: location.origin + '/' }, version: 1, created_at: Date.now(), updated_at: Date.now() }))`);
  await p.eval(`__mod('lib/workspace.ts').then(m => m.openBoard('wb_selftest'))`);
  const selfPane = await p.eval(`__mod('lib/workspace.ts').then(m => m.paneShowingBoard('wb_selftest'))`);
  await p.waitFor(`document.querySelector('[data-pane-id="${selfPane}"] .board-head')`);
  for (let i = 0; i < 100 && !webViewImports.length; i++) await sleep(80);
  check(webViewImports.length === 1 && !await p.eval(`!!document.querySelector('[data-pane-id="${selfPane}"] .wv-bar')`), 'the cold self-URL tile initially has only its header while the real web-view module is paused');
  await p.send('Fetch.continueRequest', { requestId: webViewImports[0].requestId });
  // WebViewSlot's header is not mount readiness. Wait for the actual view,
  // then run the same refusal/frame/browser-tab predicate, not a longer sleep.
  await p.waitFor(`document.querySelector('[data-pane-id="${selfPane}"] .wv-bar')`);
  await p.send('Fetch.disable');
  const self = await p.eval(`(() => { const t = document.querySelector('[data-pane-id="${selfPane}"]'); return { note: t?.querySelector('.board-note h3')?.textContent, frame: !!t?.querySelector('iframe'), shown: !!t?.querySelector('.wv-canvas.shown') }; })()`);
  const attachedSelf = (await (await fetch(MOCK + "/__mock/browser")).json()).attached.wb_selftest;
  check(/isn't shown/.test(self.note ?? "") && !self.frame && !self.shown && !attachedSelf, "a url board pointing at Tessera itself shows a refusal card: no frame, no browser tab", { self, attachedSelf });
  await p.shot(`${SHOTS}/wb-refused-desktop-dark.png`);

  // Exercise self-navigation from real sandboxed board documents. A CSP
  // error alone is not enough: the other loopback service must see no GET.
  let received = 0;
  const receiver = createServer((_req, res) => { received++; res.end("fixture"); });
  await new Promise((resolve, reject) => { receiver.once("error", reject); receiver.listen(Number(process.env.BOARD_NAV_PORT ?? 0), "127.0.0.1", resolve); });
  const otherOrigin = `http://127.0.0.1:${receiver.address().port}`;
  try {
    for (const [name, html] of [
      ["script", `<script>location.replace(${JSON.stringify(otherOrigin + "/fixture")})</script>`],
      ["refresh", `<meta http-equiv="refresh" content="0;url=${otherOrigin}/fixture">`],
      ["same-origin", `<script>location.replace(${JSON.stringify(new URL("/api/health?frame-navigation-fixture=1", iframe.src).href)})</script>`],
    ]) {
      // An explicit id updates an existing board. Omit it to create this fixture.
      const b = await mock("/__mock/board", { session_id: SID, title: `Navigation ${name}`, content: { type: "html", html }, ask: false });
      if (!b.id) throw new Error(`Could not create ${name} navigation fixture: ${JSON.stringify(b)}`);
      await p.waitFor(`__mod('lib/store.ts').then(m => !!m.state.boards[${JSON.stringify(b.id)}])`);
      const src = await p.eval(`__mod('lib/boards.ts').then(async m => m.htmlFrameSrc((await __mod('lib/store.ts')).state.boards[${JSON.stringify(b.id)}]))`);
      await p.eval(`(() => { const f = document.createElement('iframe'); f.id = 'navigation-fixture'; f.sandbox = 'allow-scripts'; f.src = ${JSON.stringify(src)}; f.style.cssText = 'position:fixed;width:1px;height:1px;opacity:0'; document.body.append(f); })()`);
      await sleep(1600);
      check([...responses, ...frameRes].some(u => u.startsWith(src.split("?")[0])), `${name} navigation fixture loaded from the real board route`);
      check(received === 0, `${name} self-navigation sends no request to the other loopback port`, received);
      check(![...responses, ...frameRes].some(u => u.includes("frame-navigation-fixture=1")), `${name} self-navigation never reaches a non-board route on the board origin`);
      await p.eval(`document.getElementById('navigation-fixture')?.remove()`);
    }
    const policy = await p.eval(`Array.from(document.querySelectorAll('meta[http-equiv="Content-Security-Policy"]'), m => m.content).find(c => c.startsWith('frame-src '))`);
    check(policy === `frame-src ${new URL(iframe.src).origin}/board/ ${new URL(iframe.src).origin}/preview/`, "the parent frame policy names only the exact board origin and board/preview routes", policy);
  } finally {
    // Headless Chrome can preconnect without a GET. Do not let an idle socket
    // keep this throwaway receiver alive after the assertions have completed.
    receiver.closeAllConnections();
    await new Promise(resolve => receiver.close(resolve));
  }
  console.log("errors", p.errors);
  finish();
});
