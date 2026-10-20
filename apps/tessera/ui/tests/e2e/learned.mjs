import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";
run(async () => {
  await mock("/__mock/reset");
  for (const [label, w, h, mobile] of [["desktop", 1440, 900, false], ["phone", 375, 812, true], ["fold", 841, 701, true]]) {
    for (const theme of ["dark", "light"]) {
      await mock("/__mock/mosaics/reset");
      const p = await device();
      await p.viewport(w, h, mobile, mobile ? 2 : 1);
      await signIn(p, `Learned ${label}`);
      await p.theme(theme);
      // new-format notice (Plan my week)
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_6f2c0a9d3e18', { pane: m.workspace.focused() }))`);
      await sleep(1300);
      const head = await p.eval(`document.querySelector('.pane.focused .learned-head')?.textContent`);
      if (theme === "dark") check(/Remembered 4 things/.test(head ?? "") && /forgot 1/.test(head ?? ""), `${label}: new notice summarised (${head})`);
      await p.shot(`${SHOTS}/learned-collapsed-${label}-${theme}.png`);
      await p.click(".pane.focused .learned-head");
      await sleep(400);
      await p.eval(`document.querySelector('.pane.focused .learned-list')?.scrollIntoView({block:'center'})`);
      await sleep(200);
      const n = await p.eval(`document.querySelectorAll('.pane.focused .learned-item').length`);
      if (theme === "dark") check(n === 5, `${label}: expands to 5 items`);
      await p.shot(`${SHOTS}/learned-open-${label}-${theme}.png`);
      // old text-only notice
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_2b7e91c04d5a', { pane: m.workspace.focused() }))`);
      await sleep(1300);
      await p.click(".pane.focused .learned-head");
      await sleep(400);
      const items = await p.eval(`[...document.querySelectorAll('.pane.focused .learned-item')].map(e => e.querySelector('.learned-kind').textContent + '|' + e.querySelector('.learned-text').textContent)`);
      if (theme === "dark") {
        check(items.length === 3 && items[0].startsWith("Note|CI flakes") && items[1] === "Skill|debug-flaky-tests" && items[2].startsWith("About you|Wants"), `${label}: legacy text parsed into 3 items`, items);
        check(!(await p.eval(`document.querySelector('.pane.focused .transcript').innerText.includes('Learned from this conversation —')`)), `${label}: no raw prefix shown`);
      }
      await p.eval(`document.querySelector('.pane.focused .learned-list')?.scrollIntoView({block:'center'})`);
      await sleep(200);
      await p.shot(`${SHOTS}/learned-legacy-${label}-${theme}.png`);
      console.log(label, theme, "errors", p.errors);
      await p.close();
    }
  }
  finish();
});
