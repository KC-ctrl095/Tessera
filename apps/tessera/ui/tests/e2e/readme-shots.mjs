// The pictures in README.md (docs/images), taken from the mock server: not a test suite, so it isn't in
// run.sh's default list. Run it with `sh tests/e2e/run.sh readme-shots`, then make them small with
// `python3 docs/images/shrink.py "$E2E_OUT/shots/readme"` from the repository root.
import path from "node:path";
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const OUT = path.join(SHOTS, "readme");
const NETWORK = "ses_wb7c1e5a9f20"; // "Explain my home network": a whiteboard beside the chat
const AUDIT = "ses_7a1c5e93b2d0"; // "Audit the reconnect logic": subagents and their reports
const WEEK = "ses_6f2c0a9d3e18"; // "Plan my week", with the bot Nova

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);

async function shot(p, name) {
  await sleep(600);
  await p.shot(path.join(OUT, `${name}.png`));
}

run(async () => {
  // ── the desktop app: a chat at work, and a whiteboard beside another one ──
  for (const theme of ["dark", "light"]) {
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(1440, 900, false, 1);
    await signIn(p, `README desktop ${theme}`);
    await p.theme(theme);
    if (theme === "dark") {
      // The chat, and the whiteboard its agent drew, beside it.
      await ws(p, `m.openSession('${NETWORK}', { pane: m.workspace.focused() })`);
      await sleep(1200);
      await p.clickText("[data-pane-id] button", "Open");
      await p.waitFor(`document.querySelector('.board-iframe') && !document.querySelector('.board-loading')`, 20000).catch(() => {});
      await sleep(3000);
      check(await p.eval(`!document.querySelector('.board-loading')`), "desktop dark: the whiteboard has loaded");
    } else {
      await ws(p, `m.openSession('${AUDIT}', { pane: m.workspace.focused() })`);
      await sleep(800);
      await ws(p, `m.openSession('${WEEK}', { newPane: true })`);
      await sleep(1500);
    }
    check(await p.eval(`document.querySelectorAll('[data-pane-id]').length >= 2`), `desktop ${theme}: tiles side by side`);
    await shot(p, `desktop-${theme}`);
    check(!p.errors.length, `no page errors (desktop ${theme})`, p.errors);
    await p.close();
  }

  // ── a phone ──
  for (const [name, w, h, dpr, session, theme] of [["phone", 390, 844, 2, AUDIT, "light"]]) {
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(w, h, true, dpr);
    await signIn(p, `README ${name}`);
    await p.theme(theme);
    await ws(p, `m.openSession('${session}', { pane: m.workspace.focused() })`);
    await sleep(2500);
    await shot(p, name);
    check(!p.errors.length, `no page errors (${name})`, p.errors);
    await p.close();
  }

  // ── the first run: setup, the AI step ──
  await mock("/__mock/reset?fresh=1");
  {
    const p = await device();
    await p.viewport(1280, 900, false, 1);
    await signIn(p, "README setup");
    await p.theme("dark");
    await p.waitFor(`document.querySelector('.setup-wizard')`, 6000).catch(() => {});
    await p.waitFor(`document.querySelectorAll('.setup-found li').length >= 2`, 6000).catch(() => {});
    check(await p.eval(`!!document.querySelector('.setup-wizard')`), "setup: the wizard is up");
    await shot(p, "setup");
    await p.close();
  }
  finish();
});
