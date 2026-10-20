// A terminal in a tile (batch 15, item 7). "Terminal" is in the one New menu;
// terminals are off on a server until turned on (the tile says so, explains
// it and offers it, sandboxed where the sandbox works); then a real terminal
// (xterm.js, loaded only now): typing, output, its size, flow control under a
// flood of output, copy and paste in the menu, how it ended and "Start again".
// It belongs to this device: never in the shared mosaics, another device is
// told it isn't open, it comes back after a reload, closing the tile ends it,
// and signing the device out ends it. Places: this server, or a machine that
// allows terminals (picked in the tile). A connection that isn't end-to-end
// encrypted can't open one. Settings → Terminal: the choice and the record
// (who, where, when; never what was typed). A phone gets the extra keys
// (Esc, Tab, Ctrl, Alt, arrows, Paste). Like a native terminal (batch 17):
// the system's monospace font, selecting copies, right-click pastes, Ctrl+C,
// Ctrl+K and the like reach the program while Alt+arrows/digits stay the
// app's, find (Ctrl+Shift+F), text size (Ctrl+= / Ctrl+- / Ctrl+0), web links
// open only with Ctrl+click, and the ⋯ menu switches between this server and
// the machines that allow terminals. Desktop, a folded phone (344 px) and the
// unfolded Fold, light and dark.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient, menuLabels, menuPick } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const waitFor = async (fn, ms = 8000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const v = await fn().catch(() => null);
    if (v) return v;
    await sleep(100);
  }
  return null;
};
const closeMenus = (p) => p.eval(`(document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })), true)`);
/** What the terminal tile `sel` shows: its rows as text. */
const screen = (p, sel = ".term-tile") => p.eval(`[...(document.querySelector('${sel} .xterm-rows')?.children ?? [])].map((r) => r.textContent).join('\\n')`);
const term = (probe) => probe.rpc("mock_terminal").then((r) => r.result);
const xtermLoaded = (p) => p.eval(`performance.getEntriesByType('resource').some((e) => /xterm/i.test(e.name))`);
const spawn = async (p, label) => {
  await p.click(`.topbar .tb-new-more`);
  await sleep(250);
  await menuPick(p, label);
  await sleep(500);
};

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  let probe = await wsClient("probe");

  // ── a computer ──────────────────────────────────────────────────────────
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Desktop");
  await p.theme("dark");
  await sleep(300);

  console.log("— the New menu, and a server where terminals are off");
  check(!(await xtermLoaded(p)), "the terminal (xterm.js) isn't loaded before a terminal opens");
  await p.click(`.topbar .tb-new-more`);
  await sleep(250);
  const menu = await menuLabels(p);
  check(!menu.includes("Terminal") && menu[menu.length - 1] === "More", "New keeps Terminal one step deeper, under More", menu);
  await p.click(".menu-pop .menu-more");
  await sleep(250);
  const more = await menuLabels(p);
  check(more.includes("Terminal"), "…More opens in place and offers Terminal", more);
  const desc = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-item')].find((e) => e.textContent.includes('Terminal'))?.textContent`);
  check(/for you only/.test(desc ?? ""), "…described as for the user only", desc);
  await p.clickText(".menu-item", "Terminal");
  const T = await waitFor(() => p.eval(`document.querySelector('.term-tile')?.closest('[data-pane-id]')?.dataset.paneId`));
  check(!!T, "Terminal opens a tile at once (no dialog)", T);
  const off = await waitFor(() => p.eval(`document.querySelector('.term-off')?.textContent`));
  check(/Terminals are off on this server/.test(off ?? "") && /as your account there/.test(off ?? "") && /agents never get one/.test(off ?? ""), "terminals are off: the tile says so and what a terminal can do", off);
  const btns = await p.eval(`[...document.querySelectorAll('.term-off button')].map((b) => b.textContent.trim())`);
  check(btns.includes("Turn on, sandboxed") && btns.includes("Turn on, full access"), "…and offers to turn them on, sandboxed first", btns);
  check(!(await p.eval(`!!document.querySelector('.modal, [role=dialog]')`)), "no dialog");
  check(!(await xtermLoaded(p)), "…still without loading the terminal");
  await p.shot(`${SHOTS}/terminal-off-desktop.png`);
  await p.clickText(".term-off button", "Turn on, full access");
  await sleep(300);
  const ask = await p.eval(`document.querySelector('.modal, [role=dialog], [role=alertdialog]')?.textContent`);
  check(/full access/i.test(ask ?? "") && /keys and saved passwords/.test(ask ?? ""), "Turn on, full access asks first, saying what it means (as Settings does)", ask?.slice(0, 160));
  await closeMenus(p);
  await sleep(300);
  check((await term(probe)).settings.access === "off" && !(await p.eval(`!!document.querySelector('.modal, [role=dialog], [role=alertdialog]')`)), "…and without a yes nothing changed");

  console.log("— turned on (sandboxed): a real terminal");
  await p.clickText(".term-off button", "Turn on, sandboxed");
  const first = await waitFor(async () => {
    const s = await screen(p);
    return /Welcome to the mock terminal/.test(s) ? s : null;
  });
  check(!!first, "the terminal opens on the server and shows its first prompt", first?.slice(0, 120));
  check(await xtermLoaded(p), "xterm.js loads now");
  check((await term(probe)).settings.access === "sandboxed", "the server's setting is sandboxed now");
  const sub = await p.eval(`document.querySelector('.term-tile .term-sub')?.textContent`);
  check(/Mock server · sandboxed/.test(sub ?? ""), "its header says where and how it is confined", sub);
  check(await p.eval(`document.activeElement?.classList.contains('xterm-helper-textarea')`), "it has the keyboard");
  await p.type("echo hello there");
  await p.key("Enter");
  const echoed = await waitFor(async () => {
    const s = await screen(p);
    return /\nhello there/.test(s) ? s : null;
  });
  check(!!echoed, "typing goes to the shell and its output shows", (await screen(p)).slice(-200));
  let st = await term(probe);
  const mine = st.terms[0];
  check(st.terms.length === 1 && mine.inputs.join("").includes("echo hello there\r"), "the keystrokes reached the terminal in order", mine?.inputs);
  await p.type("stty size");
  await p.key("Enter");
  const size = await waitFor(async () => {
    const m = /\n(\d+) (\d+)\s*\n/.exec(await screen(p));
    return m ? { rows: +m[1], cols: +m[2] } : null;
  });
  const fitted = await p.eval(`(() => { const r = document.querySelector('.term-tile .term-screen').getBoundingClientRect(); return { w: r.width, h: r.height }; })()`);
  check(!!size && size.cols >= 40 && size.rows > 15 && size.cols * 7 < fitted.w && size.cols * 12 > fitted.w, "the terminal is sized to the tile", { size, fitted });
  await p.shot(`${SHOTS}/terminal-desktop-dark.png`);

  console.log("— flow control");
  await p.type("flood 3000000");
  await p.key("Enter");
  const flooded = await waitFor(async () => {
    const t = (await term(probe)).terms[0];
    return t && t.produced > 3_000_000 && t.acked >= t.produced - 64 ? t : null;
  }, 30000);
  check(!!flooded, "a flood of 3 MB goes through, acknowledged as it is drawn", flooded && { produced: flooded.produced, acked: flooded.acked });
  check(!!flooded && flooded.max_unacked <= (256 << 10) + (16 << 10), "…never more than the window ahead of the device", flooded?.max_unacked);
  check(/\$ $/.test((await screen(p)).trimEnd() + " "), "…and the prompt comes back");

  console.log("— its menu: copy, paste, select all, clear, close");
  await p.click(`[data-pane-id="${T}"] [aria-label="Terminal options"]`);
  await sleep(250);
  const tm = await menuLabels(p);
  check(["Copy", "Paste", "Select all", "Clear the screen", "Close terminal"].every((x) => tm.includes(x)), "Copy, Paste, Select all, Clear the screen, Close terminal", tm);
  await p.clickText(".menu-item", "Clear the screen");
  await sleep(200);
  await p.type("colors");
  await p.key("Enter");
  await waitFor(async () => /green/.test(await screen(p)));
  await p.click(`[data-pane-id="${T}"] [aria-label="Terminal options"]`);
  await sleep(200);
  await p.clickText(".menu-item", "Select all");
  await sleep(150);
  const sel = await p.eval(`(() => { const t = document.querySelector('.term-tile .xterm-selection'); return !!t; })()`);
  check(sel, "Select all selects the terminal's text");
  await p.eval(`window.__copied = null; navigator.clipboard.writeText = async (t) => { window.__copied = t; }`);
  await p.click(`[data-pane-id="${T}"] [aria-label="Copy"]`);
  await sleep(200);
  check(/red green yellow blue bold/.test((await p.eval(`window.__copied`)) ?? ""), "Copy puts the selection on the clipboard", await p.eval(`(window.__copied || '').slice(-80)`));
  await p.eval(`navigator.clipboard.readText = async () => 'pasted words'`);
  await p.click(`[data-pane-id="${T}"] [aria-label="Terminal options"]`);
  await sleep(200);
  await p.clickText(".menu-item", "Paste");
  await sleep(300);
  check((await term(probe)).terms[0].inputs.join("").includes("pasted words"), "Paste types the clipboard's text into the terminal");
  await p.key("Enter");
  await sleep(200);

  console.log("— like a native terminal");
  const inputs = async () => (await term(probe)).terms[0].inputs.join("");
  const font = await p.eval(`getComputedStyle(document.querySelector('.term-tile .xterm-rows')).fontFamily`);
  check(/monospace/.test(font) && !/Martian/.test(font), "the system's monospace font (it has box drawing characters), not the app's", font);
  await p.eval(`window.__copied = null; navigator.clipboard.writeText = async (t) => { window.__copied = t; }`);
  await p.click(`[data-pane-id="${T}"] [aria-label="Terminal options"]`);
  await sleep(200);
  await p.clickText(".menu-item", "Select all");
  const selCopied = await waitFor(() => p.eval(`window.__copied`), 2000);
  check(/me@mock:~\$/.test(selCopied ?? ""), "selecting copies by itself, as natively", (selCopied ?? "").slice(-60));
  await p.eval(`navigator.clipboard.readText = async () => 'from the right button'`);
  await p.eval(`(() => { const r = document.querySelector('[data-pane-id="${T}"] .term-screen').getBoundingClientRect(); document.querySelector('[data-pane-id="${T}"] .xterm-screen').dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: r.x + 40, clientY: r.y + 40 })); })()`);
  await sleep(300);
  check((await inputs()).includes("from the right button"), "right-click pastes");
  check(!(await p.eval(`!!document.querySelector('.menu-pop')`)), "…without a menu of the page's own");
  await p.key("Escape", "Escape", 0, 27);
  await p.click(`[data-pane-id="${T}"] .term-screen`);
  await p.key("u", "KeyU", 2, 85);
  await p.key("k", "KeyK", 2, 75);
  await p.key("c", "KeyC", 2, 67);
  await sleep(300);
  const ctrl = await inputs();
  check(ctrl.includes("\x15") && ctrl.includes("\x0b") && ctrl.includes("\x03"), "Ctrl+U, Ctrl+K and Ctrl+C go to the program", JSON.stringify(ctrl.slice(-12)));
  check(!(await p.eval(`!!document.querySelector('.palette, [role=dialog]')`)), "…and Ctrl+K doesn't open the command palette here");
  const beforeAlt = await inputs();
  await p.key("1", "Digit1", 1, 49);
  await sleep(250);
  check((await inputs()) === beforeAlt, "Alt+1 stays the app's (switching mosaics), not the program's");
  check(await p.eval(`document.activeElement?.classList.contains('xterm-helper-textarea')`), "…and the keyboard is still in the terminal");
  await p.key("Enter");

  console.log("— find, and text size");
  await p.type("echo needle in a haystack");
  await p.key("Enter");
  await waitFor(async () => /\nneedle in a haystack/.test(await screen(p)));
  await p.key("F", "KeyF", 2 | 8, 70);
  const findBar = await waitFor(() => p.eval(`document.activeElement?.closest('.term-find') ? true : null`));
  check(!!findBar, "Ctrl+Shift+F opens the find bar, with the keyboard in it");
  await p.type("needle");
  await sleep(300);
  check(await p.eval(`!!document.querySelector('.term-tile .xterm-selection div') && !document.querySelector('.term-find-none')`), "…and finds the text (selected)");
  await p.eval(`(() => { const i = document.querySelector('.term-find input'); i.value = 'qqqqzzzz'; i.dispatchEvent(new InputEvent('input', { bubbles: true })); })()`);
  await sleep(300);
  check(/Not found/.test((await p.eval(`document.querySelector('.term-find')?.textContent`)) ?? ""), "…says when it isn't there");
  await p.shot(`${SHOTS}/terminal-find-desktop.png`);
  await p.key("Escape", "Escape", 0, 27);
  await sleep(200);
  check(!(await p.eval(`!!document.querySelector('.term-find')`)) && (await p.eval(`document.activeElement?.classList.contains('xterm-helper-textarea')`)), "Escape closes it and the keyboard goes back to the terminal");
  const fsize = () => p.eval(`parseFloat(getComputedStyle(document.querySelector('.term-tile .xterm-rows')).fontSize)`);
  const f0 = await fsize();
  await p.key("=", "Equal", 2, 187);
  await p.key("=", "Equal", 2, 187);
  await sleep(200);
  const f1 = await fsize();
  check(f1 === f0 + 2, "Ctrl+= makes the text bigger", { f0, f1 });
  check((await p.eval(`localStorage.getItem('tessera.terminal.fontSize')`)) === String(f1), "…remembered on this device");
  await p.key("0", "Digit0", 2, 48);
  await sleep(200);
  check((await fsize()) === f0 && !(await p.eval(`localStorage.getItem('tessera.terminal.fontSize')`)), "Ctrl+0 puts it back");

  console.log("— web links");
  await p.eval(`window.__opened = []; window.open = (u) => { window.__opened.push(u); return null; }`);
  await p.type("echo see https://example.com/docs for more");
  await p.key("Enter");
  await waitFor(async () => /\nsee https:\/\/example.com\/docs/.test(await screen(p)));
  const linkX = async () => p.eval(`(() => {
    const row = [...document.querySelectorAll('.term-tile .xterm-rows > div')].filter((r) => /^see https/.test(r.textContent)).pop();
    const range = document.createRange();
    const node = [...row.querySelectorAll('span')].find((s) => s.textContent.includes('https')) ?? row;
    const t = node.firstChild;
    const i = t.textContent.indexOf('example');
    range.setStart(t, i); range.setEnd(t, i + 1);
    const r = range.getBoundingClientRect();
    return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
  })()`);
  const where = await linkX().catch(() => null);
  check(!!where, "the link is on screen", where);
  if (where) {
    await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: where.x, y: where.y });
    await sleep(300);
    await p.mouse(where.x, where.y);
    await sleep(300);
    check((await p.eval(`window.__opened.length`)) === 0, "a plain click on a link doesn't open it (it may be the start of a selection)");
    await p.mouse(where.x, where.y, 2);
    await sleep(400);
    check(JSON.stringify(await p.eval(`window.__opened`)) === JSON.stringify(["https://example.com/docs"]), "Ctrl+click opens it in the browser", await p.eval(`window.__opened`));
  }

  console.log("— light");
  const colors = () => p.eval(`({ bg: getComputedStyle(document.querySelector('.term-tile .term-body')).backgroundColor, fg: getComputedStyle(document.querySelector('.term-tile .xterm-rows')).color })`);
  const dark = await colors();
  await p.theme("light");
  await sleep(300);
  const light = await colors();
  const lum = (c) => (c.match(/\d+/g) ?? []).slice(0, 3).reduce((a, b) => a + Number(b), 0);
  check(lum(dark.bg) < lum(dark.fg) && lum(light.bg) > lum(light.fg), "the terminal follows the theme (light text on dark, dark text on light)", { dark, light });
  await p.shot(`${SHOTS}/terminal-desktop-light.png`);
  await p.theme("dark");

  console.log("— this device's alone");
  await sleep(600);
  const layout = (await probe.rpc("get_mosaics")).result;
  const tiles = JSON.stringify(layout ?? {});
  check(!tiles.includes(T) && !/terminal/.test(tiles), "the terminal tile is never shared with other devices' mosaics");
  const id = (await term(probe)).terms[0].id;
  const other = await probe.rpc("terminal_attach", { id, cols: 80, rows: 24 });
  check(!!other.error && /isn't open/.test(other.error.message), "another device can't show it (it is told it isn't open)", other.error);
  const typed = await probe.rpc("terminal_input", { id, data: "whoami\r" });
  check(!!typed.error && !(await term(probe)).terms[0].inputs.join("").includes("whoami"), "…nor type in it");
  const ps = (await probe.rpc("terminal_status")).result;
  check(ps.open.length === 0, "…nor sees it among its own", ps.open);

  console.log("— back after a reload");
  await p.reload(1500);
  await p.waitFor(`document.querySelector('.workspace')`, 10000);
  const back = await waitFor(async () => {
    const s = await screen(p);
    return /colors/.test(s) && /green/.test(s) ? s : null;
  });
  check(!!back, "after a reload the tile shows the same terminal again, its last output redrawn", (await screen(p)).slice(-160));
  check((await term(probe)).terms.length === 1 && (await term(probe)).terms[0].id === id, "…the same one (nothing new opened)");

  console.log("— back after the connection dropped");
  await mock("/__mock/drop");
  probe = await wsClient("probe"); // dropped too
  await waitFor(() => p.eval(`__mod('lib/store.ts').then((m) => m.state.conn.state === 'connected' && m.state.ready)`), 15000);
  await p.click(`[data-pane-id="${T}"] .term-screen`);
  await p.type("echo still here");
  await p.key("Enter");
  const still = await waitFor(async () => /\nstill here/.test(await screen(p)));
  check(still, "after the connection dropped the tile shows the terminal again and typing works", (await screen(p)).slice(-160));
  const afterDrop = (await term(probe)).terms;
  check(afterDrop.length === 1 && afterDrop[0].id === id, "…the same one (nothing new opened)", afterDrop.map((t) => t.id));
  check(/colors/.test(await screen(p)), "…with what it showed before");

  console.log("— how it ended, and Start again");
  await p.click(`[data-pane-id="${T}"] .term-screen`);
  await p.type("exit 3");
  await p.key("Enter");
  const ended = await waitFor(() => p.eval(`document.querySelector('.term-ended')?.textContent`));
  check(/Exited with code 3/.test(ended ?? ""), "the tile says how it ended", ended);
  check(/Start again/.test(ended ?? "") && /Close/.test(ended ?? ""), "…and offers Start again and Close");
  await p.shot(`${SHOTS}/terminal-ended-desktop.png`);
  await p.clickText(".term-ended button", "Start again");
  const again = await waitFor(async () => ((await term(probe)).terms.length === 1 && (await term(probe)).terms[0].id !== id ? (await term(probe)).terms[0] : null));
  check(!!again, "Start again opens a new terminal in the tile", again?.id);
  check(!(await p.eval(`!!document.querySelector('.term-ended')`)), "…and the ended bar goes");

  console.log("— a long paste");
  await waitFor(async () => /Welcome/.test(await screen(p)));
  await p.eval(`navigator.clipboard.readText = async () => 'z'.repeat(70000)`);
  await p.click(`[data-pane-id="${T}"] [aria-label="Terminal options"]`);
  await sleep(200);
  await p.clickText(".menu-item", "Paste");
  const pieces = await waitFor(async () => {
    const ins = ((await term(probe)).terms[0]?.inputs ?? []).filter((s) => /^z+$/.test(s));
    return ins.reduce((a, s) => a + s.length, 0) === 70000 ? ins.map((s) => s.length) : null;
  });
  check(!!pieces && pieces.length > 1 && pieces.every((n) => n <= 16384), "a 70,000-character paste reaches the terminal whole, in pieces the server takes", pieces);
  check(!p.errors.length, "…without an error", p.errors.slice(0, 2));

  console.log("— closing the tile ends it");
  await p.click(`[data-pane-id="${T}"] [aria-label="Close terminal"]`);
  await sleep(500);
  st = await term(probe);
  check(st.terms.length === 0, "closing the tile ends the terminal on the server", st.terms.map((t) => t.id));
  const rec = st.audit[0];
  check(!!rec && rec.device === "Desktop" && rec.place === "Mock server" && rec.closed_at && rec.ended === "closed", "the record keeps who, where, when and how it ended", rec);
  check(!JSON.stringify(st.audit).includes("hello there") && !JSON.stringify(st.audit).includes("colors"), "…never what was typed or shown");

  console.log("— Settings → Terminal");
  await p.eval(`__mod('lib/ui.ts').then((m) => m.setSettingsTab('terminal'))`);
  await p.waitFor(`document.querySelector('.term-settings')`, 6000);
  await sleep(300);
  const cards = await p.eval(`[...document.querySelectorAll('.term-settings .perm-card')].map((c) => ({ v: c.dataset.access, on: c.getAttribute('aria-checked') === 'true' }))`);
  check(JSON.stringify(cards.map((c) => c.v)) === JSON.stringify(["off", "sandboxed", "full"]) && cards.find((c) => c.on)?.v === "sandboxed", "Off, Sandboxed, Full access; Sandboxed chosen", cards);
  const recent = await p.eval(`document.querySelector('.term-recent')?.textContent`);
  check(/Desktop on Mock server/.test(recent ?? "") && /sandboxed/.test(recent ?? ""), "Lately: who opened one, where, how confined", recent);
  const explain = await p.eval(`document.querySelector('.term-settings .explain')?.textContent`);
  check(/as your account there/.test(explain ?? "") && /agents never get one/.test(explain ?? ""), "it explains what a terminal can do", explain);
  await p.shot(`${SHOTS}/terminal-settings-desktop.png`);
  await p.click(`.term-settings .perm-card[data-access="full"]`);
  await sleep(300);
  const confirm = await p.eval(`document.querySelector('.modal, [role=dialog], [role=alertdialog]')?.textContent`);
  check(/full access/i.test(confirm ?? ""), "Full access asks first, saying what it means", confirm?.slice(0, 160));
  await closeMenus(p);
  await sleep(300);
  check((await term(probe)).settings.access === "sandboxed", "…and nothing changed without a yes");
  await p.eval(`__mod('lib/ui.ts').then((m) => m.setSettingsTab(null))`);
  await sleep(300);

  console.log("— a machine that allows terminals: the tile asks where");
  const machines = (await probe.rpc("list_machines")).result;
  const desktop = machines.find((m) => m.name === "Desktop");
  await probe.rpc("mock_machines", { list: machines.map((m) => (m.id === desktop.id ? { ...m, scope: { ...m.scope, terminal: "sandboxed" } } : m)) });
  await sleep(300);
  await spawn(p, "Terminal");
  const places = await waitFor(async () => {
    const l = await p.eval(`[...document.querySelectorAll('.term-place b')].map((b) => b.textContent)`);
    return l.length === 2 ? l : null;
  });
  check(JSON.stringify(places) === JSON.stringify(["This server", "Desktop"]), "two places: the tile asks where (this server, Desktop)", places);
  await p.shot(`${SHOTS}/terminal-pick-desktop.png`);
  await p.clickText(".term-place", "Desktop");
  const onMachine = await waitFor(async () => /Welcome to the mock terminal on Desktop/.test(await screen(p)));
  check(onMachine, "picking Desktop opens a terminal there");
  const msub = await p.eval(`document.querySelector('.term-tile .term-sub')?.textContent`);
  check(/Desktop · sandboxed/.test(msub ?? ""), "…its header says so", msub);
  check((await term(probe)).terms[0]?.machine === desktop.id, "…on that machine");

  console.log("— switching place from the ⋯ menu");
  await p.click(`.term-tile [aria-label="Terminal options"]`);
  await sleep(250);
  const heading = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-heading, .menu-pop [role=presentation]')].map((e) => e.textContent.trim()).join('|')`);
  const placesInMenu = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-item')].map((e) => ({ label: e.querySelector('.menu-label')?.textContent, on: e.getAttribute('aria-checked') === 'true' || e.classList.contains('checked') || !!e.querySelector('.menu-check') }))`);
  check(/Open on/.test(heading), "the ⋯ menu lists where a terminal can open", heading);
  const hereRow = placesInMenu.find((x) => x.label === "Desktop");
  check(!!placesInMenu.find((x) => x.label === "This server") && hereRow?.on, "…this server and Desktop, Desktop ticked", placesInMenu);
  await p.shot(`${SHOTS}/terminal-places-menu-desktop.png`);
  await p.clickText(".menu-item", "This server");
  await sleep(300);
  const sw = await p.eval(`document.querySelector('.modal, [role=dialog], [role=alertdialog]')?.textContent`);
  check(/Open a terminal on this server\?/.test(sw ?? "") && /The terminal on Desktop ends/.test(sw ?? ""), "switching asks first: the one on Desktop ends", sw?.slice(0, 160));
  await p.clickText(".modal button, [role=dialog] button", "Switch");
  const switched = await waitFor(async () => {
    const t = (await term(probe)).terms;
    return t.length === 1 && !t[0].machine && /Welcome to the mock terminal/.test(await screen(p)) ? t : null;
  });
  check(!!switched, "…then the tile shows a terminal on this server, the Desktop one closed", (await term(probe)).terms.map((t) => t.machine ?? "server"));
  check(/Mock server · sandboxed/.test((await p.eval(`document.querySelector('.term-tile .term-sub')?.textContent`)) ?? ""), "…and its header says so");
  await p.click(`.term-tile [aria-label="Close terminal"]`);
  await sleep(300);

  console.log("— Windows terminal capability, never an old Windows node");
  await probe.rpc("mock_machines", { list: [
    ...machines,
    { ...desktop, id: 'win-old', name: 'Old Windows', os: 'windows x86_64', scope: { ...desktop.scope, terminal: 'full' } },
    { ...desktop, id: 'win-new', name: 'New Windows', os: 'windows x86_64', features: ['terminal'], scope: { ...desktop.scope, terminal: 'full' } },
    { ...desktop, id: 'no-terminal', name: 'No terminal backend', features: [], scope: { ...desktop.scope, terminal: 'full' } },
  ] });
  await sleep(300);
  await spawn(p, "Terminal");
  await p.waitFor(`document.querySelector('.term-pick')`, 5000);
  for (const [layout, w, h, touch] of [['desktop', 1280, 820, false], ['phone', 344, 882, true], ['fold', 884, 1104, true]]) {
    await p.viewport(w, h, touch);
    for (const theme of ['dark', 'light']) {
      await p.theme(theme);
      await sleep(250);
      const choices = await p.eval(`[...document.querySelectorAll('.term-place b')].map(b => b.textContent)`);
      check(choices.includes('New Windows') && !choices.includes('Old Windows') && !choices.includes('No terminal backend'), `${layout} ${theme}: new Windows backend offered, old/unavailable nodes hidden`, choices);
      check(await p.eval(`(() => { const p = document.querySelector('.term-pick'); return p.scrollWidth <= p.clientWidth + 1; })()`), `${layout} ${theme}: Windows picker fits`);
      await p.shot(`${SHOTS}/terminal-windows-pick-${layout}-${theme}.png`);
    }
  }
  await p.viewport(1440, 900);
  await p.clickText('.term-place', 'New Windows');
  const winOpened = await waitFor(async () => /Welcome to the mock terminal on New Windows/.test(await screen(p)));
  check(winOpened && (await term(probe)).terms[0]?.machine === 'win-new', 'the Windows choice routes the tile to its advertised node');
  check(/New Windows · full access/.test((await p.eval(`document.querySelector('.term-tile .term-sub').textContent`))), 'Windows terminal header reports full access, never sandboxed');
  await p.click(`.term-tile [aria-label="Close terminal"]`);
  await sleep(300);
  console.log("— not over a connection that isn't end-to-end encrypted");
  await probe.rpc("mock_terminal", { private: false });
  await probe.rpc("mock_machines", { list: machines });
  await spawn(p, "Terminal");
  const refused = await waitFor(() => p.eval(`(document.querySelector('.term-tile .term-note[role=alert]') || document.querySelector('.term-tile .term-warn'))?.textContent`));
  check(/end-to-end encrypted/.test(refused ?? ""), "a terminal can't open over a connection that isn't end-to-end encrypted, and the tile says why", refused);
  const widen = await probe.rpc("set_terminal_settings", { settings: { access: "full", idle_minutes: 30 } });
  check(!!widen.error && /only from the Tessera app/.test(widen.error.message) && (await term(probe)).settings.access === "sandboxed", "…nor can terminals be given more from there", widen.error);
  await p.click(`.term-tile [aria-label="Close terminal"]`);
  await sleep(200);
  await probe.rpc("mock_terminal", { private: true });

  // ── the unfolded Fold ─────────────────────────────────────────────────────
  console.log("— the unfolded Fold");
  const f = await device();
  await f.viewport(884, 1104, true, 2);
  await signIn(f, "Fold");
  await f.theme("dark");
  await ws(f, `m.openPane({ type: 'terminal', machine: null })`);
  const fold = await waitFor(async () => /Welcome/.test(await screen(f)));
  check(fold, "the unfolded Fold opens a terminal");
  check(await f.eval(`!!document.querySelector('.term-tile .xterm-cursor')`), "…with its cursor");
  check(await f.eval(`!!document.querySelector('.term-keys')`), "…with the extra keys (a touch screen)");
  await f.shot(`${SHOTS}/terminal-fold-dark.png`);

  // ── a folded phone ────────────────────────────────────────────────────────
  console.log("— a folded phone (344 px)");
  const ph = await device();
  await ph.viewport(344, 882, true, 3);
  await signIn(ph, "Phone");
  await ph.theme("light");
  await ws(ph, `m.openPane({ type: 'terminal', machine: null })`);
  await waitFor(async () => /Welcome/.test(await screen(ph)));
  const keys = await ph.eval(`[...document.querySelectorAll('.term-keys button')].map((b) => b.getAttribute('aria-label') || b.textContent.trim())`);
  check(JSON.stringify(keys) === JSON.stringify(["Esc", "Tab", "Ctrl", "Alt", "Left", "Up", "Down", "Right", "Paste"]), "the extra keys: Esc, Tab, Ctrl, Alt, the arrows and Paste", keys);
  const phoneFont = await ph.eval(`parseFloat(getComputedStyle(document.querySelector('.term-tile .xterm-rows')).fontSize)`);
  check(phoneFont < 13, "the text is a little smaller on a narrow phone, so more fits", phoneFont);
  const fits = await ph.eval(`(() => { const r = document.querySelector('.term-keys').getBoundingClientRect(); return r.right <= innerWidth + 1 && r.left >= -1 && document.querySelector('.term-keys').scrollWidth <= r.width + 2; })()`);
  check(fits, "…all on screen at 344 px");
  const phoneDev = (await probe.rpc("list_devices")).result.filter((d) => d.name === "Phone").pop();
  const phoneTerm = (await term(probe)).terms.find((t) => t.owner === phoneDev?.id);
  const phoneId = phoneTerm?.id;
  check(!!phoneTerm && phoneTerm.cols < 50 && phoneTerm.cols >= 20, "the server knows the phone's narrow size", phoneTerm && { cols: phoneTerm.cols, rows: phoneTerm.rows });
  const tapKey = async (label) => {
    const b = await ph.eval(`(() => { const e = [...document.querySelectorAll('.term-keys button')].find((b) => (b.getAttribute('aria-label') || b.textContent.trim()) === ${JSON.stringify(label)}); const r = e.getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`);
    await ph.tap(b.x, b.y);
  };
  await tapKey("Ctrl");
  check(await ph.eval(`document.querySelector('.term-keys button[aria-pressed="true"]')?.textContent.trim() === 'Ctrl'`), "Ctrl stays pressed for the next key");
  await ph.type("c");
  await sleep(200);
  await tapKey("Up");
  await tapKey("Esc");
  await tapKey("Tab");
  await sleep(300);
  const pin = (await term(probe)).terms.find((t) => t.id === phoneId)?.inputs ?? [];
  check(pin.includes("\x03") && pin.includes("\x1b[A") && pin.includes("\x1b") && pin.includes("\t"), "Ctrl then c sends Ctrl+C; the arrows, Esc and Tab send theirs", pin);
  await ph.eval(`navigator.clipboard.readText = async () => 'from the phone'`);
  await tapKey("Paste");
  await sleep(300);
  check(((await term(probe)).terms.find((t) => t.id === phoneId)?.inputs ?? []).join("").includes("from the phone"), "Paste in the row of keys pastes");
  // Where the page may not read the clipboard (Android's web view): a field takes the system's own Paste.
  await ph.eval(`navigator.clipboard.readText = async () => { throw new Error('not allowed'); }`);
  await tapKey("Paste");
  const pasteBox = await waitFor(() => ph.eval(`document.querySelector('.dialog-body input') ? document.querySelector('.dialog-body').closest('.modal, [role=dialog]')?.textContent ?? null : null`));
  check(/Paste into the terminal/.test(pasteBox ?? "") && /Touch and hold/.test(pasteBox ?? ""), "…and where the page can't read the clipboard, it offers a field for the phone's own Paste", pasteBox);
  await ph.shot(`${SHOTS}/terminal-phone-paste-light.png`);
  await ph.eval(`(() => { const i = document.querySelector('.dialog-body input'); i.value = 'held and pasted'; i.dispatchEvent(new InputEvent('input', { bubbles: true })); })()`);
  await ph.eval(`[...document.querySelectorAll('.dialog-body button')].find((b) => b.textContent.trim() === 'Paste').click()`);
  await sleep(300);
  check(((await term(probe)).terms.find((t) => t.id === phoneId)?.inputs ?? []).join("").includes("held and pasted"), "…which then goes into the terminal");
  check(!(await ph.eval(`!!document.querySelector('.term-keys button[aria-pressed="true"]')`)), "…and Ctrl lets go after one key");
  const vis = await ph.eval(`(() => { const t = document.querySelector('.term-tile').getBoundingClientRect(); const s = document.querySelector('.term-screen').getBoundingClientRect(); return { tw: t.width, sw: s.width, sh: s.height }; })()`);
  check(vis.tw <= 345 && vis.sh > 200, "the terminal fills the phone's tile", vis);
  await ph.shot(`${SHOTS}/terminal-phone-light.png`);
  await ph.eval(`__mod('lib/ui.ts').then((m) => m.setSettingsTab('terminal'))`);
  await ph.waitFor(`document.querySelector('.term-settings .perm-card')`, 6000);
  await sleep(400);
  const setFits = await ph.eval(`(() => { const s = document.querySelector('.term-settings'); return s.scrollWidth <= s.clientWidth + 1 && [...s.querySelectorAll('.perm-card, .term-recent li')].every((c) => c.getBoundingClientRect().right <= innerWidth + 1); })()`);
  check(setFits, "Settings → Terminal fits a 344 px phone");
  await ph.shot(`${SHOTS}/terminal-settings-phone-light.png`);
  await ph.eval(`__mod('lib/ui.ts').then((m) => m.setSettingsTab(null))`);
  await sleep(300);
  await ph.theme("dark");
  await sleep(300);
  await ph.shot(`${SHOTS}/terminal-phone-dark.png`);

  console.log("— signed out: its terminals end at once");
  const before = (await term(probe)).terms.length;
  await probe.rpc("revoke_device", { id: phoneDev.id });
  await sleep(300);
  st = await term(probe);
  check(st.terms.length === before - 1 && !st.terms.some((t) => t.id === phoneId), "signing the phone out ends its terminal", st.terms.map((t) => t.id));
  check(st.audit.some((a) => a.device === "Phone" && a.ended === "its device was signed out"), "…and the record says why");

  console.log("— turned off: none stays open");
  await probe.rpc("set_terminal_settings", { settings: { access: "off", idle_minutes: 30 } });
  await sleep(200);
  check((await term(probe)).terms.filter((t) => !t.machine).length === 0, "turning terminals off closes every one on the server");
  const offAgain = await waitFor(() => f.eval(`document.querySelector('.term-ended')?.textContent`));
  check(/turned off/.test(offAgain ?? ""), "…and the Fold's tile says so", offAgain);
  const cursor = await f.eval(`(() => { const t = document.querySelector('.term-tile .xterm'); return !!t && !t.querySelector('.xterm-cursor'); })()`);
  check(cursor, "…and its cursor is gone (nothing more is typed there)");

  console.log("— off, on a folded phone");
  const ph2 = await device();
  await ph2.viewport(344, 882, true, 3);
  await signIn(ph2, "Phone 2");
  await ph2.theme("light");
  await ws(ph2, `m.openPane({ type: 'terminal' })`);
  const offPhone = await waitFor(() => ph2.eval(`document.querySelector('.term-off')?.textContent`));
  check(/Terminals are off on this server/.test(offPhone ?? ""), "the phone's tile says terminals are off", offPhone);
  // The phone pages over to the new tile: measured once it is in place.
  await waitFor(async () => {
    const left = () => ph2.eval(`document.querySelector('.term-tile').getBoundingClientRect().left`);
    const a = await left();
    await sleep(250);
    return a === (await left());
  });
  const offFits = await ph2.eval(`(() => { const o = document.querySelector('.term-pick'); return o.scrollWidth <= o.clientWidth + 1 && [...o.querySelectorAll('button')].every((b) => b.getBoundingClientRect().right <= innerWidth + 1); })()`);
  check(offFits, "…and it fits 344 px, buttons included");
  await ph2.shot(`${SHOTS}/terminal-off-phone-light.png`);

  check(!p.errors.length && !f.errors.length && !ph.errors.length && !ph2.errors.length, "no page errors", [...p.errors, ...f.errors, ...ph.errors, ...ph2.errors].slice(0, 3));
  finish();
});
