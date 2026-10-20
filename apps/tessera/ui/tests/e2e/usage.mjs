// Settings → Usage: subscriptions first, each with its 5-hour and weekly limits (how much is used,
// when it resets, how fresh that is), then the period's totals (tokens in, out, cached, all), then
// the chart. A day tapped or clicked in the chart keeps its details open under it, its notch under
// the bar, until another bar or something else is tapped (a scroll keeps it); the arrow keys move
// it and Escape puts it away; hovering with a mouse still shows a quick look. Desktop, the folded
// phone (344 px), the unfolded Fold, light and dark.
import { MOCK, SHOTS, check, device, finish, run, signIn, sleep, wsClient } from "./cdp.mjs";

const MIN = 60_000;
const DAY = 86_400_000;

run(async () => {
  const ws = await wsClient("usage script");
  const t = Date.now();
  // What each provider last reported: ChatGPT both windows; Claude both, the weekly one close, and
  // Opus's own weekly limit reached (Claude gives only a status for that one).
  const setLimits = async () => {
    const now = Date.now();
    await ws.rpc("mock_plan_limits", {
      provider: "openai-codex",
      limits: [
        { window: "five_hour", label: "5-hour limit", used_percent: 42, resets_at: now + 130.5 * MIN, updated_at: now - 12 * MIN },
        { window: "weekly", label: "Weekly limit", used_percent: 18, resets_at: now + 3 * DAY + 5 * 3_600_000, updated_at: now - 12 * MIN },
      ],
    });
    await ws.rpc("mock_plan_limits", {
      provider: "claude-code",
      limits: [
        { window: "five_hour", label: "5-hour limit", used_percent: 64, status: "ok", resets_at: now + 47.5 * MIN, updated_at: now - 20_000 },
        { window: "weekly", label: "Weekly limit", used_percent: 86, resets_at: now + 2 * DAY, updated_at: now - 20_000 },
        { window: "weekly_opus", label: "Weekly limit, Opus", status: "reached", resets_at: now + 20 * 3_600_000, updated_at: now - 20_000 },
      ],
    });
  };
  await setLimits();
  const from = (n) => {
    const d = new Date();
    d.setHours(0, 0, 0, 0);
    d.setDate(d.getDate() - n);
    return d.getTime();
  };
  const want = (await ws.rpc("get_usage", { from: from(29) })).result;
  check(want.subscriptions.map((s) => s.provider).join() === "openai-codex,claude-code", "the mock lists Claude by its current limits alone (as the server does)", want.subscriptions);

  const p = await device();
  await p.viewport(1280, 900);
  await signIn(p, "Usage desktop");
  await p.theme("dark");
  await p.eval(`localStorage.removeItem('tessera.usage.pricing')`);
  const open = async (m = p) => {
    await m.eval(`__mod('lib/ui.ts').then(u => { u.setDrawerOpen?.(false); u.openSettings('usage'); })`);
    await m.waitFor(`document.querySelector('.usage .kpi')`, 8000);
    await sleep(400);
  };
  await open();

  // ── subscriptions first ──
  const order = await p.eval(`(() => { const s = document.querySelector('.usage-subs'), k = document.querySelector('.usage-kpis'), c = document.querySelector('.usage-plot'); return !!(s && k && c && (s.compareDocumentPosition(k) & 4) && (k.compareDocumentPosition(c) & 4)); })()`);
  check(order, "subscriptions come first, then the totals, then the chart");
  const cards = () =>
    p.eval(`[...document.querySelectorAll('.usage-sub')].map(c => ({
      title: c.querySelector('.setting-title').textContent,
      meters: [...c.querySelectorAll('.usage-limit')].map(l => ({
        name: l.querySelector('.usage-limit-name').textContent, value: l.querySelector('.usage-limit-value').textContent,
        reset: l.querySelector('.usage-limit-reset')?.textContent ?? '', cls: l.className, fill: l.querySelector('.usage-limit-fill').style.width,
        now: l.querySelector('[role=meter]').getAttribute('aria-valuenow'), text: l.querySelector('[role=meter]').getAttribute('aria-valuetext') })),
      asof: c.querySelector('.usage-limits-asof')?.textContent ?? '', none: c.querySelector('.usage-limits-none')?.textContent ?? '',
      plans: c.querySelectorAll('.usage-plan').length }))`);
  let subs = await cards();
  check(subs.length === 2 && /ChatGPT/.test(subs[0].title) && /Claude/.test(subs[1].title), "a card for each subscription", subs.map((s) => s.title));
  const gpt = subs[0];
  check(gpt.meters.map((m) => m.name).join() === "5-hour limit,Weekly limit", "ChatGPT: a 5-hour and a weekly meter", gpt.meters);
  check(gpt.meters[0].value === "42% used" && gpt.meters[0].fill === "42%" && gpt.meters[0].now === "42", "…how much is used, as a meter", gpt.meters[0]);
  check(/^Resets in 2 h 1?\d min$/.test(gpt.meters[0].reset), "…when the 5-hour window resets, as a countdown", gpt.meters[0].reset);
  check(/^Resets \S+ \d/.test(gpt.meters[1].reset) && !/in /.test(gpt.meters[1].reset), "…and the weekly one by its day and time", gpt.meters[1].reset);
  check(/Updated 1[23]m ago/.test(gpt.asof), "…and how fresh the numbers are", gpt.asof);
  check(/42% used, Resets in/.test(gpt.meters[0].text ?? ""), "a screen reader hears the share and the reset", gpt.meters[0].text);
  check(gpt.plans === 0, "API prices view: no plan pickers in the cards");
  const claude = subs[1];
  check(claude.meters.length === 3 && claude.meters[2].name === "Weekly limit, Opus", "Claude: a model's own weekly limit joins the two", claude.meters.map((m) => m.name));
  check(/\bwarn\b/.test(claude.meters[1].cls) && claude.meters[1].value === "86% used", "close to a limit: marked, in words too", claude.meters[1]);
  check(/\bfull\b/.test(claude.meters[2].cls) && claude.meters[2].value === "Reached" && claude.meters[2].fill === "100%", "a reached limit without a share: full and Reached", claude.meters[2]);
  check(/Resets in 4[78] min/.test(claude.meters[0].reset), "under an hour: minutes", claude.meters[0].reset);
  check(/Updated just now/.test(claude.asof), "just reported: just now", claude.asof);
  const icons = await p.eval(`[...document.querySelectorAll('.usage-limit')].map(l => !!l.querySelector('.usage-limit-value .icon'))`);
  check(icons.join() === "false,false,false,true,true", "a mark beside close and reached limits only (never colour alone)", icons);
  await p.shot(`${SHOTS}/usage-desk-dark.png`);

  // ── totals ──
  const kpis = await p.eval(`[...document.querySelectorAll('.usage .kpi')].map(k => [k.querySelector('.kpi-label').textContent, k.querySelector('.kpi-value').textContent, k.title])`);
  // Not the app's code: three digits at most, k M B T, and a number that rounds up to 1,000 of its unit moves to the next.
  const fmt = (n) => {
    if (n < 1000) return String(n);
    const units = ["k", "M", "B", "T"];
    let v = n, i = 0;
    while (v >= 1000 && i < units.length) {
      v /= 1000;
      i++;
    }
    let s = Number(v.toPrecision(3));
    if (s >= 1000 && i < units.length) {
      s = 1;
      i++;
    }
    return `${s}${units[i - 1]}`;
  };
  const tot = want.total;
  const all = tot.input + tot.output + tot.cache_read + tot.cache_write;
  check(kpis.map((k) => k[0]).slice(0, 5).join() === "Requests,Tokens in,Tokens out,Cache read · write,All tokens", "totals: requests, tokens in, out, cached and all", kpis.map((k) => k[0]));
  check(kpis[1][1] === fmt(tot.input) && kpis[2][1] === fmt(tot.output) && kpis[4][1] === fmt(all), "…the server's numbers", { kpis, tot });
  check(kpis[4][2].replace(/\D/g, "") === String(all), "…exact on hover", kpis[4][2]);
  check(all >= 1e9 && /^\d(\.\d+)?B$/.test(kpis[4][1]) && /^\d+(\.\d+)?B · /.test(kpis[3][1]), "a billion tokens or more reads in billions (1.5B, not 1500M)", { all, kpis: kpis.slice(1, 5) });
  const page = await p.eval(`document.querySelector('.usage').innerText`);
  check(!/\b\d{4,}[kMBT]\b/.test(page), "…and no number on the page is written like 1000M or 2500M", page.match(/\b\d{4,}[kMBT]\b/g));
  const edges = [[0, "0"], [999, "999"], [1000, "1k"], [1005, "1.01k"], [1234, "1.23k"], [9995, "10k"], [42300, "42.3k"], [99949, "99.9k"], [99950, "100k"], [262144, "262k"],
    [999499, "999k"], [999500, "1M"], [999999, "1M"], [1000000, "1M"], [1048576, "1.05M"], [15300000, "15.3M"], [123456789, "123M"], [999499999, "999M"], [999500000, "1B"],
    [1499999999, "1.5B"], [1500000000, "1.5B"], [2500000000, "2.5B"], [15300000000, "15.3B"], [999999999999, "1T"], [2500000000000, "2.5T"]];
  const got = await p.eval(`__mod('lib/util.ts').then((u) => ${JSON.stringify(edges.map((e) => e[0]))}.map((n) => u.fmtTokens(n)))`);
  check(JSON.stringify(got) === JSON.stringify(edges.map((e) => e[1])), "the number format: at most three digits, k M B T, rounding up carries to the next unit (999,500 is 1M; 1,499,999,999 is 1.5B; 2.5e9 is 2.5B)", edges.filter((e, i) => got[i] !== e[1]).map((e) => [e[0], e[1], got[edges.indexOf(e)]]));
  check(kpis[5][0] === "At API prices" && kpis[5][1].startsWith("$"), "…and the cost last", kpis[5]);
  const kpiRows = await p.eval(`new Set([...document.querySelectorAll('.usage .kpi')].map(k => Math.round(k.getBoundingClientRect().top))).size`);
  check(kpiRows === 2, "desktop: the totals in two even rows", kpiRows);

  // ── the chart: hover, click to keep a day's details ──
  const hits = await p.eval(`document.querySelectorAll('.usage-hit').length`);
  check(hits === 30, "30 bars", hits);
  const hitBox = (i, m = p) => m.eval(`(() => { const h = document.querySelectorAll('.usage-hit')[${i}]; h.scrollIntoView({ block: 'nearest' }); const r = h.getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height - 12, cx: r.x + r.width / 2 }; })()`);
  const detail = (m = p) =>
    m.eval(`(() => { const d = document.querySelector('.usage-detail'); if (!d) return null; const plot = document.querySelector('.usage-plot').getBoundingClientRect();
      return { day: d.querySelector('.usage-detail-day').textContent, stats: [...d.querySelectorAll('.usage-detail-stats > div')].map(s => [s.querySelector('dt').textContent, s.querySelector('dd').textContent]),
        models: [...d.querySelectorAll('.usage-detail-models li')].map(l => l.textContent), shares: [...d.querySelectorAll('.usage-detail-share')].map(e => parseInt(e.textContent)), notch: plot.x + parseFloat(d.style.getPropertyValue('--notch')),
        dim: document.querySelectorAll('.usage-col.dim').length, tip: !!document.querySelector('.usage-tip') }; })()`);
  // A bar 20 days in, or the nearest one with use (the mock leaves about one day in eight quiet).
  const dayKey = (i) => { const k = new Date(from(29)); k.setDate(k.getDate() + i); return `${k.getFullYear()}-${String(k.getMonth() + 1).padStart(2, "0")}-${String(k.getDate()).padStart(2, "0")}`; };
  const used = (i) => want.by_day.find((x) => x.day === dayKey(i));
  const at = [20, 19, 21, 18, 22, 17, 23, 16].find((i) => used(i)) ?? 20;
  let b = await hitBox(at);
  await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: b.x, y: b.y });
  await sleep(200);
  const tip = await p.eval(`document.querySelector('.usage-tip')?.textContent ?? ''`);
  check(/Click for details/.test(tip) && (await p.eval(`document.querySelectorAll('.usage-col.dim').length`)) === 29, "hover: a quick look that says a click shows more", tip);
  await p.shot(`${SHOTS}/usage-hover-desk-dark.png`);
  await p.mouse(b.x, b.y);
  await sleep(200);
  let d = await detail();
  const day20 = used(at);
  check(!!d && !d.tip, "click: the day's details open under the chart (no quick look over them)", d);
  check(!!d && !!day20 && d.stats[0][1] === day20.totals.requests.toLocaleString() && d.stats[1][1] === fmt(day20.totals.input), "…with its requests and tokens in and out", { stats: d?.stats, day: day20?.totals });
  check(!!d && d.models.length >= 2 && d.models.every((m) => /%$/.test(m)), "…and each model's share", d?.models);
  check(!!d && Math.abs(d.notch - b.cx) < 2, "…its notch under the bar", { notch: d?.notch, bar: b.cx });
  await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x: 5, y: 5 });
  await sleep(300);
  d = await detail();
  check(!!d && d.dim === 29, "the mouse leaving keeps them, the other days quieter", d?.dim);
  check(!!d && d.shares.every((v, i) => !i || v <= d.shares[i - 1]), "…biggest model first", d?.models);
  await p.eval(`document.querySelector('.usage-detail').scrollIntoView({ block: 'center' })`);
  await p.shot(`${SHOTS}/usage-detail-desk-dark.png`);
  const b2 = await hitBox(25);
  await p.mouse(b2.x, b2.y);
  await sleep(200);
  const d2 = await detail();
  check(!!d2 && d2.day !== d.day && Math.abs(d2.notch - b2.cx) < 2, "another bar: its details, the notch follows", { before: d?.day, after: d2?.day });
  await p.click(".usage .kpi");
  await sleep(200);
  check(!(await detail()), "a click elsewhere puts them away");
  // keyboard
  await p.eval(`document.querySelector('.usage-plot svg').focus()`);
  await p.key("ArrowLeft", "ArrowLeft", 0, 37);
  await sleep(100);
  d = await detail();
  check(!!d, "the arrow keys open a day's details", d?.day);
  const lastDay = d?.day;
  await p.key("ArrowLeft", "ArrowLeft", 0, 37);
  await sleep(100);
  check((await detail())?.day !== lastDay, "…and move them", (await detail())?.day);
  await p.key("Escape", "Escape", 0, 27);
  await sleep(200);
  check(!(await detail()) && (await p.eval(`!!document.querySelector('.usage')`)), "Escape puts them away and keeps Settings open");
  // a new period: nothing picked
  b = await hitBox(3);
  await p.mouse(b.x, b.y);
  await p.clickText(".usage-bar .segmented button", "7 days");
  await sleep(700);
  check(!(await detail()) && (await p.eval(`document.querySelectorAll('.usage-hit').length`)) === 7, "another period: other bars, nothing picked");
  await p.clickText(".usage-bar .segmented button", "30 days");
  await sleep(700);

  // ── limits that reset since, and none yet ──
  await ws.rpc("mock_plan_limits", {
    provider: "openai-codex",
    limits: [{ window: "five_hour", label: "5-hour limit", used_percent: 97, resets_at: Date.now() - 2 * 3_600_000, updated_at: Date.now() - 3 * 3_600_000 }],
  });
  await ws.rpc("mock_plan_limits", { provider: "claude-code", limits: [] });
  await fetch(MOCK + "/__mock/claude-code?state=ready", { method: "POST" });
  await p.click(".usage-bar .icon-btn");
  await sleep(900);
  subs = await cards();
  check(subs[0].meters[0].value === "Started over" && /^Reset 2h ago$/.test(subs[0].meters[0].reset) && subs[0].meters[0].fill === "0%", "a window that started over since: said so, not the old share", subs[0].meters[0]);
  check(/after its next reply/.test(subs[1]?.none ?? ""), "signed in, nothing reported yet: when the limits come", subs[1]);
  await p.shot(`${SHOTS}/usage-limits-reset-desk-dark.png`);
  await setLimits();
  await p.click(".usage-bar .icon-btn");
  await sleep(900);

  // ── Subscription view: the plan under the limits ──
  await p.clickText(".usage-pricing button", "Subscription");
  await sleep(900);
  subs = await cards();
  check(subs.length === 2 && subs.every((s) => s.meters.length >= 2 && s.plans >= 3), "Subscription view: the same cards, the plans under the limits", subs.map((s) => [s.meters.length, s.plans]));
  await p.shot(`${SHOTS}/usage-sub-desk-dark.png`);
  await p.clickText(".usage-pricing button", "API prices");
  await sleep(500);
  await p.theme("light");
  await sleep(300);
  await p.shot(`${SHOTS}/usage-desk-light.png`);
  b = await hitBox(22);
  await p.mouse(b.x, b.y);
  await sleep(200);
  await p.eval(`document.querySelector('.usage-detail').scrollIntoView({ block: 'center' })`);
  await p.shot(`${SHOTS}/usage-detail-desk-light.png`);
  check(p.errors.length === 0, "desktop: no errors", p.errors);
  await p.close();

  // ── the folded phone, the unfolded Fold ──
  for (const [name, w, h] of [["phone", 344, 882], ["fold", 884, 1104]]) {
    const m = await device();
    await m.viewport(w, h, true, 2);
    await signIn(m, `Usage ${name}`);
    for (const theme of ["dark", "light"]) {
      await m.theme(theme);
      await open(m);
      await m.eval(`document.querySelector('.settings-body').scrollTop = 0`);
      await sleep(200);
      await m.shot(`${SHOTS}/usage-${name}-${theme}.png`);
      const over = await m.eval(`(() => { const b = document.querySelector('.settings-body'); return b.scrollWidth - b.clientWidth; })()`);
      check(over <= 1, `${name} ${theme}: the page fits the width`, over);
      const clipped = await m.eval(`[...document.querySelectorAll('.usage-limit-line, .kpi, .usage-detail-stats > div')].filter(e => e.scrollWidth > e.clientWidth + 1).map(e => e.textContent)`);
      check(clipped.length === 0, `${name} ${theme}: nothing cut off in the meters and totals`, clipped);
      if (theme === "light") continue;
      if (name === "phone") {
        const two = await m.eval(`(() => { const r = [...document.querySelectorAll('.usage-sub')][0].querySelectorAll('.usage-limit'); return r.length === 2 && r[1].getBoundingClientRect().top > r[0].getBoundingClientRect().bottom; })()`);
        check(two, "phone: the meters stack");
        const cols = await m.eval(`new Set([...document.querySelectorAll('.usage .kpi')].map(k => Math.round(k.getBoundingClientRect().left))).size`);
        check(cols === 2, "phone: totals two by two", cols);
      }
      // tap a bar: its details stay
      let tb = await hitBox(18, m);
      await m.tap(tb.x, tb.y);
      await sleep(900);
      let dd = await detail(m);
      check(!!dd && dd.dim === 29, `${name}: a tapped bar keeps its details open`, dd);
      check(!!dd && Math.abs(dd.notch - tb.cx) < 2, `${name}: the notch under the tapped bar`, { notch: dd?.notch, bar: tb.cx });
      await m.eval(`document.querySelector('.usage-detail').scrollIntoView({ block: 'nearest' })`);
      await sleep(200);
      await m.shot(`${SHOTS}/usage-detail-${name}-${theme}.png`);
      const fits = await m.eval(`(() => { const r = document.querySelector('.usage-detail').getBoundingClientRect(); return r.left >= 0 && r.right <= innerWidth; })()`);
      check(fits, `${name}: the details fit the screen`);
      // scrolling (a swipe) keeps them
      const body = await m.eval(`(() => { const r = document.querySelector('.settings-body').getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height * 0.7]; })()`);
      await m.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: body[0], y: body[1] }] });
      for (let k = 1; k <= 8; k++) {
        await m.send("Input.dispatchTouchEvent", { type: "touchMove", touchPoints: [{ x: body[0], y: body[1] - k * 20 }] });
        await sleep(16);
      }
      await m.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
      await sleep(500);
      check(!!(await detail(m)), `${name}: a swipe keeps them`);
      // another bar, then elsewhere
      tb = await hitBox(26, m);
      await m.tap(tb.x, tb.y);
      await sleep(400);
      const dd2 = await detail(m);
      check(!!dd2 && dd2.day !== dd.day && Math.abs(dd2.notch - tb.cx) < 2, `${name}: another bar moves them there`, { a: dd?.day, b: dd2?.day });
      const k = await m.eval(`(() => { const e = document.querySelector('.usage .kpi'); e.scrollIntoView({ block: 'center' }); const r = e.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; })()`);
      await m.tap(k[0], k[1]);
      await sleep(400);
      check(!(await detail(m)), `${name}: a tap elsewhere puts them away`);
    }
    check(m.errors.length === 0, `${name}: no errors`, m.errors);
    await m.close();
  }
  ws.close();
  check(Date.now() - t < 600_000, "ran in time");
  finish();
});
