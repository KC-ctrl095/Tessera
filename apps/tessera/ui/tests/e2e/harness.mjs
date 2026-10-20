// What the agent says stays on screen (contract 7, item 11): the words it
// writes between tool calls aren't folded into "N steps" when a subagent's
// report, a notice or the end of the turn closes the run, and streamed text
// never blinks out while it is committed. "Compacting context" counts from
// when compacting began, not from the start of the turn.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const SID = "ses_2b7e91c04d5a";
const open = (p) => p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${SID}', { pane: m.workspace.focused() }))`);
const SAID = [
  "Found it: the parser skips the last line when the file has no trailing newline.",
  "Fixed the off-by-one; all 14 parser tests pass now.",
  "Done: the parser keeps the last line.",
];
const transcript = (p) => p.eval(`document.querySelector('.pane.focused .transcript')?.textContent ?? ''`);

run(async () => {
  for (const [label, w, h, mobile] of [["desktop", 1440, 900, false], ["phone", 375, 812, true]]) {
    await mock("/__mock/reset");
    const ws = await wsClient("harness test");
    const p = await device();
    await p.viewport(w, h, mobile, mobile ? 2 : 1);
    await signIn(p, `Harness ${label}`);
    await p.theme(label === "phone" ? "dark" : "light");
    await open(p);
    await sleep(1200);

    // Every frame: once a sentence has been on screen, it must stay there.
    await p.eval(`(() => {
      const said = ${JSON.stringify(SAID)};
      window.__seen = {}; window.__lost = [];
      const tick = () => {
        const t = document.querySelector('.pane.focused .transcript')?.textContent ?? '';
        for (const s of said) {
          if (t.includes(s)) window.__seen[s] = true;
          else if (window.__seen[s] && !window.__lost.includes(s)) window.__lost.push(s);
        }
        requestAnimationFrame(tick);
      };
      tick();
      return 1;
    })()`);
    await ws.rpc("mock_harness", { narrate: SID, gap: 450 });
    await p.waitFor(`document.querySelector('.pane.focused .transcript')?.textContent.includes(${JSON.stringify(SAID[1])})`, 15000);
    await p.shot(`${SHOTS}/harness-midturn-${label}.png`);
    await p.waitFor(`document.querySelector('.pane.focused .transcript')?.textContent.includes(${JSON.stringify(SAID[2])})`, 15000);
    await p.waitFor(`!document.querySelector('.pane.focused .send-btn.stop')`, 8000);
    await sleep(800);

    const seen = await p.eval(`Object.keys(window.__seen).length`);
    check(seen === SAID.length, `${label}: all ${SAID.length} things the agent said were shown`, seen);
    const lost = await p.eval(`window.__lost`);
    check(lost.length === 0, `${label}: none of it vanished, not even for a frame (report, notice, end of turn)`, lost);
    const text = await transcript(p);
    for (const s of SAID) check(text.includes(s), `${label}: still on screen after the turn: "${s.slice(0, 44)}…"`);
    const groups = await p.eval(`document.querySelectorAll('.pane.focused .transcript .steps').length`);
    check(groups >= 1, `${label}: tool calls still fold into step groups (${groups})`);
    // The words come in order around the groups: said, steps, report, said, steps.
    const order = await p.eval(`(() => {
      const t = document.querySelector('.pane.focused .transcript').textContent;
      return ${JSON.stringify(SAID)}.map((s) => t.indexOf(s));
    })()`);
    check(order[0] >= 0 && order[0] < order[1] && order[1] < order[2], `${label}: in the order they were said`, order);
    await p.shot(`${SHOTS}/harness-narration-${label}.png`);

    // Compacting late in a long turn: the clock starts when compacting does.
    await ws.rpc("mock_harness", { compacting: SID, after: 4000, for: 3000 });
    await p.waitFor(`document.querySelector('.pane.focused .activity-verb')?.textContent === 'Compacting context'`, 9000);
    await sleep(2500);
    const t = await p.eval(`document.querySelector('.pane.focused .activity-time')?.textContent ?? ''`);
    const secs = Number.parseInt(t, 10);
    // Counted from the turn's start it would read 6s or more.
    check(secs >= 1 && secs <= 4, `${label}: "Compacting context · ${t}" counts from when it began, not from the turn's start (4s earlier)`, t);
    await p.shot(`${SHOTS}/harness-compacting-${label}.png`);
    await p.waitFor(`!document.querySelector('.pane.focused .send-btn.stop')`, 8000);

    // A `read` of several files is named by its files on its card.
    const several = await p.eval(`__mod('lib/tools.ts').then(m => [
      m.toolSummary('read', { paths: ['src/a.py', 'docs/b.md'] }),
      m.toolSummary('read', { path: 'a.py', paths: ['b.py', 'a.py'] }),
      m.toolSummary('read', { path: 'a.py' }),
    ])`);
    check(
      JSON.stringify(several) === JSON.stringify(["src/a.py, docs/b.md", "a.py, b.py", "a.py"]),
      `${label}: a read of several files shows them all on its card`,
      several,
    );

    console.log("errors", p.errors);
    check(!p.errors.length, `${label}: no page errors`, p.errors);
    ws.close();
    await p.close();
  }
  finish();
});
