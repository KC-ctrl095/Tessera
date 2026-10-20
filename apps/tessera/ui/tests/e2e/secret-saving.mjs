// SECRET SAVING: Settings → Passwords & keys, cleaned up. At the top: how
// passwords and keys typed in a chat are saved (Save automatically / Ask me
// first / Don't save; only there since batch 16). Then what chats keep for
// themselves (with Save), and the saved ones grouped (Logins, Keys & tokens,
// Two-factor), a search once the list is long, one Add, and an empty state.
// In a chat, "Ask me first" shows a card (Save / Don't save) that the server
// rewrites once answered; "Don't save" a calm line. No value on any page.
// Desktop, the unfolded Fold and the folded phone (cover screen), dark and light.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

// Formats the mock catches (dev/mock-server.mjs SETTINGS_KEY_FORMATS).
const OPENAI = "sk-proj-" + "Ab3Cd5Ef7Gh9Ij1Kl3Mn5Op7Qr9St";
const GITHUB = "ghp_" + "Q7v2Lm9Xc4".repeat(3) + "Rt8Wn1";
const SLACK = "xoxb-" + "2741-9283-mockslacktoken";
const GOOGLE = "AIza" + "Sy3Kd8Lm2Np5Qr7St9Uv1Wx4Yz6Ab0Cd2Ef";
const VALUES = [OPENAI, GITHUB, SLACK, GOOGLE, "mock-password", "owm_mock", "sk_test_mock"];

run(async () => {
  await mock("/__mock/reset");
  const ws = await wsClient("secret saving script");
  const list = async () => (await ws.rpc("list_secrets")).result;
  const sec = async () => (await ws.rpc("get_security")).result;
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Secret saving desktop");
  const openPage = async (page = p) => {
    await page.eval(`__mod('lib/ui.ts').then(m => { m.setSettingsTab(null); m.openSettings('secrets'); })`);
    await page.waitFor(`document.querySelector('.creds .cred-bar')`, 8000);
    await sleep(500);
  };
  const closeSettings = async (page = p) => {
    await page.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab(null))`);
    await sleep(300);
  };
  /** No stored or typed value anywhere: text, attributes or inputs. */
  const leaks = (page = p) =>
    page.eval(`(() => { const html = document.documentElement.outerHTML + [...document.querySelectorAll('input, textarea')].map(i => i.value).join(' '); return ${JSON.stringify(VALUES)}.filter(v => html.includes(v)); })()`);
  const groups = () => p.eval(`[...document.querySelectorAll('.cred-group[data-group]')].map(g => ({ id: g.dataset.group, title: g.querySelector('.cred-group-title').textContent.replace(/\\s+/g, ' ').trim(), names: [...g.querySelectorAll('.secret-item')].map(r => r.dataset.name) }))`);
  const choice = () => p.eval(`[...document.querySelectorAll('.cred-saving-seg [role=radio]')].map(b => ({ t: b.textContent.trim(), on: b.getAttribute('aria-checked') === 'true' }))`);
  const pick = async (label, scope = ".cred-saving") => {
    await p.clickText(`${scope} .cred-saving-seg button`, label);
    await sleep(500);
  };

  // ── the page: the choice on top, the groups ──
  await openPage();
  const first = await p.eval(`document.querySelector('.creds').firstElementChild?.className`);
  check(/cred-saving/.test(first ?? ""), "the saving choice is the first thing on the page", first);
  let c = await choice();
  check(c.map((x) => x.t).join("|") === "Save automatically|Ask me first|Don't save" && c[0].on, "three plain choices; Save automatically is chosen (today's behaviour)", c);
  check(/right away/.test(await p.eval(`document.querySelector('.cred-saving-hint').textContent`)), "its hint says what it does");
  await ws.rpc("save_secret", { name: "BANK_2FA", kind: "login", description: "Bank two-factor", fields: { totp: "JBSWY3DPEHPK3PXP", username: "ann" }, ask: true, hosts: [] });
  await openPage();
  let g = await groups();
  check(
    JSON.stringify(g.map((x) => [x.id, x.names])) === JSON.stringify([["login", ["GITHUB", "SWITCH_ADMIN"]], ["key", ["OPENWEATHER_KEY", "STRIPE_TEST"]], ["twofactor", ["BANK_2FA"]]]),
    "grouped: Logins, Keys & tokens (sets of values too), Two-factor (a code seed alone)",
    g,
  );
  check(g[0].title === "Logins 2" && g[1].title === "Keys & tokens 2" && g[2].title === "Two-factor 1", "each group says how many", g.map((x) => x.title));
  check(!(await p.eval(`!!document.querySelector('.cred-search')`)), "a short list has no search box");
  check(!(await p.eval(`!!document.querySelector('.explain, .add-choices')`)), "no big explainer or three add cards any more");
  await p.click(".cred-add");
  await sleep(300);
  const menu = await p.eval(`[...document.querySelectorAll('.menu-item .menu-label')].map(m => m.textContent)`);
  check(menu.join("|") === "Login|Key or token|Several values", "one Add, with the three kinds in a small menu", menu);
  await p.key("Escape");
  await sleep(200);
  check((await leaks()).length === 0, "no value on the page", await leaks());
  await p.shot(`${SHOTS}/ss-list-desk-dark.png`);

  // A long list gets a search box.
  for (const [n, d] of [["MAPS_KEY", "Maps API"], ["MAIL_TOKEN", "Mailgun"]]) await ws.rpc("save_secret", { name: n, kind: "token", description: d, fields: { value: `${n.toLowerCase()}-value-123` }, ask: true, hosts: [] });
  await openPage();
  check(await p.eval(`!!document.querySelector('.cred-search input')`), "seven saved: a search box shows");
  await p.eval(`(() => { const i = document.querySelector('.cred-search input'); i.focus(); const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; set.call(i, 'stripe'); i.dispatchEvent(new Event('input', { bubbles: true })); })()`);
  await sleep(300);
  let shown = await p.eval(`[...document.querySelectorAll('.cred-group .secret-item')].map(r => r.dataset.name)`);
  check(JSON.stringify(shown) === '["STRIPE_TEST"]', "search finds by description", shown);
  await p.eval(`(() => { const i = document.querySelector('.cred-search input'); const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; set.call(i, '$github_otp'); i.dispatchEvent(new Event('input', { bubbles: true })); })()`);
  await sleep(300);
  shown = await p.eval(`[...document.querySelectorAll('.cred-group .secret-item')].map(r => r.dataset.name)`);
  check(JSON.stringify(shown) === '["GITHUB"]', "…and by variable", shown);
  await p.eval(`(() => { const i = document.querySelector('.cred-search input'); const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; set.call(i, 'nothing like it'); i.dispatchEvent(new Event('input', { bubbles: true })); })()`);
  await sleep(300);
  check(/Nothing matches/.test(await p.eval(`document.querySelector('.cred-none')?.textContent ?? ''`)), "no match says so");
  await p.shot(`${SHOTS}/ss-search-desk-dark.png`);
  for (const n of ["MAPS_KEY", "MAIL_TOKEN"]) await ws.rpc("delete_secret", { name: n });

  // ── Ask me first ──
  await openPage();
  await pick("Ask me first");
  check((await sec()).secret_saving === "ask", "Ask me first is saved on the server");
  c = await choice();
  check(c[1].on && /asks whether to save it/.test(await p.eval(`document.querySelector('.cred-saving-hint').textContent`)), "…and its hint says the chat will ask", c);
  await p.shot(`${SHOTS}/ss-ask-desk-dark.png`);
  await closeSettings();
  const ses = (await ws.rpc("create_session", { title: "Deploy script", cwd: null, model: null })).result;
  await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession(${JSON.stringify(ses.id)}))`);
  await sleep(800);
  const send = (text) => ws.rpc("send", { session_id: ses.id, content: [{ type: "text", text }], delivery: "auto" });
  const card = () => p.eval(`(() => { const c = [...document.querySelectorAll('.secret-ask')].pop(); return c && { title: c.querySelector('.secret-ask-title').textContent, what: c.querySelector('.secret-saved-what')?.textContent, vars: [...c.querySelectorAll('.var-chip')].map(x => x.textContent), buttons: [...c.querySelectorAll('.secret-ask-actions button')].map(b => b.textContent.trim()) }; })()`);
  await send(`use my OpenAI key ${OPENAI} for the script`);
  await p.waitFor(`document.querySelector('.secret-ask')`, 8000).catch(() => {});
  await sleep(500);
  let k = await card();
  check(k?.title === "Save to Passwords & keys?" && k.what === "OpenAI API key" && k.vars[0] === "$OPENAI_API_KEY" && k.buttons.join("|") === "Save|Don't save", "a card asks: Save to Passwords & keys? (Save / Don't save)", k);
  let s = (await list()).find((x) => x.name === "OPENAI_API_KEY");
  check(s?.chat === ses.id, "until then this chat keeps it", s);
  check((await leaks()).length === 0, "the key is nowhere on the page", await leaks());
  const bubble = await p.eval(`[...document.querySelectorAll('.msg-user .bubble-text')].map(b => b.textContent).pop()`);
  check(/\$OPENAI_API_KEY/.test(bubble ?? ""), "the message keeps only its name", bubble);
  await p.shot(`${SHOTS}/ss-card-desk-dark.png`);
  await p.clickText(".secret-ask .secret-ask-actions button", "Save");
  await p.waitFor(`!document.querySelector('.secret-ask')`, 6000).catch(() => {});
  await sleep(400);
  let line = await p.eval(`[...document.querySelectorAll('.secret-saved')].map(n => n.querySelector('.secret-saved-label').textContent)`);
  check(line.includes("Saved to Passwords & keys") && !(await p.eval(`!!document.querySelector('.secret-ask')`)), "Save: the card turns into “Saved to Passwords & keys”", line);
  s = (await list()).find((x) => x.name === "OPENAI_API_KEY");
  check(s && !s.chat, "…and it is in Passwords & keys for every chat", s);

  await send(`and the GitHub one ${GITHUB}`);
  await p.waitFor(`document.querySelector('.secret-ask')`, 8000).catch(() => {});
  await sleep(500);
  await p.clickText(".secret-ask .secret-ask-actions button", "Don't save");
  await p.waitFor(`!document.querySelector('.secret-ask')`, 6000).catch(() => {});
  await sleep(400);
  line = await p.eval(`[...document.querySelectorAll('.secret-saved.kept')].map(n => n.querySelector('.secret-saved-label').textContent)`);
  check(line.includes("Kept for this chat only"), "Don't save: “Kept for this chat only”", line);
  s = (await list()).find((x) => x.name === "GITHUB_TOKEN");
  check(s?.chat === ses.id, "…it stays this chat's", s);
  await send(`slack bot ${SLACK}`);
  await p.waitFor(`document.querySelector('.secret-ask')`, 8000).catch(() => {});
  await sleep(500);
  check((await leaks()).length === 0, "still no value on the page", await leaks());

  // On the page: kept for one chat, with Save; deleting one there settles the chat too.
  await openPage();
  const kept = await p.eval(`[...document.querySelectorAll('.cred-kept .secret-item')].map(r => ({ name: r.dataset.name, who: r.querySelector('.cred-who').textContent, save: !!r.querySelector('.cred-keep-save'), edit: [...r.querySelectorAll('button')].some(b => b.textContent.includes('Edit')) }))`);
  check(kept.length === 2 && kept.every((x) => x.save && !x.edit && /Only in “Deploy script”/.test(x.who)), "“Kept for one chat”: each with its chat, Save and Delete", kept);
  check(!(await p.eval(`[...document.querySelectorAll('.cred-group[data-group] .secret-item')].some(r => r.dataset.name === 'GITHUB_TOKEN')`)), "kept ones aren't among the saved");
  await p.shot(`${SHOTS}/ss-kept-desk-dark.png`);
  await p.click(`.cred-kept .secret-item[data-name="SLACK_TOKEN"] .cred-keep-save`);
  await sleep(700);
  s = (await list()).find((x) => x.name === "SLACK_TOKEN");
  check(s && !s.chat && (await p.eval(`!!document.querySelector('.cred-group[data-group="key"] .secret-item[data-name="SLACK_TOKEN"]')`)), "Save there: it joins Keys & tokens", s);
  await p.click(`.cred-kept .secret-item[data-name="GITHUB_TOKEN"] .icon-btn.danger`);
  await sleep(400);
  const dlg = await p.eval(`document.querySelector('.dialog-text')?.textContent ?? ''`);
  check(/Only “Deploy script” keeps it/.test(dlg), "deleting a kept one says which chat loses it", dlg);
  await p.click(".dialog-actions button[type=submit]");
  await sleep(700);
  check(!(await list()).some((x) => x.name === "GITHUB_TOKEN") && !(await p.eval(`!!document.querySelector('.cred-kept')`)), "deleted; nothing kept for a chat any more");
  await closeSettings();
  await sleep(300);
  const chatNow = await p.eval(`({ ask: document.querySelectorAll('.secret-ask').length, labels: [...document.querySelectorAll('.secret-saved .secret-saved-label')].map(l => l.textContent), forgot: [...document.querySelectorAll('.notice')].some(n => /Forgot GITHUB_TOKEN/.test(n.textContent)) })`);
  check(chatNow.ask === 0 && chatNow.labels.filter((l) => l === "Saved to Passwords & keys").length === 2 && chatNow.forgot, "the chat follows: the Slack card says saved, the GitHub line says forgotten", chatNow);

  // ── Don't save: the choice lives only on Passwords & keys (Privacy & safety has no copy) ──
  await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings('security'))`);
  await p.waitFor(`document.querySelector('.sec .sec-glance')`, 8000).catch(() => {});
  await sleep(400);
  check(!(await p.eval(`!!document.querySelector('.sec-saving, .sec .cred-saving-seg')`)), "Privacy & safety has no second copy of the saving choice");
  await openPage();
  await pick("Don't save");
  check((await sec()).secret_saving === "chat_only", "Don't save, chosen on Passwords & keys, is saved");
  await p.shot(`${SHOTS}/ss-security-desk-dark.png`);
  await closeSettings();
  await send(`maps key ${GOOGLE}`);
  await sleep(1200);
  const quiet = await p.eval(`({ ask: document.querySelectorAll('.secret-ask').length, kept: [...document.querySelectorAll('.secret-saved.kept .var-chip')].map(c => c.textContent) })`);
  check(quiet.ask === 0 && quiet.kept.includes("$GOOGLE_API_KEY"), "Don't save: no question, a calm “Kept for this chat only” line", quiet);
  check((await list()).find((x) => x.name === "GOOGLE_API_KEY")?.chat === ses.id, "…kept for this chat");
  // Taking them out turned off: the choice can't apply and says why.
  await ws.rpc("set_security", { save_secrets_from_chat: false });
  await openPage();
  const off = await p.eval(`({ warn: document.querySelector('.cred-saving-off')?.textContent ?? '', disabled: [...document.querySelectorAll('.cred-saving-seg button')].every(b => b.disabled) })`);
  check(/reaches the AI/.test(off.warn) && off.disabled, "with taking them out turned off, the page says so and the choice waits", off);
  await ws.rpc("set_security", { save_secrets_from_chat: true, secret_saving: "auto" });

  // ── empty ──
  for (const x of await list()) await ws.rpc("delete_secret", { name: x.name });
  await openPage();
  const empty = await p.eval(`document.querySelector('.cred-empty')?.textContent ?? ''`);
  check(/Nothing saved yet/.test(empty) && /type one in a chat/.test(empty), "an empty list explains what to do", empty);
  await p.shot(`${SHOTS}/ss-empty-desk-dark.png`);
  await p.theme("light");
  await sleep(300);
  await p.shot(`${SHOTS}/ss-empty-desk-light.png`);
  check(p.errors.length === 0, "desktop: no errors", p.errors);
  await p.close();

  // ── sizes and themes: a full page and a card in a chat ──
  await mock("/__mock/reset");
  const ws2 = await wsClient("secret saving sizes");
  await ws2.rpc("set_security", { secret_saving: "ask" });
  await ws2.rpc("save_secret", { name: "BANK_2FA", kind: "login", description: "Bank two-factor", fields: { totp: "JBSWY3DPEHPK3PXP" }, ask: true, hosts: [] });
  const chat = (await ws2.rpc("create_session", { title: "A rather long chat title about the deploy", cwd: null, model: null })).result;
  const say = (text) => ws2.rpc("send", { session_id: chat.id, content: [{ type: "text", text }], delivery: "auto" });
  await say(`github ${GITHUB}`);
  await sleep(600);
  await ws2.rpc("decide_chat_secret", { session_id: chat.id, name: "GITHUB_TOKEN", choice: "chat_only" });
  await say(`and openai ${OPENAI}`);
  await sleep(600);
  for (const [name, w, h] of [["desk", 1440, 900], ["fold", 884, 1104], ["folded", 344, 882]]) {
    for (const theme of ["dark", "light"]) {
      const m = await device();
      if (name === "desk") await m.viewport(w, h);
      else await m.viewport(w, h, true, 2);
      await signIn(m, `Secret saving ${name} ${theme}`);
      await m.theme(theme);
      await m.eval(`__mod('lib/ui.ts').then(u => { u.setDrawerOpen?.(false); u.openSettings('secrets'); })`);
      await m.waitFor(`document.querySelector('.creds .cred-bar')`, 8000);
      await sleep(700);
      await m.shot(`${SHOTS}/ss-page-${name}-${theme}.png`);
      const over = await m.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth - b.clientWidth; })()`);
      check(over <= 1, `${name} ${theme}: the page fits the width`, over);
      const seg = await m.eval(`(() => { const r = [...document.querySelectorAll('.cred-saving-seg button')].map(b => b.getBoundingClientRect()); const box = document.querySelector('.cred-saving').getBoundingClientRect(); return { n: r.length, inside: r.every(x => x.left >= box.left - 1 && x.right <= box.right + 1), h: Math.min(...r.map(x => x.height)), oneRow: r.every(x => Math.abs(x.top - r[0].top) < 2) }; })()`);
      check(seg.n === 3 && seg.inside && seg.oneRow && seg.h >= (name === "desk" ? 30 : 34), `${name} ${theme}: the three choices sit in one row inside the card, big enough to tap`, seg);
      const spill = await m.eval(`[...document.querySelectorAll('.secret-item')].flatMap(r => { const b = r.getBoundingClientRect(); return [...r.querySelectorAll('.secret-title, .secret-actions, .var-chip, .cred-scope > span')].filter(e => { const x = e.getBoundingClientRect(); return x.right > b.right + 1 || x.left < b.left - 1; }).map(e => r.dataset.name + ':' + e.className); })`);
      check(spill.length === 0, `${name} ${theme}: nothing spills out of a row`, spill);
      const bar = await m.eval(`(() => { const a = document.querySelector('.cred-add').getBoundingClientRect(); const b = document.querySelector('.cred-bar').getBoundingClientRect(); return a.right <= b.right + 1 && a.top < b.top + 40; })()`);
      check(bar, `${name} ${theme}: Add sits at the top right of the list`);
      await m.eval(`document.querySelector('.cred-kept')?.scrollIntoView({ block: 'start' })`);
      await sleep(200);
      await m.shot(`${SHOTS}/ss-page-kept-${name}-${theme}.png`);
      check((await leaks(m)).length === 0, `${name} ${theme}: no value on the page`, await leaks(m));
      await m.eval(`__mod('lib/ui.ts').then(u => u.setSettingsTab(null))`);
      await m.eval(`__mod('lib/workspace.ts').then(w => w.openSession(${JSON.stringify(chat.id)}))`);
      await m.waitFor(`document.querySelector('.secret-ask')`, 8000).catch(() => {});
      await sleep(700);
      await m.eval(`document.querySelector('.secret-ask')?.scrollIntoView({ block: 'center' })`);
      await sleep(300);
      await m.shot(`${SHOTS}/ss-chat-${name}-${theme}.png`);
      const fits = await m.eval(`(() => { const out = []; for (const n of document.querySelectorAll('.secret-ask, .secret-saved')) { const r = n.getBoundingClientRect(); if (r.right > innerWidth + 1 || r.left < -1) out.push(n.className); } const a = document.querySelector('.secret-ask'); const btns = a ? [...a.querySelectorAll('.secret-ask-actions button')].map(b => b.getBoundingClientRect().height) : []; return { out, card: !!a, btns }; })()`);
      check(fits.card && fits.out.length === 0 && fits.btns.every((x) => x >= 28), `${name} ${theme}: the card and the lines fit the screen`, fits);
      check(m.errors.length === 0, `${name} ${theme}: no errors`, m.errors);
      await m.close();
    }
  }
  ws2.close();
  ws.close();
  finish();
});
