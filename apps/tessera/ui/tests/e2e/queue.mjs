// Queue while the agent works: Enter queues, /steer steers, cards edit/steer/send now/remove,
// and a card goes once its message reached the agent (also after a reconnect, a snapshot race,
// and on a second device).
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";
import { hitTarget } from './target-input.mjs';

const SID = "ses_2b7e91c04d5a";
const queued = (p) => p.eval(`__mod('lib/store.ts').then(m => JSON.parse(JSON.stringify(m.state.sessions['${SID}']?.queued ?? [])))`);
const cards = (p) => p.eval(`[...document.querySelectorAll('.pane.focused .qcard:not(.leaving)')].map(c => ({ tag: c.querySelector('.qtag').textContent.trim(), text: c.querySelector('.qtext, .qinput')?.textContent || c.querySelector('.qinput')?.value }))`);
const users = (p) => p.eval(`__mod('lib/store.ts').then(m => (m.state.sessions['${SID}']?.entries ?? []).filter(e => e.kind === 'user' && !e.author).map(e => e.content.filter(c => c.type === 'text').map(c => c.text).join('\\n')))`);
const status = (p) => p.eval(`__mod('lib/store.ts').then(m => m.state.sessions['${SID}']?.info.status)`);
/** The card (in the focused tile) whose text matches `re`. */
const cardSel = (re) => `[...document.querySelectorAll('.pane.focused .qcard:not(.leaving)')].find(c => ${re}.test(c.textContent))`;
const clickIn = (p, re, label) => hitTarget(p, `${cardSel(re)}.querySelector('[aria-label="${label}"]')`, !!p.touchInput, `queue ${label}`);

async function typeSend(p, text, mobile) {
  await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
  await p.type(text);
  if (mobile) await p.click(".pane.focused .send-btn:not(.stop)");
  else await p.key("Enter", "Enter", 0, 13);
  await sleep(250);
}

async function openChat(p, label, w, h, mobile, theme) {
  const ok = await p.viewport(w, h, mobile, mobile ? 2 : 1);
  p.touchInput = mobile;
  await signIn(p, `Queue ${label}`);
  await p.theme(theme);
  await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${SID}', { pane: m.workspace.focused() }))`);
  await sleep(1200);
  return ok;
}

/** Until `fn()` is truthy (or the time is up); returns its last value. */
async function until(fn, ms = 15000) {
  const t0 = Date.now();
  let v;
  while (Date.now() - t0 < ms) {
    v = await fn();
    if (v) return v;
    await sleep(100);
  }
  return v;
}

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  // mock semantics first, over a plain socket
  const c = await wsClient("queue-probe");
  await c.rpc("send", { session_id: SID, content: [{ type: "text", text: "stream please" }], delivery: "auto", attachments: [] });
  await sleep(400);
  await c.rpc("send", { session_id: SID, content: [{ type: "text", text: "auto while busy" }], delivery: "auto", attachments: [] });
  await c.rpc("send", { session_id: SID, content: [{ type: "text", text: "explicit steer" }], delivery: "steer", attachments: [] });
  let snap = (await c.rpc("get_session", { session_id: SID, since_seq: 999 })).result;
  check(JSON.stringify(snap.queued) === JSON.stringify([{ text: "explicit steer", steer: true, attachments: 0 }, { text: "auto while busy", steer: false, attachments: 0 }]), "mock: auto queues a follow-up, steer steers", snap.queued);
  await c.rpc("edit_queued", { session_id: SID, index: 1, text: "auto while busy (edited)" });
  snap = (await c.rpc("get_session", { session_id: SID, since_seq: 999 })).result;
  check(snap.queued[1]?.text === "auto while busy (edited)", "mock: edit_queued changes the text", snap.queued);
  await c.rpc("steer_queued", { session_id: SID, index: 1 });
  snap = (await c.rpc("get_session", { session_id: SID, since_seq: 999 })).result;
  check(snap.queued.every((q) => q.steer) && snap.queued.length === 2, "mock: steer_queued moves it into steering", snap.queued);
  const bad = await c.rpc("dequeue", { session_id: SID, index: 9 });
  check(bad.error?.code === "not_found", "mock: out-of-range index is not_found");
  // Send now: it goes first (being sent), the turn stops, and the next one starts with it.
  await c.rpc("send", { session_id: SID, content: [{ type: "text", text: "later" }], delivery: "follow_up", attachments: [] });
  await c.rpc("send", { session_id: SID, content: [{ type: "text", text: "right now" }], delivery: "follow_up", attachments: [] });
  await c.rpc("interrupt_queued", { session_id: SID, index: 3 });
  snap = (await c.rpc("get_session", { session_id: SID, since_seq: 999 })).result;
  check(snap.queued[0]?.text === "right now" && snap.queued[0]?.interrupt === true && snap.queued.length === 4, "mock: interrupt_queued shows it first, being sent", snap.queued);
  await sleep(700);
  snap = (await c.rpc("get_session", { session_id: SID })).result;
  const said = snap.entries.filter((e) => e.kind === "user").map((e) => e.content.map((x) => x.text).join(""));
  check(said.at(-1) === "right now" && snap.info.status === "running", "mock: the next turn started with it, the chat still busy", said.slice(-2));
  check(!snap.queued.some((q) => q.interrupt) && snap.queued.some((q) => q.text === "later" && !q.steer), "mock: the other messages stay queued", snap.queued);
  // Stop right after Send now: it waits in the queue instead.
  await c.rpc("send", { session_id: SID, content: [{ type: "text", text: "stop wins" }], delivery: "interrupt", attachments: [] });
  await c.rpc("abort", { session_id: SID });
  await sleep(700);
  snap = (await c.rpc("get_session", { session_id: SID, since_seq: 999 })).result;
  check(snap.info.status === "idle" && snap.queued[0]?.text === "stop wins" && !snap.queued[0]?.interrupt, "mock: Stop keeps what was being sent queued", snap.queued);
  // Idle: Send now simply sends it.
  await c.rpc("interrupt_queued", { session_id: SID, index: 0 });
  await sleep(300);
  snap = (await c.rpc("get_session", { session_id: SID })).result;
  check(snap.info.status === "running" && snap.entries.filter((e) => e.kind === "user").at(-1)?.content[0]?.text === "stop wins", "mock: Send now on an idle chat sends it");
  await c.rpc("abort", { session_id: SID });
  await sleep(600);
  c.close();
  await mock("/__mock/reset");

  for (const [label, w, h, mobile] of [["desktop", 1440, 900, false], ["phone", 344, 882, true], ["fold", 884, 1104, true]]) {
    for (const theme of ["dark", "light"]) {
      await mock("/__mock/reset");
      await mock("/__mock/mosaics/reset");
      const p = await device();
      await openChat(p, label, w, h, mobile, theme);
      await typeSend(p, "stream the answer slowly please", mobile);
      await sleep(1200);
      const ph = await p.eval(`document.querySelector('.pane.focused .composer textarea').placeholder`);
      if (theme === "dark") check(!/steer the agent/i.test(ph), `${label}: no "steer by default" placeholder (${ph})`);
      await typeSend(p, "Also check the docs folder when you're done, and summarise what changed in the README over the last two releases, with links to the relevant commits.", mobile);
      await typeSend(p, "Then run the linter.", mobile);
      await typeSend(p, "/steer skip the benchmarks, they take too long", mobile);
      await typeSend(p, "Say hi in Greek.", mobile);
      await sleep(400);
      let q = await queued(p);
      if (theme === "dark") {
        check(q.filter((x) => !x.steer).length === 3, `${label}: Enter queued three follow-ups`, q);
        check(q.some((x) => x.steer && /benchmarks/.test(x.text)), `${label}: /steer steered`, q);
      }
      // Compact: words where there is room, icons on narrow tiles and phones (named for screen readers).
      const btns = await p.eval(`(() => { const c = ${cardSel("/linter/")}; return ['Edit', 'Steer', 'Send now', 'Remove'].map(l => { const b = c?.querySelector('[aria-label="' + l + '"]'); const s = b?.querySelector('span'); const r = b?.getBoundingClientRect(); return { l, ok: !!b, w: r?.width ?? 0, h: r?.height ?? 0, words: !!s && getComputedStyle(s).display !== 'none' }; }); })()`);
      const ink = await p.eval(`(() => { const c = ${cardSel("/linter/")}; const a = c.querySelector('.qacts'); return Number(getComputedStyle(a).opacity); })()`);
      check(ink === 1, `${label} ${theme}: available queue actions keep full contrast before hover`, ink);
      check(btns.every((b) => b.ok), `${label}: a queued card has Edit, Steer, Send now and Remove`, btns);
      const tile = await p.eval(`Math.round(document.querySelector('.pane.focused').getBoundingClientRect().width)`);
      const wide = tile > 560;
      check(btns.filter((b) => b.l === "Steer" || b.l === "Send now").every((b) => b.words === wide), `${label}: Steer and Send now ${wide ? "with words" : "as icons only"} (tile ${tile}px)`, btns);
      if (mobile) check(btns.every((b) => b.w >= 44 && b.h >= 44), `${label} ${theme}: all four queue controls have real 44px touch targets`, btns);
      const tips = await p.eval(`(() => { const c = ${cardSel("/linter/")}; return [c.querySelector('[aria-label="Steer"]').dataset.tip, c.querySelector('[aria-label="Send now"]').dataset.tip]; })()`);
      if (theme === "dark") check(/next step, without stopping/.test(tips[0]) && /Stop the agent and send this now/.test(tips[1]), `${label}: tooltips say what each does`, tips);
      const noSteerOnSteer = await p.eval(`(() => { const c = ${cardSel("/benchmarks/")}; return !c.querySelector('[aria-label="Steer"]') && !!c.querySelector('[aria-label="Send now"]'); })()`);
      check(noSteerOnSteer, `${label}: a steering card can be sent now, not steered again`);
      await sleep(250);
      await p.shot(`${SHOTS}/queue-cards-${label}-${theme}.png`);
      if (theme === "dark") {
        // inline edit of the second follow-up
        await clickIn(p, "/linter/", "Edit");
        await sleep(200);
        if (mobile) {
          const editTargets = await p.eval(`(() => { const c = document.querySelector('.pane.focused .qedit').closest('.qcard'); return [...c.querySelectorAll('.qbtn')].map(b => { const r = b.getBoundingClientRect(); return { text: b.textContent.trim(), w: r.width, h: r.height }; }); })()`);
          check(editTargets.length === 2 && editTargets.every(b => b.w >= 44 && b.h >= 44), `${label} ${theme}: queue Save and Cancel also have real 44px touch targets`, editTargets);
        }
        await p.shot(`${SHOTS}/queue-editing-${label}-${theme}.png`);
        await p.eval(`(() => { const t = document.querySelector('.pane.focused .qinput'); t.select(); })()`);
        await p.type("Then run the linter and fix warnings.");
        await p.key("Enter", "Enter", 0, 13);
        await sleep(500);
        q = await queued(p);
        check(q.some((x) => x.text === "Then run the linter and fix warnings." && !x.steer), `${label}: edit saved with Enter`, q);
        // Esc cancels an edit
        await clickIn(p, "/docs folder/", "Edit");
        await sleep(150);
        await p.type(" NOPE");
        await p.key("Escape", "Escape", 0, 27);
        await sleep(300);
        q = await queued(p);
        check(!q.some((x) => /NOPE/.test(x.text)), `${label}: Esc cancels the edit`, q);
        // Steer
        await clickIn(p, "/linter/", "Steer");
        await sleep(450);
        q = await queued(p);
        check(q.some((x) => /linter/.test(x.text) && x.steer), `${label}: Steer moved it into steering`, q);
        await p.shot(`${SHOTS}/queue-steered-${label}-${theme}.png`);
        // remove
        await clickIn(p, "/linter/", "Remove");
        await sleep(500);
        q = await queued(p);
        check(!q.some((x) => /linter/.test(x.text)), `${label}: Remove withdrew it`, q);
        const shown = await cards(p);
        check(shown.length === q.length, `${label}: cards match the queue after animations (${shown.length}/${q.length})`, shown);
      }
      // Send now: the card says it is being sent, then the next turn has it and the card goes.
      await clickIn(p, "/Greek/", "Send now");
      await sleep(60);
      const sending = await p.eval(`(() => { const c = ${cardSel("/Greek/")}; return c ? { tag: c.querySelector('.qtag').textContent.trim(), busy: c.getAttribute('aria-busy'), acts: c.querySelectorAll('.qacts button').length, first: c === document.querySelector('.pane.focused .qcard') } : null; })()`);
      check(sending?.tag === "Sending" && sending.busy === "true" && sending.acts === 0 && sending.first, `${label}: Send now shows it first, being sent`, sending);
      await p.shot(`${SHOTS}/queue-sending-${label}-${theme}.png`);
      const greek = await until(async () => (await users(p)).at(-1) === "Say hi in Greek.");
      check(greek, `${label}: Send now delivered it as the next message`, await users(p));
      await sleep(350);
      const after = await cards(p);
      check(!after.some((x) => /Greek/.test(x.text)), `${label}: its card is gone once delivered`, after);
      check(after.some((x) => /docs folder/.test(x.text) && x.tag === "Queued"), `${label}: the other follow-ups stay queued`, after);
      check(await status(p) === "running", `${label}: the agent works on it`);
      // The stale-card regression: when the turn ends, the next follow-up starts the next turn, and its card goes.
      const docs = await until(async () => (await users(p)).at(-1)?.startsWith("Also check the docs folder"), 30000);
      check(docs, `${label}: the queued follow-up was delivered when the agent was done`, await users(p));
      await sleep(350);
      const left = await cards(p);
      check(!left.some((x) => /docs folder/.test(x.text)), `${label}: a delivered follow-up leaves no card behind`, left);
      check(await status(p) === "running", `${label}: (while the agent is still working on it)`);
      if (theme === "dark") await p.shot(`${SHOTS}/queue-delivered-${label}-${theme}.png`);
      await p.eval(`document.querySelector('.pane.focused .send-btn.stop')?.click()`);
      await sleep(500);
      console.log(label, theme, "errors", p.errors);
      await p.close();
    }
  }

  // Keyboard: Ctrl+Shift+Enter in the composer stops the agent and sends it now.
  {
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await openChat(p, "keys", 1440, 900, false, "dark");
    await typeSend(p, "stream the answer slowly please", false);
    await sleep(1500);
    await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
    await p.type("Never mind, say hi in Greek.");
    await p.key("Enter", "Enter", 2 | 8, 13);
    await sleep(120);
    const q = await queued(p);
    check(q.length === 1 && q[0].interrupt, "keys: Ctrl+Shift+Enter sends it now (shown as being sent)", q);
    check(await until(async () => (await users(p)).at(-1) === "Never mind, say hi in Greek.", 5000), "keys: …and the next turn starts with it", await users(p));
    check(!(await cards(p)).length, "keys: no card left");
    const tip = await p.eval(`document.querySelector('.pane.focused .send-btn:not(.stop)')?.dataset.tip ?? ''`);
    await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
    await p.type("x");
    const tip2 = await p.eval(`document.querySelector('.pane.focused .send-btn:not(.stop)')?.dataset.tip ?? ''`);
    check(/send now/i.test(tip || tip2), "keys: the send button's tip names the shortcut", tip || tip2);
    await p.eval(`document.querySelector('.pane.focused .send-btn.stop')?.click()`);
    await sleep(400);
    await p.close();
  }

  // Reconnect mid-turn and a snapshot crossing a turn's last events: the card still goes,
  // on this device and a second one.
  {
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    const p2 = await device();
    await openChat(p, "race", 1440, 900, false, "dark");
    await openChat(p2, "race-2", 375, 812, true, "light");
    const c2 = await wsClient("queue-race");
    await typeSend(p, "stream the answer slowly please", false);
    await sleep(1500);
    await typeSend(p, "Then run the linter.", false);
    await typeSend(p, "Say hi in Greek.", false);
    await sleep(300);
    check((await cards(p2)).length === 2, "second device: shows the same queue", await cards(p2));
    // Events lost while this device was away: the follow-up is delivered meanwhile.
    await c2.rpc("mock_composer", { mute: { session_id: SID, on: true } });
    await sleep(100);
    await c2.rpc("interrupt_queued", { session_id: SID, index: 1 });
    await sleep(900);
    check((await cards(p)).some((x) => /Greek/.test(x.text)), "reconnect: (with its events lost, the card is still shown)");
    await c2.rpc("mock_composer", { mute: { session_id: SID, on: false }, resync: true });
    check(await until(async () => !(await cards(p)).some((x) => /Greek/.test(x.text)), 5000), "reconnect: after a resync the delivered card is gone", await cards(p));
    check(await until(async () => !(await cards(p2)).some((x) => /Greek/.test(x.text)), 5000), "second device: gone there too", await cards(p2));
    // A snapshot asked before the delivery arrives after it (stale): the card must not come back.
    await c2.rpc("mock_composer", { slow_snapshots: 1500, resync: true });
    await sleep(150);
    await c2.rpc("interrupt_queued", { session_id: SID, index: 0 });
    await sleep(2200);
    const mid = await cards(p);
    await sleep(2000);
    const end = await cards(p);
    check(!end.some((x) => /linter/.test(x.text)) && (await users(p)).at(-1) === "Then run the linter.", "snapshot race: the delivered card does not come back", { mid, end });
    check(!(await cards(p2)).length, "second device: no card left", await cards(p2));
    await c2.rpc("mock_composer", { slow_snapshots: 0 });
    await p.eval(`document.querySelector('.pane.focused .send-btn.stop')?.click()`);
    await sleep(400);
    c2.close();
    console.log("race errors", p.errors, p2.errors);
    await p.close();
    await p2.close();
  }
  finish();
});
