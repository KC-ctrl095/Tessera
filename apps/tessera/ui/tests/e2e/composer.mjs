import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const measure = (p) => p.eval(`(() => { const ta = document.querySelector('.pane.focused .composer textarea'); const f = ta.parentElement; const c = ta.closest('.composer'); return { ta: ta.getBoundingClientRect().height, field: f.getBoundingClientRect().height, box: c.getBoundingClientRect().height, top: ta.scrollTop, over: getComputedStyle(ta).overflowY }; })()`);

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  for (const [label, w, h, mobile] of [["desktop", 1440, 900, false], ["phone", 375, 812, true], ["fold", 841, 701, true]]) {
    for (const theme of ["dark", "light"]) {
      await mock("/__mock/reset");
      await mock("/__mock/mosaics/reset");
      const p = await device();
      await p.viewport(w, h, mobile, mobile ? 2 : 1);
      await signIn(p, `Composer ${label}`);
      await p.theme(theme);
      await sleep(300);
      // hero composer
      await p.shot(`${SHOTS}/composer-hero-${label}-${theme}.png`);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_2b7e91c04d5a', { pane: m.workspace.focused() }))`);
      await sleep(1400);
      const rest = await measure(p);
      console.log(label, theme, "rest", JSON.stringify(rest));
      if (theme === "dark") check(rest.box <= (mobile ? 52 : 44), `${label}: composer at rest is slim (${rest.box}px)`);
      await p.shot(`${SHOTS}/composer-rest-${label}-${theme}.png`);
      if (theme === "dark") {
        // grow line by line: heights must only go up, text never scrolls until the limit
        await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
        const hs = [];
        let jitter = false;
        for (let i = 1; i <= (mobile ? 14 : 20); i++) {
          await p.type(i === 1 ? "First line of a longer message" : `\nline ${i} of the message, still typing`);
          await sleep(30);
          const m = await measure(p);
          hs.push(Math.round(m.ta));
          if (m.over === "hidden" && m.top !== 0) jitter = true;
        }
        await sleep(200);
        const grown = await measure(p);
        console.log(label, "heights", hs.join(","), "final", JSON.stringify(grown));
        check(hs.every((v, i) => i === 0 || v >= hs[i - 1]), `${label}: grows monotonically`);
        check(!jitter, `${label}: no inner scroll while growing`);
        const vh = await p.eval(`window.visualViewport ? visualViewport.height : innerHeight`);
        check(grown.ta <= Math.max(96, vh * 0.4) + 1 && grown.over === "auto", `${label}: caps at 40% of the view and scrolls inside (${grown.ta} of ${vh})`);
        await p.shot(`${SHOTS}/composer-grown-${label}-${theme}.png`);
        // send → shrinks back
        await p.key("Enter", "Enter", 0, 13);
        if (mobile) await p.click(".pane.focused .send-btn");
        await sleep(600);
        const after = await measure(p);
        check(after.box <= (mobile ? 52 : 44), `${label}: shrinks back after sending (${after.box}px)`);
        await p.eval(`document.querySelector('.pane.focused .send-btn.stop')?.click()`);
        await sleep(600);
      }
      console.log("errors", p.errors);
      await p.close();
    }
  }
  finish();
});
