// Usage counts days in the device's own time zone (batch 17, the user's "October 6th hasn't
// happened yet": the server bucketed by its own zone, UTC, while they are in Chicago). The app
// sends its zone (`utc_offsets`, summer time included) with `get_usage`; a request made in the
// evening there is on that evening's day, and the chart never shows a day that hasn't come yet.
// Across midnight and both summer-time changes; desktop and the folded phone, light and dark.
import { MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const DAY = 86_400_000;

run(async () => {
  await mock("/__mock/reset");
  const ws = await wsClient("usage days");
  const p = await device();
  await p.send("Emulation.setTimezoneOverride", { timezoneId: "America/Chicago" });
  await p.viewport(1280, 900);
  await signIn(p, "Chicago desktop");
  await p.theme("dark");
  const util = (js) => p.eval(`__mod('lib/util.ts').then((m) => ${js})`);

  // ── the zone as the app describes it ──
  check((await p.eval(`Intl.DateTimeFormat().resolvedOptions().timeZone`)) === "America/Chicago", "the page runs in Chicago's time zone");
  const fall = await util(`m.utcOffsets(Date.UTC(2026, 9, 25), Date.UTC(2026, 10, 8))`);
  check(JSON.stringify(fall) === JSON.stringify([{ since: 0, minutes: -300 }, { since: Date.UTC(2026, 10, 1, 7), minutes: -360 }]), "end of summer time: -5 h, then -6 h from 2 a.m. on Nov 1 (07:00 UTC) exactly", fall);
  const spring = await util(`m.utcOffsets(Date.UTC(2026, 2, 1), Date.UTC(2026, 2, 15))`);
  check(JSON.stringify(spring) === JSON.stringify([{ since: 0, minutes: -360 }, { since: Date.UTC(2026, 2, 8, 8), minutes: -300 }]), "start of summer time: -6 h, then -5 h from 2 a.m. on Mar 8 (08:00 UTC) exactly", spring);
  const plain = await util(`m.utcOffsets(Date.UTC(2026, 5, 1), Date.UTC(2026, 5, 30))`);
  check(plain.length === 1 && plain[0].minutes === -300, "a month without a change: one offset", plain);
  const all = await util(`m.utcOffsets(0)`);
  check(all.length >= 9 && all.length <= 12 && all.every((o, i) => !i || o.since > all[i - 1].since), "All: five years of changes, oldest first", all.length);

  // ── a request at 00:30 UTC is the evening before in Chicago ──
  const now = Date.now();
  let ts = Math.floor(now / DAY) * DAY + 30 * 60_000;
  if (ts > now - 60_000) ts -= DAY;
  await fetch(MOCK + "/__mock/usage-row", { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ ts, provider: "openrouter", model: "tz-probe", input: 4321, output: 1 }) });
  const chicagoDay = await p.eval(`(() => { const d = new Date(${ts}); return d.getFullYear() + '-' + String(d.getMonth() + 1).padStart(2, '0') + '-' + String(d.getDate()).padStart(2, '0'); })()`);
  const utcDay = new Date(ts).toISOString().slice(0, 10);
  check(chicagoDay !== utcDay, "the probe falls on different days in Chicago and in UTC", { chicagoDay, utcDay });
  const from = Date.now() - 6 * DAY;
  const offsets = await util(`m.utcOffsets(${from})`);
  const rep = (await ws.rpc("get_usage", { from, utc_offsets: offsets })).result;
  const probeDay = rep.by_day.find((d) => d.models.some((m) => m.model === "tz-probe"))?.day;
  check(probeDay === chicagoDay, "with the device's zone, the request is on Chicago's day", { probeDay, chicagoDay });
  const old = (await ws.rpc("get_usage", { from })).result;
  const oldDay = old.by_day.find((d) => d.models.some((m) => m.model === "tz-probe"))?.day;
  check(!!oldDay, "an older app (no zone) still gets its usage, by the server's days", oldDay);
  const refused = await ws.rpc("get_usage", { from, utc_offsets: [{ since: 0, minutes: 2000 }] });
  check(!!refused.error, "an offset beyond 18 hours is refused", refused);

  // ── the page: the chart's days are Chicago's, none after today ──
  const open = async (pg) => {
    await pg.eval(`__mod('lib/ui.ts').then(u => { u.setDrawerOpen?.(false); u.openSettings('usage'); })`);
    await pg.waitFor(`document.querySelector('.usage .kpi')`, 8000);
    await sleep(500);
  };
  const todayKey = `(() => { const d = new Date(); return d.getFullYear() + '-' + String(d.getMonth() + 1).padStart(2, '0') + '-' + String(d.getDate()).padStart(2, '0'); })()`;
  const days = (pg) => pg.eval(`[...document.querySelectorAll('.usage-hit')].map((r) => r.dataset.day)`);
  await open(p);
  await p.clickText(".usage-bar .segmented button", "7 days");
  await sleep(800);
  const today = await p.eval(todayKey);
  let d = await days(p);
  check(d.length === 7 && d[d.length - 1] === today && d.every((k) => k <= today), "7 days: seven columns ending today (Chicago), none in the future", { d, today });
  check(d.includes(chicagoDay), "…the probe's Chicago day is one of them", { d, chicagoDay });
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    await sleep(250);
    await p.shot(`${SHOTS}/usage-days-chicago-desktop-${theme}.png`);
  }
  await p.clickText(".usage-bar .segmented button", "30 days");
  await sleep(800);
  d = await days(p);
  check(d.length === 30 && d[d.length - 1] === today && d.every((k) => k <= today), "30 days: thirty columns ending today, none in the future", { n: d.length, last: d[d.length - 1], today });
  check(!p.errors.length, "desktop: no page errors", p.errors);
  await p.close();

  // ── the folded phone, far east (Tokyo, ahead of UTC): its own days too ──
  const ph = await device();
  await ph.send("Emulation.setTimezoneOverride", { timezoneId: "Asia/Tokyo" });
  await ph.viewport(344, 760, true, 2);
  await signIn(ph, "Tokyo phone");
  const tk = await ph.eval(`__mod('lib/util.ts').then((m) => m.utcOffsets(Date.now() - 6 * ${DAY}))`);
  check(tk.length === 1 && tk[0].minutes === 540, "Tokyo: +9 h, no summer time", tk);
  await open(ph);
  await ph.clickText(".usage-bar .segmented button", "7 days");
  await sleep(800);
  const tToday = await ph.eval(todayKey);
  d = await days(ph);
  check(d.length === 7 && d[d.length - 1] === tToday && d.every((k) => k <= tToday), "phone (Tokyo): seven columns ending its today", { d, tToday });
  for (const theme of ["dark", "light"]) {
    await ph.theme(theme);
    await sleep(250);
    await ph.shot(`${SHOTS}/usage-days-tokyo-phone-${theme}.png`);
  }
  check(!ph.errors.length, "phone: no page errors", ph.errors);
  await ph.close();
  ws.close();
  await mock("/__mock/reset");
  finish();
});
