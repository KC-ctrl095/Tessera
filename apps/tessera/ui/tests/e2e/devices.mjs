// Batch 15 (DEV): Settings → Devices, one entry per computer. Desktop's desktop app and "Terminal on Desktop" (whose
// machine side is Desktop) are one entry, the same on every client; the laptop's app "Dell Laptop" and its machine
// (paired by link under the computer's own name, DESKTOP-EXAMPLE) are one entry saying what agents call it; two
// older terminals on two Raspberry Pis stay apart. Renaming (the whole computer takes the name; a machine's own
// name only changes on it, shown read-only elsewhere), signing out a sign-in that carries a machine says so, the
// phone app names a Galaxy Z Fold by its model (never "Android tablet"), and the desktop app on the laptop gives
// its machine the entry's name in one click. Desktop, folded phone (344 px), unfolded Fold, dark and light.
import { APP, MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const K_DESKTOP = "a1".repeat(16);
const K_LAPTOP = "b2".repeat(16);
const FOLD_UA = "Mozilla/5.0 (Linux; Android 15; SM-F966B Build/AP3A.240905.015.A2; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/139.0.0.0 Safari/537.36";

const machine = (id, name, extra = {}) => ({
  id, name, os: "linux x86_64", online: true, last_seen: Date.now(), paused: false, version: "0.1.0", sandbox: "on (namespaces + Landlock)", sandbox_ok: true, running: 0,
  preset: "work_in_folders", home: "/home/me", scope: { folders: [{ path: "/home/me/proj", write: true }], commands: true, ask: "always", timeout_secs: 300 }, recent: [], ...extra,
});

/** Desktop (app + terminal running its machine), the laptop (app + a machine paired on its own), a Fold, two Pis. */
async function scenario(ws) {
  const t = Date.now();
  await ws.rpc("mock_devices", {
    add: [
      { id: "dev_desktop_app", name: "Linux desktop", client: "app", key: K_DESKTOP, host: "Desktop", created_at: t - 30 * 864e5, last_seen: t - 60e3 },
      { id: "dev_desktop_term", name: "Terminal on Desktop", client: "terminal", key: K_DESKTOP, host: "Desktop", created_at: t - 20 * 864e5, last_seen: t - 30e3 },
      { id: "dev_laptop", name: "Dell Laptop", client: "app", key: K_LAPTOP, host: "DESKTOP-EXAMPLE", created_at: t - 10 * 864e5, last_seen: t - 3600e3 },
      // An older machine (no key): it joins the laptop by the computer's name.
      { id: "dev_laptop_node", name: "DESKTOP-EXAMPLE", kind: "machine", created_at: t - 9 * 864e5, last_seen: t - 3600e3 },
      { id: "dev_fold", name: "folding phone", client: "phone", model: "Galaxy Z Fold", named: true, created_at: t - 5 * 864e5, last_seen: t - 7200e3 },
      // Two older terminals on two Raspberry Pis: never one entry.
      { id: "dev_pi1", name: "Terminal on raspberrypi", created_at: t - 4 * 864e5, last_seen: t - 86400e3 },
      { id: "dev_pi2", name: "Terminal on raspberrypi", created_at: t - 3 * 864e5, last_seen: t - 86400e3 },
      { id: "dev_studio", name: "Studio PC", kind: "machine", key: "c3".repeat(16), host: "studio", created_at: t - 2 * 864e5, last_seen: t - 86400e3 },
    ],
    machines: [
      machine("dev_desktop_term", "Desktop"),
      machine("dev_laptop_node", "DESKTOP-EXAMPLE", { os: "windows x86_64, shell PowerShell", preset: "look_only", scope: { folders: [], commands: false, ask: "always", timeout_secs: 300 } }),
      machine("dev_studio", "Studio PC", { online: false }),
    ],
  });
}

const helpers = (p) => {
  const open = async (tab) => {
    await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings(${JSON.stringify(tab)}))`);
    await sleep(700);
  };
  const items = () =>
    p.eval(`[...document.querySelectorAll('.dev-item')].map(li => ({ name: li.querySelector('.dev-name').textContent, meta: li.querySelector('.dev-meta').textContent, badge: li.querySelector('.dev-mbadge')?.textContent.trim() ?? null, here: li.classList.contains('here'), open: li.classList.contains('open') }))`);
  const entry = (name) => `[...document.querySelectorAll('.dev-item')].find(li => li.querySelector('.dev-name').textContent === ${JSON.stringify(name)})`;
  const toggle = async (name) => {
    await p.eval(`${entry(name)}.querySelector('.dev-row').click()`);
    await sleep(400);
  };
  const details = (name) =>
    p.eval(`(() => { const li = ${entry(name)}; if (!li) return null; return { signins: [...li.querySelectorAll('.dev-device')].map(s => ({ client: s.dataset.client, role: s.querySelector('.dev-role').textContent, line: s.querySelector('.muted').textContent, out: s.querySelector('button')?.textContent.trim() ?? null })), naming: li.querySelector('.dev-naming-text')?.textContent ?? '', rename: !!li.querySelector('.dev-naming button'), agent: li.querySelector('.dev-agentname')?.textContent ?? null, machines: li.querySelectorAll('.dev-machine').length, sections: [...li.querySelectorAll('.dev-sec-title')].map(x => x.textContent) }; })()`);
  const dialog = () => p.eval(`document.querySelector('.dialog-body') ? { title: [...document.querySelectorAll('.modal h2')].pop()?.textContent ?? '', body: document.querySelector('.dialog-text')?.textContent ?? '', input: document.querySelector('.dialog-body input')?.value ?? null } : null`);
  const noSideScroll = () => p.eval(`(() => { const b = document.querySelector('.settings-body') || document.scrollingElement; return [...document.querySelectorAll('.settings-body *')].every(e => e.getBoundingClientRect().right <= innerWidth + 1) && b.scrollWidth <= b.clientWidth + 1; })()`);
  return { open, items, entry, toggle, details, dialog, noSideScroll };
};

/** A stand-in for the app's shell (Tauri): the app's own commands go to the mock over a WebSocket, `device_report` answers `report`. */
function shellScript(token, report, node = null) {
  return `(() => {
    const TOKEN = ${JSON.stringify(token)};
    let n = 0, ws = null, rid = 0;
    const cbs = {}, listeners = {}, pending = new Map();
    window.__calls = [];
    window.__node = ${JSON.stringify(node)};
    const emit = (event, payload) => (listeners[event] || []).forEach((id) => cbs[id] && cbs[id]({ event, id: 1, payload }));
    const status = (state) => emit("tessera://status", { state, serverId: "srv_mock", endpoint: location.origin, error: null, encrypted: true });
    const info = () => ({ node: window.__node ? JSON.parse(JSON.stringify(window.__node)) : null, appServer: "Mock server", serverHere: false });
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
        window.__calls.push({ cmd, args: JSON.parse(JSON.stringify(args)) });
        switch (cmd) {
          case "plugin:event|listen": (listeners[args.event] ||= []).push(args.handler); return args.handler;
          case "plugin:event|unlisten": return null;
          case "servers_get": return TOKEN ? { servers: [{ id: "srv_mock", name: "Mock server", endpoints: [location.origin], headers: {}, token: "", serverKey: "", fingerprint: "1111 2222", deviceName: "Paired name" }], active: "srv_mock" } : { servers: [], active: null };
          case "servers_save": window.__saved = args.config; return null;
          case "server_connect": if (!ws) connect(); else status("connected"); return null;
          case "server_disconnect": return null;
          case "app_resumed": return null;
          case "device_report": return ${JSON.stringify(report)};
          case "rpc": return new Promise((resolve, reject) => { const id = ++rid; pending.set(id, { resolve, reject }); const r = args.request; ws.send(JSON.stringify({ id, method: r.method, ...(r.params !== undefined ? { params: r.params } : {}) })); });
          case "this_computer_status": if (!window.__node) throw "command this_computer_status not found"; return info();
          case "this_computer_name": window.__node.name = args.name; return info();
          case "this_computer_log": return [];
          case "this_computer_home": return "/home/me";
          case "this_computer_startup_changes": return [];
          default: throw "command " + cmd + " not found";
        }
      },
    };
  })()`;
}

async function pairToken(name) {
  const r = await fetch(MOCK + "/api/pair", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ code: "123456", device_name: name }) });
  return (await r.json()).token;
}

run(async () => {
  await mock("/__mock/reset");
  const ws = await wsClient("devices script");
  await scenario(ws);
  const list = async () => (await ws.rpc("list_devices")).result;

  // ── the server's groups ──
  let l = await list();
  const group = (id) => l.find((d) => d.id === id)?.computer;
  check(group("dev_desktop_app") === group("dev_desktop_term") && group("dev_desktop_app") === "dev_desktop_app", "Desktop's app and its terminal (running its machine) are one computer, named by the first paired", l.filter((d) => d.id.startsWith("dev_desktop")));
  check(group("dev_laptop_node") === group("dev_laptop"), "the laptop's older machine joins it by the computer's own name");
  check(group("dev_pi1") !== group("dev_pi2"), "two older terminals on two Raspberry Pis stay apart");
  check(!l.some((d) => "key" in d) && l.find((d) => d.id === "dev_desktop_term").client === "terminal", "no computer key leaves the server; each says what it is");

  // ── desktop web UI ──
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Chrome on Linux");
  const h = helpers(p);
  await h.open("devices");
  await p.waitFor(`document.querySelector('.dev-list .dev-item')`, 8000);
  await sleep(500);
  let items = await h.items();
  const names = items.map((x) => x.name);
  check(items[0]?.here && items[0].meta.startsWith("This device · Browser"), "this device first: “This device · Browser” (it said what it is)", items[0]);
  check(names.filter((n) => n === "Desktop").length === 1 && !names.some((n) => /^Terminal on Desktop$|^Linux desktop$/.test(n)), "Desktop shows once (no “Terminal on Desktop” beside it)", names);
  const hg = items.find((x) => x.name === "Desktop");
  check(/Tessera app, terminal and machine for agents/.test(hg?.meta ?? "") && hg.badge === "Machine · Work in folders", "…saying what it is used for, with its machine's preset", hg);
  const lap = items.find((x) => x.name === "Dell Laptop");
  check(!!lap && !names.includes("DESKTOP-EXAMPLE") && /Tessera app and machine for agents/.test(lap.meta) && /Windows/.test(lap.meta), "the laptop is one entry, “Dell Laptop”", lap);
  check(names.filter((n) => n === "Terminal on raspberrypi").length === 2, "the two Pis stay two entries", names);
  check(names.includes("folding phone"), "the phone keeps the name the user gave it", names);
  await p.shot(`${SHOTS}/devices15-desk-dark.png`);

  await h.toggle("Desktop");
  let d = await h.details("Desktop");
  check(d.signins.length === 2 && d.signins[0].role === "Tessera app" && /^Terminal/.test(d.signins[1].role) && /the machine uses it/.test(d.signins[1].role), "inside: its app and its terminal, the machine using the terminal's sign-in", d.signins);
  check(d.signins[1].line.includes("“Terminal on Desktop”") && d.signins.every((s) => s.out === "Sign out"), "each sign-in with its old name and its own Sign out", d.signins);
  check(d.machines === 1 && d.sections.includes("Machine for agents") && d.rename && /Called “Desktop” on all your devices/.test(d.naming), "…its machine, and Rename", d);
  check(d.agent === null, "agents call it Desktop too: no second name");
  await p.shot(`${SHOTS}/devices15-desktop-desk-dark.png`);

  // Signing out the terminal says the machine goes with it.
  await p.eval(`[...${h.entry("Desktop")}.querySelectorAll('.dev-device')].find(s => s.dataset.client === 'terminal').querySelector('button').click()`);
  await sleep(400);
  let dlg = await h.dialog();
  check(!!dlg && /machine connection uses this sign-in/.test(dlg.body) && /agents can't use Desktop/.test(dlg.body), "signing out the terminal says the machine stops too", dlg);
  await p.click(".dialog-actions .btn-ghost");
  await sleep(300);
  check((await list()).some((x) => x.id === "dev_desktop_term"), "Cancel: still signed in");

  // Rename: the whole computer takes the name; the machine keeps its own (only Desktop changes it).
  await p.eval(`${h.entry("Desktop")}.querySelector('.dev-naming button').click()`);
  await sleep(400);
  dlg = await h.dialog();
  check(!!dlg && dlg.title === "Rename Desktop" && dlg.input === "Desktop", "Rename asks for the new name, filled in", dlg);
  await p.eval(`(() => { const i = document.querySelector('.dialog-body input'); const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; set.call(i, "Alex's PC"); i.dispatchEvent(new Event('input', { bubbles: true })); })()`);
  await p.click(".dialog-actions button[type=submit]");
  await sleep(800);
  l = await list();
  const renamed = l.filter((x) => x.id === "dev_desktop_app" || x.id === "dev_desktop_term");
  check(renamed.every((x) => x.name === "Alex's PC" && x.named), "both of its sign-ins are renamed, and marked as named by the user", renamed);
  check((await ws.rpc("list_machines")).result.find((m) => m.id === "dev_desktop_term").name === "Desktop", "…its machine's name is untouched");
  items = await h.items();
  check(items.some((x) => x.name === "Alex's PC") && !items.some((x) => x.name === "Desktop"), "the entry reads Alex's PC", items.map((x) => x.name));
  d = await h.details("Alex's PC");
  check(/Agents call it Desktop\. That name changes only on Desktop itself/.test(d?.agent ?? ""), "…and says plainly what agents call it and where that changes", d?.agent);
  await p.shot(`${SHOTS}/devices15-renamed-desk-dark.png`);

  // The laptop: what agents call it, read-only here; its machine-only sign-in can be unpaired, not renamed.
  await h.toggle("Dell Laptop");
  d = await h.details("Dell Laptop");
  check(/Agents call it DESKTOP-EXAMPLE\. That name changes only on DESKTOP-EXAMPLE itself/.test(d.agent ?? ""), "the laptop: agents call it DESKTOP-EXAMPLE, changed only on that computer", d.agent);
  check(d.signins.length === 2 && d.signins[1].client === "machine" && d.signins[1].out === "Sign out", "…its machine's own sign-in can be signed out", d.signins);
  const fine = await p.eval(`${h.entry("Dell Laptop")}.querySelector('.dev-machine .mach-fine').textContent`);
  check(/its name, on that computer/.test(fine), "the machine part says its name and what it allows change on that computer", fine);
  const widen = await p.eval(`${h.entry("Dell Laptop")}.querySelectorAll('.dev-machine [role=switch], .dev-machine [role=radio], .dev-machine input').length`);
  check(widen === 0, "nothing there can widen the machine or rename it");
  await p.shot(`${SHOTS}/devices15-laptop-desk-dark.png`);
  await p.theme("light");
  await sleep(300);
  await p.shot(`${SHOTS}/devices15-laptop-desk-light.png`);
  await p.theme("dark");

  // A machine on its own: no Rename here; the server refuses it too.
  await h.toggle("Studio PC");
  d = await h.details("Studio PC");
  check(!d.rename && /changes only on that computer/.test(d.naming), "a machine on its own: no Rename, one line saying where", d.naming);
  const refused = await ws.rpc("rename_device", { id: "dev_studio", name: "Studio" });
  check(refused.error?.code === "bad_request" && /machine only/.test(refused.error.message), "…and the server refuses renaming it", refused.error);
  for (const [bad, why] of [["", "empty"], ["x".repeat(61), "too long"], ["two\nlines", "two lines"], ["a\u202eb", "hidden characters"]]) {
    const r = await ws.rpc("rename_device", { id: "dev_laptop", name: bad });
    check(r.error?.code === "bad_request", `a name that is ${why} is refused`, r.error);
  }

  // Another device sees the same entries (and the rename) live.
  const ph = await device();
  await ph.viewport(344, 882, true, 3);
  await signIn(ph, "Second browser");
  const hp = helpers(ph);
  await hp.open("devices");
  await ph.waitFor(`document.querySelector('.dev-list .dev-item')`, 8000);
  await sleep(400);
  const other = (await hp.items()).map((x) => x.name).filter((n) => !/browser|Chrome on Linux|Second/i.test(n));
  const mine = (await h.items()).map((x) => x.name).filter((n) => !/browser|Chrome on Linux|Second/i.test(n));
  check(JSON.stringify([...other].sort()) === JSON.stringify([...mine].sort()), "a second device shows the very same entries", { other, mine });
  await ws.rpc("rename_device", { id: "dev_laptop", name: "Kitchen laptop" });
  await sleep(700);
  check((await hp.items()).some((x) => x.name === "Kitchen laptop") && (await h.items()).some((x) => x.name === "Kitchen laptop"), "a rename shows up on every device without reloading");

  // ── folded phone (344 px), dark and light ──
  for (const theme of ["dark", "light"]) {
    await ph.theme(theme);
    await sleep(300);
    check(await hp.noSideScroll(), `folded phone (${theme}): nothing runs off the side`);
    const nm = await ph.eval(`[...document.querySelectorAll('.dev-name')].map(n => n.scrollWidth <= n.clientWidth + 1)`);
    check(nm.every(Boolean), `folded phone (${theme}): names show in full`);
    await ph.shot(`${SHOTS}/devices15-phone-${theme}.png`);
    if (!(await ph.eval(`${hp.entry("Alex's PC")}.classList.contains('open')`))) await hp.toggle("Alex's PC");
    check(await hp.noSideScroll(), `folded phone (${theme}): an open computer fits`);
    const btns = await ph.eval(`[...${hp.entry("Alex's PC")}.querySelectorAll('.dev-details button')].map(b => { const r = b.getBoundingClientRect(); return { t: b.textContent.trim(), h: r.height, right: r.right }; })`);
    check(btns.length >= 4 && btns.every((b) => b.h >= 28 && b.right <= 344), `folded phone (${theme}): its buttons fit and are tappable`, btns);
    await ph.shot(`${SHOTS}/devices15-desktop-phone-${theme}.png`);
  }
  // ── unfolded Fold ──
  await ph.viewport(884, 1104, true, 2.5);
  await sleep(400);
  check(await hp.noSideScroll(), "unfolded Fold: nothing runs off the side");
  await ph.shot(`${SHOTS}/devices15-fold-open.png`);
  // Both remote machines and plain phones must survive the server's list refreshes.
  for (const [page, helper, layout, width, height, mobile] of [[p, h, "desktop", 1440, 900, false], [ph, hp, "phone", 344, 882, true], [ph, hp, "fold", 884, 1104, true]]) {
    await page.viewport(width, height, mobile);
    for (const theme of ["dark", "light"]) {
      await page.theme(theme);
      for (const name of ["Alex's PC", "folding phone"]) {
        if (!(await page.eval(`${helper.entry(name)}.classList.contains('open')`))) await helper.toggle(name);
        const machine = name === "Alex's PC";
        const before = await page.eval(`(() => {
          const panel = ${helper.entry(name)}, b = document.querySelector('.settings-body');
          const e = panel.querySelector(${JSON.stringify(machine ? '.mach-actions button[aria-label="Pause Desktop"]' : '.dev-naming button')});
          b.scrollTop += e.getBoundingClientRect().top - b.getBoundingClientRect().top - b.clientHeight / 2;
          e.focus({ preventScroll: true });
          window.__place = { panel, e, b, top: b.scrollTop };
          return b.scrollTop;
        })()`);
        check(before > 40, `${layout} ${theme}: ${name} starts scrolled down`, before);
        if (machine) await page.eval(`window.__place.e.click()`);
        else await ws.rpc("rename_device", { id: "dev_fold", name: "folding phone" });
        await sleep(800);
        const after = await page.eval(`(() => { const { panel, e, b, top } = window.__place; return { panel: panel.isConnected, control: e.isConnected, focus: document.activeElement === e, delta: b.scrollTop - top }; })()`);
        check(after.panel && after.control && after.focus && Math.abs(after.delta) <= 2, `${layout} ${theme}: ${name} keeps its panel, control, focus and scroll`, after);
        if (machine) {
          check((await ws.rpc("list_machines")).result.find(m => m.id === "dev_desktop_term").paused, `${layout} ${theme}: the machine was paused`);
          await ws.rpc("set_machine_paused", { id: "dev_desktop_term", paused: false });
          await sleep(400);
        }
        check(await helper.noSideScroll(), `${layout} ${theme}: ${name} fits`);
        await page.shot(`${SHOTS}/devices-scroll-${machine ? 'remote' : 'plain-phone'}-${layout}-${theme}.png`);
      }
    }
  }
  console.log("errors:", p.errors, ph.errors);
  check(!p.errors.length && !ph.errors.length, "no page errors (web)");
  await p.close();
  await ph.close();

  // ── the Fold: named by its model, never "Android tablet" ──
  const f = await device();
  await f.viewport(884, 1104, true, 2.5);
  await f.send("Emulation.setUserAgentOverride", { userAgent: FOLD_UA });
  await f.goto(APP, 1200);
  const guess = await f.eval(`Promise.all([__mod('lib/util.ts').then(m => m.defaultDeviceName()), __mod('lib/devices.ts').then(m => m.suggestedName('fallback'))])`);
  check(guess[0] === "Android device" && guess[1] === "Galaxy Z Fold", "an unfolded Fold in a browser: “Galaxy Z Fold” from its model, “Android device” at worst, never a tablet", guess);
  const line = await f.eval(`document.querySelector('.device-name-line b')?.textContent ?? null`);
  check(line === null || line === "Galaxy Z Fold", "pairing suggests that name", line);
  await f.close();

  // The phone app on a Fold: the shell knows the maker and model; the server hears what it is.
  const foldToken = await pairToken("folding phone");
  const a = await device();
  await a.viewport(884, 1104, true, 2.5);
  await a.send("Emulation.setUserAgentOverride", { userAgent: FOLD_UA });
  await a.send("Page.addScriptToEvaluateOnNewDocument", { source: shellScript(foldToken, { computer: null, host: null, manufacturer: "samsung", model: "SM-F966B" }) });
  await a.goto(APP, 1800);
  await a.waitFor(`document.querySelector('.workspace')`, 12000);
  await sleep(800);
  l = await list();
  const fold = l.filter((x) => x.name === "folding phone").pop();
  check(fold?.client === "phone" && fold.model === "Galaxy Z Fold", "the phone app says it is a phone app on a Galaxy Z Fold", fold);
  check(l.some((x) => x.name === "folding phone" && x.id === fold.id), "…and its name stays the one it paired with");
  const suggest = await a.eval(`__mod('lib/devices.ts').then(m => m.suggestedName('fallback'))`);
  check(suggest === "Galaxy Z Fold", "pairing again from the app suggests “Galaxy Z Fold”", suggest);
  console.log("phone app errors:", a.errors);
  check(!a.errors.length, "no page errors (phone app)");
  await a.close();

  // ── the desktop app on the laptop: it joins its entry by its key, and gives its machine the entry's name ──
  const lapToken = await pairToken("Dell Laptop");
  const node = {
    enabled: true, revoked: false, name: "DESKTOP-EXAMPLE", version: "0.1.0", scope: { folders: [], commands: false, ask: "always", timeout_secs: 300 }, paired: true, running: true, connected: true,
    paused: false, calls: 0, sandbox: "off (no sandbox on Windows yet)", sandbox_ok: false, service: { available: true, installed: true, active: true, enabled: true, linger: false },
    log_file: "C:/tessera/node.log", preset: "look_only", home: "/home/me", startup_changes: 0,
  };
  const w = await device();
  await w.viewport(1280, 860);
  await w.send("Page.addScriptToEvaluateOnNewDocument", { source: shellScript(lapToken, { computer: K_LAPTOP, host: "DESKTOP-EXAMPLE", manufacturer: null, model: null }, node) });
  await w.goto(APP, 1800);
  await w.waitFor(`document.querySelector('.workspace')`, 12000);
  const hw = helpers(w);
  await hw.open("thiscomputer");
  await w.waitFor(`document.querySelector('.dev-this')`, 8000);
  await sleep(500);
  items = await hw.items();
  check(items[0]?.here && items[0].name === "Kitchen laptop" && items[0].open && !items.some((x) => x.name === "Dell Laptop" || x.name === "DESKTOP-EXAMPLE"), "the laptop's desktop app: its own entry first, the same one the others show (by its key)", items.slice(0, 3));
  d = await hw.details("Kitchen laptop");
  check(d.sections.includes("Machine for agents · this computer") && /Agents call it DESKTOP-EXAMPLE/.test(d.agent ?? "") && /Rename it here/.test(d.agent ?? "") && d.rename, "on that computer: the local controls, and its one Rename gives agents the same name", d);
  check(!(await w.eval(`!!document.querySelector('.dev-agentname .link-btn, .mach-namerow, [aria-label="Change the name agents use"]')`)), "…no second or third way to name it (batch 16: one name per computer)");
  await w.shot(`${SHOTS}/devices15-thiscomputer-before.png`);
  await w.eval(`document.querySelector('.dev-this .dev-naming button').click()`);
  await w.waitFor(`document.querySelector('.dialog-body input')`, 4000);
  check((await w.eval(`document.querySelector('.dialog-body input').value`)) === "Kitchen laptop", "Rename starts from its name");
  await w.click(".dialog-actions button[type=submit]");
  await sleep(700);
  const named = await w.eval(`window.__calls.filter(c => c.cmd === 'this_computer_name').map(c => c.args.name)`);
  d = await hw.details("Kitchen laptop");
  check(JSON.stringify(named) === JSON.stringify(["Kitchen laptop"]) && d.agent === null, "a local command renames the machine; one name everywhere", { named, agent: d.agent });
  check(/and by your agents/.test(d.naming), "…and the entry says so", d.naming);
  check(!(await w.eval(`window.__calls.some(c => c.cmd === 'rpc' && /node|machine_name/.test(c.args.request.method))`)), "…never through the server");
  await w.shot(`${SHOTS}/devices15-thiscomputer-after.png`);
  console.log("desktop app errors:", w.errors);
  check(!w.errors.length, "no page errors (desktop app)");
  await w.close();

  ws.close();
  finish();
});
