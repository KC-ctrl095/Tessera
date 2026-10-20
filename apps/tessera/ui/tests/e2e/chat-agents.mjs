// Subagents pinned under the chat's header while they work: a row each (name,
// what it is doing, how long), folding to the names, a tap opens that
// subagent's chat, it stays while the chat scrolls and goes when the last one
// is done. A swarm shows only the rows in view. Desktop, phone and Fold, dark
// and light. ONLY=phone-dark runs one of them.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const SID = "ses_7d3a91e5c4b2"; // "Add a /healthz endpoint"
const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const open = (p, sid) => ws(p, `return m.openSession('${sid}', { pane: m.workspace.focused() })`);
const rect = (p, sel) =>
  p.eval(`(() => { const e = document.querySelector('.pane.focused ${sel}'); if (!e) return null; const r = e.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height, b: r.bottom }; })()`);
const count = (p, sel) => p.eval(`document.querySelectorAll(${JSON.stringify(sel)}).length`);
const text = (p, sel) => p.eval(`document.querySelector(${JSON.stringify(sel)})?.textContent ?? null`);
const until = (p, expr, ms = 4000) => p.waitFor(expr, ms).then(() => true, () => false);

async function send(p, mobile, msg) {
  await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
  await p.type(msg);
  // Enter sends on a desktop; on a phone it is a new line and the button sends.
  if (mobile) await p.click(".pane.focused .send-btn");
  else await p.key("Enter", "Enter", 0, 13);
}

run(async () => {
  for (const [label, w, h, mobile, dpr] of [["desktop", 1440, 900, false, 1], ["phone", 375, 812, true, 2], ["fold", 841, 701, true, 2.625]]) {
    for (const theme of ["dark", "light"]) {
      const tag = `${label}-${theme}`;
      if (process.env.ONLY && !tag.includes(process.env.ONLY)) continue;
      console.log(`— ${label} ${theme}`);
      const first = theme === "dark";
      await mock("/__mock/reset");
      await mock("/__mock/mosaics/reset");
      const probe = await wsClient("chat-agents probe");
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Chat agents ${label}`);
      await p.theme(theme);
      // A chat's subagents come with the chat (they aren't in the session list): a device that never
      // saw them live counts the same ones, in the state they are in now.
      if (first && label === "desktop") {
        await open(p, "ses_7a1c5e93b2d0");
        await sleep(1200);
        const kids = await p.eval(`__mod('lib/store.ts').then((m) => ['ses_c2e8a4f61b37', 'ses_5d9b0f3e7a24'].map((id) => m.state.sessions[id]?.info.kind ?? null))`);
        check(kids.every((k) => k === "subagent"), `${tag}: a chat's subagents come with the chat`, kids);
      }
      await open(p, SID);
      await sleep(1300);
      await send(p, mobile, "/goal Ship the healthz endpoint");
      await until(p, `document.querySelector('.pane.focused .composer-dock .goal')`);

      // ── subagents pinned under the header while they work ──
      await send(p, mobile, "delegate the reconnect research in parallel");
      await p.waitFor(`document.querySelector('.pane.focused .agents-bar')`, 25000);
      await sleep(1800);
      // Measured once it has slid into place (a slow machine draws its first frames late).
      await until(p, `document.querySelector('.pane.focused .agents-bar')?.getAnimations().every((a) => a.playState !== 'running')`, 6000);
      const bar = await rect(p, ".agents-bar");
      const head2 = await rect(p, ".pane-head");
      check(!!bar && Math.abs(bar.y - head2.b) <= 1, `${tag}: the bar is pinned right under the header`, { bar, head2 });
      // Folded at first everywhere (desktop and phone): one line of names, never pushing the chat down.
      check((await count(p, ".pane.focused .agent-row")) === 0, `${tag}: it starts as one line`);
      check(bar.h <= 40, `${tag}: …(${Math.round(bar.h)}px)`);
      check(/Map the reconnect path/.test((await text(p, ".pane.focused .agents-names")) ?? ""), `${tag}: …naming who is working`);
      await p.shot(`${SHOTS}/chatui-bar-folded-${tag}.png`);
      await p.click(".pane.focused .agents-head");
      await until(p, `document.querySelectorAll('.pane.focused .agent-row').length >= 1`);
      const rows = await p.eval(`[...document.querySelectorAll('.pane.focused .agent-row')].map((r) => ({ name: r.querySelector('.agent-name').textContent, role: r.querySelector('.agent-role')?.textContent ?? '', doing: r.querySelector('.agent-doing').textContent, time: r.querySelector('.agent-time').textContent, status: r.dataset.status }))`);
      check(rows.length === 3 && rows.every((r) => r.name && r.doing), `${tag}: a row each: name and what it is doing`, rows);
      check(JSON.stringify(rows.map((r) => r.role).filter(Boolean).sort()) === JSON.stringify(["Explore", "Review"]), `${tag}: …and its role, when it was started as one (explore, review)`, rows);
      check(rows.some((r) => /\d+s/.test(r.time)), `${tag}: …with the time it has been at it`, rows);
      const rowsNow = () => count(p, ".pane.focused .agent-row");
      /** Wait (the machine may be busy) until the bar has this many rows. */
      const rowsAre = async (n) => {
        for (let k = 0; k < 40; k++) {
          if ((await rowsNow()) === n) return true;
          await sleep(150);
        }
        return false;
      };
      // (First, while they still work: a batch is over within seconds.)
      await p.click(".pane.focused .agents-head");
      check((await rowsAre(0)) && !!(await text(p, ".pane.focused .agents-names")), `${tag}: it folds to the names`);
      await p.click(".pane.focused .agents-head");
      check(await rowsAre(3), `${tag}: and opens again`);
      const barOpen = (await rect(p, ".agents-bar")) ?? { h: 0 };
      check(barOpen.h <= (mobile ? 150 : 128), `${tag}: three rows stay slim (${Math.round(barOpen.h)}px)`);
      await p.shot(`${SHOTS}/chatui-bar-${tag}.png`);
      // Pinned: the transcript scrolls under it.
      await p.eval(`document.querySelector('.pane.focused .transcript').scrollTop = 0`);
      await sleep(250);
      const pinned = await rect(p, ".agents-bar");
      check(!pinned || Math.abs(pinned.y - bar.y) <= 1, `${tag}: it stays put when the chat scrolls`, { pinned, bar });
      // The clock moves.
      const t1 = await text(p, ".pane.focused .agent-time");
      await sleep(2200);
      const t2 = await text(p, ".pane.focused .agent-time");
      check(t1 !== t2 || !t1, `${tag}: its clock keeps moving (${t1} → ${t2})`);
      // Fold and open it.
      if (label === "desktop" && first) {
        // Tapping a row opens that subagent's own chat.
        const kids = await p.eval(`__mod('lib/store.ts').then((m) => Object.values(m.state.sessions).map((s) => s.info).filter((i) => i.parent_id === '${SID}' && i.kind === 'subagent').map((i) => i.id))`);
        await p.click(".pane.focused .agent-row");
        await sleep(900);
        const shown = await ws(p, `return Object.values(m.workspace.panes).some((pn) => pn.content.type === 'chat' && ${JSON.stringify(kids)}.includes(pn.content.sessionId))`);
        check(shown, `${tag}: a row opens that subagent's chat`);
        // Back on the parent: the bar goes when the last one is done.
        await ws(p, `m.focusPane(Object.keys(m.workspace.panes).find((id) => m.workspace.panes[id].content.sessionId === '${SID}'))`);
        await sleep(300);
        let gone = false;
        for (let k = 0; k < 140 && !gone; k++) {
          await sleep(500);
          gone = !(await p.eval(`!!document.querySelector('[data-pane-id] .agents-bar')`));
        }
        check(gone, `${tag}: the bar goes when the last subagent is done`);
        check((await count(p, ".agent-row")) === 0, `${tag}: …and nothing of it is left`);
      }

      // A swarm: folded to one line; a few rows at a time; the DOM stays small.
      if (first && label !== "fold") {
        await probe.rpc("abort", { session_id: SID }); // the first batch (phones didn't wait it out)
        await p.waitFor(`!document.querySelector('.pane.focused .agents-bar')`, 8000);
        await send(p, mobile, "swarm: fan out the audit");
        await p.waitFor(`document.querySelectorAll('.pane.focused .agents-bar').length === 1`, 25000);
        await sleep(2500);
        const sum = await text(p, ".pane.focused .agents-sum");
        check(/working/.test(sum ?? "") && /waiting/.test(sum ?? ""), `${tag}: a swarm reads "${sum}"`);
        if ((await count(p, ".pane.focused .agent-row")) === 0) await p.click(".pane.focused .agents-head");
        await sleep(400);
        const nrows = await count(p, ".pane.focused .agent-row");
        const sw = await rect(p, ".agents-bar");
        check(nrows > 0 && nrows <= 12, `${tag}: only the rows in view exist (${nrows} of 32)`);
        check(sw.h <= 200, `${tag}: …and the bar stays short (${Math.round(sw.h)}px)`);
        await p.shot(`${SHOTS}/chatui-bar-swarm-${tag}.png`);
        await p.eval(`document.querySelector('.pane.focused .send-btn.stop')?.click()`);
        await sleep(800);
      }

      console.log("errors", p.errors);
      check(!p.errors.length, `${tag}: no page errors`, p.errors);
      probe.close();
      await p.close();
    }
  }
  finish();
});
