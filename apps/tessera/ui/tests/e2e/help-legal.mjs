// Batch 17: Settings → Help & tour → Legal: the terms of use, privacy and the open-source licences,
// read in place (built into the server like the guides). Each opens, says what matters first, fits the
// screen and closes with Escape. Desktop, the folded phone (344 px) and the unfolded Fold; dark and light.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const ui = (p, js) => p.eval(`__mod('lib/ui.ts').then((u) => { ${js} })`);
const openHelp = async (p) => {
  await ui(p, `u.openSettings('help')`);
  await p.waitFor(`document.querySelector('.help-legal')`, 8000).catch(() => {});
  await sleep(500);
};
const legal = (p) =>
  p.eval(`(() => { const c = document.querySelector('.help-legal')?.closest('.card'); return c && { title: c.querySelector('.setting-title')?.textContent, note: c.querySelector('.help-legal-note')?.textContent ?? '', links: [...c.querySelectorAll('.help-legal li')].map((l) => l.textContent.trim()) }; })()`);
/** Open one of the Legal links; what the reader shows. */
const read = async (p, name) => {
  await p.eval(`(() => { const b = [...document.querySelectorAll('.help-legal .link-btn')].find((b) => b.textContent.trim() === ${JSON.stringify(name)}); b.scrollIntoView({ block: 'center' }); b.click(); })()`);
  await p.waitFor(`document.querySelector('.guide-modal .guide-body')`, 8000).catch(() => {});
  await sleep(400);
  return p.eval(`(() => { const m = document.querySelector('.guide-modal'); if (!m) return null; const r = m.getBoundingClientRect(); const b = m.querySelector('.guide-body'); const p0 = b?.querySelector('p')?.getBoundingClientRect(); return { title: m.querySelector('.modal-head h2')?.textContent, text: b?.textContent ?? '', error: /couldn't be loaded/.test(m.textContent), tables: m.querySelectorAll('.guide-body table').length, left: r.left, right: r.right, vw: innerWidth, pageFits: document.documentElement.scrollWidth <= innerWidth + 1, inset: p0 ? Math.min(p0.left - r.left, r.right - p0.right) : 0, long: !!b && b.scrollHeight > b.clientHeight + 40, inside: !!b && b.getBoundingClientRect().bottom <= r.bottom + 1 }; })()`);
};
/** Scroll the open guide to its end: whether it moved and its last line came into view. */
const scrollsToEnd = (p) =>
  p.eval(`(async () => { const m = document.querySelector('.guide-modal'); const b = m?.querySelector('.guide-body'); if (!b) return false; b.scrollTop = b.scrollHeight; await new Promise((r) => setTimeout(r, 300)); const last = b.lastElementChild?.getBoundingClientRect(); return b.scrollTop > 0 && !!last && last.bottom <= m.getBoundingClientRect().bottom + 1; })()`);
const close = async (p) => {
  await p.key("Escape", "Escape", 0, 27);
  await sleep(400);
  return !(await p.eval(`!!document.querySelector('.guide-modal')`));
};

run(async () => {
  await mock("/__mock/reset");

  // ── a computer ──
  {
    const p = await device();
    await p.viewport(1440, 900);
    await signIn(p, "Legal");
    await p.theme("dark");
    await openHelp(p);
    const l = await legal(p);
    check(l?.title === "Legal" && JSON.stringify(l.links) === JSON.stringify(["Terms of use", "Privacy", "Open-source licences"]), "Help & tour: a Legal card with the terms, privacy and licences", l);
    check(/open-source/.test(l?.note) && /no warranty/.test(l?.note) && /your responsibility/.test(l?.note), "…saying in a line: open source, no warranty, agents act for you", l?.note);
    await p.shot(`${SHOTS}/help-legal-desk.png`);

    let g = await read(p, "Terms of use");
    check(g?.title === "Terms of use" && !g.error, "Terms of use opens in place", g && { title: g.title, error: g.error });
    check(/No warranty/.test(g?.text) && /AI agents act on real systems/.test(g?.text) && /unmodified Claude Code/.test(g?.text), "…no warranty, the disclaimer for agents, Claude plans only through your own Claude Code");
    check(/Apache License/.test(g?.text) && /No affiliation/.test(g?.text), "…the licence wins, and no affiliation");
    check(g?.inset >= 12 && g?.inside, "…its text sits inside the dialog, clear of its edges", g && { inset: g.inset, inside: g.inside });
    check(g?.long && (await scrollsToEnd(p)), "…and the long page scrolls to its end inside the dialog");
    await p.shot(`${SHOTS}/help-legal-terms-desk.png`);
    check(await close(p), "Escape closes it");
    check(await p.eval(`!!document.querySelector('.help-legal')`), "…back on Help & tour");

    g = await read(p, "Privacy");
    check(g?.title === "Privacy" && /no telemetry/.test(g?.text) && /Deleting your data/.test(g?.text), "Privacy: no telemetry, what goes where, deleting your data", g && { title: g.title, error: g.error });
    check(await close(p), "Escape closes it");

    g = await read(p, "Open-source licences");
    check(g?.title === "Open-source licences" && g.tables >= 2 && /MIT OR Apache-2\.0/.test(g?.text) && /Apache License 2\.0/.test(g?.text) && /cloudflared/.test(g?.text), "Open-source licences: the packages by licence and what is downloaded instead", g && { title: g.title, tables: g.tables });
    check(g?.pageFits, "…the long lists don't widen the page");
    await p.shot(`${SHOTS}/help-legal-licences-desk.png`);
    check(await close(p), "Escape closes it");
    check(p.errors.length === 0, "desktop: no errors", p.errors);
    await p.close();
  }

  // ── the folded phone and the unfolded Fold, dark and light ──
  for (const [name, w, h] of [["cover", 344, 882], ["fold", 673, 841]]) {
    for (const theme of ["dark", "light"]) {
      const p = await device();
      await p.viewport(w, h, true, 2.625);
      await signIn(p, `Legal ${name} ${theme}`);
      await p.theme(theme);
      await openHelp(p);
      const fits = await p.eval(`(() => { const b = document.querySelector('.settings-body'); return !!b && b.scrollWidth <= b.clientWidth + 1; })()`);
      check(fits && (await legal(p))?.links.length === 3, `${name} ${theme}: the Legal card fits the width`);
      await p.eval(`document.querySelector('.help-legal')?.scrollIntoView({ block: 'center' })`);
      await sleep(300);
      await p.shot(`${SHOTS}/help-legal-${name}-${theme}.png`);
      for (const doc of ["Terms of use", "Open-source licences"]) {
        const g = await read(p, doc);
        check(!!g && !g.error && g.pageFits && g.left >= -1 && g.right <= g.vw + 1 && g.inset >= 12, `${name} ${theme}: ${doc} fits the screen, clear of its edges`, g && { left: g.left, right: g.right, vw: g.vw, pageFits: g.pageFits, inset: g.inset });
        check(await scrollsToEnd(p), `${name} ${theme}: ${doc} scrolls to its end`);
        if (doc === "Open-source licences") await p.shot(`${SHOTS}/help-legal-licences-${name}-${theme}.png`);
        check(await close(p), `${name} ${theme}: ${doc} closes`);
      }
      check(p.errors.length === 0, `${name} ${theme}: no errors`, p.errors);
      await p.close();
    }
  }
  finish();
});
