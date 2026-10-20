// Regression: a menu opened over a board closes when you tap or click on the board. A visual's frame
// never passes taps to the app, so frames stop taking pointer events while a menu is open (and take
// them again once it's closed). A web view is drawn by the app: the tap that closes a menu doesn't also
// click the page. Fold (anchored menus), phone (sheets), desktop.
import { SHOTS, MOCK, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const menuOpen = (p) => p.eval(`!!document.querySelector('.popover')`);
let KIND = "chart";
const FRAME = () => (KIND === "url" ? ".wv-canvas" : ".board-frame iframe");
const BOX = () => (KIND === "url" ? ".wv-body" : ".board-frame");
const MORE = () => (KIND === "url" ? "Web view options" : "Whiteboard options");
const frameState = (p, pid) =>
  p.eval(`(() => { const f = document.querySelector('[data-pane-id="${pid}"] ${FRAME()}'); return f ? { pe: getComputedStyle(f).pointerEvents, layer: document.body.classList.contains('layer-open') } : null; })()`);
/** Clicks and touches the page got (web views). */
const pageDowns = async () => (await (await fetch(`${MOCK}/__mock/browser`)).json()).log.filter((l) => l.event && ((l.event.type === "mouse" && l.event.kind === "down") || (l.event.type === "touch" && l.event.kind === "start"))).length;
/** A point on the frame that the menu doesn't cover. */
const framePoint = (p, pid) =>
  p.eval(`(() => {
    const f = document.querySelector('[data-pane-id="${pid}"] ${BOX()}').getBoundingClientRect();
    const m = document.querySelector('.popover')?.getBoundingClientRect();
    for (const [fx, fy] of [[0.5, 0.75], [0.25, 0.85], [0.75, 0.9], [0.5, 0.5]]) {
      const x = f.left + f.width * fx, y = f.top + f.height * fy;
      if (!m || x < m.left || x > m.right || y < m.top || y > m.bottom) return [x, y];
    }
    return [f.left + 10, f.bottom - 10];
  })()`);

async function scenario(label, w, h, mobile, kind) {
  KIND = kind;
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const p = await device();
  await p.viewport(w, h, mobile, mobile ? 2 : 1);
  await signIn(p, `Menu ${label}`);
  await ws(p, `m.openSession('ses_wb7c1e5a9f20', { pane: m.workspace.focused() })`);
  await sleep(800);
  // A web page on this machine (the mock's web app), or a sandboxed visual.
  const b = await mock(
    "/__mock/board",
    kind === "url" ? { session_id: "ses_wb7c1e5a9f20", title: "Local page", content: { type: "url", url: "http://127.0.0.1:7991/" }, ask: false } : { session_id: "ses_wb7c1e5a9f20", kind, ask: false },
  );
  await sleep(1500);
  const pid = await ws(p, `return m.paneShowingBoard('${b.id}')`);
  await ws(p, `m.focusPane('${pid}')`);
  await p.waitFor(`document.querySelector('[data-pane-id="${pid}"] ${kind === "url" ? ".wv-canvas.shown" : ".board-frame iframe"}')`, 8000).catch(() => {});
  await p.eval(`__mod('lib/ui.ts').then(m => m.toasts().forEach(t => m.dismissToast(t.id)))`);
  await sleep(600);
  const tapAt = async ([x, y]) => (mobile ? p.tap(x, y) : p.mouse(x, y));

  // The board's own ⋯ menu, then a tap on the frame.
  const more = await p.box(`[data-pane-id="${pid}"] [aria-label="${MORE()}"]`);
  await tapAt([more.cx, more.cy]);
  await sleep(350);
  check(await menuOpen(p), `${label} ${kind}: the board's menu opens`);
  const during = await frameState(p, pid);
  if (kind !== "url") check(during?.pe === "none", `${label} ${kind}: while it's open the frame lets taps through to the app`, during);
  await p.shot(`${SHOTS}/menu-frame-${label}-${kind}.png`);
  const downs0 = kind === "url" ? await pageDowns() : 0;
  await tapAt(await framePoint(p, pid));
  await sleep(350);
  check(!(await menuOpen(p)), `${label} ${kind}: a tap on the board closes the menu`);
  if (kind === "url") check((await pageDowns()) === downs0, `${label} ${kind}: …without also clicking the page`, { before: downs0, after: await pageDowns() });
  const after = await frameState(p, pid);
  check(after && after.pe !== "none" && !after.layer, `${label} ${kind}: …and the ${kind === "url" ? "page" : "frame"} takes taps again`, after);

  // A menu from elsewhere (the chat's options), closed by a tap on a board beside it (two tiles on screen).
  const chatPane = await ws(p, `return m.paneShowing('ses_wb7c1e5a9f20')`);
  const both = await p.eval(`(() => { const a = document.querySelector('[data-pane-id="${chatPane}"]').getBoundingClientRect(), b = document.querySelector('[data-pane-id="${pid}"]').getBoundingClientRect(); const on = (r) => r.right > 4 && r.left < innerWidth - 4; return on(a) && on(b); })()`);
  if (both) {
    const opts = await p.box(`[data-pane-id="${chatPane}"] .opts-btn, [data-pane-id="${chatPane}"] .title-menu`);
    await tapAt([opts.cx, opts.cy]);
    await sleep(350);
    check(await menuOpen(p), `${label} ${kind}: the chat's options open`);
    await tapAt(await framePoint(p, pid));
    await sleep(350);
    check(!(await menuOpen(p)), `${label} ${kind}: a tap on the board beside it closes them`);
  }

  // Focus leaving for a frame (or another window) closes a menu too.
  await tapAt([more.cx, more.cy]);
  await sleep(350);
  await p.eval(`window.dispatchEvent(new Event('blur'))`);
  await sleep(200);
  check(!(await menuOpen(p)), `${label} ${kind}: the window losing focus closes it`);
  const end = await frameState(p, pid);
  check(end && end.pe !== "none" && !end.layer, `${label} ${kind}: nothing is left blocking the frame`, end);
  console.log(label, kind, "errors", p.errors);
  check(!p.errors.length, `${label} ${kind}: no page errors`);
  await p.close();
}

run(async () => {
  await scenario("fold", 673, 841, true, "url");
  await scenario("fold", 673, 841, true, "chart");
  await scenario("phone", 375, 812, true, "url");
  await scenario("desktop", 1440, 900, false, "url");
  await scenario("desktop", 1440, 900, false, "chart");
  finish();
});
