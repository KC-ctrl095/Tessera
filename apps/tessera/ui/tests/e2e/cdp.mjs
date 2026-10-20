// Small CDP client for headless Chromium: isolated browser contexts (separate
// storage per "device"), flattened sessions, screenshots, input.
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

// Screenshots, logs and the browser profile go to E2E_OUT (default: a folder
// in the system temp directory), never into the source tree.
export const SCRATCH = process.env.E2E_OUT ?? path.join(os.tmpdir(), "tessera-e2e");
export const SHOTS = path.join(SCRATCH, "shots");
export const BEFORE = path.join(SCRATCH, "shots/before");
export const APP = process.env.APP ?? "http://localhost:5191/";
export const MOCK = process.env.MOCK ?? "http://127.0.0.1:7891";
const PORT = Number(process.env.CDP_PORT ?? 9347);
const profile = path.join(SCRATCH, "chrome");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export { sleep };

async function getJson(url) {
  const r = await fetch(url);
  return r.json();
}

async function ensureBrowser() {
  try {
    return await getJson(`http://127.0.0.1:${PORT}/json/version`);
  } catch {}
  const p = spawn(
    // CHROMIUM: another build (for example, Chrome for Testing instead of system Chromium).
    process.env.CHROMIUM || "/usr/bin/chromium",
    [
      "--headless=new",
      `--remote-debugging-port=${PORT}`,
      `--user-data-dir=${profile}`,
      "--no-first-run",
      // Scrollbar-drag tests need native controls, not just Emulation.setScrollbarsHidden(false).
      ...(process.env.E2E_SCROLLBARS === '1' ? [] : ['--hide-scrollbars']),
      "--use-fake-ui-for-media-stream",
      "--use-fake-device-for-media-stream",
      "--autoplay-policy=no-user-gesture-required",
      // Software compositing. GPU compositing through SwiftShader (E2E_GPU=swiftshader) costs four to
      // five times the CPU on a machine without a GPU, and its frames fall so far behind that
      // transitions and animations are still running long after the time they take (composer, drag).
      ...(process.env.E2E_GPU === "swiftshader" ? ["--enable-unsafe-swiftshader", "--use-angle=swiftshader"] : ["--disable-gpu"]),
      "--window-size=1440,900",
      // Headless has no pointing device (hover: none): be a desktop with a mouse; touch emulation makes phones.
      "--blink-settings=primaryPointerType=4,availablePointerTypes=4,primaryHoverType=2,availableHoverTypes=2",
      "about:blank",
    ],
    { detached: true, stdio: "ignore" },
  );
  if (process.env.E2E_BROWSER_PID_FILE && p.pid) {
    // The runner stops only the browser this run started, not an arbitrary
    // process matching a command line (nor a browser it connected to).
    fs.writeFileSync(process.env.E2E_BROWSER_PID_FILE, JSON.stringify({
      pid: p.pid,
      start: process.platform === "linux" ? fs.readFileSync(`/proc/${p.pid}/stat`, "utf8").split(") ").pop().split(" ")[19] : null,
    }));
  }
  p.unref();
  for (let i = 0; i < 60; i++) {
    await sleep(200);
    try {
      return await getJson(`http://127.0.0.1:${PORT}/json/version`);
    } catch {}
  }
  throw new Error("chromium did not start");
}

class Conn {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.listeners = new Map(); // key: sessionId|method
    ws.onmessage = (e) => {
      const m = JSON.parse(e.data);
      if (m.method) {
        const k = `${m.sessionId ?? ""}|${m.method}`;
        for (const cb of this.listeners.get(k) ?? []) cb(m.params);
      }
      if (m.id && this.pending.has(m.id)) {
        const { resolve, reject } = this.pending.get(m.id);
        this.pending.delete(m.id);
        if (m.error) reject(new Error(JSON.stringify(m.error)));
        else resolve(m.result);
      }
    };
  }
  send(method, params = {}, sessionId) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  }
  on(sessionId, method, cb) {
    const k = `${sessionId ?? ""}|${method}`;
    if (!this.listeners.has(k)) this.listeners.set(k, new Set());
    this.listeners.get(k).add(cb);
    return () => this.listeners.get(k).delete(cb);
  }
}

export class Page {
  constructor(conn, sessionId, targetId) {
    this.conn = conn;
    this.sid = sessionId;
    this.targetId = targetId;
    this.errors = [];
    this.logs = [];
  }
  send(method, params = {}) {
    return this.conn.send(method, params, this.sid);
  }
  on(method, cb) {
    return this.conn.on(this.sid, method, cb);
  }
  async init() {
    await this.send("Page.enable");
    await this.send("Runtime.enable");
    // Import the app's own module instance (Vite may serve it with ?t= after edits).
    await this.send("Page.addScriptToEvaluateOnNewDocument", {
      source: `window.__mod = (n) => { const want = '/src/' + n; const hit = performance.getEntriesByType('resource').map((e) => e.name).filter((u) => { try { return new URL(u).pathname === want; } catch { return false; } }).pop(); return import(hit ?? want); };`,
    });
    this.on("Runtime.exceptionThrown", (e) => this.errors.push(e.exceptionDetails?.exception?.description?.slice(0, 400) ?? e.exceptionDetails?.text));
    this.on("Runtime.consoleAPICalled", (e) => {
      const text = e.args.map((a) => a.value ?? a.description ?? "").join(" ");
      this.logs.push(`[${e.type}] ${text}`);
      if (e.type === "error") this.errors.push("console.error: " + text.slice(0, 400));
    });
  }
  async viewport(width, height, mobile = false, dpr = 1) {
    await this.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: dpr, mobile });
    await this.send("Emulation.setTouchEmulationEnabled", mobile ? { enabled: true, maxTouchPoints: 5 } : { enabled: false });
    await this.send("Emulation.setUserAgentOverride", {
      userAgent: mobile
        ? "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Mobile Safari/537.36"
        : "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36",
    });
  }
  async theme(t) {
    // Through the app's own prefs when it's running (so everything follows), else storage.
    await this.eval(`(async () => { try { if (window.__mod && document.querySelector('.app')) { (await __mod('lib/prefs.ts')).setPrefs('theme', ${JSON.stringify(t)}); await new Promise((r) => setTimeout(r, 60)); return; } } catch {} const p = JSON.parse(localStorage.getItem('tessera.prefs') || '{}'); p.theme = ${JSON.stringify(t)}; localStorage.setItem('tessera.prefs', JSON.stringify(p)); document.documentElement.dataset.theme = ${JSON.stringify(t)}; })()`);
  }
  async goto(url, wait = 1200) {
    await this.send("Page.navigate", { url });
    await sleep(wait);
  }
  async reload(wait = 1200) {
    await this.send("Page.reload", {});
    await sleep(wait);
  }
  async eval(expr) {
    const r = await this.send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true, userGesture: true });
    if (r.exceptionDetails) throw new Error("eval: " + JSON.stringify(r.exceptionDetails).slice(0, 600));
    return r.result.value;
  }
  wait(ms) {
    return sleep(ms);
  }
  async waitFor(expr, timeout = 8000) {
    const t0 = Date.now();
    while (Date.now() - t0 < timeout) {
      try {
        if (await this.eval(`(async () => !!(await (${expr})))()`)) return true;
      } catch {}
      await sleep(80);
    }
    throw new Error("waitFor timeout: " + expr);
  }
  async box(selector) {
    return this.eval(`(() => { const el = document.querySelector(${JSON.stringify(selector)}); if (!el) return null; el.scrollIntoView({block:'nearest'}); const r = el.getBoundingClientRect(); return {x:r.x,y:r.y,w:r.width,h:r.height,cx:r.x + r.width/2, cy:r.y + r.height/2}; })()`);
  }
  async click(selector) {
    const b = await this.box(selector);
    if (!b) throw new Error("no element " + selector);
    await this.mouse(b.cx, b.cy);
  }
  async clickText(sel, text) {
    const b = await this.eval(`(() => { const els = [...document.querySelectorAll(${JSON.stringify(sel)})].filter(e => e.textContent.includes(${JSON.stringify(text)}) && e.getClientRects().length); const e = els[0]; if (!e) return null; e.scrollIntoView({block:'nearest'}); const r = e.getBoundingClientRect(); return [r.x + r.width/2, r.y + r.height/2]; })()`);
    if (!b) throw new Error(`no ${sel} with ${text}`);
    await this.mouse(b[0], b[1]);
  }
  async mouse(x, y, mods = 0) {
    await this.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
    await this.send("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", clickCount: 1, modifiers: mods });
    await this.send("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", clickCount: 1, modifiers: mods });
    await sleep(120);
  }
  async down(x, y) {
    await this.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
    await this.send("Input.dispatchMouseEvent", { type: "mousePressed", x, y, button: "left", clickCount: 1 });
  }
  async move(x, y) {
    await this.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y, button: "left", buttons: 1 });
  }
  async up(x, y) {
    await this.send("Input.dispatchMouseEvent", { type: "mouseReleased", x, y, button: "left", clickCount: 1 });
  }
  async tap(x, y) {
    await this.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x, y }] });
    await sleep(40);
    await this.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
    await sleep(150);
  }
  async type(text) {
    await this.send("Input.insertText", { text });
    await sleep(40);
  }
  async key(key, code = key, mods = 0, keyCode = 0) {
    const text = key === "Enter" ? "\r" : key.length === 1 ? key : undefined;
    await this.send("Input.dispatchKeyEvent", { type: "keyDown", key, code, modifiers: mods, windowsVirtualKeyCode: keyCode, text });
    await this.send("Input.dispatchKeyEvent", { type: "keyUp", key, code, modifiers: mods, windowsVirtualKeyCode: keyCode });
    await sleep(60);
  }
  async shot(file, clip) {
    const r = await this.send("Page.captureScreenshot", { format: "png", ...(clip ? { clip: { ...clip, scale: 1 } } : {}) });
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, Buffer.from(r.data, "base64"));
    console.log("saved", path.relative(SCRATCH, file));
  }
  async close() {
    await this.conn.send("Target.closeTarget", { targetId: this.targetId }).catch(() => {});
  }
}

let conn = null;
export async function browser() {
  if (conn) return conn;
  const v = await ensureBrowser();
  const ws = new WebSocket(v.webSocketDebuggerUrl);
  await new Promise((r, j) => {
    ws.onopen = r;
    ws.onerror = j;
  });
  conn = new Conn(ws);
  return conn;
}

/** A fresh isolated "device" (own cookies/localStorage) with one page. */
export async function device(url = "about:blank") {
  const c = await browser();
  const { browserContextId } = await c.send("Target.createBrowserContext", { disposeOnDetach: true });
  const { targetId } = await c.send("Target.createTarget", { url: "about:blank", browserContextId });
  const { sessionId } = await c.send("Target.attachToTarget", { targetId, flatten: true });
  const p = new Page(c, sessionId, targetId);
  p.contextId = browserContextId;
  await p.init();
  if (url !== "about:blank") await p.goto(url);
  return p;
}

export async function mock(pathname, body) {
  const r = await fetch(MOCK + pathname, { method: "POST", headers: { "content-type": "application/json" }, body: body ? JSON.stringify(body) : undefined });
  return r.json();
}

/**
 * A one-time pairing code: the mock's fixed one, or (a real throwaway server, REAL_TOKEN = a token
 * file such as its cli-token) a fresh code from `create_pairing` (only the newest code is valid).
 */
async function pairingCode() {
  if (!process.env.REAL_TOKEN) return "123456";
  const token = fs.readFileSync(process.env.REAL_TOKEN, "utf8").trim();
  const ws = new WebSocket(MOCK.replace("http", "ws") + `/api/ws?token=${encodeURIComponent(token)}`);
  await new Promise((res, rej) => {
    ws.onopen = res;
    ws.onerror = () => rej(new Error("can't reach the server for a pairing code"));
  });
  const ask = (id, method, params) =>
    new Promise((res) => {
      ws.onmessage = (m) => {
        const msg = JSON.parse(m.data);
        if (msg.id === id) res(msg);
      };
      ws.send(JSON.stringify({ id, method, ...(params ? { params } : {}) }));
    });
  await ask(1, "hello", { client: "test", version: "0" });
  const r = await ask(2, "create_pairing");
  ws.close();
  return r.result.code;
}
/**
 * In an open menu (the New menu), pick the row `label`; a row under "More" opens More first.
 * `touch`: tap it (phones) instead of clicking.
 */
export async function menuPick(p, label, touch = false) {
  await p.waitFor(`document.querySelector('.menu-pop .menu-item')`, 3000).catch(() => {});
  const row = (l) => p.eval(`(() => { const e = [...document.querySelectorAll('.menu-pop .menu-item')].find((x) => x.querySelector('.menu-label')?.textContent === ${JSON.stringify(l)}); if (!e) return null; e.scrollIntoView({ block: 'nearest' }); const r = e.getBoundingClientRect(); return { cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);
  const hit = (b) => (touch ? p.tap(b.cx, b.cy) : p.mouse(b.cx, b.cy));
  let b = await row(label);
  if (!b) {
    const more = await row("More");
    if (more) {
      await hit(more);
      await sleep(250);
      b = await row(label);
    }
  }
  if (!b) throw new Error(`no menu row ${label}`);
  await hit(b);
}

/** The New menu's rows as shown (labels), More still folded. */
export const menuLabels = (p) => p.eval(`[...document.querySelectorAll('.menu-pop .menu-item .menu-label')].map((e) => e.textContent)`);

/** Pair a page with the mock and load the app signed in. */
export async function signIn(p, name = "Test device") {
  await p.goto(APP, 600);
  const code = await pairingCode();
  await p.eval(`fetch('/api/pair',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({code:${JSON.stringify(code)},device_name:${JSON.stringify(name)}})}).then(r=>r.json()).then(j=>localStorage.setItem('tessera.token', JSON.stringify(j.token)))`);
  await p.goto(APP, 1500);
  // A fresh throwaway checkout may still be optimizing Vite dependencies.
  // This is setup only; transcript/composer latency is measured separately.
  await p.waitFor(`document.querySelector('.workspace')`, 120000);
}

/** Scroll away from the live tail like a reader before placing an older row in view. */
export async function readEarlier(p, selector = ".pane.focused .transcript") {
  const r = await p.box(selector);
  if (!r) throw new Error("no transcript " + selector);
  await p.send("Input.dispatchMouseEvent", { type: "mouseWheel", x: r.x + 8, y: r.cy, deltaX: 0, deltaY: -1000 });
  // A carousel or nested code scroller can consume the native wheel. Arm the
  // transcript itself before positioning the requested older row.
  await p.eval(`(() => { const t = document.querySelector(${JSON.stringify(selector)}); t.dispatchEvent(new WheelEvent('wheel', { deltaY: -1000, bubbles: true })); t.scrollTop = 0; })()`);
  await sleep(250);
}
let failures = 0;
export function check(cond, msg, extra) {
  if (cond) console.log("  ok  ", msg);
  else {
    failures++;
    console.log("  FAIL", msg, extra !== undefined ? JSON.stringify(extra).slice(0, 600) : "");
  }
  return cond;
}
export function finish() {
  console.log(failures ? `\n${failures} FAILURE(S)` : "\nALL OK");
  process.exitCode = failures ? 1 : 0;
}

/** A plain WebSocket client for the mock (no UI). */
export async function wsClient(name = "script") {
  const pr = await fetch(MOCK + "/api/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: await pairingCode(), device_name: name }) });
  const { token } = await pr.json();
  if (!token) throw new Error("pairing failed");
  const ws = new WebSocket(MOCK.replace("http", "ws") + `/api/ws?token=${token}`);
  const events = [];
  const pending = new Map();
  let id = 0;
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.event) events.push(msg.event);
    else if (pending.has(msg.id)) {
      pending.get(msg.id)(msg);
      pending.delete(msg.id);
    }
  };
  await new Promise((res, rej) => {
    ws.onopen = res;
    ws.onerror = rej;
  });
  const rpc = (method, params = {}) =>
    new Promise((resolve) => {
      const i = ++id;
      pending.set(i, resolve);
      // Unit requests take no params (the real server refuses `{}` for them).
      ws.send(JSON.stringify({ id: i, method, ...(Object.keys(params).length ? { params } : {}) }));
    });
  await rpc("hello", { client: "test", version: "0" });
  return { ws, rpc, events, close: () => ws.close() };
}

export async function run(fn) {
  try {
    await fn();
  } catch (e) {
    console.error("SCRIPT ERROR", e.stack ?? e.message);
    process.exitCode = 1;
  }
  process.exit(process.exitCode ?? 0);
}
