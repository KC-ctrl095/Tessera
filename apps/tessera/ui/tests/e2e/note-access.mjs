// Who can read a note (batch 17): agents and bots read every note unless the user keeps it from
// them. A note's options have one plain switch, "Keep from agents" (off by default); the choice
// reaches every device at once, and the notes list marks a kept note. A note made in a chat still
// has "Ask about this" (its title and id in that chat's message box: sent, that chat may read it
// even when the note is kept from agents; the server keeps the grant, core tests note_access.rs); a
// note of the user's own has no "mention it in the chat beside it" (a [[link]] in a message brings it
// into a chat). Desktop, folded phone (344 px) and the unfolded Fold, light and dark.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const until = (p, expr, ms = 8000) => p.waitFor(expr, ms).then(() => true, () => false);

run(async () => {
  await mock("/__mock/reset");
  const ws = await wsClient("note access test");
  const n = (await ws.rpc("save_note", { title: "Trip plan", content: "# Trip plan\n\nTrain to Lyon on the 12th, back on the 16th." })).result;
  check(n.keep_from_agents === false && n.all_bots === true, "a new note is for every agent and bot to read", n);

  const items = (p) => p.eval(`[...document.querySelectorAll('.menu .menu-item, .menu .menu-heading')].map((e) => ({ text: e.querySelector('.menu-label')?.textContent ?? e.textContent.trim(), heading: e.classList.contains('menu-heading'), checked: e.getAttribute('aria-checked') }))`);
  /** Opens a note in a tile (focused) and gives the tile's id (the layout is shared: other tiles may be there). */
  const openNote = async (p, id = n.id) => {
    await p.eval(`__mod('lib/notes.ts').then(m => m.openNote('${id}', { newPane: true }))`);
    let pane = null;
    for (let k = 0; k < 100 && !pane; k++) {
      pane = await p.eval(`__mod('lib/workspace.ts').then(m => m.paneShowingNote('${id}'))`);
      if (!pane) await sleep(100);
    }
    await until(p, `document.querySelector('[data-pane-id="${pane}"] [aria-label="Note options"]')`, 10000);
    await sleep(300);
    return pane;
  };
  const openMenu = async (p, pane) => {
    await p.click(`[data-pane-id="${pane}"] [aria-label="Note options"]`);
    await until(p, `document.querySelector('.menu .menu-item')`, 3000);
    await sleep(200);
  };
  const closeMenu = async (p) => {
    await p.key("Escape", "Escape", 0, 27);
    await sleep(200);
  };

  for (const [label, w, h, mobile, dpr, theme] of [
    ["desktop", 1440, 900, false, 1, "light"],
    ["phone", 344, 820, true, 2, "dark"],
    ["fold", 884, 1104, true, 2, "light"],
  ]) {
    console.log(`— ${label}`);
    await ws.rpc("set_note_agents", { id: n.id, keep_from_agents: false });
    const p = await device();
    await p.viewport(w, h, mobile, dpr);
    await signIn(p, `Notes ${label}`);
    await p.theme(theme);
    const tile = await openNote(p);
    await openMenu(p, tile);
    let list = await items(p);
    const kept = (l) => l.find((i) => i.text.startsWith("Keep from agents"));
    check(!!kept(list), `${label}: the note's options have "Keep from agents"`, list.map((i) => i.text));
    check(kept(list)?.checked !== "true", `${label}: …off by default (agents and bots can read it)`, kept(list));
    check(!list.some((i) => /Only bots I add|All my bots/.test(i.text)), `${label}: …and nothing else about bots`, list.map((i) => i.text));
    const fits = await p.eval(`(() => { const m = document.querySelector('.menu'); const r = m.getBoundingClientRect(); return r.left >= 0 && r.right <= innerWidth + 1 && r.width > 150; })()`);
    check(fits, `${label}: the menu fits the screen`);
    await p.shot(`${SHOTS}/note-access-${label}-${theme}.png`);

    // A second device has the same note open.
    const other = await device();
    await other.viewport(1200, 800, false, 1);
    await signIn(other, `Other ${label}`);
    const otherTile = await openNote(other);

    await p.clickText(".menu-item", "Keep from agents");
    let saved = false;
    for (let k = 0; k < 30 && !saved; k++) {
      saved = (await ws.rpc("get_note", { id: n.id })).result.keep_from_agents === true;
      if (!saved) await sleep(100);
    }
    check(saved, `${label}: "Keep from agents" is kept on the server`);
    const said = await until(p, `document.querySelector('.note-tile')?.textContent.includes('Kept from agents')`, 3000);
    check(said, `${label}: the tile says so`);
    await openMenu(p, tile);
    list = await items(p);
    check(kept(list)?.checked === "true", `${label}: the menu shows it switched on`, kept(list));
    for (const t of ["light", "dark"]) {
      await p.theme(t);
      await sleep(200);
      await p.shot(`${SHOTS}/note-access-${label}-kept-${t}.png`);
    }
    await closeMenu(p);
    // The notes list marks it.
    const marked = await p.eval(`__mod('lib/store.ts').then((m) => !!m.state.notes['${n.id}']?.keep_from_agents)`);
    check(marked, `${label}: the notes list knows it is kept from agents`);

    // The other device's open tile follows at once (no new version of the text).
    await openMenu(other, otherTile);
    const there = await items(other);
    check(kept(there)?.checked === "true", `${label}: another device's open note shows the change`, there.map((i) => [i.text, i.checked]));
    await closeMenu(other);
    const v = (await ws.rpc("get_note", { id: n.id })).result;
    check(v.version === n.version && v.content === n.content, `${label}: the note's text and version are untouched`, { version: v.version });

    // And back.
    await openMenu(p, tile);
    await p.clickText(".menu-item", "Keep from agents");
    let back = false;
    for (let k = 0; k < 30 && !back; k++) {
      back = (await ws.rpc("get_note", { id: n.id })).result.keep_from_agents === false;
      if (!back) await sleep(100);
    }
    check(back, `${label}: switched off again, agents can read it`);
    // An older app's "All my bots" / "Only bots I add" still works.
    const old = (await ws.rpc("set_note_bots", { id: n.id, all_bots: false })).result;
    check(old.keep_from_agents === true && old.all_bots === false, `${label}: an older app's "Only bots I add" keeps it from agents`, old);
    await ws.rpc("set_note_bots", { id: n.id, all_bots: true });

    // A note of the user's own: nothing offers to mention it in the chat beside it (even with a chat there).
    const chat = (await ws.rpc("create_session", { title: "Planning" })).result;
    await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${chat.id}', { pane: m.workspace.focused() }))`);
    await sleep(600);
    const own = await openNote(p);
    await openMenu(p, own);
    list = await items(p);
    check(list.length > 4 && !list.some((i) => /Ask about this|beside it/.test(i.text)), `${label}: a note of your own has no "mention it in the chat beside it"`, list.map((i) => i.text));
    await closeMenu(p);
    const draft = await p.eval(`[...document.querySelectorAll('textarea')].map((t) => t.value).join('')`);
    check(!draft.includes(n.id), `${label}: …and no message box names it`, draft);

    // A note made in a chat: "Ask about this" names it (title and id) in that chat's message box.
    const made = (await ws.rpc("save_note", { title: "Packing list", content: "- passport\n- charger", session_id: chat.id })).result;
    await openMenu(p, await openNote(p, made.id));
    list = await items(p);
    check(list.some((i) => i.text.startsWith("Ask about this")) && !list.some((i) => /beside it/.test(i.text)), `${label}: a note made in a chat offers "Ask about this" for that chat`, list.map((i) => i.text));
    await p.clickText(".menu-item", "Ask about this");
    const drafted = await until(p, `[...document.querySelectorAll('textarea')].some((t) => t.value.includes('About the “Packing list” note (${made.id})'))`, 4000);
    check(drafted, `${label}: "Ask about this" names the note and its id in its chat's message box`);
    const where = await p.eval(`__mod('lib/workspace.ts').then(m => m.getDraft(m.paneShowing('${chat.id}')))`);
    check(where.includes(made.id), `${label}: …the box of the chat it was made in`, where);
    await p.shot(`${SHOTS}/note-access-${label}-ask.png`);
    // Tidy for the next device (the layout is shared): the note and its tile go.
    await ws.rpc("delete_note", { id: made.id });

    check(!p.errors.length && !other.errors.length, `${label}: no page errors`, [...p.errors, ...other.errors]);
    await other.close();
    await p.close();
  }
  ws.close();
  finish();
});
