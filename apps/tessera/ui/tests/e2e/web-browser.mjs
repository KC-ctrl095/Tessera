// "Use Tessera in a web browser", for real: a throwaway `tessera serve` (its own TESSERA_HOME, a free
// port, the built web app) behind a small proxy that forwards like cloudflared does (https, the
// browser's address), and a browser that pairs at /web/ with a code made on another device, end to end
// encrypted with a key of its own that no script can read out. Checks: nothing at /web/ while off;
// the page's security policy blocks nothing the app needs; pairing and the app working; the browser in
// Devices; signed out there it goes back to pairing; turned off it stops, and comes back when on again.
// Needs the server binary (TESSERA_BIN, else $CARGO_TARGET_DIR or target/debug/tessera) and the built
// web app (npm run build): without them it says it skipped.
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { SHOTS, check, device, finish, run, sleep } from "./cdp.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const ui = path.resolve(here, "../..");
const repo = path.resolve(ui, "../../..");
const dist = path.join(ui, "dist");
const bin =
  process.env.TESSERA_BIN ??
  [process.env.CARGO_TARGET_DIR && path.join(process.env.CARGO_TARGET_DIR, "debug/tessera"), path.join(repo, "target/debug/tessera")].find((f) => f && fs.existsSync(f));

const freePort = () =>
  new Promise((res) => {
    const s = net.createServer();
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => res(port));
    });
  });

/** A proxy on "this computer" that forwards like cloudflared: the original host, https, the browser's address. The
 * browser reaches it over plain http here (no certificate), so the page's origin goes on as the https one it would
 * have behind a real edge. */
function tunnel(port, target) {
  const forwarded = (h) => ({ ...h, ...(h.origin ? { origin: h.origin.replace(/^http:/, "https:") } : {}), "x-forwarded-proto": "https", "x-forwarded-for": "198.51.100.23", "cf-connecting-ip": "198.51.100.23", "cf-visitor": '{"scheme":"https"}' });
  const server = http.createServer((req, res) => {
    const up = http.request({ host: "127.0.0.1", port: target, method: req.method, path: req.url, headers: forwarded(req.headers) }, (r) => {
      res.writeHead(r.statusCode ?? 502, r.headers);
      r.pipe(res);
    });
    up.on("error", () => res.destroy());
    req.pipe(up);
  });
  server.on("upgrade", (req, socket, head) => {
    const up = net.connect(target, "127.0.0.1", () => {
      const lines = [`${req.method} ${req.url} HTTP/1.1`];
      for (const [k, v] of Object.entries(forwarded(req.headers))) lines.push(`${k}: ${v}`);
      up.write(lines.join("\r\n") + "\r\n\r\n");
      if (head?.length) up.write(head);
      up.pipe(socket);
      socket.pipe(up);
    });
    up.on("error", () => socket.destroy());
    socket.on("error", () => up.destroy());
  });
  return new Promise((res) => server.listen(port, "127.0.0.1", () => res(server)));
}

/** Type a pairing code into the pairing page's field. */
const enterCode = (p, code) =>
  p.eval(`(() => { const i = document.querySelector('.pair-box input'); const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; set.call(i, ${JSON.stringify(code)}); i.dispatchEvent(new Event('input', { bubbles: true })); })()`);
const pairButton = (p) => p.clickText("button", "Pair & connect");

run(async () => {
  if (!bin || !fs.existsSync(path.join(dist, "index.html"))) {
    console.log(`ALL OK (skipped: needs ${!bin ? "the tessera binary (TESSERA_BIN)" : "the built web app (npm run build)"})`);
    return;
  }
  const home = fs.mkdtempSync(path.join(os.tmpdir(), "tessera-web-e2e-"));
  // A home of its own: nothing of the user's (their Claude Code, browsers, settings) is looked at.
  const PATH = (process.env.PATH ?? "").split(":").filter((d) => d && !fs.existsSync(path.join(d, "claude"))).join(":");
  const env = { ...process.env, PATH, HOME: home, TESSERA_HOME: home, XDG_CONFIG_HOME: path.join(home, "xdg-config"), XDG_DATA_HOME: path.join(home, "xdg-data"), XDG_CACHE_HOME: path.join(home, "xdg-cache"), RUST_LOG: "warn" };
  const port = await freePort();
  const server = spawn(bin, ["serve", "--bind", `127.0.0.1:${port}`, "--web", dist, "--no-scheduler"], { env, stdio: ["ignore", "ignore", "pipe"] });
  let serverLog = "";
  server.stderr.on("data", (d) => (serverLog = (serverLog + d).slice(-4000)));
  const proxyPort = await freePort();
  const proxy = await tunnel(proxyPort, port);
  const done = () => {
    proxy.close();
    server.kill();
    fs.rmSync(home, { recursive: true, force: true });
  };
  try {
    const local = `http://127.0.0.1:${port}`;
    const remote = `http://localhost:${proxyPort}`;
    for (let i = 0; i < 80; i++) {
      if (await fetch(`${local}/api/health`).then((r) => r.ok).catch(() => false)) break;
      await sleep(250);
    }
    // "Another device": the terminal on the server (its own login).
    spawnSync(bin, ["sessions", "--plain"], { env, timeout: 30_000 });
    const token = fs.readFileSync(path.join(home, "cli-token"), "utf8").trim();
    const ws = new WebSocket(`ws://127.0.0.1:${port}/api/ws?token=${encodeURIComponent(token)}`);
    await new Promise((res, rej) => {
      ws.onopen = res;
      ws.onerror = () => rej(new Error("no connection to the server"));
    });
    const waiting = new Map();
    let ids = 0;
    ws.onmessage = (m) => {
      const msg = JSON.parse(m.data);
      if (msg.id && waiting.has(msg.id)) waiting.get(msg.id)(msg);
    };
    const rpc = (method, params) =>
      new Promise((res) => {
        const id = ++ids;
        waiting.set(id, res);
        ws.send(JSON.stringify({ id, method, ...(params ? { params } : {}) }));
      });
    await rpc("hello", { client: "test", version: "0" });
    await rpc("complete_setup");
    // The proxy's address, as the user adds one of their own (Settings → Connection): the API answers only pages
    // from the server's own addresses.
    await rpc("set_remote_access", { urls: [`https://localhost:${proxyPort}`] });
    const newCode = async () => (await rpc("create_pairing")).result.code;

    // ── off: nothing there ──
    const off = await fetch(`${remote}/web/`);
    check(off.status === 404 && (await off.text()) === "", "off (the default): nothing at /web/", off.status);
    const st = (await rpc("set_web_access", { on: true })).result?.web;
    check(st?.on === true, "turned on from a paired device", st);

    // ── the page and its policy ──
    const page = await fetch(`${remote}/web/`);
    const csp = page.headers.get("content-security-policy") ?? "";
    check(page.status === 200 && csp.startsWith("default-src 'none'") && /frame-ancestors 'none'/.test(csp), "the web app, with its strict policy", { status: page.status, csp });
    const foreign = await fetch(`${remote}/web/api/pair/start`, { method: "POST", headers: { origin: "http://elsewhere.example", "content-type": "application/json" }, body: JSON.stringify({ pake: "AAAA" }) });
    check(foreign.status === 403, "a page from another site gets nothing from its API", foreign.status);

    const p = await device();
    await p.viewport(1280, 820);
    // Every security policy violation on the page (the app must need none).
    await p.send("Page.addScriptToEvaluateOnNewDocument", { source: `window.__csp = []; addEventListener('securitypolicyviolation', (e) => window.__csp.push(e.violatedDirective + ' ' + e.blockedURI));` });
    await p.goto(`${remote}/web/`, 2500);
    await p.waitFor(`/Pair this browser/.test(document.body.textContent)`, 15000);
    check(/Settings → Devices/.test(await p.eval(`document.querySelector('.pair-box')?.textContent ?? ''`)), "the pairing page says where a code comes from");
    for (const theme of ["dark", "light"]) {
      await p.theme(theme);
      await sleep(200);
      await p.shot(`${SHOTS}/web-browser-pair-${theme}.png`);
    }
    await p.theme("dark");

    // A wrong code first: refused in plain words.
    await newCode();
    await enterCode(p, "1111-2222");
    await pairButton(p);
    await p.waitFor(`!!document.querySelector('.form-error')`, 10000).catch(() => {});
    check(/wrong pairing code/i.test(await p.eval(`document.querySelector('.form-error')?.textContent ?? ''`)), "a wrong code: refused, plainly", await p.eval(`document.querySelector('.form-error')?.textContent ?? ''`));

    // The right one: paired and in.
    const code = await newCode();
    await enterCode(p, code);
    await pairButton(p);
    await p.waitFor(`/You're in/.test(document.body.textContent) || !!document.querySelector('.workspace')`, 20000).catch(() => {});
    check(await p.eval(`/You're in/.test(document.body.textContent) || !!document.querySelector('.workspace')`), "paired with the code, end to end encrypted", await p.eval(`document.querySelector('.form-error')?.textContent ?? document.body.textContent.slice(0, 200)`));
    const fp = await p.eval(`document.querySelector('.fp-value')?.textContent ?? ''`);
    check(/^[0-9a-f]{4}( [0-9a-f]{4}){4}$/.test(fp.trim()), "it shows the server key's fingerprint", fp);
    await p.shot(`${SHOTS}/web-browser-paired.png`);
    await p.goto(`${remote}/web/`, 2500);
    await p.waitFor(`!!document.querySelector('.workspace')`, 20000).catch(() => {});
    check(await p.eval(`!!document.querySelector('.workspace')`), "after a reload: straight into the app");
    await sleep(1500);
    check(!(await p.eval(`/couldn't|offline|Reconnecting/i.test(document.querySelector('.conn-banner, .offline-banner')?.textContent ?? '')`)), "connected (no offline banner)");

    // What it keeps: the token, the server's key; its own key only as an unreadable key object.
    const kept = await p.eval(`(async () => {
      const w = JSON.parse(localStorage.getItem('tessera.web') || 'null');
      const pair = await new Promise((res) => { const r = indexedDB.open('tessera-web'); r.onsuccess = () => { const g = r.result.transaction('keys').objectStore('keys').get('device'); g.onsuccess = () => res(g.result); }; });
      let exported = 'refused';
      try { await crypto.subtle.exportKey('pkcs8', pair.privateKey); exported = 'exported'; } catch {}
      return { token: !!w?.token, serverKey: !!w?.serverKey, keyObject: pair?.privateKey instanceof CryptoKey, extractable: pair?.privateKey?.extractable, exported, inStorage: JSON.stringify(localStorage).includes('BEGIN') };
    })()`);
    check(kept.token && kept.serverKey && kept.keyObject && kept.extractable === false && kept.exported === "refused", "its key can't be read out (a non-extractable Web Crypto key)", kept);
    const violations = await p.eval(`window.__csp ?? []`);
    check(Array.isArray(violations) && violations.length === 0, "the app needs nothing its security policy blocks", violations);
    check(!p.errors.some((e) => /Content Security Policy|Refused to/.test(e)), "no policy errors in the console", p.errors.slice(0, 3));

    // It works: a chat made here is on the server.
    const listed = (await rpc("list_devices")).result;
    const me = listed.find((d) => d.browser);
    check(!!me && /on Linux|Chrome|Browser/.test(me.name), "it shows up in Devices, as a browser", listed.map((d) => [d.name, d.browser]));

    // ── signed out in Devices: back to pairing ──
    await rpc("revoke_device", { id: me.id });
    await p.waitFor(`/Pair this browser/.test(document.body.textContent)`, 20000).catch(() => {});
    check(await p.eval(`/Pair this browser/.test(document.body.textContent)`), "signed out in Devices: it's back at pairing");
    check(await p.eval(`localStorage.getItem('tessera.web') === null`), "…and forgot its login");

    // ── paired again, then turned off: it stops; on again: it's back ──
    const code2 = await newCode();
    await enterCode(p, code2);
    await pairButton(p);
    await p.waitFor(`/You're in/.test(document.body.textContent) || !!document.querySelector('.workspace')`, 20000).catch(() => {});
    await p.goto(`${remote}/web/`, 2500);
    await p.waitFor(`!!document.querySelector('.workspace')`, 20000).catch(() => {});
    await rpc("set_web_access", { on: false });
    await p.waitFor(`/turned off on this server/.test(document.body.textContent)`, 20000).catch(() => {});
    check(await p.eval(`/turned off on this server/.test(document.body.textContent)`), "turned off: the browser stops, and says why", await p.eval(`document.body.textContent.slice(0, 300)`));
    await p.shot(`${SHOTS}/web-browser-off.png`);
    check((await fetch(`${remote}/web/`)).status === 404, "…and nothing is served");
    await rpc("set_web_access", { on: true });
    await p.waitFor(`!/turned off on this server/.test(document.body.textContent)`, 30000).catch(() => {});
    check(!(await p.eval(`/turned off on this server/.test(document.body.textContent)`)), "on again: it reconnects by itself, still paired");

    // ── a phone-sized pairing page ──
    const m = await device();
    await m.viewport(344, 882, true, 2.625);
    await m.goto(`${remote}/web/`, 2500);
    await m.waitFor(`/Pair this browser/.test(document.body.textContent)`, 15000).catch(() => {});
    check(await m.eval(`document.documentElement.scrollWidth <= innerWidth + 1`), "the pairing page fits a folded phone's cover screen");
    await m.shot(`${SHOTS}/web-browser-pair-cover.png`);
    await m.close();
    await p.close();
    ws.close();
  } catch (e) {
    console.log("server log:", serverLog);
    throw e;
  } finally {
    done();
  }
  finish();
});
