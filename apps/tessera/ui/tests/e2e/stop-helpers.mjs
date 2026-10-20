// Stop is a whole-turn action in both engines. Its persistent notice names
// working and waiting helpers, without adding a cramped phone dialog.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";
const SID = "ses_7d3a91e5c4b2";
const CLAUDE = "ses_cc4b2e81d9f3";
const api = (p, js) => p.eval(`__mod('lib/store.ts').then(m => ${js})`);
const open = (p, id) => p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${id}', { pane: m.workspace.focused() }))`);
const until = (p, expr, ms = 8000) => p.waitFor(expr, ms).then(() => true, () => false);

run(async () => {
  for (const [label, w, h, mobile, dpr] of [["desktop", 1440, 900, false, 1], ["phone", 344, 760, true, 2.6], ["fold", 841, 701, true, 2.625]]) {
    for (const theme of ["dark", "light"]) {
      const tag = `${label}-${theme}`;
      await mock("/__mock/reset");
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Stop helpers ${label}`);
      await p.theme(theme);
      await open(p, SID);
      await sleep(900);
      await api(p, `m.api.setHelpers(2)`);
      await api(p, `m.api.send('${SID}', [{ type: 'text', text: 'delegate the reconnect research in parallel and wait' }])`);
      check(await until(p, `document.querySelectorAll('.pane.focused .subagent').length === 3`, 25000), `${tag}: three native helpers, including one waiting`);
      const snapshot = await api(p, `m.api.getSession('${SID}')`);
      const names = snapshot.children.map((i) => i.title.replace(/^↳ /, ""));
      check(snapshot.children.some((i) => i.helper_queued && i.status === "running" && i.steps === 0), `${tag}: a helper is waiting for a slot (queued, still running for older apps)`);
      await p.click(".pane.focused .send-btn.stop");
      const expected = `Stopping this chat, including 3 helpers: ${names.join(', ')}.`;
      check(await until(p, `document.querySelector('.pane.focused .transcript')?.textContent.includes(${JSON.stringify(expected)})`), `${tag}: Stop names all three helpers in the chat`);
      check(await until(p, `!document.querySelector('.pane.focused .agents-bar')`), `${tag}: all native helpers stop`);
      if (await p.eval(`!!document.querySelector('.pane.focused .jump-latest')`)) await p.click('.pane.focused .jump-latest');
      await sleep(300);
      // The notice wraps, remains readable, and survives loading the chat again.
      check(await p.eval(`(() => { const t = document.querySelector('.pane.focused .transcript'); return t.scrollWidth <= t.clientWidth + 1; })()`), `${tag}: no sideways transcript overflow`);
      check(await p.eval(`(() => { const n = [...document.querySelectorAll('.pane.focused .notice')].find(e => e.textContent.includes(${JSON.stringify(expected)})); const r = n?.getBoundingClientRect(), t = document.querySelector('.pane.focused .transcript').getBoundingClientRect(); return r && r.top >= t.top && r.bottom <= t.bottom + 1; })()`), `${tag}: helper notice is fully readable above the composer`);
      await p.shot(`${SHOTS}/stop-helpers-native-${tag}.png`);
      const after = await api(p, `m.api.getSession('${SID}')`);
      check(after.entries.some((e) => e.kind === "notice" && e.text === expected), `${tag}: the notice is saved, not a toast`);

      // Claude's own watch-only helper ends with its parent too.
      await mock("/__mock/claude-code?state=ready");
      await open(p, CLAUDE);
      await sleep(700);
      await mock("/__mock/claude-helper");
      await api(p, `m.api.send('${CLAUDE}', [{ type: 'text', text: 'stream a long reply' }])`);
      check(await until(p, `document.querySelector('.pane.focused .send-btn.stop')`), `${tag}: Claude chat offers Stop`);
      await p.click(".pane.focused .send-btn.stop");
      const ccNotice = "Stopping this chat, including 1 helper: Check the backups.";
      check(await until(p, `document.querySelector('.pane.focused .transcript')?.textContent.includes(${JSON.stringify(ccNotice)})`), `${tag}: Claude Stop names its helper`);
      const cc = await api(p, `m.api.getSession('${CLAUDE}')`);
      check(cc.children[0]?.status === "interrupted", `${tag}: Claude helper is stopped too`);
      check(await p.eval(`(() => { const t = document.querySelector('.pane.focused .transcript'); return t.scrollWidth <= t.clientWidth + 1; })()`), `${tag}: Claude notice also fits`);
      await p.shot(`${SHOTS}/stop-helpers-claude-${tag}.png`);
      check(!p.errors.length, `${tag}: no page errors`, p.errors);
      await p.close();
    }
  }
  finish();
});
