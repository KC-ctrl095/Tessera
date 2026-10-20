// Agents don't pop tiles (AGENT-TILES). The user's report: an agent checking their router opened the web
// view in the mosaic and worked in it; tiles should come up only when the user must see or do something,
// or asked to. So, on a desktop and a phone:
//   · an agent's browsing stays in the background: no board, no tile; each step's card has a small
//     picture of the page, and the latest one a "Watch" that opens the live page where you are;
//   · "show me …" (the agent's explicit `show`) puts one tile beside the chat, in the mosaic it is in,
//     and never takes the user to another mosaic;
//   · a sign-in only the user can do (`needs_user`) puts the page on screen with the reason, Done and
//     I can't, also in the chat, and the agent waits for it;
//   · a note or a whiteboard the agent makes on its own is a card in the chat with "Open" (an image shows
//     in the chat itself), no tile until opened.
import { SHOTS, MOCK, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const B_SID = "ses_5a3e8f10c2d9";
const order = (p) => ws(p, `return m.workspace.order()`);
const boardPanes = (p, boardId) => ws(p, `return Object.values(m.workspace.panes).filter(x => x.content.boardId === '${boardId}').map(x => x.id)`);
const tilesOfKind = (p, type) => ws(p, `return Object.values(m.workspace.panes).filter(x => x.content.type === '${type}').length`);
const boards = async () => (await (await fetch(`${MOCK}/__mock/boards`)).json()).boards;
const waitFor = async (fn, ms = 6000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const v = await fn();
    if (v) return v;
    await sleep(80);
  }
  return null;
};
const send = async (p, pane, text) => {
  await ws(p, `m.focusComposer('${pane}')`);
  await p.type(text);
  await p.key("Enter", "Enter");
  // Let the turn start before anything waits for the chat to be idle again.
  await sleep(500);
};
const idle = (probe, sid, ms = 20000) =>
  waitFor(async () => {
    const r = await probe.rpc("get_session", { session_id: sid });
    return r.result?.info?.status === "idle";
  }, ms);
const serverTiles = async (probe) => {
  const L = (await probe.rpc("get_mosaics")).result;
  return L.mosaics.flatMap((m) => m.tiles);
};

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const probe = await wsClient("probe");
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Desktop");
  await ws(p, `m.openSession('${B_SID}', { pane: m.workspace.focused() })`);
  await sleep(800);
  const C = await ws(p, `return m.workspace.order().find(id => m.workspace.panes[id].content.sessionId === '${B_SID}')`);
  const sel = (s) => `[data-pane-id="${C}"] ${s}`;

  // ── the agent browses: in the background ───────────────────────────────
  console.log("— browsing in the background");
  const tiles0 = (await order(p)).length;
  const urls0 = (await boards()).filter((b) => b.content.type === "url").length;
  await send(p, C, "go to router.lan and check the WAN status");
  await waitFor(() => p.eval(`document.querySelectorAll('${sel(".tool-browser .sq-ok")}').length === 3`), 15000);
  await idle(probe, B_SID);
  await sleep(300);
  check((await boards()).filter((b) => b.content.type === "url").length === urls0, "the agent's browsing made no web view");
  check((await order(p)).length === tiles0, "…and no tile came up", { before: tiles0, after: (await order(p)).length });
  check(!(await serverTiles(probe)).some((t) => t.board_id), "…nor in the synced layout");
  // The finished steps fold; the fold keeps the latest picture and Watch in view.
  const folded = await p.eval(`(() => { const r = [...document.querySelectorAll('${sel(".steps-row")}')].pop(); return r ? { thumb: !!r.querySelector('.tool-thumb img'), watch: r.querySelector('.tool-watch')?.textContent.trim() ?? null } : null; })()`);
  check(!!folded && folded.thumb && /Watch/.test(folded.watch ?? ""), "the folded steps show the page's picture and Watch", folded);
  await p.eval(`[...document.querySelectorAll('${sel(".steps-head")}')].pop().click()`);
  await sleep(300);
  const thumbs = await p.eval(`document.querySelectorAll('${sel(".tool-browser .tool-thumb img")}').length`);
  check(thumbs === 3, "opened, each step shows a small picture of the page", thumbs);
  const watchBtns = await p.eval(`[...document.querySelectorAll('${sel(".tool-watch")}')].map(b => b.textContent.trim())`);
  check(watchBtns.length === 1 && /Watch/.test(watchBtns[0]), "…and the latest one offers Watch (one button)", watchBtns);
  check(!(await p.eval(`!!document.querySelector('${sel(".tool-show:not(.tool-watch)")}')`)), "no “Show tile”: there is no tile");
  await p.shot(`${SHOTS}/agent-tiles-steps-desktop.png`, await p.eval(`(() => { const r = document.querySelector('[data-pane-id="${C}"]').getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; })()`));

  // Watch: the live page opens where the user is, theirs.
  const active0 = await ws(p, `return m.workspace.active()`);
  await p.click(sel(".tool-watch"));
  const watched = await waitFor(async () => (await boards()).find((b) => b.content.type === "url" && b.session_id === B_SID));
  check(!!watched && watched.by_user === true, "Watch makes the tab a web view of the user's", watched && { by_user: watched.by_user, url: watched.content.url });
  check(!!watched && /router\.lan\/next/.test(watched.content.url), "…at the page the agent was on", watched?.content.url);
  const wp = watched ? await waitFor(async () => (await boardPanes(p, watched.id)).length === 1) : null;
  check(!!wp, "…in one tile");
  check((await ws(p, `return m.workspace.active()`)) === active0, "…in the mosaic on screen");
  check(!!watched && (await ws(p, `const t = m.paneShowingBoard('${watched?.id}'); return !!t && m.workspace.paneWorkspace(t) === m.workspace.active()`)), "…beside the chat");
  await sleep(400);
  check(!(await p.eval(`!!document.querySelector('${sel(".tool-watch")}')`)), "watching: the step no longer offers Watch");

  // ── "show me": the agent's explicit show ───────────────────────────────
  console.log("— the agent shows a page when asked");
  await ws(p, `m.closePane(m.paneShowingBoard('${watched?.id}'))`);
  await sleep(300);
  const n1 = (await order(p)).length;
  await send(p, C, "show me example.org please");
  await idle(probe, B_SID);
  await sleep(500);
  const shown = (await boards()).filter((b) => b.content.type === "url" && b.session_id === B_SID && /example\.org/.test(b.content.url));
  check(shown.length === 1 && !shown[0].by_user, "“show me” puts the agent's page up", shown.map((b) => b.content.url));
  check((await order(p)).length === n1 + 1 && (await boardPanes(p, shown[0]?.id)).length === 1, "…in one tile beside the chat", { n1, now: (await order(p)).length });

  // ── a sign-in only the user can do ─────────────────────────────────────
  console.log("— handed over to sign in");
  await send(p, C, "sign in to bank.example and check my balance");
  const asking = await waitFor(async () => (await boards()).find((b) => b.ask && b.session_id === B_SID && b.content.type === "url"), 10000);
  check(!!asking, "the agent hands the page over with a question on it");
  check(!!asking && /Sign in to bank\.example/.test(asking.ask.prompt), "…saying why", asking?.ask?.prompt);
  const hp = asking ? await waitFor(async () => (await boardPanes(p, asking.id))[0]) : null;
  check(!!hp, "…and it is on screen");
  await sleep(600);
  const labels = hp ? await p.eval(`[...document.querySelectorAll('[data-pane-id="${hp}"] .ask-actions button')].map(b => b.textContent.trim())`) : [];
  check(JSON.stringify(labels) === JSON.stringify(["I can't", "Done"]), "the tile asks Done or I can't (no other choice)", labels);
  const eyebrow = hp ? await p.eval(`document.querySelector('[data-pane-id="${hp}"] .ask-eyebrow')?.textContent ?? ''`) : "";
  check(/needs you here/.test(eyebrow), "“The agent needs you here”", eyebrow);
  const pointer = await p.eval(`document.querySelector('${sel(".ask-pointer")}')?.textContent ?? document.querySelector('${sel(".chat-ask")}')?.textContent ?? ''`);
  check(/Sign in to bank\.example/.test(pointer), "the chat says so too", pointer.slice(0, 160));
  await p.shot(`${SHOTS}/agent-tiles-needs-user-desktop.png`);
  if (hp) await p.clickText(`[data-pane-id="${hp}"] .ask-actions button`, "Done");
  await idle(probe, B_SID);
  await sleep(300);
  const last = await p.eval(`[...document.querySelectorAll('${sel(".msg-assistant")}')].pop()?.textContent ?? ''`);
  check(/signed in/.test(last), "Done: the agent carries on", last.slice(0, 120));

  // ── notes and whiteboards: cards ───────────────────────────────────────
  console.log("— notes and boards are cards");
  const notes0 = await tilesOfKind(p, "note");
  await send(p, C, "take a note about the router settings");
  await idle(probe, B_SID);
  await sleep(400);
  check((await tilesOfKind(p, "note")) === notes0, "a note the agent writes doesn't open a tile");
  const card = await p.eval(`(() => { const c = [...document.querySelectorAll('${sel(".note-card")}')].pop(); return c ? c.textContent : null; })()`);
  check(!!card && /Router settings/i.test(card) && /Open/.test(card), "…it's a card in the chat with Open", card);
  await p.eval(`[...document.querySelectorAll('${sel(".note-card")}')].pop().click()`);
  await sleep(500);
  check((await tilesOfKind(p, "note")) === notes0 + 1, "Open shows it in a tile");

  const wb0 = await tilesOfKind(p, "whiteboard");
  const nb0 = (await boards()).length;
  await send(p, C, "sketch the network for me");
  await idle(probe, B_SID);
  await sleep(400);
  check((await boards()).length === nb0 + 1, "the agent made a whiteboard");
  check((await tilesOfKind(p, "whiteboard")) === wb0, "…without a tile");
  const bcard = await p.eval(`[...document.querySelectorAll('${sel(".board-card")}')].filter(c => !c.classList.contains('note-card')).pop()?.textContent ?? ''`);
  check(/Open/.test(bcard) && !/Tile closed/.test(bcard), "…a card with Open", bcard);
  await send(p, C, "illustrate it with an image");
  await idle(probe, B_SID);
  await sleep(500);
  check((await tilesOfKind(p, "whiteboard")) === wb0, "an image the agent shares opens no tile");
  const inline = await p.eval(`(() => { const i = [...document.querySelectorAll('${sel(".board-inline-image img")}')].pop(); return i ? { w: i.getBoundingClientRect().width, loaded: i.complete && i.naturalWidth > 0 } : null; })()`);
  check(!!inline && inline.w > 100 && inline.loaded, "…it shows in the chat itself", inline);
  await p.shot(`${SHOTS}/agent-tiles-cards-desktop.png`, await p.eval(`(() => { const r = document.querySelector('[data-pane-id="${C}"]').getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; })()`));
  if (inline) {
    await p.eval(`[...document.querySelectorAll('${sel(".board-inline-image")}')].pop().click()`);
    // The viewer is its own chunk, loaded the first time.
    const viewer = await waitFor(() => p.eval(`!!document.querySelector('.lightbox-view, .lightbox')`), 6000);
    check(!!viewer, "a tap views it full size");
    await p.key("Escape");
    await sleep(300);
  }

  // ── in mosaic 2: shown beside the chat there, the user stays there ──────
  console.log("— mosaic 2");
  const other = (await probe.rpc("create_session", {})).result;
  await ws(p, `m.newWorkspace()`);
  await sleep(500);
  const m2 = await ws(p, `return m.workspace.active()`);
  await ws(p, `m.openSession('${other.id}', { pane: m.workspace.focused() })`);
  await sleep(700);
  const C2 = await ws(p, `return m.workspace.order().find(id => m.workspace.panes[id].content.sessionId === '${other.id}')`);
  check((await ws(p, `return m.workspace.active()`)) === m2 && !!C2, "a chat in a new mosaic 2", { m2, C2 });
  await send(p, C2, "show me example.net");
  await sleep(300);
  await idle(probe, other.id);
  await sleep(800);
  const s2 = (await boards()).find((b) => b.content.type === "url" && /example\.net/.test(b.content.url));
  check((await ws(p, `return m.workspace.active()`)) === m2, "the user stays in mosaic 2");
  const there = s2 ? await waitFor(() => ws(p, `const t = m.paneShowingBoard('${s2.id}'); return !!t && m.workspace.paneWorkspace(t) === ${m2}`), 6000) : null;
  check(!!there, "the page goes up beside the chat there", s2 && { board: s2.id, where: await ws(p, `const t = m.paneShowingBoard('${s2.id}'); return t ? m.workspace.paneWorkspace(t) : null`) });
  console.log("desktop errors:", p.errors);
  check(!p.errors.length, "no page errors (desktop)");
  await p.close();

  // ── the phone ──────────────────────────────────────────────────────────
  console.log("— phone");
  const ph = await device();
  await ph.viewport(412, 915, true, 2.625);
  await signIn(ph, "Phone");
  const visBox = (s) => ph.eval(`(() => { const e = [...document.querySelectorAll(${JSON.stringify(s)})].find(e => { const r = e.getBoundingClientRect(); return r.width && r.left >= 0 && r.right <= innerWidth; }); if (!e) return null; const r = e.getBoundingClientRect(); return { cx: r.x + r.width / 2, cy: r.y + r.height / 2 }; })()`);
  await ws(ph, `m.switchWorkspace(0); m.openSession('${B_SID}', { newPane: true })`);
  await sleep(800);
  const PC = await ws(ph, `return m.workspace.focused()`);
  const before = (await order(ph)).length;
  const urlsBefore = (await boards()).filter((b) => b.content.type === "url").length;
  await ws(ph, `m.focusComposer('${PC}')`);
  await ph.type("go to modem.lan please");
  let bx = await visBox(`[data-pane-id="${PC}"] .send-btn[aria-label="Send"]`);
  await ph.tap(bx.cx, bx.cy);
  await waitFor(() => ph.eval(`document.querySelectorAll('[data-pane-id="${PC}"] .tool-browser .sq-ok').length >= 3`), 15000);
  await idle(probe, B_SID);
  await sleep(500);
  check((await boards()).filter((b) => b.content.type === "url").length === urlsBefore, "on the phone too, browsing makes no web view");
  check((await order(ph)).length === before && (await ws(ph, `return m.workspace.focused()`)) === PC, "…and the phone stays on the chat", { before, now: (await order(ph)).length });
  check((await ph.eval(`document.querySelectorAll('[data-pane-id="${PC}"] .tool-thumb img').length`)) >= 3, "…steps with pictures");
  await ph.shot(`${SHOTS}/agent-tiles-steps-phone.png`);
  bx = await visBox(`[data-pane-id="${PC}"] .tool-watch`);
  check(!!bx, "…and Watch on the latest");
  if (bx) await ph.tap(bx.cx, bx.cy);
  const pw = await waitFor(async () => (await boards()).find((b) => b.content.type === "url" && /modem\.lan/.test(b.content.url)));
  await sleep(800);
  const focusedIsIt = pw ? await ws(ph, `const f = m.workspace.focused(); return m.workspace.panes[f]?.content.boardId === '${pw.id}'`) : false;
  check(!!pw && pw.by_user && focusedIsIt, "Watch on the phone opens the page and goes to it", pw && { by_user: pw.by_user, focusedIsIt });
  await ph.shot(`${SHOTS}/agent-tiles-watch-phone.png`);
  // Asked to see a page on the phone: the agent's `show` puts it up (one tile), and the phone offers it.
  await ws(ph, `m.focusPane('${PC}')`);
  await sleep(400);
  await ws(ph, `m.focusComposer('${PC}')`);
  await ph.type("show me example.com");
  bx = await visBox(`[data-pane-id="${PC}"] .send-btn[aria-label="Send"]`);
  if (bx) await ph.tap(bx.cx, bx.cy);
  const ps = await waitFor(async () => (await boards()).find((b) => b.content.type === "url" && /example\.com/.test(b.content.url) && !b.by_user), 12000);
  await sleep(1200);
  check(!!ps && (await boardPanes(ph, ps.id)).length === 1, "“show me” on the phone puts the page up in one tile", ps && (await boardPanes(ph, ps.id)));
  await ph.shot(`${SHOTS}/agent-tiles-show-phone.png`);
  console.log("phone errors:", ph.errors);
  check(!ph.errors.length, "no page errors (phone)");
  await ph.close();
  probe.close();
  finish();
});
