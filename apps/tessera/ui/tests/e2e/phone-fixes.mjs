// Batch 9 phone fixes: the model menu doesn't raise the keyboard until its search is tapped; on the
// folded phone New is at the top right; Close is in its single-line header and sheet;
// attach menu follows its button when the keyboard goes away; a queued message is edited whole;
// reasoning sent in several parts (some empty) reads as one "Thinking" row, live and after.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const SID = "ses_7d3a91e5c4b2"; // "Add a /healthz endpoint"
const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const open = (p, sid) => ws(p, `return m.openSession('${sid}', { pane: m.workspace.focused() })`);
const count = (p, sel) => p.eval(`document.querySelectorAll(${JSON.stringify(sel)}).length`);
/** The element on screen (pages to the side don't count) matching `sel`. */
const vis = (p, sel) =>
  p.eval(`(() => { const e = [...document.querySelectorAll(${JSON.stringify(sel)})].find((e) => { const r = e.getBoundingClientRect(); return r.width && r.left >= 0 && r.right <= innerWidth + 1; }); if (!e) return null; const r = e.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height, b: r.bottom, r: r.right, cx: r.x + r.width / 2, cy: r.y + r.height / 2, op: getComputedStyle(e).opacity }; })()`);

async function send(p, msg) {
  await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
  await p.type(msg);
  await p.click(".pane.focused .send-btn:not(.stop):not(.rec-stop)");
}

run(async () => {
  for (const theme of ["dark", "light"]) {
    console.log(`— folded phone (${theme})`);
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(344, 882, true, 3);
    await signIn(p, `Phone fixes ${theme}`);
    await p.theme(theme);
    await open(p, SID);
    await sleep(900);

    // Single-line headers have New and Close; the title also offers Close in its sheet.
    check((await count(p, ".pane-head .head-new")) >= 1 && !!(await vis(p, ".pane.focused .head-close")), `${theme}: the tile on screen has New and a visible Close`);
    const oneLine = await p.eval(`(() => { const h=document.querySelector('.pane.focused .pane-head'); const r=h.getBoundingClientRect(); const c=h.querySelector('.head-close'); const cr=c.getBoundingClientRect(); const hit=document.elementFromPoint(cr.x+cr.width/2,cr.y+cr.height/2); return !h.querySelector('.model-sub,.head-sub') && r.height<60 && (hit===c || c.contains(hit)); })()`);
    check(oneLine, `${theme}: the phone header stays one line and Close is tappable`);
    let b = await vis(p, ".pane-head .head-new");
    check(!!b && 344 - b.r < 16, `${theme}: …at the top right`, b);
    await p.shot(`${SHOTS}/phone-fixes-header-${theme}.png`);
    check(!(await p.eval(`!!document.querySelector('.drawer .sb-new')`)), `${theme}: the side menu has no New of its own`);
    await p.tap(b.cx, b.cy);
    await p.waitFor(`document.querySelector('.menu-pop .menu-item')`, 3000).catch(() => {});
    await sleep(250);
    const labels = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-label')].map((e) => e.textContent)`);
    check(["Chat", "Private chat", "Note", "Web view", "More"].every((l) => labels.includes(l)), `${theme}: …offering a chat, a private chat, a note, a web view and More`, labels);
    const menu = await vis(p, ".menu-pop");
    check(!!menu && menu.h < 600 && menu.r <= 344, `${theme}: …as a compact menu, not a sheet`, menu);
    await p.shot(`${SHOTS}/phone-fixes-new-${theme}.png`);
    const before = await ws(p, `return m.workspace.order().length`);
    b = await p.eval(`(() => { const e = [...document.querySelectorAll('.menu-pop .menu-item')].find((x) => x.querySelector('.menu-label')?.textContent === 'Chat'); const r = e.getBoundingClientRect(); return { cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);
    await p.tap(b.cx, b.cy);
    await sleep(700);
    check((await ws(p, `return m.workspace.order().length`)) === before + 1, `${theme}: Chat makes a tile`);
    // Closing that tile from its sheet (the title), once it has slid into view.
    await sleep(600);
    b = await vis(p, ".pane.focused .title-menu");
    await p.tap(b.cx, b.cy);
    await p.waitFor(`document.querySelector('.tile-opts')`, 3000).catch(() => {});
    await sleep(250);
    await p.shot(`${SHOTS}/phone-fixes-sheet-${theme}.png`);
    b = await p.eval(`(() => { const e = [...document.querySelectorAll('.tile-opts .act-btn')].find((x) => x.textContent.trim() === 'Close'); if (!e) return null; const r = e.getBoundingClientRect(); return { cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);
    check(!!b, `${theme}: the chat's sheet has Close`);
    if (b) await p.tap(b.cx, b.cy);
    await p.waitFor(`document.querySelectorAll('[data-pane-id]').length === ${before}`, 3000).catch(() => {});
    const after = await ws(p, `return m.workspace.order().length`);
    if (after !== before) await p.shot(`${SHOTS}/phone-fixes-x-${theme}.png`);
    check(after === before, `${theme}: …and Close closes it`, { before, after, dialog: await p.eval(`document.querySelector('.modal, [role=dialog]')?.textContent?.slice(0, 120) ?? null`) });

    // ── the model menu: no keyboard until the search is tapped ──
    await ws(p, `m.focusPane(m.workspace.order()[0])`);
    await sleep(400);
    await p.eval(`__mod('lib/bus.ts').then((b) => __mod('lib/workspace.ts').then((w) => b.paneAction(w.workspace.focused(), 'model')))`);
    await p.waitFor(`document.querySelector('.model-menu')`, 3000);
    await sleep(350);
    check(!(await p.eval(`document.activeElement?.matches('input, textarea')`)), `${theme}: opening the model menu doesn't raise the keyboard`);
    b = await vis(p, ".model-menu .menu-search input");
    check(!!b, `${theme}: …its search is there`);
    await p.tap(b.cx, b.cy);
    await sleep(200);
    check(await p.eval(`document.activeElement?.matches('.model-menu .menu-search input')`), `${theme}: …and a tap on it gives it the keyboard`);
    await p.shot(`${SHOTS}/phone-fixes-model-${theme}.png`);
    await p.eval(`history.back()`);
    await sleep(400);

    // ── the attach menu follows its button when the keyboard goes away ──
    // The keyboard is up: the app is as tall as what's left (App.tsx trackViewport sets --app-h).
    await p.eval(`document.documentElement.style.setProperty('--app-h', '520px')`);
    await sleep(300);
    b = await vis(p, ".pane.focused .composer .comp-btn[aria-haspopup='menu']");
    await p.tap(b.cx, b.cy);
    await p.waitFor(`document.querySelector('.attach-pop')`, 3000);
    await sleep(250);
    // The keyboard goes: the app is full height again and the composer drops to the bottom.
    await p.eval(`document.documentElement.style.setProperty('--app-h', '882px'); visualViewport.dispatchEvent(new Event('resize'))`);
    await sleep(500);
    const btn = await vis(p, ".pane.focused .composer .comp-btn[aria-haspopup='menu']");
    const pop = await vis(p, ".attach-pop");
    check(!!btn && !!pop && btn.y > 700 && Math.abs(pop.b + 6 - btn.y) <= 3, `${theme}: the attach menu stays on its button`, { btn, pop });
    await p.shot(`${SHOTS}/phone-fixes-attach-${theme}.png`);
    await p.eval(`history.back()`);
    await sleep(300);

    // ── reasoning in parts reads as one row; a queued message is edited whole ──
    await send(p, "think in parts about the timeout");
    await p.waitFor(`document.querySelector('.pane.focused .thinking.live')`, 6000).catch(() => {});
    await sleep(2500);
    const live = await count(p, ".pane.focused .thinking.live");
    check(live === 1, `${theme}: while it thinks, one Thinking row (${live})`);
    const long = `${"Please also check the retry logic and the backoff. ".repeat(6)}\nAnd a second line at the end.`;
    await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
    await p.type(long);
    await p.click(".pane.focused .send-btn:not(.stop):not(.rec-stop)");
    await p.waitFor(`document.querySelector('.pane.focused .qcard')`, 3000);
    b = await vis(p, ".pane.focused .qcard .qbtn[aria-label='Edit']");
    await p.tap(b.cx, b.cy);
    await sleep(300);
    const val = await p.eval(`document.querySelector('.pane.focused .qinput')?.value ?? null`);
    check(val === long.trim(), `${theme}: editing a queued message starts from all of it`, { len: val?.length, want: long.trim().length });
    const card = await vis(p, ".pane.focused .qcard.editing");
    const field = await vis(p, ".pane.focused .qinput");
    check(!!card && !!field && field.w >= card.w - 30, `${theme}: …in a field as wide as the card`, { card: card?.w, field: field?.w });
    await p.shot(`${SHOTS}/phone-fixes-queue-edit-${theme}.png`);
    await p.clickText(".pane.focused .qbtn", "Cancel");
    await p.waitFor(`!document.querySelector('.pane.focused .thinking.live')`, 15000).catch(() => {});
    await p.waitFor(`!document.querySelector('.pane.focused .qcard')`, 15000).catch(() => {});
    await sleep(800);
    // The rows of that reply: from its question to the next message of the user's.
    const turn = `(() => { const items = [...document.querySelectorAll('.pane.focused .item')]; const i = items.findIndex((e) => e.querySelector('.msg-user') && e.textContent.includes('think in parts about the timeout')); if (i < 0) return []; const out = []; for (const e of items.slice(i + 1)) { if (e.querySelector('.msg-user')) break; out.push(...e.querySelectorAll('.thinking')); } return out; })()`;
    const n = await p.eval(`${turn}.length`);
    check(n === 1, `${theme}: after the reply, one block for its reasoning (${n})`);
    const body = await p.eval(`(async () => { const t = ${turn}[0]; if (!t) return null; if (t.querySelector('.thinking-head').getAttribute('aria-expanded') === 'false') t.querySelector('.thinking-head').click(); await new Promise((r) => setTimeout(r, 300)); return t.querySelector('.thinking-body')?.textContent ?? null; })()`);
    check(!!body && body.includes("the errors start right after the deploy") && body.includes("the new timeout is far too short"), `${theme}: …holding both parts`, body);

    // ── Settings → Devices doesn't pan sideways; what agents did has its own section ──
    const noSide = () => p.eval(`(() => { const b = document.querySelector('.settings-body'); return !!b && [...b.querySelectorAll('*')].every((e) => e.getBoundingClientRect().right <= innerWidth + 1) && b.scrollWidth <= b.clientWidth + 1 && b.scrollLeft === 0; })()`);
    await p.eval(`__mod('lib/ui.ts').then((m) => m.openSettings('devices'))`);
    await p.waitFor(`document.querySelector('.dev-item')`, 5000);
    await sleep(500);
    await p.eval(`[...document.querySelectorAll('.dev-item')].find((li) => li.querySelector('.dev-name').textContent.startsWith('Desktop')).querySelector('.dev-row').click()`);
    await sleep(500);
    check(await noSide(), `${theme}: Devices, a machine open: nothing runs off the side`);
    await p.shot(`${SHOTS}/phone-fixes-devices-${theme}.png`);
    await p.clickText(".dev-details button", "Activity");
    await p.waitFor(`document.querySelector('.act-row')`, 4000).catch(() => {});
    await sleep(500);
    const acts = await p.eval(`[...document.querySelectorAll('.act-row')].map((r) => r.textContent)`);
    check(acts.length >= 3 && acts.some((t) => /cargo build --release/.test(t)), `${theme}: Activity lists what agents did`, acts);
    check(await noSide(), `${theme}: …and nothing runs off the side`);
    b = await vis(p, ".act-row");
    const h0 = b.h;
    await p.tap(b.cx, b.cy);
    await sleep(250);
    check(await p.eval(`document.querySelector('.act-row').getAttribute('aria-expanded') === 'true'`), `${theme}: a tap opens a row`, { h0 });
    await p.shot(`${SHOTS}/phone-fixes-activity-${theme}.png`);
    await p.eval(`__mod('lib/ui.ts').then((m) => m.setSettingsTab(null))`);
    await sleep(300);
    check(p.errors.length === 0, `${theme}: no page errors`, p.errors);
    await p.close();
  }

  // ── unfolded: every tile has its close X ──
  console.log("— unfolded");
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const f = await device();
  await f.viewport(841, 701, true, 2.625);
  await signIn(f, "Phone fixes fold");
  await open(f, SID);
  await ws(f, `m.openSession('ses_5a3e8f10c2d9', { newPane: true })`);
  await sleep(900);
  const xs = await f.eval(`[...document.querySelectorAll('[data-pane-id] .head-close')].map((e) => { const r = e.getBoundingClientRect(); return { on: r.width > 0 && r.left >= 0 && r.right <= innerWidth + 1, op: getComputedStyle(e).opacity }; })`);
  check(xs.filter((x) => x.on && x.op === "1").length === 2, "both tiles on screen show their close X", xs);
  await f.shot(`${SHOTS}/phone-fixes-fold.png`);
  check(f.errors.length === 0, "unfolded: no page errors", f.errors);
  await f.close();
  finish();
});
