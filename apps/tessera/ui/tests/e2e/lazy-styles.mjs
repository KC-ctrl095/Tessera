// Parts most visits never open (Settings, setup, the bot and channel editors, the keyboard
// cheatsheet, web views, the coach marks, the command palette, whiteboard tiles, the phone's tile
// overview) load on first use, and their styles come with them: not in the app's first download,
// yet already there the moment they appear (no flash of unstyled content). The bits of those
// styles the app itself uses (a secret's $NAME chip in a chat) stay with the app.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const SID = "ses_wb7c1e5a9f20";
const CTRL = 2;
const ALT = 1;

// Probes: plain elements with a feature's class, styled only once its stylesheet is in.
const PROBES = {
  settings: [".settings-layout", "display", "grid"],
  setup: [".setup", "display", "flex"],
  editor: [".be", "display", "grid"],
  cheatsheet: [".cheat-body", "display", "grid"],
  tour: [".tour", "position", "fixed"],
  webview: [".wv-bar", "gap", "2px"],
  // Note tiles and a whiteboard's question: not in the first download either, but they may load
  // once the app is idle (note tiles when there are notes), so only "styled when shown" is checked.
  notes: [".note-panel", "position", "absolute"],
};
const probe = (p) =>
  p.eval(`(() => {
    const out = {};
    for (const [k, [sel, prop, want]] of Object.entries(${JSON.stringify(PROBES)})) {
      const e = document.createElement('div');
      e.className = sel.slice(1);
      e.style.visibility = 'hidden';
      document.body.append(e);
      out[k] = getComputedStyle(e).getPropertyValue(prop) === want;
      e.remove();
    }
    const chip = document.createElement('code');
    chip.className = 'var-chip';
    document.body.append(chip);
    out.varChip = getComputedStyle(chip).paddingLeft === '6px';
    chip.remove();
    return out;
  })()`);

// The moment an element matching `sel` is added, before the browser draws it, note a style of it.
const watch = (p, key, sel, prop) =>
  p.eval(`(() => {
    window.__first ??= {};
    delete window.__first[${JSON.stringify(key)}];
    const take = (e) => (window.__first[${JSON.stringify(key)}] = getComputedStyle(e).getPropertyValue(${JSON.stringify(prop)}));
    const mo = new MutationObserver(() => {
      const e = document.querySelector(${JSON.stringify(sel)});
      if (e) {
        take(e);
        mo.disconnect();
      }
    });
    mo.observe(document.documentElement, { childList: true, subtree: true });
  })()`);
const first = (p, key) => p.eval(`window.__first?.[${JSON.stringify(key)}] ?? null`);
const shown = (p, sel) => p.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); return !!e && e.getClientRects().length > 0; })()`);
const escape = async (p) => {
  await p.key("Escape", "Escape", 0, 27);
  await sleep(400);
};
const palette = async (p, text) => {
  await p.key("k", "KeyK", CTRL, 75);
  await p.waitFor(`document.querySelector('.palette input')`, 6000);
  await p.type(text);
  await sleep(250);
  await p.key("Enter", "Enter", 0, 13);
};

run(async () => {
  for (const theme of ["dark", "light"]) {
    console.log(`— desktop (${theme})`);
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, `Lazy styles ${theme}`);
    await p.theme(theme);
    await sleep(400);

    let pr = await probe(p);
    if (theme === "dark")
      check(!pr.settings && !pr.setup && !pr.editor && !pr.cheatsheet && !pr.tour && !pr.webview, "the app starts without the styles of Settings, setup, the bot editor, the cheatsheet, the coach marks and web views", pr);
    check(pr.varChip, "a secret's $NAME chip (shown in chats) keeps its style without Settings", pr);

    // The command palette: the first Ctrl+K opens it, styled.
    await watch(p, "palette", ".palette-input", "display");
    await p.key("k", "KeyK", CTRL, 75);
    await p.waitFor(`document.querySelector('.palette input')`, 6000).catch(() => {});
    check((await shown(p, ".palette input")) && (await first(p, "palette")) === "flex", "the first Ctrl+K opens the command palette, styled", await first(p, "palette"));
    await escape(p);

    // Settings
    await watch(p, "settings", ".settings-layout", "display");
    await p.key(",", "Comma", CTRL, 188);
    await p.waitFor(`document.querySelector('.settings-layout')`, 8000);
    check((await first(p, "settings")) === "grid", "Settings is styled the moment it appears", await first(p, "settings"));
    await sleep(400);
    await p.shot(`${SHOTS}/lazy-styles-settings-desktop-${theme}.png`);
    pr = await probe(p);
    check(pr.settings && pr.setup, "…its styles (and setup's, used in its sections) came with it", pr);

    // The coach marks, replayed from Settings → Help & tour ("Show hints again" needs a hint seen)
    await p.eval(`__mod('lib/prefs.ts').then(m => m.dismissHint('lazy-styles'))`);
    await p.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab('help'))`);
    await p.waitFor(`[...document.querySelectorAll('.settings-body button')].some(b => b.textContent.includes('Show hints again'))`, 8000).catch(() => {});
    await sleep(200);
    await watch(p, "tour", ".tour", "position");
    await p.clickText(".settings-body button", "Show hints again");
    await p.waitFor(`document.querySelector('.tour-card')`, 8000).catch(() => {});
    check((await first(p, "tour")) === "fixed" && (await shown(p, ".tour-card")), "the coach marks show, styled from their first frame", await first(p, "tour"));
    await p.shot(`${SHOTS}/lazy-styles-tour-desktop-${theme}.png`);
    await p.clickText(".tour-card button", "Skip tips");
    await sleep(300);
    check(!(await shown(p, ".tour-card")), "…and Skip tips closes them");

    // The bot editor (from the palette)
    await watch(p, "editor", ".be", "display");
    await palette(p, "New bot");
    await p.waitFor(`document.querySelector('.be')`, 8000);
    check((await first(p, "editor")) === "grid", "the bot editor is styled the moment it appears", await first(p, "editor"));
    await sleep(500);
    check(await p.eval(`document.querySelectorAll('.mavp img, .mavp .mav').length > 0`), "…with its avatar picker", await p.eval(`document.querySelector('.mavp')?.outerHTML.slice(0, 200) ?? null`));
    await p.shot(`${SHOTS}/lazy-styles-bot-editor-desktop-${theme}.png`);
    await escape(p);

    // The channel editor
    await watch(p, "channel", ".member-grid", "display");
    await palette(p, "New channel");
    await p.waitFor(`document.querySelector('.member-grid')`, 8000);
    check((await first(p, "channel")) === "grid", "the channel editor is styled the moment it appears", await first(p, "channel"));
    await escape(p);

    // The keyboard cheatsheet
    await watch(p, "cheatsheet", ".cheat-body", "display");
    await p.key("/", "Slash", ALT, 191);
    await p.waitFor(`document.querySelector('.cheat-body')`, 8000);
    check((await first(p, "cheatsheet")) === "grid", "the keyboard cheatsheet is styled the moment it appears", await first(p, "cheatsheet"));
    await escape(p);

    // Whiteboards and web views beside a chat
    await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${SID}', { pane: m.workspace.focused() }))`);
    await sleep(900);
    const img = await mock("/__mock/board", { session_id: SID, kind: "image" });
    await p.waitFor(`document.querySelector('.board .img-view img')`, 8000).catch(() => {});
    check(await shown(p, ".board .img-view"), "an image board's tile shows (the whiteboard tile loads on first use)", img.id);
    await watch(p, "webview", ".wv-bar", "gap");
    await mock("/__mock/board", { session_id: SID, kind: "url" });
    await p.waitFor(`document.querySelector('.wv-bar')`, 8000);
    check((await first(p, "webview")) === "2px", "a web view's bar is styled the moment it appears", await first(p, "webview"));
    await sleep(1500);
    await p.shot(`${SHOTS}/lazy-styles-boards-desktop-${theme}.png`);

    // A note tile: its code and styles arrive with the first note shown, styled from its first frame.
    await watch(p, "note", ".note-body", "display");
    await p.eval(`__mod('lib/notes.ts').then(m => m.newNote({ newPane: true }))`);
    await p.waitFor(`document.querySelector('.note-body')`, 8000).catch(() => {});
    check((await first(p, "note")) === "flex", "a note tile is styled the moment it appears", await first(p, "note"));
    pr = await probe(p);
    check(pr.notes, "…its styles came with it", pr);
    await sleep(600);
    await p.shot(`${SHOTS}/lazy-styles-note-desktop-${theme}.png`);

    // The setup wizard (Settings → Run setup again)
    await watch(p, "setup", ".setup-wizard", "position");
    await p.eval(`__mod('lib/ui.ts').then(u => u.setSetupOpen(true))`);
    await p.waitFor(`document.querySelector('.setup-wizard')`, 8000);
    check((await first(p, "setup")) === "fixed", "the setup wizard is styled the moment it appears", await first(p, "setup"));
    await sleep(500);
    await p.shot(`${SHOTS}/lazy-styles-setup-desktop-${theme}.png`);

    check(!p.errors.length, "no errors", p.errors);
    await p.close();
  }

  // ── a phone (the folded Fold's cover screen): Settings, the tile overview ──
  for (const theme of ["dark", "light"]) {
    console.log(`— cover screen (${theme})`);
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(344, 882, true, 2.625);
    await signIn(p, `Lazy styles cover ${theme}`);
    await p.theme(theme);
    await sleep(400);
    // The overview button shows once a mosaic has two tiles (a chat opened "in a new tile" takes the
    // place of an unused new chat, so the first one goes into the tile on screen).
    await p.eval(`__mod('lib/workspace.ts').then(m => { m.openSession('${SID}', { pane: m.workspace.focused() }); m.openSession('ses_2b7e91c04d5a', { newPane: true }); })`);
    await sleep(900);
    const b = await p.box(".pane.focused .head-overview");
    check(!!b, "with two tiles, a tile has its overview button", b);
    if (b) {
      await p.tap(b.cx, b.cy);
      await p.waitFor(`document.querySelector('.overview')`, 6000).catch(() => {});
      check(await shown(p, ".overview"), "a tap opens the tile overview");
      await sleep(400);
      await p.shot(`${SHOTS}/lazy-styles-overview-cover-${theme}.png`);
      await p.eval(`history.back()`);
      await sleep(500);
    }
    await watch(p, "settings", ".settings-layout", "display");
    await p.eval(`__mod('lib/ui.ts').then(u => u.openSettings())`);
    await p.waitFor(`document.querySelector('.settings-layout')`, 8000);
    check((await first(p, "settings")) === "grid", "Settings on a phone is styled the moment it appears", await first(p, "settings"));
    await sleep(400);
    await p.shot(`${SHOTS}/lazy-styles-settings-cover-${theme}.png`);
    await watch(p, "editor", ".be", "display");
    await p.eval(`__mod('lib/ui.ts').then(u => { u.setSettingsTab(null); u.openBotEditor(); })`);
    await p.waitFor(`document.querySelector('.be')`, 8000);
    check((await first(p, "editor")) === "flex", "the bot editor on a phone is styled the moment it appears", await first(p, "editor"));
    await sleep(500);
    await p.shot(`${SHOTS}/lazy-styles-bot-editor-cover-${theme}.png`);
    check(!p.errors.length, "no errors", p.errors);
    await p.close();
  }
  finish();
});
