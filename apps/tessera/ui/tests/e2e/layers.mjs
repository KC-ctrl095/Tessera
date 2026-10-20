// What floats inside a tile stays under what opens above the tiles (batch 17, the user's "the
// note editor's format bar pops through Settings"). The note editor's own floating parts (the
// format bubble over a selection, the / and [[ suggestions, the find bar) carry high z-indexes of
// their own; Settings, any dialog, the phone's drawer and sheets must still cover them. A menu
// left open when a dialog opens over it (Settings by its shortcut) closes. Desktop, folded phone
// (344 px), the unfolded Fold; light and dark.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const ui = (p, js) => p.eval(`__mod('lib/ui.ts').then((u) => { ${js} })`);

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  const probe = await wsClient("probe");
  const until = async (fn, ms = 8000) => {
    const t0 = Date.now();
    while (Date.now() - t0 < ms) {
      if (await fn()) return true;
      await sleep(120);
    }
    return false;
  };
  /** A note with `content` open in a tile of its own. */
  const show = async (p, title, content) => {
    const n = (await probe.rpc("save_note", { title, content })).result;
    await p.eval(`__mod('lib/notes.ts').then(m => m.openNote('${n.id}', { newPane: true }))`);
    let pane = null;
    await until(async () => (pane = await p.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().find(id => m.workspace.panes[id].content.noteId === '${n.id}') ?? null)`)) && (await p.eval(`!!document.querySelector('[data-pane-id="${pane}"] .note-ed .cm-content')`)), 10000);
    await sleep(300);
    return { id: n.id, pane, sel: `[data-pane-id="${pane}"]` };
  };
  const V = (t) => `document.querySelector('${t.sel} .cm-content').cmTile.root.view`;
  /** Where `sel` is drawn, and what is on top at its middle: itself, or something covering it. */
  const onTop = (p, sel) =>
    p.eval(`(() => {
      const e = document.querySelector(${JSON.stringify(sel)});
      if (!e || !e.getClientRects().length) return null;
      const r = e.getBoundingClientRect();
      const x = r.left + r.width / 2, y = r.top + Math.min(r.height / 2, 40);
      const hit = document.elementFromPoint(x, y);
      return { own: !!hit && e.contains(hit), modal: !!hit?.closest('.modal-backdrop'), drawer: !!hit?.closest('.drawer-layer'), sheet: !!hit?.closest('.sheet-backdrop, .popover'), hit: hit ? hit.className.toString().slice(0, 60) : null, w: Math.round(r.width) };
    })()`);
  const settingsOpen = (p) => p.eval(`!!document.querySelector('.settings-layout')`);
  const openSettings = async (p) => {
    await ui(p, `u.openSettings('appearance')`);
    await until(() => settingsOpen(p), 5000);
    await sleep(400);
  };
  const closeSettings = async (p) => {
    await ui(p, `u.setSettingsTab(null)`);
    await sleep(300);
  };
  const long = Array.from({ length: 30 }, (_, i) => `Line ${i + 1} of a note with enough words to select.`).join("\n\n");

  // ── a computer ─────────────────────────────────────────────────────────
  const p = await device();
  await p.viewport(1280, 860);
  await signIn(p, "Desktop");
  await p.theme("dark");
  const t = await show(p, "Layers", long);

  // The format bubble over a selection: shown on the note, covered by Settings.
  await p.eval(`(() => { const v = ${V(t)}; v.focus(); const l = v.state.doc.line(5); v.dispatch({ selection: { anchor: l.from, head: l.to } }); })()`);
  await until(() => p.eval(`!!document.querySelector('${t.sel} .cm-bubble')`), 3000);
  await sleep(300);
  const bubble = `${t.sel} .cm-bubble`;
  let r = await onTop(p, bubble);
  check(!!r && r.own, "the format bubble shows over the note's selection", r);
  for (const theme of ["dark", "light"]) {
    await p.theme(theme);
    await openSettings(p);
    r = await onTop(p, bubble);
    check(!!r && !r.own && r.modal, `${theme}: Settings covers the note's format bubble (it no longer shows through)`, r);
    await p.shot(`${SHOTS}/layers-settings-over-bubble-desktop-${theme}.png`);
    await closeSettings(p);
  }
  await p.theme("dark");
  r = await onTop(p, bubble);
  check(!!r && r.own, "…and it is there again once Settings closes", r);

  // A confirm dialog (any dialog, not only Settings) covers it too.
  await ui(p, `u.confirmDialog({ title: 'A question', body: 'Over the note', confirmLabel: 'OK' })`);
  await until(() => p.eval(`!!document.querySelector('.modal-backdrop')`), 3000);
  await sleep(300);
  r = await onTop(p, bubble);
  check(!!r && !r.own && r.modal, "a dialog covers the format bubble", r);
  await p.key("Escape", "Escape", 0, 27);
  await sleep(300);

  // The / suggestions at the start of a line.
  await p.eval(`(() => { const v = ${V(t)}; v.focus(); const l = v.state.doc.line(9); v.dispatch({ selection: { anchor: l.to } }); })()`);
  await p.key("Enter", "Enter", 0, 13);
  await p.type("/");
  const sug = await until(() => p.eval(`!!document.querySelector('${t.sel} .cm-tooltip-autocomplete')`), 3000);
  check(sug, "typing / at the start of a line suggests blocks");
  if (sug) {
    await openSettings(p);
    r = await onTop(p, `${t.sel} .cm-tooltip-autocomplete`);
    check(!r || (!r.own && r.modal), "Settings covers the / suggestions", r);
    await closeSettings(p);
  }
  await p.key("Escape", "Escape", 0, 27);
  await sleep(200);

  // The find bar (Ctrl+F in the note).
  await p.eval(`(() => { const v = ${V(t)}; v.focus(); })()`);
  await p.key("f", "KeyF", 2, 70);
  const find = await until(() => p.eval(`!!document.querySelector('${t.sel} .cm-panels .cm-search')`), 3000);
  check(find, "Ctrl+F opens the note's find bar");
  if (find) {
    await openSettings(p);
    r = await onTop(p, `${t.sel} .cm-panels`);
    check(!!r && !r.own && r.modal, "Settings covers the note's find bar", r);
    await closeSettings(p);
  }

  // A menu left open when a dialog opens over it closes (Settings by its shortcut while the note's menu is open).
  await p.click(`${t.sel} button[aria-label="Note options"]`);
  await until(() => p.eval(`!!document.querySelector('.menu-pop')`), 3000);
  check(await p.eval(`!!document.querySelector('.menu-pop')`), "the note's menu opens");
  await openSettings(p);
  check(!(await p.eval(`!!document.querySelector('.menu-pop')`)) && (await settingsOpen(p)), "Settings opening over it closes the menu (nothing floats above Settings)");
  // A menu inside Settings still opens above it.
  await ui(p, `u.setSettingsTab('models')`);
  await until(() => p.eval(`!!document.querySelector('.default-model .role-pick')`), 6000);
  await p.click(".default-model .role-pick");
  await until(() => p.eval(`!!document.querySelector('.pop-model')`), 3000);
  r = await onTop(p, ".pop-model");
  check(!!r && r.own, "a menu opened inside Settings sits above it", r);
  await p.key("Escape", "Escape", 0, 27);
  await closeSettings(p);
  check(!p.errors.length, "desktop: no page errors", p.errors);
  await p.close();

  // ── phones: the folded phone (344 px) and the unfolded Fold ───────────────
  for (const [label, w, h] of [["phone", 344, 760], ["fold", 841, 701]]) {
    const ph = await device();
    await ph.viewport(w, h, true, 2);
    await signIn(ph, label);
    for (const theme of ["dark", "light"]) {
      await ph.theme(theme);
      const tp = await show(ph, `Phone layers ${theme}`, long);
      await ph.eval(`(() => { const v = ${V(tp)}; v.focus(); const l = v.state.doc.line(3); v.dispatch({ selection: { anchor: l.from + 2 } }); })()`);
      await sleep(500);
      // Whatever the phone shows while writing (its format bar), Settings covers it.
      const barShown = await ph.eval(`(() => { const e = document.querySelector('${tp.sel} .nb-tools'); return !!e && e.getClientRects().length > 0 && e.getBoundingClientRect().height > 0; })()`);
      const what = barShown ? `${tp.sel} .nb-tools` : `${tp.sel} .cm-content`;
      r = await onTop(ph, what);
      check(!!r && r.own, `${label} ${theme}: the note (and its format bar) shows`, r);
      await openSettings(ph);
      r = await onTop(ph, what);
      // Covered by Settings, or not drawn at all under it.
      check(!r || (!r.own && r.modal), `${label} ${theme}: Settings covers the note being written`, r);
      await ph.shot(`${SHOTS}/layers-settings-${label}-${theme}.png`);
      await closeSettings(ph);
      // The drawer (folded phone) covers the note too.
      if (label === "phone") {
        await ui(ph, `u.setDrawerOpen(true)`);
        await sleep(500);
        r = await onTop(ph, `${tp.sel} .cm-content`);
        check(!r || (!r.own && r.drawer), `${label} ${theme}: the drawer covers the note`, r);
        await ph.shot(`${SHOTS}/layers-drawer-${label}-${theme}.png`);
        await ui(ph, `u.setDrawerOpen(false)`);
        await sleep(400);
      }
    }
    check(!ph.errors.length, `${label}: no page errors`, ph.errors);
    await ph.close();
  }
  probe.close();
  finish();
});
