// APP-SETTINGS: Settings → Privacy & safety and its page What commands can reach (sandbox, folders,
// switches, confirms; batch 16 split the old Security section),
// Settings → Usage pricing (API prices / Subscription, plans, unknown prices)
// and the chat notice for a secret saved from a message. Desktop and phone,
// dark and light.
import { MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const REAL = !!process.env.REAL; // against a real throwaway server: no mock-only controls
const KEY = "sk-proj-" + "Q7v2Lm9Xc4Rt8Wn1Bz6Hk3Jd5Fs0Ga2P";

run(async () => {
  if (!REAL) await mock("/__mock/reset");
  const ws = await wsClient("settings script");
  if (REAL) await ws.rpc("complete_setup"); // a fresh server would open its setup on every load
  const sec = async () => (await ws.rpc("get_security")).result;
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Settings desktop");
  if (REAL) {
    // A fresh server opens its setup; not what this checks.
    await p.eval(`__mod('lib/ui.ts').then(m => m.setSetupOpen(false))`);
    await sleep(400);
  }
  const open = async (tab) => {
    await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings(${JSON.stringify(tab)}))`);
    await sleep(700);
  };
  const dialog = () => p.eval(`document.querySelector('.dialog-body') ? { title: [...document.querySelectorAll('.modal h2')].pop()?.textContent ?? '', body: document.querySelector('.dialog-text')?.textContent ?? '', danger: !!document.querySelector('.dialog-actions .btn-danger') } : null`);
  const answer = async (ok) => {
    await p.click(ok ? ".dialog-actions button[type=submit]" : ".dialog-actions .btn-ghost");
    await sleep(500);
  };
  const modeOn = () => p.eval(`[...document.querySelectorAll('.sec-mode .perm-card')].find(b => b.getAttribute('aria-checked') === 'true')?.querySelector('.perm-label').textContent`);
  const toggle = (title) =>
    title === "Keep passwords and keys out of chats"
      ? `document.querySelector('.cred-saving .toggle')`
      : `[...document.querySelectorAll('.sec .setting-row')].find(r => r.querySelector('.setting-title')?.textContent === ${JSON.stringify(title)})?.querySelector('.toggle')`;
  /** Privacy & safety's More, opened. */
  const openMore = async () => {
    await p.waitFor(`document.querySelector('.sec-more .disclosure')`, 8000);
    if ((await p.eval(`document.querySelector('.sec-more .disclosure').getAttribute('aria-expanded')`)) !== "true") await p.click(".sec-more .disclosure");
    await sleep(300);
  };
  const clickToggle = async (title) => {
    const b = await p.eval(`(() => { const t = ${toggle(title)}; t.scrollIntoView({ block: 'center' }); const r = t.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; })()`);
    await p.mouse(b[0], b[1]);
    await sleep(400);
  };

  // ── Privacy & safety ──
  await open("security");
  await p.waitFor(`document.querySelector('.sec .sec-glance')`, 8000);
  const nav = await p.eval(`[...document.querySelectorAll('.settings-tab')].map(t => t.textContent.trim())`);
  check(nav.includes("Privacy & safety") && !nav.includes("Security") && !nav.includes("Passwords & keys"), "the old name lands on Privacy & safety; Passwords & keys is a page of it, not a section", nav);
  const s0 = await sec();
  check(s0.sandbox === "auto" && s0.ask_outside && s0.redact_patterns && s0.save_secrets_from_chat && s0.folders.length === 0, "secure defaults on the server", s0);
  check(/agents can't/i.test(await p.eval(`document.querySelector('.sec .explain').textContent`)), "it says agents can't change these");
  const glance = await p.eval(`[...document.querySelectorAll('.sec-glance li')].map(l => l.dataset.id)`);
  check(["sandbox", "chats", "e2ee"].every((x) => glance.includes(x)), "at a glance: the sandbox, passwords out of chats, encryption", glance);
  const pages = await p.eval(`[...document.querySelectorAll('.sec-pages .page-row .setting-title')].map(e => e.textContent)`);
  check(JSON.stringify(pages) === JSON.stringify(["Passwords & keys", "What commands can reach"]), "two pages one step deeper: Passwords & keys, What commands can reach", pages);
  check(/Sandbox: automatic/.test(await p.eval(`[...document.querySelectorAll('.sec-pages .page-row')][1].textContent`)), "…each says where it stands");
  check(!(await p.eval(`!!document.querySelector('.sec .sec-saving, .sec .sec-machines')`)), "no second copy of the saving choice or the machines choice (they live on Passwords & keys)");
  // The encryption line leads to its row under More: More opens.
  await p.eval(`document.querySelector('.sec-glance li[data-id="e2ee"] button').click()`);
  await sleep(700);
  check(await p.eval(`(() => { const r = document.getElementById('sec-e2ee'); if (!r) return false; const b = r.getBoundingClientRect(), v = document.querySelector('.settings-body').getBoundingClientRect(); return b.top >= v.top - 1 && b.bottom <= v.bottom + 1; })()`), "the encryption line at a glance opens More at its row");
  await openMore();
  const protectedList = await p.eval(`[...document.querySelectorAll('.sec-protected li')].map(l => l.textContent)`);
  check(protectedList.length > 0 && protectedList.length === s0.protected.length, "what stays out of reach is listed (under More, behind a disclosure)", protectedList);
  // The security guide opens in place (built into the server, no website needed).
  await p.eval(`[...document.querySelectorAll('.sec-doc button')].find(b => /Security guide/.test(b.textContent)).click()`);
  await p.waitFor(`(document.querySelector('.guide-modal .guide-body')?.textContent.length ?? 0) > 500`, 6000).catch(() => {});
  const guide = await p.eval(`(() => { const m = document.querySelector('.guide-modal'); return m && { title: m.querySelector('.modal-title, h2, header')?.textContent ?? '', text: m.querySelector('.guide-body')?.textContent ?? '' }; })()`);
  check(!!guide && /sandbox/i.test(guide.text) && guide.text.length > 500, "the Security guide opens in place with its text", guide && { title: guide.title, len: guide.text.length });
  await p.key("Escape");
  await sleep(250);
  check(!(await p.eval(`!!document.querySelector('.guide-modal')`)), "Escape closes the guide");
  await p.shot(`${SHOTS}/as-safety-desk-dark.png`);

  // ── What commands can reach ──
  await p.clickText(".sec-pages .page-row", "What commands can reach");
  await p.waitFor(`document.querySelector('.sec .sec-status')`, 8000);
  check((await modeOn()) === "Automatic", "Automatic is the chosen sandbox mode");
  const status = await p.eval(`({ tone: document.querySelector('.sec-status').dataset.tone, title: document.querySelector('.sec-status .setting-title').textContent })`);
  if (!REAL) check(status.tone === "ok" && /run in a sandbox/.test(status.title), "a complete sandbox reads as good news", status);
  else check(!!status.title, "the sandbox status is shown", status);
  check(!(await p.eval(`!!document.querySelector('.sec-hint')`)) || REAL, "no hint when nothing is missing");
  check(await p.eval(`!!document.querySelector('#set-terminal .term-settings, #set-terminal .card, #set-terminal')`), "…and the terminal on this server is on the same page");
  await p.shot(`${SHOTS}/as-security-desk-dark.png`);

  // sandbox off: a clear confirm; Cancel keeps it on
  await p.clickText(".sec-mode .perm-label", "Off");
  await sleep(300);
  let d = await dialog();
  check(!!d && d.danger && /keys/.test(d.body), "turning the sandbox off asks first, plainly, as a dangerous choice", d);
  await p.shot(`${SHOTS}/as-security-confirm-off.png`);
  await answer(false);
  check((await sec()).sandbox === "auto" && (await modeOn()) === "Automatic", "Cancel: the sandbox stays on");
  await p.clickText(".sec-mode .perm-label", "Off");
  await sleep(300);
  await answer(true);
  check((await sec()).sandbox === "off" && (await modeOn()) === "Off", "confirmed: the server has it off");
  check((await p.eval(`document.querySelector('.sec-status').dataset.tone`)) === "bad" && /off/.test(await p.eval(`document.querySelector('.sec-status .setting-title').textContent`)), "the status says the sandbox is off, in red");
  await p.clickText(".sec-mode .perm-label", "Automatic");
  await sleep(500);
  check(!(await dialog()) && (await sec()).sandbox === "auto", "turning it back on doesn't ask");

  // a kernel that only supports part of it: the hint, and strict asks
  if (!REAL) {
    await ws.rpc("mock_security", { status: "partial (namespaces only: no Landlock on this kernel)", complete: false, hint: "Ubuntu 22.04: install the newer kernel with `sudo apt install linux-generic-hwe-22.04`, then reboot." });
    await p.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab('secrets'))`);
    await sleep(300);
    await open("commands");
    await p.waitFor(`document.querySelector('.sec .sec-status')`);
    const hint = await p.eval(`(() => { const h = document.querySelector('.sec-hint'); return h && { text: h.textContent, code: h.querySelector('code')?.textContent, copy: !!h.querySelector('[aria-label="Copy command"]') }; })()`);
    check(!!hint && hint.code === "sudo apt install linux-generic-hwe-22.04" && hint.copy, "partial sandbox: the hint shows the command, ready to copy", hint);
    check((await p.eval(`document.querySelector('.sec-status').dataset.tone`)) === "warn", "a partial sandbox is a warning, not an error");
    check(/Every command asks first/.test(await p.eval(`document.querySelector('.sec-status').textContent`)), "Automatic explains the approval fallback on incomplete systems");
    await p.shot(`${SHOTS}/as-security-partial-desk-dark.png`);
    await p.clickText(".sec-mode .perm-label", "Strict");
    await sleep(300);
    d = await dialog();
    check(!!d && !d.danger && /won't be able to run commands/.test(d.body), "strict on a partial sandbox warns that commands would stop", d);
    await answer(false);
    check((await sec()).sandbox === "auto", "…and nothing changes on Cancel");
    await ws.rpc("mock_security", { status: "unavailable (this system cannot create namespaces)", complete: false, hint: null });
    await open("commands");
    await p.eval(`__mod('components/settings/securityState.ts').then(m => m.loadSecurity())`);
    await p.waitFor(`document.querySelector('.sec-status').textContent.includes('Every command asks first')`);
    check(/one-time choice of Off/.test(await p.eval(`document.querySelector('.sec-status').textContent`)), "unavailable sandbox explains the explicit choice in its one settings page");
    for (const [width, height, label] of [[1440, 900, 'desktop'], [344, 760, 'cover'], [884, 1104, 'fold']]) {
      await p.viewport(width, height);
      for (const theme of ['dark', 'light']) {
        await p.theme(theme);
        await sleep(250);
        check(await p.eval(`document.querySelector('.settings-body').scrollWidth <= document.querySelector('.settings-body').clientWidth + 1`), `${label} ${theme}: fallback wording fits without side scrolling`);
        await p.shot(`${SHOTS}/sandbox-fallback-${label}-${theme}.png`);
      }
    }
    await p.viewport(1440, 900);
    await p.theme('dark');
    await ws.rpc("mock_security", { status: "on (namespaces + Landlock)", complete: true, hint: null });
    await p.eval(`__mod('components/settings/securityState.ts').then(m => m.loadSecurity())`);
  }

  // folders: add through the folder picker, switch to read only, remove
  await p.click(".sec-add .btn");
  await p.waitFor(`document.querySelector('.dirb-path input')`);
  const home = await p.eval(`__mod('lib/store.ts').then(m => m.state.server.home)`);
  const target = REAL ? process.env.FOLDER : `${home}/projects/website`;
  await p.eval(`(() => { const i = document.querySelector('.dirb-path input'); const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; set.call(i, ${JSON.stringify(target)}); i.dispatchEvent(new Event('input', { bubbles: true })); i.form.requestSubmit(); })()`);
  await sleep(700);
  await p.clickText(".dirb-foot .btn-primary", "Use this folder");
  await sleep(700);
  let s1 = await sec();
  check(s1.folders.length === 1 && s1.folders[0].path === target && s1.folders[0].write === true, "a chosen folder is added, read & write", s1.folders);
  const row = await p.eval(`[...document.querySelectorAll('.sec-folder:not(.fixed)')].map(r => ({ path: r.querySelector('.sec-folder-path').textContent, on: r.querySelector('[aria-checked="true"]')?.textContent }))`);
  check(row.length === 1 && row[0].on === "Read & write" && (row[0].path.startsWith("~/") || REAL), "it shows with ~ for home and its access", row);
  await p.clickText(".sec-folder .segmented button", "Read only");
  await sleep(500);
  s1 = await sec();
  check(s1.folders[0].write === false, "Read only is saved", s1.folders);
  // the whole home folder deserves a second look
  await p.click(".sec-add .btn");
  await p.waitFor(`document.querySelector('.dirb-path input')`);
  await p.eval(`(() => { const i = document.querySelector('.dirb-path input'); const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; set.call(i, ${JSON.stringify(home)}); i.dispatchEvent(new Event('input', { bubbles: true })); i.form.requestSubmit(); })()`);
  await sleep(600);
  await p.clickText(".dirb-foot .btn-primary", "Use this folder");
  await sleep(500);
  d = await dialog();
  check(!!d && d.danger, "opening the whole home folder asks first", d);
  await answer(false);
  check((await sec()).folders.length === 1, "…Cancel adds nothing");
  await p.shot(`${SHOTS}/as-security-folders-desk-dark.png`);
  await p.click(".sec-folder:not(.fixed) .icon-btn");
  await sleep(500);
  check((await sec()).folders.length === 0 && !(await p.eval(`!!document.querySelector('.sec-folder:not(.fixed)')`)), "a folder can be removed");

  // switches: off asks, on doesn't (two under Privacy & safety's More, one at the top of Passwords & keys)
  for (const [title, key, where] of [["Ask before writing elsewhere", "ask_outside", "safety"], ["Hide passwords in what commands return", "redact_patterns", "safety"], ["Keep passwords and keys out of chats", "save_secrets_from_chat", "secrets"]]) {
    await open(where);
    if (where === "safety") await openMore();
    else await p.waitFor(`document.querySelector('.cred-saving .toggle')`, 8000);
    await clickToggle(title);
    d = await dialog();
    check(!!d && d.danger, `${key}: turning it off asks first`, d);
    await answer(false);
    check((await sec())[key] === true, `${key}: Cancel keeps it on`);
    await clickToggle(title);
    await answer(true);
    check((await sec())[key] === false && (await p.eval(`${toggle(title)}.getAttribute('aria-checked')`)) === "false", `${key}: confirmed, it's off`);
    await clickToggle(title);
    check(!(await dialog()) && (await sec())[key] === true, `${key}: back on without a question`);
  }
  await open("commands");
  await p.waitFor(`document.querySelector('.sec .sec-status')`, 8000);
  {
    // the server refuses Tessera's own folders; the UI says why
    await p.click(".sec-add .btn");
    await p.waitFor(`document.querySelector('.dirb-path input')`);
    await p.eval(`(() => { const i = document.querySelector('.dirb-path input'); const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; set.call(i, ${JSON.stringify(s0.protected.find((x) => /\.config\/tessera$/.test(x)) ?? s0.protected[0])}); i.dispatchEvent(new Event('input', { bubbles: true })); i.form.requestSubmit(); })()`);
    await sleep(600);
    await p.clickText(".dirb-foot .btn-primary", "Use this folder");
    await sleep(700);
    const toastText = await p.eval(`[...document.querySelectorAll('.toast')].map(t => t.textContent).join(' | ')`);
    check(/Tessera's own data/.test(toastText) && (await sec()).folders.length === 0, "a folder the server refuses: its reason is shown, nothing is added", toastText);
    await p.eval(`__mod('lib/ui.ts').then(m => { m.toasts().forEach(t => m.dismissToast(t.id)); m.setDirBrowser(null); })`);
  }
  check(p.errors.length === 0, "security: no errors in the page", p.errors);

  // ── Usage pricing ──
  for (const s0 of (await ws.rpc("get_usage", { from: Date.now() - 30 * 864e5 })).result.subscriptions ?? []) await ws.rpc("set_subscription", { provider: s0.provider });
  await p.eval(`localStorage.removeItem('tessera.usage.pricing')`);
  await open("usage");
  await p.waitFor(`document.querySelector('.usage .kpi')`, 8000);
  await sleep(300);
  const from = await p.eval(`(() => { const d = new Date(); d.setHours(0,0,0,0); d.setDate(d.getDate() - 29); return d.getTime(); })()`);
  let rep = (await ws.rpc("get_usage", { from })).result;
  const kpi = () => p.eval(`(() => { const k = [...document.querySelectorAll('.usage .kpi')].pop(); return { label: k.querySelector('.kpi-label').textContent, value: k.querySelector('.kpi-value').textContent, sub: k.querySelector('.kpi-sub')?.textContent ?? '' }; })()`);
  const usd = (v) => (v <= 0 ? "$0" : v < 0.01 ? "<$0.01" : v >= 100 ? `$${Math.round(v)}` : `$${v.toFixed(2)}`);
  check(Array.isArray(rep.subscriptions) && "unpriced" in rep, "the server reports subscriptions and unpriced models", Object.keys(rep));
  let k = await kpi();
  check(await p.eval(`!!document.querySelector('.usage-bar .usage-pricing')`), "a toggle: API prices / Subscription");
  check(k.label === "At API prices" && k.value === usd(rep.total.api_cost_usd ?? 0), "API prices: the total at pay-per-token prices, subscriptions included", { k, api: rep.total.api_cost_usd, billed: rep.total.cost_usd });
  const subModel = rep.by_model.find((m) => rep.subscriptions.some((s) => s.provider === m.provider));
  if (subModel) {
    const cell = await p.eval(`[...document.querySelectorAll('.usage-table tbody tr')].find(r => r.querySelector('.usage-model-provider')?.textContent === ${JSON.stringify(subModel.provider)})?.lastElementChild.textContent`);
    check(subModel.totals.api_cost_usd != null && cell === usd(subModel.totals.api_cost_usd), "a subscription model shows what it would cost per token", { cell, api: subModel.totals.api_cost_usd });
  }
  check((await p.eval(`[...document.querySelectorAll('.usage-table thead th')].pop().textContent`)) === "API price", "the column says API price");
  const unpricedText = await p.eval(`document.querySelector('.usage-unpriced')?.textContent ?? ''`);
  if (rep.unpriced.length) {
    const names = await p.eval(`__mod('lib/store.ts').then(m => ${JSON.stringify(rep.unpriced)}.map(r => m.modelLabel({ provider: r.split('/')[0], model: r.split('/').slice(1).join('/') })))`);
    check(/No price known/.test(unpricedText) && names.every((n) => unpricedText.includes(n)), "models without a price are named", { unpricedText, names });
  }
  await p.shot(`${SHOTS}/as-usage-api-desk-dark.png`);

  // a price for a model Tessera has none for: typed, then "Free"
  if (rep.unpriced.length) {
    const target = rep.unpriced[0];
    const [prov, ...rest] = target.split("/");
    const label = await p.eval(`__mod('lib/store.ts').then(m => m.modelLabel({ provider: ${JSON.stringify(prov)}, model: ${JSON.stringify(rest.join("/"))} }))`);
    const cellOf = () => p.eval(`[...document.querySelectorAll('.usage-table tbody tr')].find(r => r.querySelector('.usage-model-name')?.textContent === ${JSON.stringify(label)})?.lastElementChild.textContent`);
    check((await cellOf()) === "—", "no price: a dash, not $0");
    await p.clickText(".usage-unpriced .link-btn", "Set");
    await p.waitFor(`document.querySelector('.usage-price')`, 3000).catch(async () => {
      await p.shot(`${SHOTS}/as-debug-prices.png`);
      await p.clickText(".usage-unpriced .link-btn", "Set");
    });
    await sleep(300);
    check((await p.eval(`document.querySelectorAll('.usage-price').length`)) === rep.unpriced.length && (await p.eval(`document.querySelectorAll('.usage-price:first-child input').length`)) === 2, "a row per unknown model, with input and output prices");
    await p.shot(`${SHOTS}/as-usage-prices-desk-dark.png`);
    await p.click(".usage-price:first-child input");
    await p.type("abc");
    await p.click(".usage-price:first-child .btn:not(.btn-ghost)");
    await sleep(300);
    check(/Enter both prices/.test(await p.eval(`document.querySelector('.usage-price .form-error')?.textContent ?? ''`)), "a price that isn't a number is explained, not sent");
    await p.eval(`document.querySelectorAll('.usage-price:first-child input').forEach((i, n) => { const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; set.call(i, n ? '0.6' : '0.15'); i.dispatchEvent(new Event('input', { bubbles: true })); })`);
    await p.click(".usage-price:first-child .btn:not(.btn-ghost)");
    await sleep(1000);
    rep = (await ws.rpc("get_usage", { from })).result;
    const priced = rep.by_model.find((m) => `${m.provider}/${m.model}` === target);
    check(!rep.unpriced.includes(target) && priced?.price?.input === 0.15 && priced?.price?.output === 0.6 && priced?.price_source === "custom", "a typed price is saved as the user's own", priced);
    check(/^\$|^<\$/.test((await cellOf()) ?? ""), "…and the model has a cost now", await cellOf());
    check(!(await p.eval(`!!document.querySelector('.usage-unpriced')`)), "nothing unknown left: the note goes away");
    // "Free" (a model on your own hardware)
    await ws.rpc("set_model_price", { provider: prov, model: rest.join("/") });
    await p.click(".usage-bar .icon-btn");
    await sleep(900);
    await p.clickText(".usage-unpriced .link-btn", "Set");
    await sleep(300);
    await p.click(".usage-price:first-child .btn-ghost");
    await sleep(1000);
    rep = (await ws.rpc("get_usage", { from })).result;
    check(!rep.unpriced.includes(target) && (await cellOf()) === "$0", "Free counts it at $0", { cell: await cellOf(), unpriced: rep.unpriced });
    await ws.rpc("set_model_price", { provider: prov, model: rest.join("/") });
    await p.click(".usage-bar .icon-btn");
    await sleep(800);
  }
  const local = rep.by_model.find((m) => m.billing === "local");
  if (local) {
    const cell = await p.eval(`[...document.querySelectorAll('.usage-table tbody tr')].find(r => r.querySelector('.usage-model-provider')?.textContent === ${JSON.stringify(local.provider)})?.lastElementChild.textContent`);
    check(cell === "Free" || local.totals.cost_usd != null, "a local model nobody priced is Free", cell);
  }

  // Subscription view
  await p.clickText(".usage-pricing button", "Subscription");
  await sleep(400);
  rep = (await ws.rpc("get_usage", { from })).result;
  k = await kpi();
  check(k.label === "You pay" && k.value === usd(rep.total.cost_usd ?? 0) && /plan above/.test(k.sub), "Subscription: what you pay; no plan yet, so only per-token billing", k);
  check((await p.eval(`[...document.querySelectorAll('.usage-table thead th')].pop().textContent`)) === "Billed", "the column says Billed");
  if (subModel) {
    const cell = await p.eval(`[...document.querySelectorAll('.usage-table tbody tr')].find(r => r.querySelector('.usage-model-provider')?.textContent === ${JSON.stringify(subModel.provider)})?.lastElementChild.textContent`);
    check(cell === "In plan", "a subscription model is in the plan, not billed per token", cell);
  }
  const subs = await p.eval(`[...document.querySelectorAll('.usage-sub')].map(c => ({ title: c.querySelector('.setting-title').textContent, plans: [...c.querySelectorAll('.usage-plan')].map(b => b.textContent) }))`);
  check(subs.length === rep.subscriptions.length && (rep.subscriptions.length === 0 || subs[0].plans.some((x) => /Plus/.test(x)) && subs[0].plans.some((x) => /Pro/.test(x))), "each subscription offers its common plans", subs);
  await p.shot(`${SHOTS}/as-usage-sub-noplan-desk-dark.png`);
  if (rep.subscriptions.length) {
    await p.clickText(".usage-plan", "Plus");
    await sleep(900);
    rep = (await ws.rpc("get_usage", { from })).result;
    const sub = rep.subscriptions[0];
    check(sub.plan === "Plus" && sub.monthly_usd === 20, "picking Plus saves plan and price on the server", sub);
    const use = sub.api_cost_usd ?? 0;
    const cmp = await p.eval(`[...document.querySelectorAll('.usage-compare-row')].map(r => [r.querySelector('.usage-compare-label').textContent, r.querySelector('.usage-compare-value').textContent])`);
    check(cmp.length === 2 && cmp[0][1] === usd(sub.period_usd) && cmp[1][1] === usd(use), "the plan's share of the period against the same use per token (the server's numbers)", { cmp, sub });
    const verdict = await p.eval(`document.querySelector('.usage-verdict').textContent`);
    check(use > sub.period_usd ? /You saved/.test(verdict) && verdict.includes(usd(use - sub.period_usd)) : /less/.test(verdict), "the verdict: what the plan saved (or didn't)", verdict);
    k = await kpi();
    check(k.value === usd(sub.period_usd + (rep.total.cost_usd ?? 0)) && k.sub.startsWith(`${usd(sub.period_usd)} plan`), "You pay = the plan + per-token billing", k);
    await p.shot(`${SHOTS}/as-usage-sub-plan-desk-dark.png`);
    // 7 days: the plan is spread over the days
    await p.clickText(".usage-bar .segmented button", "7 days");
    await sleep(800);
    const cmp7 = await p.eval(`document.querySelector('.usage-compare-row .usage-compare-value')?.textContent`);
    const rep7 = (await ws.rpc("get_usage", { from: await p.eval(`(() => { const d = new Date(); d.setHours(0,0,0,0); d.setDate(d.getDate() - 6); return d.getTime(); })()`) })).result;
    check(cmp7 === usd(rep7.subscriptions[0].period_usd) && rep7.subscriptions[0].period_usd < 20 * 7.5 / 30.44, "7 days: a week's share of the plan", { cmp7, period: rep7.subscriptions[0].period_usd });
    await p.clickText(".usage-bar .segmented button", "All");
    await sleep(800);
    const cmpAll = await p.eval(`document.querySelector('.usage-compare-row .usage-compare-value')?.textContent`);
    const repAll = (await ws.rpc("get_usage", { from: 0 })).result;
    const firstDay = repAll.by_day[0]?.day;
    const expectAll = await p.eval(`(() => { const [y, m, d] = ${JSON.stringify(firstDay)}.split('-').map(Number); return 20 * (Date.now() - new Date(y, m - 1, d).getTime()) / (30.44 * 864e5); })()`);
    check(cmpAll === usd(expectAll), "All: the plan since the first request (not since 1970)", { cmpAll, expectAll, server: repAll.subscriptions[0].period_usd });
    await p.clickText(".usage-bar .segmented button", "30 days");
    await sleep(800);
    // a plan of one's own
    await p.clickText(".usage-sub .btn", "Change");
    await sleep(200);
    await p.clickText(".usage-plan", "Other");
    await sleep(200);
    await p.eval(`(() => { const [n, m] = document.querySelectorAll('.usage-plan-form input'); const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set; set.call(n, 'Team'); n.dispatchEvent(new Event('input', { bubbles: true })); set.call(m, '$25'); m.dispatchEvent(new Event('input', { bubbles: true })); })()`);
    await p.click(".usage-plan-form button[type=submit]");
    await sleep(900);
    rep = (await ws.rpc("get_usage", { from })).result;
    check(rep.subscriptions[0].plan === "Team" && rep.subscriptions[0].monthly_usd === 25, "a plan of one's own, with its price", rep.subscriptions[0]);
    check(/Team · \$25\/month/.test(await p.eval(`document.querySelector('.usage-sub .setting-help').textContent`)), "…shown under the subscription's name");
    // the view is remembered on this device
    await p.reload(1500);
    await p.waitFor(`document.querySelector('.app')`, 8000);
    await open("usage");
    await p.waitFor(`document.querySelector('.usage .kpi')`, 8000);
    check((await p.eval(`document.querySelector('.usage-pricing [aria-checked="true"]').textContent`)) === "Subscription", "the chosen view is remembered");
    await p.clickText(".usage-sub .btn", "Change");
    await sleep(200);
    await p.click(".usage-plan-clear");
    await sleep(900);
    rep = (await ws.rpc("get_usage", { from })).result;
    check(rep.subscriptions[0].monthly_usd == null && (await p.eval(`!!document.querySelector('.usage-plans')`)), "Forget this plan: back to choosing one", rep.subscriptions[0]);
  }
  // Claude Code (the user's own, signed in by itself): its provider card and its plan
  if (!REAL) {
    await fetch(MOCK + "/__mock/claude-code?state=signed_out", { method: "POST" });
    await sleep(500);
    await open("providers");
    await p.waitFor(`[...document.querySelectorAll('.provider')].some(c => c.textContent.includes('Your Claude Code'))`, 8000);
    await sleep(500);
    const card = () => p.eval(`(() => { const c = [...document.querySelectorAll('.provider')].find(c => c.textContent.includes('Your Claude Code')); return { signIn: [...c.querySelectorAll('button')].some(b => b.textContent.trim() === 'Sign in'), hint: c.querySelector('.provider-hint')?.textContent ?? null, check: [...c.querySelectorAll('button')].some(b => b.textContent.includes('Check again')), pill: c.querySelector('.auth-pill').textContent }; })()`);
    let cc = await card();
    check(!cc.signIn && /claude/.test(cc.hint ?? "") && cc.check, "Claude Code: no Tessera sign-in; its hint says what to do, and it can be checked again", cc);
    await p.shot(`${SHOTS}/as-providers-claude-code-desk-dark.png`);
    await fetch(MOCK + "/__mock/claude-code?state=ready", { method: "POST" });
    // The server says so by itself (providers_changed), which can take the button away first; else Check again asks.
    await p.eval(`[...document.querySelectorAll('.provider button')].find((b) => b.textContent.includes('Check again'))?.click()`);
    await p.waitFor(`[...document.querySelectorAll('.provider')].some((c) => c.textContent.includes('Your Claude Code') && /Signed in/.test(c.querySelector('.auth-pill')?.textContent ?? ''))`, 6000).catch(() => {});
    await sleep(300);
    cc = await card();
    check(cc.hint === null && /Signed in/.test(cc.pill), "…once signed in there: no hint, signed in", cc);
    await open("usage");
    await p.waitFor(`document.querySelector('.usage .kpi')`, 8000);
    await sleep(500);
    const claude = await p.eval(`(() => { const c = [...document.querySelectorAll('.usage-sub')].find(c => /Claude/.test(c.querySelector('.setting-title').textContent)); return c && [...c.querySelectorAll('.usage-plan')].map(b => b.textContent); })()`);
    check(!!claude && claude.some((x) => /Max 5×/.test(x)) && claude.some((x) => /Max 20×/.test(x)), "a Claude plan through Claude Code: Pro and Max to pick from", claude);
    await p.shot(`${SHOTS}/as-usage-sub-claude-desk-dark.png`);
    await fetch(MOCK + "/__mock/claude-code?state=missing", { method: "POST" });
  }
  check(p.errors.length === 0, "usage: no errors in the page", p.errors);
  // Usage is a section of its own (batch 17): no crumb, and Escape closes Settings.
  check((await p.eval(`document.querySelector('.settings-head h3')?.textContent`)) === "Usage" && !(await p.eval(`!!document.querySelector('.settings-crumb')`)), "Usage is a section of its own");
  await p.key("Escape", "Escape", 0, 27);
  await sleep(300);
  check(!(await p.eval(`!!document.querySelector('.settings-layout')`)), "…and Escape closes Settings");

  // ── a secret typed into a chat ──
  const ses = (await ws.rpc("create_session", { title: "Key check", cwd: process.env.FOLDER ?? null, model: null })).result;
  await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession(${JSON.stringify(ses.id)}))`);
  await sleep(800);
  // typed into the composer like a person would, then Enter
  await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
  await p.type(`use my OpenAI key ${KEY} for the script`);
  await sleep(500); // drafts are kept for 400 ms after typing
  const draftBefore = await p.eval(`localStorage.getItem('tessera.drafts.v2') ?? ''`);
  await p.key("Enter", "Enter", 0, 13);
  await p.waitFor(`document.querySelector('.secret-saved')`, 8000).catch(() => {});
  await sleep(900);
  const notice = await p.eval(`(() => { const n = document.querySelector('.secret-saved'); return n && { label: n.querySelector('.secret-saved-label').textContent, names: [...n.querySelectorAll('.var-chip')].map(c => c.textContent), manage: !!n.querySelector('.link-btn') }; })()`);
  check(!!notice && notice.label === "Saved to Passwords & keys" && notice.names.some((x) => /^\$[A-Z_]+/.test(x)) && notice.manage, "a calm notice: “Saved to Passwords & keys”, its $NAME, and Manage", notice);
  const bubble = await p.eval(`[...document.querySelectorAll('.msg-user .bubble-text')].map(b => b.textContent).pop()`);
  check(!!bubble && !bubble.includes(KEY) && /\$[A-Z_]+/.test(bubble), "the message keeps only the name", bubble);
  check(!(await p.eval(`document.body.innerText.includes(${JSON.stringify(KEY)}) || document.body.innerHTML.includes(${JSON.stringify(KEY)})`)), "the key is nowhere on the page");
  await sleep(500);
  check(draftBefore.includes(KEY) && !(await p.eval(`['tessera.drafts', 'tessera.drafts.v2'].some(k => (localStorage.getItem(k) ?? '').includes(${JSON.stringify(KEY)}))`)), "the unsent draft held it; once sent, this device keeps no copy");
  const stored = (await ws.rpc("get_session", { session_id: ses.id })).result;
  check(!JSON.stringify(stored).includes(KEY), "the server's copy of the chat doesn't have it either");
  await p.shot(`${SHOTS}/as-secret-notice-desk-dark.png`);
  await p.click(".secret-saved .link-btn");
  await p.waitFor(`document.querySelector('.secret-item')`, 8000).catch(() => {});
  await sleep(300);
  const listed = await p.eval(`[...document.querySelectorAll('.secret-item .var-chip')].map(c => c.textContent)`);
  if (!listed.length) {
    await p.shot(`${SHOTS}/as-debug-manage.png`);
    console.log(await p.eval(`({ tab: document.querySelector('.settings-tab.active')?.textContent, body: document.querySelector('.settings-body')?.textContent.slice(0, 200), modal: !!document.querySelector('.settings') })`));
  }
  check((await p.eval(`document.querySelector('.settings-head h3')?.textContent.trim()`)) === "Passwords & keys" && notice && listed.includes(notice.names[0]), "Manage opens Passwords & keys, where it is listed", listed);
  await p.key("Escape", "Escape", 0, 27);
  await sleep(300);
  if (!REAL) {
    // a login only the agent recognises: saved by its tool, then gone from what is shown
    const ses2 = (await ws.rpc("create_session", { title: "Login check" })).result;
    await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession(${JSON.stringify(ses2.id)}))`);
    await sleep(700);
    await ws.rpc("send", { session_id: ses2.id, content: [{ type: "text", text: "for the library site my login is ann / correct-horse-battery" }], delivery: "auto" });
    await sleep(900);
    check(await p.eval(`[...document.querySelectorAll('.msg-user .bubble-text')].some(b => b.textContent.includes('correct-horse-battery'))`), "(before) the login is in the message as typed");
    await ws.rpc("mock_secret_notice", { session_id: ses2.id });
    await sleep(700);
    const agent = await p.eval(`(() => { const n = [...document.querySelectorAll('.secret-saved')].pop(); return { label: n.querySelector('.secret-saved-label').textContent, what: n.querySelector('.secret-saved-what')?.textContent, vars: [...n.querySelectorAll('.var-chip')].map(c => c.textContent) }; })()`);
    check(agent.label === "Saved to Passwords & keys" && agent.what === "Login" && agent.vars.includes("$LIBRARY_LOGIN_PASSWORD"), "a login the agent saved: “Saved to Passwords & keys · Login”, with its variables", agent);
    // (The mock's live reasoning quotes the message; the server rewrites stored replies when the turn ends.)
    check(!(await p.eval(`[...document.querySelectorAll('.msg-user .bubble-text')].some(b => b.textContent.includes('correct-horse-battery'))`)) && (await p.eval(`[...document.querySelectorAll('.msg-user .bubble-text')].some(b => b.textContent.includes('$LIBRARY_LOGIN_PASSWORD'))`)), "the message on screen is rewritten at once (entries_changed): no password left");
  }
  // the server's wording, and only it
  const parsed = await p.eval(`__mod('components/chat/SecretNotice.tsx').then(m => [
    m.savedSecrets({ level: 'info', text: 'Saved to Passwords & keys: OPENAI_API_KEY (OpenAI API key). It was taken out of your message; the agent uses it as $OPENAI_API_KEY without seeing the value.' }),
    m.savedSecrets({ level: 'info', text: 'Saved to Passwords & keys: GITHUB_TOKEN (GitHub token), NPM_TOKEN (npm token). They were taken out of your message; the agent uses them as $GITHUB_TOKEN, $NPM_TOKEN without seeing the value.' }),
    m.savedSecrets({ level: 'info', text: 'Saved to Secrets & logins: ANN (login). It was taken out of the conversation; the agent uses it as $ANN_PASSWORD, $ANN_USERNAME without seeing the value.' }),
    m.savedSecrets({ level: 'info', text: 'Your message held the stored secret $GITHUB_PASSWORD; the agent got the name instead.' }),
    m.savedSecrets({ level: 'info', text: 'Learned from this conversation — note: x' }),
    m.savedSecrets({ level: 'warning', text: "Took OpenAI API key out of your message: private chats don't keep secrets. Add it in Settings → Passwords & keys to use it." }),
  ])`);
  check(parsed[0]?.names[0] === "OPENAI_API_KEY" && parsed[0].what === "OpenAI API key" && parsed[0].vars[0] === "OPENAI_API_KEY" && !parsed[0].byAgent, "one saved key is read from the server's notice", parsed[0]);
  check(parsed[1]?.names.length === 2 && parsed[1].vars.join() === "GITHUB_TOKEN,NPM_TOKEN", "two at once", parsed[1]);
  check(parsed[2]?.kind === "login" && parsed[2].byAgent && parsed[2].vars.length === 2 && parsed[2].what === "", "a login the agent saved", parsed[2]);
  check(parsed[3]?.how === "known" && parsed[3].vars[0] === "GITHUB_PASSWORD", "a stored value typed again", parsed[3]);
  check(parsed[4] === null && parsed[5] === null, "other notices (and the private-chat warning) stay plain notices", parsed.slice(4));
  check(p.errors.length === 0, "chat: no errors in the page", p.errors);

  // ── themes and widths ──
  const shots = [];
  for (const theme of ["light", "dark"]) {
    await p.theme(theme);
    await open("commands");
    await p.waitFor(`document.querySelector('.sec .sec-status')`).catch(async (e) => {
      await p.shot(`${SHOTS}/as-debug-${theme}.png`);
      console.log(await p.eval(`({ tab: document.querySelector('.settings-tab.active')?.textContent, body: document.querySelector('.settings-body')?.textContent.slice(0, 300), modal: !!document.querySelector('.modal') })`));
      throw e;
    });
    await p.shot(`${SHOTS}/as-security-desk-${theme}.png`);
    await open("usage");
    await p.waitFor(`document.querySelector('.usage .kpi')`, 8000);
    await sleep(400);
    await p.shot(`${SHOTS}/as-usage-desk-${theme}.png`);
    await p.key("Escape", "Escape", 0, 27);
    await sleep(300);
    await p.shot(`${SHOTS}/as-secret-notice-desk-${theme}.png`);
    shots.push(theme);
  }
  await p.close();

  for (const [name, w, h] of [["phone", 412, 915], ["fold", 884, 1104]]) {
    const m = await device();
    await m.viewport(w, h, true, 2);
    await signIn(m, `Settings ${name}`);
    for (const theme of ["dark", "light"]) {
      await m.theme(theme);
      for (const tab of ["safety", "commands", "usage"]) {
        await m.eval(`__mod('lib/ui.ts').then(u => { u.setDrawerOpen(false); u.openSettings(${JSON.stringify(tab)}); })`);
        await sleep(1100);
        await m.shot(`${SHOTS}/as-${tab}-${name}-${theme}.png`);
        const overflow = await m.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth - b.clientWidth; })()`);
        check(overflow <= 1, `${name} ${theme}: ${tab} fits the width`, overflow);
        if (tab === "commands") {
          const mode = await m.eval(`(() => { const r = [...document.querySelectorAll('#sec-sandbox .sec-mode .perm-card')].map(c => c.getBoundingClientRect()); return { n: r.length, h: Math.min(...r.map(x => x.height)) }; })()`);
          check(mode.n === 3 && mode.h >= 44, `${name}: the sandbox choices are big enough to tap`, mode);
        }
        if (tab === "usage" && theme === "dark") {
          const b = await m.eval(`(() => { const e = [...document.querySelectorAll('.usage-pricing button')].find(x => x.textContent === 'Subscription'); const r = e.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; })()`);
          await m.tap(b[0], b[1]);
          await sleep(500);
          await m.shot(`${SHOTS}/as-usage-sub-${name}-${theme}.png`);
          check((await m.eval(`document.querySelector('.usage-pricing [aria-checked="true"]').textContent`)) === "Subscription", `${name}: tapping Subscription switches the view`);
          const over2 = await m.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth - b.clientWidth; })()`);
          check(over2 <= 1, `${name}: the subscription view fits too`, over2);
          await m.tap(...(await m.eval(`(() => { const e = [...document.querySelectorAll('.usage-pricing button')].find(x => x.textContent === 'API prices'); const r = e.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; })()`)));
          await sleep(300);
        }
      }
      await m.eval(`__mod('lib/ui.ts').then(u => u.setSettingsTab(null))`);
      await m.eval(`__mod('lib/workspace.ts').then(w => w.openSession(${JSON.stringify(ses.id)}))`);
      await sleep(900);
      await m.shot(`${SHOTS}/as-secret-notice-${name}-${theme}.png`);
      const fits = await m.eval(`(() => { const n = document.querySelector('.secret-saved'); if (!n) return null; const r = n.getBoundingClientRect(); return r.right <= innerWidth + 1 && r.left >= -1; })()`);
      check(fits === true, `${name} ${theme}: the secret notice fits the screen`, fits);
    }
    check(m.errors.length === 0, `${name}: no errors`, m.errors);
    await m.close();
  }
  ws.close();
  finish();
});
