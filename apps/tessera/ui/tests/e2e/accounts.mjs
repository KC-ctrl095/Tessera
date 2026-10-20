// More than one ChatGPT or Claude subscription (batch 17, "I know a lot of people do this").
// Settings → Models: "Another ChatGPT account" (a name, then the same ChatGPT sign-in) and "Another
// Claude account" (a name, then the user signs it in once through Claude Code, with its own folder:
// the command to run is shown, Tessera never sees the login). Each account is its own entry with its
// own models, renamed in its details; a chat uses an account by picking its model (the model shows
// which account when there are several); Usage shows each account's plan and limits; with two or
// more accounts of a kind, "go on with the next one" at a limit can be switched on. Desktop, folded
// phone (344 px), the unfolded Fold; light and dark.
import { MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

run(async () => {
  await mock("/__mock/reset");
  const ws = await wsClient("accounts script");
  // The usual ChatGPT account, signed in.
  await ws.rpc("complete_oauth", { provider: "openai-codex", input: "code" });
  const providers = async () => (await ws.rpc("list_providers")).result;

  const p = await device();
  await p.viewport(1280, 900);
  await signIn(p, "Accounts desktop");
  await p.theme("dark");
  const open = async (pg, tab) => {
    await pg.eval(`__mod('lib/ui.ts').then(m => { m.setDrawerOpen?.(false); m.openSettings(${JSON.stringify(tab)}); })`);
    // A section loads the first time it opens (slowly while the dev server warms up).
    const ready = tab === "models" ? ".models-connected .section-head .btn" : tab === "usage" ? ".usage .kpi" : ".settings-body > *";
    await pg.waitFor(`document.querySelector(${JSON.stringify(ready)})`, 20000).catch(() => {});
    await sleep(400);
  };
  const connectTitles = (pg) => pg.eval(`[...document.querySelectorAll('.connect-card-btn .cc-title')].map((e) => e.textContent)`);
  const card = (pg, name) =>
    pg.eval(`(() => { const c = [...document.querySelectorAll('.card.provider')].find((c) => c.querySelector('.provider-name')?.firstChild?.textContent?.trim() === ${JSON.stringify(name)}); if (!c) return null; return { pill: c.querySelector('.auth-pill')?.textContent.trim() ?? '', hint: c.querySelector('.provider-hint')?.textContent ?? null, cmd: c.querySelector('.provider-cmd code')?.textContent ?? null, badge: c.querySelector('.badge')?.textContent ?? '' }; })()`);
  const openConnect = async (pg) => {
    if (!(await pg.eval(`!!document.querySelector('.connect-grid')`))) await pg.click(".models-connected .section-head .btn");
    await sleep(300);
  };

  await open(p, "models");
  await openConnect(p);
  let titles = await connectTitles(p);
  check(titles.includes("Another ChatGPT account") && !titles.includes("Sign in with ChatGPT"), "signed in to ChatGPT: Connect offers another ChatGPT account", titles);

  // ── another ChatGPT account: a name, then the same sign-in ──
  await p.clickText(".connect-card-btn", "Another ChatGPT account");
  await p.waitFor(`document.querySelector('.account-form input')`, 3000);
  check(/Name this ChatGPT account/.test(await p.eval(`document.querySelector('.account-form .field-label').textContent`)), "it asks only for a name");
  await p.shot(`${SHOTS}/accounts-chatgpt-form-desktop-dark.png`);
  await p.click(".account-form input");
  await p.type("Work");
  await p.clickText(".account-form button", "Add account");
  await p.waitFor(`document.querySelector('.oauth-code')`, 6000);
  const added = (await providers()).find((x) => x.name === "ChatGPT (Work)");
  check(!!added && added.kind === "openai_codex" && added.id !== "openai-codex", "a second ChatGPT entry, named by the user, with an id of its own", added);
  check(!!(await p.eval(`document.querySelector('.oauth-code')`)), "…and its ChatGPT sign-in opens at once");
  await ws.rpc("complete_oauth", { provider: added.id, input: "code" });
  await p.waitFor(`!document.querySelector('.oauth-code')`, 6000).catch(() => {});
  await sleep(600);
  let c = await card(p, "ChatGPT (Work)");
  check(!!c && /Signed in as work@example.com/.test(c.pill), "the new account is signed in on its own", c);
  check(/Signed in as you@example.com/.test((await card(p, "ChatGPT (Codex)"))?.pill ?? ""), "…and the first one still is");

  // ── going on with the next account at a limit: offered once there are two ──
  await p.waitFor(`document.querySelector('.accounts-row .toggle')`, 4000).catch(() => {});
  check(await p.eval(`!!document.querySelector('.accounts-row .toggle')`), "two ChatGPT accounts: the switch to go on with the next one shows");
  check((await p.eval(`document.querySelector('.accounts-row .toggle').getAttribute('aria-checked')`)) === "false", "…off by default");
  await p.eval(`document.querySelector('.accounts-row .toggle').click()`);
  await sleep(600);
  check((await ws.rpc("get_accounts")).result.continue_on_next === true, "…switched on, the server keeps it");
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    await sleep(250);
    await p.shot(`${SHOTS}/accounts-models-desktop-${theme}.png`);
  }
  await p.theme("dark");

  // ── renaming an account (in its details) ──
  await p.eval(`(() => { const c = [...document.querySelectorAll('.card.provider')].find((c) => c.textContent.includes('ChatGPT (Work)')); c.querySelector('.provider-actions .btn-ghost').click(); })()`);
  await sleep(300);
  await p.eval(`(() => { const c = [...document.querySelectorAll('.card.provider')].find((c) => c.textContent.includes('ChatGPT (Work)')); [...c.querySelectorAll('button')].find((b) => b.textContent.includes('Rename')).click(); })()`);
  // The rename dialog: the topmost dialog, over Settings.
  const dlgInput = `[...document.querySelectorAll('.modal')].pop()?.querySelector('input')`;
  await p.waitFor(`document.querySelectorAll('.modal').length > 1 && ${dlgInput}`, 3000);
  check((await p.eval(`${dlgInput}.value`)) === "Work", "Rename starts from the account's own name");
  await p.eval(`(() => { const i = ${dlgInput}; i.focus(); i.select(); })()`);
  await p.type("Office");
  await p.eval(`[...document.querySelectorAll('.modal')].pop().querySelector('.dialog-actions button[type=submit]').click()`);
  await sleep(700);
  check(!!(await card(p, "ChatGPT (Office)")), "renamed: “ChatGPT (Office)”");
  const office = (await providers()).find((x) => x.id === added.id);
  check(office?.name === "ChatGPT (Office)", "…on the server too", office?.name);

  // ── a chat uses an account by its model; the model says which account ──
  const label = await p.eval(`__mod('lib/store.ts').then((s) => s.modelLabel({ provider: ${JSON.stringify(added.id)}, model: 'gpt-5.5' }))`);
  check(label === "GPT-5.5 · Office", "with two ChatGPT accounts, a model says which one", label);
  const plain = await p.eval(`__mod('lib/store.ts').then((s) => s.modelLabel({ provider: 'anthropic', model: 'claude-sonnet-5-5' }))`);
  check(plain === "Claude Sonnet 5.5", "…other models stay as they were", plain);
  const chat = (await ws.rpc("create_session", { title: "On the office plan", model: { provider: added.id, model: "gpt-5.5" } })).result;
  check(chat.model.provider === added.id, "a chat can run on the second account", chat.model);

  // ── another Claude account: signed in by the user through Claude Code, with its own folder ──
  await fetch(MOCK + "/__mock/claude-code?state=ready", { method: "POST" });
  await sleep(500);
  await openConnect(p);
  titles = await connectTitles(p);
  check(titles.includes("Another Claude account") && !titles.includes("Your Claude plan"), "Claude ready: Connect offers another Claude account", titles);
  await p.clickText(".connect-card-btn", "Another Claude account");
  await p.waitFor(`document.querySelector('.account-form input')`, 3000);
  await p.click(".account-form input");
  await p.type("Family");
  await p.clickText(".account-form button", "Add account");
  await p.waitFor(`document.querySelector('.claude-setup .claude-cmd code')`, 6000).catch(() => {});
  await sleep(400);
  const setup = await p.eval(`(() => { const s = document.querySelector('.claude-setup'); return s && { title: s.querySelector('.claude-setup-title').textContent, cmd: [...s.querySelectorAll('.claude-cmd code')].map((c) => c.textContent).pop() ?? '', copy: !!s.querySelector('.claude-cmd button') }; })()`);
  check(!!setup && setup.title === "Sign in to Claude (Family)", "the new Claude account's steps open right there", setup);
  check(!!setup && /^CLAUDE_CONFIG_DIR='.+claude-code-family' claude$/.test(setup.cmd) && setup.copy, "…with the command that signs it in with its own folder, and a copy button", setup);
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    await sleep(250);
    await p.shot(`${SHOTS}/accounts-claude-setup-desktop-${theme}.png`);
  }
  await p.theme("dark");
  const family = (await providers()).find((x) => x.name === "Claude (Family)");
  c = await card(p, "Claude (Family)");
  check(!!c && /Not signed in/.test(c.pill) && /claude-code-family/.test(c.cmd ?? ""), "its card says it isn't signed in yet, with the same command", c);
  // The user signs in there; Check again finds it.
  await fetch(`${MOCK}/__mock/claude-account?id=${family.id}&state=ready`, { method: "POST" });
  await p.clickText(".claude-setup button", "Check again");
  await sleep(900);
  c = await card(p, "Claude (Family)");
  check(!!c && /Signed in/.test(c.pill), "after signing in through Claude Code: signed in", c);
  check(/is ready/.test(await p.eval(`document.querySelector('.claude-setup .claude-setup-title')?.textContent ?? ''`)), "…and its steps say it's ready");

  // ── Usage: each account's plan and limits ──
  await ws.rpc("mock_plan_limits", { provider: added.id, limits: [{ window: "five_hour", label: "5-hour limit", used_percent: 77, resets_at: Date.now() + 90 * 60_000, updated_at: Date.now() - 60_000 }] });
  await open(p, "usage");
  await p.waitFor(`document.querySelector('.usage .kpi')`, 8000).catch(() => {});
  await sleep(500);
  const subs = await p.eval(`[...document.querySelectorAll('.usage-sub')].map((c) => c.querySelector('.setting-title')?.textContent ?? '')`);
  check(subs.some((t) => /ChatGPT \(Office\)/.test(t)) && subs.some((t) => /ChatGPT \(Codex\)/.test(t)), "Usage lists each ChatGPT account by its name", subs);
  const officeLimits = await p.eval(`(() => { const c = [...document.querySelectorAll('.usage-sub')].find((c) => /Office/.test(c.textContent)); return c ? c.textContent : ''; })()`);
  check(/77/.test(officeLimits), "…each with its own limits", officeLimits.slice(0, 200));
  await p.shot(`${SHOTS}/accounts-usage-desktop-dark.png`);
  check(!p.errors.length, "desktop: no page errors", p.errors);
  await p.close();

  // ── the folded phone and the unfolded Fold ──
  for (const [label2, w, h] of [["phone", 344, 760], ["fold", 884, 1104]]) {
    const q = await device();
    await q.viewport(w, h, true, 2);
    await signIn(q, `Accounts ${label2}`);
    for (const theme of ["dark", "light"]) {
      await q.theme(theme);
      await open(q, "models");
      await openConnect(q);
      await q.clickText(".connect-card-btn", "Another ChatGPT account");
      await q.waitFor(`document.querySelector('.account-form input')`, 3000).catch(() => {});
      await sleep(300);
      const fits = await q.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth <= b.clientWidth + 1; })()`);
      check(fits, `${label2} ${theme}: the account form fits the width`);
      await q.shot(`${SHOTS}/accounts-form-${label2}-${theme}.png`);
      const row = await q.eval(`(() => { const r = document.querySelector('.accounts-row'); if (!r) return null; const b = r.getBoundingClientRect(); return { w: Math.round(b.width), right: Math.round(b.right) }; })()`);
      check(!!row && row.right <= w + 1, `${label2} ${theme}: the next-account switch fits`, row);
      await q.eval(`(() => { const c = [...document.querySelectorAll('.card.provider')].find((c) => c.textContent.includes('Claude (Family)')); c?.scrollIntoView({ block: 'center' }); })()`);
      await sleep(200);
      await q.shot(`${SHOTS}/accounts-cards-${label2}-${theme}.png`);
      await q.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab(null))`);
      await sleep(300);
    }
    check(!q.errors.length, `${label2}: no page errors`, q.errors);
    await q.close();
  }
  ws.close();
  await mock("/__mock/reset");
  finish();
});
