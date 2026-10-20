// "Use Tessera in a web browser" (Settings → Connection → Web browsers): off by default, one switch
// with what it changes in plain words; turning it on asks first; once on, where to open it and how a
// browser gets in; a Cloudflare tunnel Tessera set up needs a token once to let browsers through (asked
// right there, never kept); a server without the web app can't turn it on; paired browsers counted;
// desktop, the folded Fold's cover screen, the unfolded Fold, light and dark.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const ui = (p, js) => p.eval(`__mod('lib/ui.ts').then((u) => { ${js} })`);
const panel = ".web-access";
const toggle = `document.querySelector('${panel} .toggle')`;
const on = (p) => p.eval(`${toggle}?.getAttribute('aria-checked') === 'true'`);
const text = (p, sel = panel) => p.eval(`document.querySelector(${JSON.stringify(sel)})?.textContent ?? ''`);
const dialog = (p) => p.eval(`document.querySelector('.dialog-body') ? { title: [...document.querySelectorAll('.modal h2')].pop()?.textContent ?? '', body: document.querySelector('.dialog-text')?.textContent ?? '' } : null`);
const answer = async (p, ok) => {
  await p.click(ok ? ".dialog-actions button[type=submit]" : ".dialog-actions .btn-ghost");
  await sleep(600);
};
const open = async (p) => {
  await ui(p, `u.openSettings('servers')`);
  await p.waitFor(`!!document.querySelector('${panel}')`, 6000);
  await p.eval(`document.querySelector('${panel}').scrollIntoView({ block: 'center' })`);
  await sleep(300);
};
// Nothing in the panel sticks out sideways.
const fits = (p) =>
  p.eval(`(() => { const c = document.querySelector('${panel}'); if (!c) return false; const r = c.getBoundingClientRect(); return document.documentElement.scrollWidth <= innerWidth + 1 && [...c.querySelectorAll('*')].every((e) => { const b = e.getBoundingClientRect(); return !b.width || b.right <= r.right + 1; }); })()`);
const flip = async (p) => {
  await p.eval(`${toggle}.click()`);
  await sleep(500);
};

run(async () => {
  const c = await wsClient("web-access-suite");
  await mock("/__mock/reset");
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Web access desk");
  await open(p);

  // ── off by default, explained ──
  check((await on(p)) === false, "off by default");
  const facts = await p.eval(`[...document.querySelectorAll('${panel} .web-facts li')].map((l) => l.textContent)`);
  check(facts.length === 3, "three plain facts about it", facts);
  check(/pair with a code/.test(facts[0]) && /end-to-end encrypted/.test(facts[0]), "…who gets in, and that it stays encrypted", facts[0]);
  check(/anyone who finds your address/i.test(facts[1]), "…that strangers can see the sign-in page", facts[1]);
  check(/less protected than the apps/.test(facts[2]) && /could change the page/.test(facts[2]), "…and the honest risk: the page comes through whoever runs the address", facts[2]);
  check(/Off: only the Tessera apps can connect/.test(await text(p, `${panel} .setting-help`)), "the switch says what off means");
  check(!(await p.eval(`!!document.querySelector('${panel} .web-url')`)), "no address shown while off");
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    await sleep(200);
    await p.shot(`${SHOTS}/web-access-desk-off-${theme}.png`);
  }
  await p.theme("dark");

  // ── turning it on asks first; Cancel leaves it off ──
  await flip(p);
  let d = await dialog(p);
  check(!!d && /web browser/i.test(d.title) && /Only browsers you pair/.test(d.body), "turning it on asks first, in plain words", d);
  await answer(p, false);
  check((await on(p)) === false && (await c.rpc("remote_status")).result.web.on === false, "Cancel: it stays off");
  await flip(p);
  await answer(p, true);
  await p.waitFor(`${toggle}.getAttribute('aria-checked') === 'true'`, 4000).catch(() => {});
  check((await c.rpc("remote_status")).result.web.on === true && (await on(p)), "confirmed: on, on the server too");
  const url = await text(p, `${panel} .web-url .mono`);
  check(url === "https://tessera.example.com/web/", "where to open it: the https address + /web/", url);
  const steps = await p.eval(`[...document.querySelectorAll('${panel} .web-steps li')].map((l) => l.textContent)`);
  check(steps.length === 3 && /pairing code/.test(steps[1]) && /Devices/.test(steps[2]), "…and the three steps to get a browser in", steps);
  check(await fits(p), "nothing sticks out (desktop)");
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    await sleep(200);
    await p.shot(`${SHOTS}/web-access-desk-on-${theme}.png`);
  }
  await p.theme("dark");

  // ── off again: no question ──
  await flip(p);
  check(!(await dialog(p)), "turning it off doesn't ask");
  await sleep(300);
  check((await c.rpc("remote_status")).result.web.on === false && (await on(p)) === false, "off again");

  // ── a tunnel Tessera set up: a token, once, asked right there ──
  await c.rpc("mock_tunnel", { hostname: "tessera.example.com" });
  await p.reload(1500);
  await open(p);
  check((await c.rpc("remote_status")).result.web.needs_token === true, "the tunnel lets only the apps through yet");
  await flip(p);
  await answer(p, true);
  check(await p.eval(`!!document.querySelector('${panel} form.cf-card')`), "turning it on asks for a Cloudflare token");
  const ask = await text(p, `${panel} form.cf-card`);
  check(/Access: Apps and Policies/.test(ask) && /never stored/.test(ask), "…naming the one permission it needs, and that it isn't kept", ask);
  check((await on(p)) === false, "…and it isn't on before that");
  await p.shot(`${SHOTS}/web-access-desk-token-dark.png`);
  await p.click(`${panel} form.cf-card button[type=submit]`);
  await sleep(300);
  check(/Paste the token/.test(await text(p, `${panel} .form-error`)), "no token: says to paste one");
  await p.click(`${panel} form.cf-card input`);
  await p.type("bad-token");
  await p.click(`${panel} form.cf-card button[type=submit]`);
  await sleep(800);
  check(/Cloudflare rejected/.test(await text(p, `${panel} .form-error`)) && (await on(p)) === false, "a token Cloudflare refuses: the reason, still off", await text(p, `${panel} .form-error`));
  check((await p.eval(`document.querySelector('${panel} form.cf-card input')?.value ?? ''`)) === "", "the token field is emptied either way");
  await flip(p);
  await answer(p, true);
  await p.click(`${panel} form.cf-card input`);
  await p.type("good-token");
  await p.click(`${panel} form.cf-card button[type=submit]`);
  await p.waitFor(`${toggle}.getAttribute('aria-checked') === 'true'`, 5000).catch(() => {});
  const st = (await c.rpc("remote_status")).result.web;
  check(st.on && !st.needs_token, "a good token: browsers let through, and it's on", st);
  check(!(await p.eval(`!!document.querySelector('${panel} form.cf-card')`)), "the token form is gone");
  // Once let through, on/off needs no token again.
  await flip(p);
  await flip(p);
  await answer(p, true);
  check((await c.rpc("remote_status")).result.web.on === true && !(await p.eval(`!!document.querySelector('${panel} form.cf-card')`)), "on again without a token");

  // ── paired browsers are counted; without the web app it can't be turned on ──
  await c.rpc("mock_web_access", { browsers: 2 });
  await c.rpc("set_web_access", { on: false });
  await p.reload(1500);
  await open(p);
  check(/2 browsers are paired \(they can't connect while this is off\)/.test(await text(p)), "paired browsers counted, and what off means for them", await text(p));
  const devices = (await c.rpc("list_devices")).result;
  check(devices.filter((x) => x.browser).length === 2, "they are in Devices, marked as browsers");
  await c.rpc("mock_web_access", { available: false, browsers: 0 });
  await p.reload(1500);
  await open(p);
  check(await p.eval(`${toggle}.disabled`), "no web app on the server: the switch can't be turned on");
  check(/doesn't include the web app/.test(await text(p)), "…and says why, with what to do", await text(p));
  await c.rpc("mock_web_access", { available: true });
  await c.rpc("set_web_access", { on: true });
  await p.close();

  // ── the folded Fold's cover screen and the unfolded Fold ──
  for (const [w, h, dpr, tag] of [
    [344, 882, 2.625, "cover"],
    [884, 1104, 2.25, "fold"],
  ]) {
    const m = await device();
    await m.viewport(w, h, true, dpr);
    await signIn(m, `Web access ${tag}`);
    for (const theme of ["dark", "light"]) {
      await m.theme(theme);
      await open(m);
      check(await fits(m), `${tag} ${theme}: nothing sticks out sideways`);
      const row = await m.eval(`(() => { const t = document.querySelector('${panel} .toggle').getBoundingClientRect(); const s = document.querySelector('${panel} .setting-text').getBoundingClientRect(); return { toggleIn: t.right <= innerWidth + 1, beside: t.left >= s.right - 1 }; })()`);
      check(row.toggleIn && row.beside, `${tag} ${theme}: the switch sits beside its words, on screen`, row);
      await m.shot(`${SHOTS}/web-access-${tag}-${theme}.png`);
      await m.key("Escape");
      await sleep(300);
    }
    await m.close();
  }
  c.close?.();
  finish();
});
