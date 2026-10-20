// Engine additions: search API keys, usage cost, done-when criteria, harness notes, model fallback notice.
import { MOCK, SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

run(async () => {
  for (const [label, w, h, mobile] of [["desktop", 1440, 900, false], ["phone", 375, 812, true]]) {
    for (const theme of ["dark", "light"]) {
      await mock("/__mock/reset");
      await mock("/__mock/mosaics/reset");
      const p = await device();
      await p.viewport(w, h, mobile, mobile ? 2 : 1);
      await signIn(p, `Extras ${label}`);
      await p.theme(theme);
      // SQLite chat: fallback notice, completion check, criteria, cost
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_8c1f2a9d4e70', { pane: m.workspace.focused() }))`);
      await sleep(1300);
      await p.eval(`document.querySelector('.pane.focused .harness')?.scrollIntoView({ block: 'center' })`);
      await sleep(300);
      const sw = await p.eval(`document.querySelector('.pane.focused .notice-switch')?.textContent`);
      const hn = await p.eval(`document.querySelector('.pane.focused .harness-head')?.textContent`);
      const raw = await p.eval(`[...document.querySelectorAll('.pane.focused .msg-user')].some(e => e.textContent.includes('[Completion check]'))`);
      if (theme === "dark") {
        check(/Continued with/.test(sw ?? "") && /failed/.test(sw ?? ""), `${label}: the model switch reads as one calm line (${sw})`);
        check(!hn && !raw, `${label}: completion protocol is not a system or user row`);
      }
      await p.shot(`${SHOTS}/extras-harness-${label}-${theme}.png`);
      // criteria in the goal panel
      await p.click(".pane.focused .goal-chev");
      await sleep(350);
      await p.eval(`document.querySelector('.pane.focused .criteria')?.scrollIntoView({ block: 'center' })`);
      await sleep(150);
      const crit = await p.eval(`[...document.querySelectorAll('.pane.focused .crit')].map(e => e.className)`);
      if (theme === "dark") check(crit.length === 3 && crit[0].includes("crit-met") && crit[1].includes("crit-unverifiable"), `${label}: done-when criteria show their state`, crit);
      await p.shot(`${SHOTS}/extras-criteria-${label}-${theme}.png`);
      await p.click(".pane.focused .goal-chev");
      await sleep(200);
      // cost in the options popover
      await p.click(".pane.focused .opts-btn, .pane.focused .title-menu");
      await sleep(400);
      // Chat length and cost are directly reachable.
      const cost = await p.eval(`document.querySelector('.opt-cost')?.textContent ?? null`);
      if (theme === "dark") check(/\$\d/.test(cost ?? ""), `${label}: session cost shows on the chat length line (${cost})`);
      await p.shot(`${SHOTS}/extras-cost-${label}-${theme}.png`);
      await p.key("Escape", "Escape", 0, 27);
      await sleep(250);
      // a local model's chat shows no cost at all
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_91d0c3e7a8b2', { pane: m.workspace.focused() }))`);
      await sleep(900);
      await p.click(".pane.focused .opts-btn, .pane.focused .title-menu");
      await sleep(300);
      const localCost = await p.eval(`document.querySelector('.opt-cost')?.textContent ?? null`);
      if (theme === "dark") check(localCost === null, `${label}: a local model shows no cost (not "$0")`);
      await p.key("Escape", "Escape", 0, 27);
      await sleep(200);
      // Completion protocol needs no separate row, mark or footer.
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_2b7e91c04d5a', { pane: m.workspace.focused() }))`);
      await sleep(1000);
      const quiet = await p.eval(`(() => {
        const pane = document.querySelector('.pane.focused');
        const mark = [...pane.querySelectorAll('.msg-assistant')].some((e) => e.textContent.trim() === '✓');
        return { rows: pane.querySelectorAll('.harness').length, chips: pane.querySelectorAll('.turn-check').length, mark,
          steps: [...pane.querySelectorAll('.steps-detail, .tool-name')].some((e) => /task/i.test(e.textContent)) };
      })()`);
      check(quiet.rows === 0 && quiet.chips === 0 && !quiet.mark && !quiet.steps, `${label}-${theme}: completion protocol leaves no rows, marks or redundant footer`, quiet);
      await p.shot(`${SHOTS}/extras-quiet-check-${label}-${theme}.png`);
      // harness restarted note
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_a6c3f0e82b19', { pane: m.workspace.focused() }))`);
      await sleep(1000);
      await p.shot(`${SHOTS}/extras-restarted-${label}-${theme}.png`);
      // web search keys
      await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings('search'))`);
      await sleep(900);
      // Backup search keys are under Web & voice's More.
      await p.eval(`[...document.querySelectorAll('.settings-body .disclosure')].find(d => /backup search keys/.test(d.textContent))?.click()`);
      await sleep(300);
      await p.eval(`document.querySelector('.search-key')?.scrollIntoView({ block: 'center' })`);
      await p.shot(`${SHOTS}/extras-search-keys-${label}-${theme}.png`);
      if (theme === "dark") {
        await p.click(`input[aria-label="Brave Search API key"]`);
        await p.type("BSA-test-key-123");
        await p.clickText(".search-key button", "Save");
        await sleep(700);
        const st = await p.eval(`({ value: document.querySelector('input[aria-label="Brave Search API key"]').value, saved: document.querySelector('.search-key .search-key-state.on')?.textContent ?? null, page: document.body.innerText.includes('BSA-test-key-123') })`);
        check(st.value === "" && /Saved/.test(st.saved ?? "") && !st.page, `${label}: a key is saved write-only (field cleared, never shown)`, st);
        const c = await wsClient("keys");
        const info = (await c.rpc("get_web_search")).result;
        check(JSON.stringify(info.keys) === JSON.stringify(["brave"]), `${label}: the server has the Brave key`, info.keys);
        await p.shot(`${SHOTS}/extras-search-keys-saved-${label}-${theme}.png`);
        await p.clickText(".search-key button", "Remove");
        await sleep(700);
        const info2 = (await c.rpc("get_web_search")).result;
        check(info2.keys.length === 0, `${label}: Remove clears it (empty key)`);
        c.close();
      }
      console.log(label, theme, "errors", p.errors);
      await p.close();
    }
  }
  finish();
});
