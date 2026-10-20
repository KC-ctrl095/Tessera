// "New folder": the folder picker makes a folder inside the one being browsed and opens it (names the
// server refuses show where they were typed; something already there is never replaced; the picker for
// This computer, which can't make folders yet, doesn't offer it), and the bot editor's home folder field
// can make ~/<the bot's name> in one click (a folder already there is offered, not taken). Desktop, the
// folded phone (344 px), the unfolded Fold, light and dark.
import { SHOTS, check, device, finish, run, signIn, sleep, wsClient } from "./cdp.mjs";

run(async () => {
  const c = await wsClient("new folder script");
  const rpc = async (method, params) => c.rpc(method, params);
  const list = async (dir) => (await rpc("list_dir", { path: dir })).result ?? [];

  const p = await device();
  await p.viewport(1280, 900);
  await signIn(p, "New folder");
  const home = await p.eval(`__mod('lib/store.ts').then(m => m.state.server.home)`);

  // ── the request itself (the mock mirrors engine.rs create_dir) ──
  const made = (await rpc("create_dir", { parent: home, name: "  Ada's notes  " })).result;
  check(made?.name === "Ada's notes" && made.path === `${home}/Ada's notes` && made.is_dir === true, "create_dir makes the folder (the name is trimmed) and says where", made);
  check((await list(home)).some((e) => e.name === "Ada's notes" && e.is_dir), "…and the listing shows it");
  check((await list(made.path)).length === 0, "…empty");
  const inside = (await rpc("create_dir", { parent: "~/Ada's notes", name: "deeper" })).result;
  check(inside?.path === `${home}/Ada's notes/deeper`, "~ in the parent works as it does in the picker", inside);
  const again = await rpc("create_dir", { parent: home, name: "Ada's notes" });
  check(again.error?.code === "conflict" && /A folder called Ada's notes is already in/.test(again.error.message), "something already there is a conflict, left alone", again.error);
  const onFile = await rpc("create_dir", { parent: `${home}/projects/notes`, name: "inbox.md" });
  check(onFile.error?.code === "conflict" && /^A file called/.test(onFile.error.message), "…a file too", onFile.error);
  const bads = ["", "   ", ".", "..", "a/b", "../x", "a\\b", "tab\there", "nul\0x", "flip‮gnp", "zero​width", "x".repeat(256)];
  const refused = [];
  for (const name of bads) {
    const r = await rpc("create_dir", { parent: home, name });
    if (r.error?.code !== "bad_request" || !r.error.message) refused.push([name, r]);
  }
  check(!refused.length, "names that are paths, empty, odd or too long are refused (bad_request)", refused);
  check(!(await list(home)).some((e) => /^(\.\.|a|x{20,})$/.test(e.name)), "…and nothing was made");
  const own = await rpc("create_dir", { parent: `${home}/.config/tessera`, name: "mine" });
  check(own.error?.code === "bad_request" && /Tessera's own/.test(own.error.message), "Tessera's own folders are off limits", own.error);
  const none = await rpc("create_dir", { parent: `${home}/nowhere`, name: "x" });
  const rel = await rpc("create_dir", { parent: "projects", name: "x" });
  check(none.error?.code === "bad_request" && /no folder/i.test(none.error.message) && rel.error?.code === "bad_request" && /full path/.test(rel.error.message), "the parent must be a folder that exists, given in full", [none.error, rel.error]);

  // ── the picker, and the bot editor ──
  const fill = (sel, v) =>
    p.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); e.focus(); Object.getOwnPropertyDescriptor(Object.getPrototypeOf(e), 'value').set.call(e, ${JSON.stringify(v)}); e.dispatchEvent(new Event('input', { bubbles: true })); })()`);
  const text = (sel) => p.eval(`document.querySelector(${JSON.stringify(sel)})?.textContent ?? null`);
  const pickerPath = () => p.eval(`document.querySelector('.dirb-path input')?.value ?? null`);
  const picked = () => p.eval(`window.__picked`);
  const openPicker = async (start, source) => {
    await p.eval(`__mod('lib/ui.ts').then((m) => { window.__picked = undefined; m.browseDir(${JSON.stringify(start)}, 'Test folder'${source ? ", " + source : ""}).then((v) => { window.__picked = v; }); })`);
    await p.waitFor(`document.querySelector('.dirb-list .dirb-item, .dirb-list .dirb-new-btn')`, 6000);
    await sleep(250);
  };
  const closePicker = () => p.eval(`__mod('lib/ui.ts').then((m) => m.setDirBrowser(null))`);
  const fits = (sel) => p.eval(`(() => { const e = document.querySelector(${JSON.stringify(sel)}); if (!e) return null; const r = e.getBoundingClientRect(); return r.left >= 0 && r.right <= innerWidth + 0.5 && e.scrollWidth <= e.clientWidth + 1; })()`);

  const scenario = async (label, w, h, mobile, theme, full) => {
    const tag = `${label} ${theme}`;
    const q = p;
    await q.viewport(w, h, mobile, mobile ? 2 : 1);
    await q.theme(theme);
    await sleep(250);
    const stamp = `${label}-${theme}`;

    // picker: New folder is the first row of the list
    await openPicker(home);
    const btn = await q.eval(`(() => { const b = document.querySelector('.dirb-list .dirb-new-btn'); if (!b) return null; const r = b.getBoundingClientRect(), l = document.querySelector('.dirb-list').getBoundingClientRect(); return { text: b.textContent.trim(), top: r.top - l.top, within: r.right <= l.right }; })()`);
    check(!!btn && btn.text === "New folder" && btn.top < 12 && btn.within, `${tag}: the picker's list starts with "New folder"`, btn);
    await q.click(".dirb-new-btn");
    await sleep(200);
    check(await q.eval(`document.activeElement === document.querySelector('.dirb-new-name')`), `${tag}: it opens a name field, ready to type`);
    check(await q.eval(`document.querySelector('.dirb-new-form button[type=submit]').disabled`), `${tag}: Create waits for a name`);
    check(await fits(".dirb-new-form"), `${tag}: the name field fits the list`);
    if (full) {
      await q.key("Escape", "Escape", 0, 27);
      await sleep(250);
      check(!(await q.eval(`!!document.querySelector('.dirb-new-form')`)) && (await q.eval(`!!document.querySelector('.dirb')`)) && (await q.eval(`!!document.querySelector('.dirb-new-btn')`)), `${tag}: Escape puts away the name field and keeps the picker`);
      await q.click(".dirb-new-btn");
      await sleep(200);
    }
    // a name the server refuses: told where it was typed, nothing changes
    await fill(".dirb-new-name", "Bad/name");
    await q.click(".dirb-new-form button[type=submit]");
    await q.waitFor(`document.querySelector('.dirb-new-error')`, 4000);
    const err = await text(".dirb-new-error");
    check(/slash/.test(err ?? "") && (await pickerPath()) === home, `${tag}: a name with a slash is refused in the field, the picker stays where it was`, err);
    check(await fits(".dirb-new-error"), `${tag}: …the message fits`);
    await q.shot(`${SHOTS}/newfolder-picker-error-${stamp}.png`);
    await fill(".dirb-new-name", "..");
    check((await q.eval(`!document.querySelector('.dirb-new-error')`)), `${tag}: typing again clears the message`);
    // a good one: made, opened, and "Use this folder" picks it
    const good = `Folder ${stamp}`;
    await fill(".dirb-new-name", good);
    await q.key("Enter", "Enter", 0, 13);
    await q.waitFor(`document.querySelector('.dirb-path input').value.endsWith(${JSON.stringify("/" + good)})`, 4000);
    await sleep(300);
    check((await pickerPath()) === `${home}/${good}` && (await list(home)).some((e) => e.name === good && e.is_dir), `${tag}: Enter makes the folder and opens it`, await pickerPath());
    check((await text(".dirb-list")).includes("Empty folder") && (await q.eval(`!document.querySelector('.dirb-new-form')`)), `${tag}: it's empty, the name field is gone`);
    await q.shot(`${SHOTS}/newfolder-picker-made-${stamp}.png`);
    await q.clickText(".dirb-foot .btn-primary", "Use this folder");
    await sleep(300);
    check((await picked()) === `${home}/${good}`, `${tag}: "Use this folder" picks the new one`, await picked());
    // the same name again: not replaced
    await openPicker(home);
    await q.click(".dirb-new-btn");
    await fill(".dirb-new-name", good);
    await q.click(".dirb-new-form button[type=submit]");
    await q.waitFor(`document.querySelector('.dirb-new-error')`, 4000);
    check(/A folder called .* is already in/.test((await text(".dirb-new-error")) ?? "") && (await pickerPath()) === home, `${tag}: a folder of that name is never replaced`, await text(".dirb-new-error"));
    await closePicker();
    await sleep(200);

    // the picker for This computer can't make folders yet: no "New folder" there
    if (full) {
      await openPicker("/home/me", `{ home: '/home/me', list: async () => [{ name: 'code', path: '/home/me/code', is_dir: true }] }`);
      check(await q.eval(`!document.querySelector('.dirb-new-btn') && !!document.querySelector('.dirb-item')`), `${tag}: a source that can't make folders (This computer) doesn't offer it`);
      await closePicker();
      await openPicker(home, `{ home: ${JSON.stringify(home)}, list: (d) => __mod('lib/store.ts').then((m) => m.api.listDir(d)), create: (d, n) => Promise.resolve({ name: n, path: d + '/' + n, is_dir: true }) }`);
      check(await q.eval(`!!document.querySelector('.dirb-new-btn')`), `${tag}: …one that can does`);
      await closePicker();
      await sleep(200);
    }

    // the bot editor: a folder for the bot in one click
    const botName = `Ada ${stamp}`;
    const slug = `ada-${stamp}`;
    const homeField = `.be-sec:has(.setting-row) input.mono.grow`;
    await q.eval(`__mod('lib/ui.ts').then((m) => m.openBotEditor(null))`);
    await q.waitFor(`document.querySelector('#be-name')`, 6000);
    // A new bot's home folder is under "More settings".
    await q.eval(`document.querySelector('.bot-editor .be-more')?.click()`);
    await sleep(500);
    await q.eval(`document.querySelector('.be-home-new')?.scrollIntoView({ block: 'center' })`);
    check(/Name your bot/.test((await text(".be-home-new")) ?? "") && (await q.eval(`!document.querySelector('.be-home-btn')`)), `${tag}: with no name yet, the home folder asks for one first`, await text(".be-home-new"));
    await fill("#be-name", botName);
    await sleep(200);
    const label2 = ((await text(".be-home-btn")) ?? "").trim();
    check(label2 === `Create ~/${slug}`, `${tag}: the button names the folder it will make (~/<the bot's name>)`, label2);
    await q.eval(`document.querySelector('.be-home-new').scrollIntoView({ block: 'center' })`);
    await sleep(200);
    check((await fits(".be-home-new")) && (await q.eval(`(() => { const r = document.querySelector('.be-home-btn').getBoundingClientRect(); return r.left >= 0 && r.right <= innerWidth; })()`)), `${tag}: the button fits the screen`);
    await q.shot(`${SHOTS}/newfolder-bot-before-${stamp}.png`);
    await q.click(".be-home-btn");
    await q.waitFor(`document.querySelector(${JSON.stringify(homeField)}).value !== ''`, 4000);
    await sleep(300);
    check((await q.eval(`document.querySelector(${JSON.stringify(homeField)}).value`)) === `${home}/${slug}`, `${tag}: one click makes it and fills the home folder`, await q.eval(`document.querySelector(${JSON.stringify(homeField)}).value`));
    check((await list(home)).some((e) => e.name === slug && e.is_dir) && (await q.eval(`!document.querySelector('.be-home-btn')`)), `${tag}: the folder exists, and the button is gone`);
    await q.eval(`document.querySelector(${JSON.stringify(homeField)}).scrollIntoView({ block: 'center' })`);
    await q.shot(`${SHOTS}/newfolder-bot-made-${stamp}.png`);
    // saving keeps it
    await q.clickText(".be-actions .btn", "Create and keep editing");
    await q.waitFor(`document.body.innerText.includes(${JSON.stringify(botName + " created")})`, 6000);
    const bot = ((await rpc("list_bots")).result ?? []).find((b) => b.name === botName);
    check(bot?.cwd === `${home}/${slug}`, `${tag}: the saved bot's home is that folder`, bot?.cwd);
    await q.eval(`__mod('lib/ui.ts').then((m) => m.setBotEditor(null))`);
    await sleep(300);

    // the same name for another bot: the folder is there already: offered, never taken without a click
    await q.eval(`__mod('lib/ui.ts').then((m) => m.openBotEditor(null))`);
    await q.waitFor(`document.querySelector('#be-name')`, 6000);
    await q.eval(`document.querySelector('.bot-editor .be-more')?.click()`);
    await sleep(400);
    await fill("#be-name", botName);
    await sleep(150);
    await q.eval(`document.querySelector('.be-home-new').scrollIntoView({ block: 'center' })`);
    await q.click(".be-home-btn");
    await q.waitFor(`document.querySelector('.be-home-note')`, 4000);
    const note = await text(".be-home-note");
    check(/already in/.test(note ?? "") && (await q.eval(`document.querySelector(${JSON.stringify(homeField)}).value`)) === "", `${tag}: a folder already there isn't taken or replaced`, note);
    check(/Use ~\/.+/.test((await text(".be-home-note .link-btn")) ?? ""), `${tag}: …it is offered instead`, await text(".be-home-note .link-btn"));
    check(await fits(".be-home-note"), `${tag}: the note fits the screen`);
    await q.shot(`${SHOTS}/newfolder-bot-there-${stamp}.png`);
    await q.click(".be-home-note .link-btn");
    await sleep(250);
    check((await q.eval(`document.querySelector(${JSON.stringify(homeField)}).value`)) === `${home}/${slug}` && (await q.eval(`!document.querySelector('.be-home-note')`)), `${tag}: "Use" fills it in`, await q.eval(`document.querySelector(${JSON.stringify(homeField)}).value`));
    if (full) {
      // an odd name still gives a plain folder name
      await q.eval(`(() => { const e = document.querySelector(${JSON.stringify(homeField)}); e.focus(); Object.getOwnPropertyDescriptor(Object.getPrototypeOf(e), 'value').set.call(e, ''); e.dispatchEvent(new Event('input', { bubbles: true })); })()`);
      await fill("#be-name", "  Q/A: Ünï bot!!  ");
      await sleep(200);
      check((((await text(".be-home-btn")) ?? "").trim()) === "Create ~/q-a-ünï-bot", `${tag}: slashes and punctuation in a name become dashes`, await text(".be-home-btn"));
      await fill("#be-name", "!!!");
      await sleep(200);
      check((((await text(".be-home-btn")) ?? "").trim()) === "Create ~/bot", `${tag}: a name with no letters gives "bot"`, await text(".be-home-btn"));
    }
    await q.eval(`__mod('lib/ui.ts').then((m) => m.setBotEditor(null))`);
    await sleep(300);
  };

  await scenario("desktop", 1280, 900, false, "dark", true);
  await scenario("desktop", 1280, 900, false, "light", false);
  await scenario("cover", 344, 780, true, "light", true);
  await scenario("cover", 344, 780, true, "dark", false);
  await scenario("fold", 841, 701, true, "dark", false);
  await scenario("fold", 841, 701, true, "light", false);
  finish();
});
