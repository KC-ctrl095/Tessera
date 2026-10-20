// Notes: a live page you write in like a document (markdown underneath,
// drawn as you type, Obsidian-style): typing shortcuts that format by
// themselves, keys, smart lists, wikilinks and backlinks, the insert menu,
// tables edited in place, tasks, rich text pasted as markdown, pictures
// (pasted, picked, from a phone's camera) kept on the server and fetched
// over the app's connection, markdown that comes back byte for byte, saving
// that never mistakes its own saves for someone else's, real conflicts
// asked about, an agent's changes shown, tinted and undone, version
// history, read only, export, and on phones and the Fold: swiping between
// pages over a note, the format bar, light and dark.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const touch = (p, type, pts) => p.send("Input.dispatchTouchEvent", { type, touchPoints: pts.map(([x, y], id) => ({ x, y, id })) });
const swipe = async (p, x0, y, x1, { steps = 10, hold = 0 } = {}) => {
  await touch(p, "touchStart", [[x0, y]]);
  if (hold) await sleep(hold);
  for (let k = 1; k <= steps; k++) {
    await touch(p, "touchMove", [[x0 + ((x1 - x0) * k) / steps, y + k * 0.3]]);
    await sleep(16);
  }
  await touch(p, "touchEnd", []);
  await sleep(650);
};
const CTRL = 2;
const SHIFT = 8;
const ALT = 1;

/** Real-world notes that must come back exactly as written. */
const CORPUS = {
  "front matter + Obsidian": `---
title: Weekly review
tags: [review, weekly]
date: 2026-09-28
---

# Weekly review

> [!info] Context
> Done on Sundays. Links: [[Goals 2026]], [[Reading list|books]].

## Wins
* Shipped the *notes* editor
* Fixed __two__ flaky tests
    * one was a race
    * the other a timeout

## Next
1) Plan the trip
2) Call mum

Footnote here.[^1]

[^1]: A footnote that most editors would drop.
`,
  "GitHub issue": `### Steps to reproduce

1. Open the app
2. Paste \`<b>bold</b>\`
3. See *nothing*

| Platform | Works? | Notes |
|:---------|:------:|------:|
| Linux    | ✅     | WebKitGTK 2.48 |
| Android  | ❌     | Gboard, see #42 |

- [x] reproduced
- [ ] fixed
- [ ] tested

\`\`\`rust
fn main() {
    println!("hello {}", 1 + 1);
}
\`\`\`

<details>
<summary>Logs</summary>

    2026-10-01T10:00:00Z ERROR something broke
    2026-10-01T10:00:01Z INFO  retrying

</details>
`,
  "messy hand-written": `Groceries
=========

Remember: **eggs**, milk & bread
(two spaces above make a hard break)\\
and a backslash break.

Some text with a_snake_case_name, 2*3*4, and a [ref link][r1].

[r1]: https://example.com/a(b)c "Title"

+ plus bullets
+ more

***

Tabs:	here	and	there.
Emoji 🙂 and RTL: שלום עולם.
Trailing spaces at the end
<!-- a comment -->
<span style="color:red">inline html</span>


Two blank lines above, none at the end`,
  "Windows line ends": "Line one\r\n\r\n- a\r\n- b\r\n\r\n**bold**\r\n",
  "long lines and escapes": `\\# not a heading, \\*not italic\\*, 1\\. not a list

${"A very long line of words that goes on and on to wrap many times over in a narrow tile, ".repeat(6)}

==highlighted== ~~struck~~ ~not sub~ ^not sup^ $x^2$ and $$\\int$$

> quote
continued lazily
> > nested quote

    indented code block
    second line
`,
};

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");
  let probe = await wsClient("probe");
  const server = async (id) => (await probe.rpc("get_note", { id })).result;
  const p = await device();
  await p.viewport(1280, 860);
  await signIn(p, "Desktop");
  /** Poll until `fn` is true (the machine may be busy: no fixed waits for saves). */
  const until = async (fn, ms = 8000) => {
    const t0 = Date.now();
    while (Date.now() - t0 < ms) {
      if (await fn()) return true;
      await sleep(120);
    }
    return false;
  };
  const saved = (id, fn, ms) => until(async () => fn(await server(id)), ms);
  const paneOf = (pg, id) => pg.eval(`__mod('lib/workspace.ts').then(m => m.workspace.order().find(id => m.workspace.panes[id].content.noteId === '${id}') ?? null)`);
  const show = async (title, content, pg = p) => {
    const n = (await probe.rpc("save_note", { title, content })).result;
    await pg.eval(`__mod('lib/notes.ts').then(m => m.openNote('${n.id}', { newPane: true }))`);
    let pane = null;
    await until(async () => (pane = await paneOf(pg, n.id)) && (await pg.eval(`!!document.querySelector('[data-pane-id="${pane}"] .note-ed .cm-content')`)), 10000);
    await sleep(250);
    return { id: n.id, pane, sel: `[data-pane-id="${pane}"]` };
  };
  const V = (t) => `document.querySelector('${t.sel} .cm-content').cmTile.root.view`;
  const doc = (t, pg = p) => pg.eval(`${V(t)}.state.doc.toString()`);
  const head = (t) => p.eval(`${V(t)}.state.selection.main.head`);
  const caret = (t, pos) => p.eval(`(() => { const v = ${V(t)}; v.focus(); v.dispatch({ selection: { anchor: ${pos} } }); })()`);
  const caretEnd = (t) => p.eval(`(() => { const v = ${V(t)}; v.focus(); v.dispatch({ selection: { anchor: v.state.doc.length } }); })()`);
  const shown = (t) => p.eval(`document.querySelector('${t.sel} .cm-content').innerText`);
  const K = (key, code, kc, mods = 0, pg = p) => pg.key(key, code, mods, kc);
  const enter = () => K("Enter", "Enter", 13);
  const status = (t) => p.eval(`document.querySelector('${t.sel} .note-status')?.textContent ?? ''`);
  const warn = (t) => p.eval(`!!document.querySelector('${t.sel} .note-bar.warn')`);
  const bar = (t) => p.eval(`document.querySelector('${t.sel} .note-bar:not(.warn) .note-bar-text')?.textContent ?? null`);
  const editing = (t) => p.eval(`document.activeElement?.matches('${t.sel} .cm-content') ?? false`);
  /** Where `text` is on the page: `frac` of the way along it. */
  const spotOf = (t, text, frac = 0.5, pg = p) =>
    pg.eval(`(() => { const want = ${JSON.stringify(text)}; const root = document.querySelector('${t.sel} .cm-content'); const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT); let n; while ((n = w.nextNode()) && !n.data.includes(want)); if (!n) return null; n.parentElement.scrollIntoView({ block: 'center' }); const r = document.createRange(); const i = n.data.indexOf(want); r.setStart(n, i); r.setEnd(n, i + want.length); const b = r.getBoundingClientRect(); return { x: b.x + Math.max(1, Math.min(b.width - 1, b.width * ${frac})), y: b.y + b.height / 2 }; })()`);
  const clickText = async (t, text, frac = 0.5) => {
    const b = await spotOf(t, text, frac);
    if (!b) throw new Error(`no text ${text}`);
    await p.mouse(b.x, b.y);
    await sleep(150);
  };
  const tileShot = async (t, name, pg = p, height) => {
    await pg.shot(`${SHOTS}/${name}.png`, await pg.eval(`(() => { const r = document.querySelector('${t.sel}').getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: ${height ?? "r.height"} }; })()`));
  };
  const menu = async (t, label) => {
    await p.click(`${t.sel} [aria-label="Note options"]`);
    await sleep(250);
    if (label) await p.clickText(".menu-item", label);
    await sleep(250);
  };
  // Links open outside the app: noted here instead.
  await p.eval(`window.__opened = []; window.open = (u) => { window.__opened.push(u); return null; }`);

  // ── the logic underneath (lib/noteSync.ts, lib/textDiff.ts) ───────────
  console.log("— keeping in step: the rules");
  const lib = await p.eval(`Promise.all([__mod('lib/noteSync.ts'), __mod('lib/textDiff.ts')]).then(([s, d]) => {
    const note = (version, content, title = 'T') => ({ id: 'n', title, content, version, created_at: 0, updated_at: 0, by_user: true, agent_edit: false });
    const base = note(3, 'a\\nb\\nc');
    const r = (local, remote, sent = []) => s.reconcile(base, { title: 'T', content: local }, remote, sent).kind;
    const m = (b, x, y) => { const o = d.merge3(b, x, y); return o.ok ? o.text : null; };
    const map = d.offsetMap(d.diffText('one two three', 'one 2 two three!'));
    return {
      older: r('a\\nb\\nc', note(3, 'zzz')),
      ownEcho: r('a\\nb\\nc typed on', note(4, 'a\\nb\\nc typed'), [s.noteKey('T', 'a\\nb\\nc typed')]),
      versionOnly: r('a\\nb\\nc!', note(4, 'a\\nb\\nc')),
      follow: r('a\\nb\\nc', note(4, 'a\\nB\\nc')),
      same: r('a\\nB\\nc', note(4, 'a\\nB\\nc')),
      merge: s.reconcile(base, { title: 'T', content: 'A\\nb\\nc' }, note(4, 'a\\nb\\nC'), []),
      conflict: r('a\\nB1\\nc', note(4, 'a\\nB2\\nc')),
      titleClash: s.reconcile(base, { title: 'Mine', content: 'a\\nb\\nc' }, note(4, 'a\\nb\\nc', 'Theirs'), []).kind,
      typingAtEndAndAppend: m('para', 'para more', 'para\\n\\nAgent: added'),
      bothAppend: m('x\\n', 'x\\nmine\\n', 'x\\ntheirs\\n'),
      sameChange: m('a\\nb', 'a\\nB', 'a\\nB'),
      titles: [s.serverTitle('Groceries  '), s.serverTitle(''), s.serverTitle('  two\\nlines'), s.serverTitle('é'.repeat(70))],
      offsets: [map(0), map(4), map(8, -1), map(13, -1), map(13, 1)],
      stat: d.diffStat(d.diffText('## Launch\\n\\n- calm', '## Launch\\n\\n- calm\\n- Send the announcement')),
    };
  })`);
  check(lib.older === "ignore" && lib.ownEcho === "rebase" && lib.versionOnly === "rebase", "its own save coming back (or only the version moving) never counts as someone else's", lib);
  check(lib.follow === "follow" && lib.same === "rebase", "with nothing typed it follows; the same words both sides are no question");
  check(lib.merge.kind === "merge" && lib.merge.content === "A\nb\nC", "changes to different lines combine", lib.merge);
  check(lib.conflict === "conflict" && lib.titleClash === "conflict", "the same line, or two new titles, is a question");
  check(lib.typingAtEndAndAppend === "para more\n\nAgent: added", "typing at the end while an agent appends: both kept", lib.typingAtEndAndAppend);
  check(lib.bothAppend === null && lib.sameChange === "a\nB", "two different additions at one place ask; the same change is made once");
  check(JSON.stringify(lib.titles) === JSON.stringify(["Groceries", "Untitled note", "two", "é".repeat(60) + "…"]), "titles are compared as the server keeps them", lib.titles);
  check(JSON.stringify(lib.offsets) === JSON.stringify([0, 6, 10, 15, 16]), "offsets move with the text around them", lib.offsets);
  check(lib.stat.added === 1 && lib.stat.removed === 0, "one added line counts as one", lib.stat);

  // ── markdown comes back exactly as it was written ─────────────────────
  console.log("— round trip: real notes, untouched and touched");
  for (const [name, content] of Object.entries(CORPUS)) {
    const t = await show(name, content);
    const v0 = (await server(t.id)).version;
    // The editor works with plain line breaks; a note with Windows ones gets its own back.
    const crlf = content.includes("\r\n");
    const lf = content.replace(/\r\n/g, "\n");
    const same = (await doc(t)) === lf;
    // Look around in it (caret moves, a scroll): still nothing to save.
    await caret(t, Math.floor(content.length / 2));
    await caret(t, 3);
    await p.eval(`document.querySelector('${t.sel} .cm-scroller').scrollTop = 1e6`);
    await sleep(1200);
    const n = await server(t.id);
    check(same && n.version === v0 && n.content === content && (await status(t)) === "Saved", `“${name}”: opened, looked through, left: not a byte changed`, { same, version: [v0, n.version] });
    // Two letters typed in the middle of a word of a line of text (not code, not a table): only they change.
    let pos = -1;
    let fence = false;
    let at = 0;
    for (const line of lf.split("\n")) {
      if (/^\s*(```|~~~)/.test(line)) fence = !fence;
      else if (!fence && at > lf.length / 3 && /^(?:[-*+] |\d+[.)] |#+ )?[A-Za-z]/.test(line)) {
        const word = /[A-Za-z]{4,}/.exec(line);
        if (word) {
          pos = at + word.index + 2;
          break;
        }
      }
      at += line.length + 1;
    }
    if (pos >= 0) {
      await caret(t, pos);
      await p.type("XY");
      const typed = lf.slice(0, pos) + "XY" + lf.slice(pos);
      const want = crlf ? typed.replace(/\n/g, "\r\n") : typed;
      await saved(t.id, (n) => n.content.includes("XY"));
      const got = (await server(t.id)).content;
      check(got === want, `“${name}”: two letters typed in a line change just those two letters`, got === want ? undefined : { at: pos, got: got.slice(Math.max(0, pos - 30), pos + 30) });
    }
    await ws(p, `m.closePane('${t.pane}')`);
    await sleep(200);
  }

  // ── saving: its own saves are never "changed elsewhere" ───────────────
  console.log("— saving on a slow link");
  const a = await show("Plans", "");
  check(!(await p.eval(`!!document.querySelector('${a.sel} .note-modes, ${a.sel} [aria-label="Edit"], ${a.sel} textarea.note-editor')`)), "no Write/Read switch, no Edit button, no source view");
  await probe.rpc("mock_note_latency", { ms: 500 });
  await p.click(`${a.sel} .cm-content`);
  check(await editing(a), "a click on an empty note starts writing");
  await p.type("Hello");
  await sleep(850); // the save goes out…
  await p.type(" world"); // …and typing goes on while it travels (its note_updated comes first)
  await sleep(500);
  await p.type(", again");
  await saved(a.id, (n) => n.content === "Hello world, again");
  await until(async () => (await status(a)) === "Saved");
  check(!(await warn(a)), "typing on while saves travel raises no “changed elsewhere”");
  check((await server(a.id)).content === "Hello world, again" && (await status(a)) === "Saved", "…and all of it is saved", [(await server(a.id)).content, await status(a)]);

  console.log("— a save whose answer is lost");
  await probe.rpc("mock_note_latency", { ms: 1500 });
  await p.type(" More.");
  await sleep(900); // sent; the answer would come in 1.5 s
  await mock("/__mock/drop");
  probe = await wsClient("probe"); // dropped too
  await probe.rpc("mock_note_latency", { ms: 0 });
  await sleep(300);
  await until(() => p.eval(`__mod('lib/store.ts').then(m => m.state.conn.state === 'connected' && m.state.ready)`), 15000);
  await p.type(" Then this.");
  await saved(a.id, (n) => n.content.endsWith("Then this."));
  check(!(await warn(a)), "a save whose answer was lost isn't taken for someone else's");
  check((await server(a.id)).content === "Hello world, again More. Then this.", "…and what came after is saved on top", (await server(a.id)).content);

  console.log("— title with a space at the end");
  await p.click(`${a.sel} .note-title`);
  await p.eval(`document.querySelector('${a.sel} .note-title').select()`);
  await p.type("Groceries ");
  await probe.rpc("mock_note_latency", { ms: 400 });
  await sleep(800);
  await p.type("list");
  await saved(a.id, (n) => n.title === "Groceries list");
  await probe.rpc("mock_note_latency", { ms: 0 });
  check(!(await warn(a)) && (await server(a.id)).title === "Groceries list", "a title saved while still being typed raises nothing", (await server(a.id)).title);
  await enter();
  check((await editing(a)) && (await head(a)) === 0, "Enter in the title goes on into the text, at its start");
  await ws(p, `m.closePane('${a.pane}')`);

  // ── typing: it formats itself ─────────────────────────────────────────
  console.log("— typing formats by itself");
  const w = await show("Writing", "");
  await p.click(`${w.sel} .cm-content`);
  await p.type("#");
  check((await shown(w)).includes("#"), "a lone # while typing stays a # (no flicker to a heading)");
  await p.type(" Weekend");
  const h1 = await p.eval(`(() => { const l = document.querySelector('${w.sel} .cm-line'); return { cls: l.className, text: l.innerText }; })()`);
  check(/cm-h1/.test(h1.cls) && h1.text === "Weekend", "“# ” makes a heading at once, and the # isn't shown", h1);
  await enter();
  await p.type("Bring **sun cream**");
  check((await shown(w)).includes("Bring sun cream") && (await p.eval(`!!document.querySelector('${w.sel} .cm-strong')`)), "**bold** turns bold as soon as it closes, its stars hidden", await shown(w));
  await p.type(" and *a hat* and `keys` and ~~worries~~ and ==joy==.");
  const inl = await p.eval(`['.cm-em', '.cm-icode', '.cm-strike', '.cm-hl'].map(c => document.querySelector('${w.sel} ' + c)?.textContent ?? null)`);
  check(JSON.stringify(inl) === JSON.stringify(["a hat", "keys", "worries", "joy"]), "italic, code, strike and highlight too", inl);
  check(!(await shown(w)).includes("*") && !(await shown(w)).includes("`"), "…with their marks out of sight", await shown(w));
  // The caret inside a styled span shows its marks (to see where it starts and stops).
  const sunAt = (await doc(w)).indexOf("sun cream") + 3;
  await caret(w, sunAt);
  await sleep(100);
  check((await shown(w)).includes("**sun cream**"), "the caret inside bold shows its stars", (await shown(w)).split("\n")[1]);
  await caretEnd(w);
  await enter();
  await p.type("- passport");
  await enter();
  await p.type("tickets");
  check((await doc(w)).endsWith("- passport\n- tickets") && (await p.eval(`document.querySelectorAll('${w.sel} .cm-mark-bullet').length`)) === 2, "“- ” makes a list; Enter carries it on", (await doc(w)).slice(-30));
  await K("Tab", "Tab", 9);
  check((await doc(w)).endsWith("- passport\n  - tickets"), "Tab nests the item under the one above", (await doc(w)).slice(-30));
  await K("Tab", "Tab", 9, SHIFT);
  check((await doc(w)).endsWith("- passport\n- tickets"), "Shift+Tab brings it back", (await doc(w)).slice(-30));
  await enter();
  await enter();
  check((await doc(w)).endsWith("- tickets\n"), "Enter on an empty item ends the list", JSON.stringify((await doc(w)).slice(-20)));
  await p.type("1. first");
  await enter();
  await p.type("second");
  check((await doc(w)).endsWith("1. first\n2. second"), "numbered lists count on", (await doc(w)).slice(-20));
  // A phone keyboard pressing Enter right after a word hands over "word\n" in one piece: still Enter.
  await p.type(" half\n");
  check((await doc(w)).endsWith("2. second half\n3. "), "a word and a line break typed as one (a phone keyboard) still carry the list on", JSON.stringify((await doc(w)).slice(-24)));
  await enter();
  await enter();
  await p.type("[ ] ");
  await p.type("call the vet");
  check((await doc(w)).endsWith("- [ ] call the vet") && (await p.eval(`!!document.querySelector('${w.sel} .cm-task')`)), "“[ ] ” at a line's start makes a task with a box", (await doc(w)).slice(-24));
  await enter();
  await p.type("feed the cat");
  check((await doc(w)).endsWith("- [ ] call the vet\n- [ ] feed the cat"), "Enter on a task starts the next task", (await doc(w)).slice(-40));
  await K("Enter", "Enter", 13, CTRL);
  check((await doc(w)).endsWith("- [x] feed the cat"), "Ctrl+Enter ticks it", (await doc(w)).slice(-20));
  await enter();
  await enter();
  await p.type("> worth it");
  check(/cm-quote/.test(await p.eval(`[...document.querySelectorAll('${w.sel} .cm-line')].at(-1).className`)) && !(await shown(w)).includes(">"), "“> ” makes a quote");
  await enter();
  await enter();
  await p.type("```js");
  await enter();
  await p.type("let x = 1;");
  check((await doc(w)).endsWith("```js\nlet x = 1;\n```"), "``` and Enter: a code block, closed for you, the caret in it", (await doc(w)).slice(-30));
  await caretEnd(w);
  await enter();
  await p.type("---");
  await enter();
  check((await p.eval(`!!document.querySelector('${w.sel} .cm-rule')`)) && /\n\n---\n$/.test(await doc(w)), "--- and Enter: a divider (kept apart from the text above)", JSON.stringify((await doc(w)).slice(-12)));
  // Keys.
  await p.type("plain words here");
  await K("ArrowLeft", "ArrowLeft", 37, CTRL);
  await K("ArrowLeft", "ArrowLeft", 37, CTRL | SHIFT);
  await K("b", "KeyB", 66, CTRL);
  check((await doc(w)).endsWith("plain **words** here"), "Ctrl+B bolds the selection", (await doc(w)).slice(-24));
  await K("b", "KeyB", 66, CTRL);
  check((await doc(w)).endsWith("plain words here"), "…and again takes it off", (await doc(w)).slice(-24));
  await K("i", "KeyI", 73, CTRL);
  await K("X", "KeyX", 88, CTRL | SHIFT);
  check((await doc(w)).endsWith("plain *~~words~~* here"), "Ctrl+I and Ctrl+Shift+X: italic and struck", (await doc(w)).slice(-26));
  await K("z", "KeyZ", 90, CTRL);
  await K("z", "KeyZ", 90, CTRL);
  check((await doc(w)).endsWith("plain words here"), "Ctrl+Z undoes, step by step", (await doc(w)).slice(-24));
  await K("Z", "KeyZ", 90, CTRL | SHIFT);
  check((await doc(w)).endsWith("plain *words* here"), "Ctrl+Shift+Z redoes", (await doc(w)).slice(-24));
  await K("2", "Digit2", 50, CTRL | ALT);
  check((await doc(w)).endsWith("## plain *words* here"), "Ctrl+Alt+2 makes the line a heading", (await doc(w)).slice(-24));
  await K("Home", "Home", 36);
  check((await head(w)) === (await doc(w)).lastIndexOf("## ") + 3, "Home goes to the heading's text, never into its #", await head(w));
  await K("Backspace", "Backspace", 8);
  check((await doc(w)).endsWith("\nplain *words* here"), "Backspace at a heading's start makes it text again", (await doc(w)).slice(-24));
  // A link from a selection: Ctrl+K, then its address.
  await caretEnd(w);
  await enter();
  await p.type("see the site");
  await K("ArrowLeft", "ArrowLeft", 37, CTRL | SHIFT);
  await K("k", "KeyK", 75, CTRL);
  await p.type("https://example.org/");
  await caretEnd(w);
  check((await doc(w)).endsWith("see the [site](https://example.org/)") && (await shown(w)).endsWith("see the site"), "Ctrl+K makes a link: the address is typed, then out of sight", (await doc(w)).slice(-40));
  const site = await spotOf(w, "site");
  await p.mouse(site.x, site.y);
  check((await p.eval(`window.__opened`)).includes("https://example.org/"), "a click on a link opens it", await p.eval(`window.__opened`));
  // A selection typed over with * is wrapped.
  await caretEnd(w);
  await enter();
  await p.type("wrap me");
  await K("ArrowLeft", "ArrowLeft", 37, CTRL | SHIFT);
  await p.type("*");
  check((await doc(w)).endsWith("wrap *me*"), "typing * over a selection wraps it", (await doc(w)).slice(-10));
  // A click left of a bullet puts the caret at the item's text.
  const tk = await spotOf(w, "tickets", 0);
  await p.mouse(tk.x - 14, tk.y);
  await sleep(120);
  check((await head(w)) === (await doc(w)).indexOf("tickets"), "a click on a bullet puts the caret at its text", [await head(w), (await doc(w)).indexOf("tickets")]);
  // Ticking a box by click.
  await p.click(`${w.sel} .cm-task[aria-checked="false"]`);
  check((await doc(w)).includes("- [x] call the vet"), "a click on a task box ticks it");
  await saved(w.id, (n) => n.content.includes("- [x] call the vet") && n.content.endsWith("wrap *me*"));
  check((await server(w.id)).content.includes("## Weekend") === false && (await server(w.id)).content.startsWith("# Weekend\nBring **sun cream**"), "all of it is saved as plain markdown", (await server(w.id)).content.slice(0, 40));
  await p.theme("dark");
  await caret(w, 0);
  await tileShot(w, "notes-writing-desktop");
  await p.theme("light");
  await tileShot(w, "notes-writing-desktop-light");
  await p.theme("dark");
  await ws(p, `m.closePane('${w.pane}')`);

  // ── links between notes ───────────────────────────────────────────────
  console.log("— wikilinks and backlinks");
  const target = (await probe.rpc("save_note", { title: "Packing list", content: "- passport\n- charger" })).result;
  await probe.rpc("save_note", { title: "Paris", content: "Croissants." });
  const l = await show("Trip", "Plans for the trip.");
  await caretEnd(l);
  await enter();
  await p.type("Bring [[");
  await until(() => p.eval(`!!document.querySelector('.cm-tooltip-autocomplete li')`), 3000);
  const sugs = await p.eval(`[...document.querySelectorAll('.cm-tooltip-autocomplete li')].map(e => e.textContent)`);
  check(sugs.some((s) => s.includes("Packing list")) && sugs.some((s) => s.includes("Paris")) && !sugs.some((s) => s.includes("Trip")), "[[ suggests the other notes (not this one)", sugs);
  await p.type("pack");
  await sleep(250);
  await enter();
  check((await doc(l)).endsWith("Bring [[Packing list]]") && (await shown(l)).endsWith("Bring Packing list"), "picking one writes the link, drawn as its title", (await doc(l)).slice(-26));
  await p.type(" and [[Someday notes]]");
  await saved(l.id, (n) => n.content.includes("[[Someday notes]]"));
  check(await p.eval(`!!document.querySelector('${l.sel} .cm-wiki-new')`), "a link to a note that isn't there yet looks different");
  await clickText(l, "Packing list");
  const opened = await until(async () => !!(await paneOf(p, target.id)), 4000);
  check(opened, "a click on a link opens its note");
  const tg = { id: target.id, pane: await paneOf(p, target.id) };
  tg.sel = `[data-pane-id="${tg.pane}"]`;
  await until(() => p.eval(`!!document.querySelector('${tg.sel} .note-links')`), 5000);
  const back = await p.eval(`document.querySelector('${tg.sel} .note-links')?.innerText ?? ''`);
  check(/Linked from/i.test(back) && back.includes("Trip"), "the linked note says what links to it (Linked from)", back);
  await p.click(`${tg.sel} .note-link`);
  await sleep(500);
  check((await ws(p, `return m.workspace.panes[m.workspace.focused()]?.content.noteId`)) === l.id, "…and a click there goes back", await ws(p, `return m.workspace.panes[m.workspace.focused()]?.content.noteId`));
  await clickText(l, "Someday notes");
  await sleep(300);
  await p.clickText(".menu-item", "Someday notes");
  await until(async () => (await probe.rpc("list_notes")).result.some((n) => n.title === "Someday notes"), 4000);
  check((await probe.rpc("list_notes")).result.some((n) => n.title === "Someday notes"), "a link to a missing note offers to make it, and does");
  await sleep(400);
  check(!(await p.eval(`!!document.querySelector('${l.sel} .cm-wiki-new')`)), "…and the link is drawn as found then");
  await tileShot(l, "notes-links-desktop", p, 260);
  for (const id of await ws(p, `return m.workspace.order().filter(id => m.workspace.panes[id].content.type === 'note')`)) await ws(p, `m.closePane('${id}')`);

  // ── the insert menu and tables ────────────────────────────────────────
  console.log("— the insert menu, tables");
  const tb = await show("Shopping", "Things to buy:");
  await caretEnd(tb);
  await enter();
  await p.type("/");
  await until(() => p.eval(`!!document.querySelector('.cm-tooltip-autocomplete li')`), 3000);
  const items = await p.eval(`[...document.querySelectorAll('.cm-tooltip-autocomplete li')].map(e => e.textContent.replace(/^\\W+|^H\\d/, ''))`);
  check(["Heading 1", "Bulleted list", "To-do list", "Table", "Image", "Code block", "Callout", "Divider"].every((x) => items.some((i) => i.includes(x))), "“/” offers headings, lists, tasks, table, image, code, callout, divider", items);
  await p.type("tab");
  await sleep(250);
  check((await p.eval(`[...document.querySelectorAll('.cm-tooltip-autocomplete li')].map(e => e.textContent)`)).every((x) => /Table/.test(x)), "typing after / narrows it");
  await enter();
  await until(() => p.eval(`document.activeElement?.classList.contains('cm-cell-edit') ?? false`), 4000);
  const cell = await p.eval(`(() => { const a = document.activeElement; return a?.classList.contains('cm-cell-edit') ? { v: a.value, sel: a.selectionEnd - a.selectionStart } : null; })()`);
  check(cell?.v === "Column 1" && cell.sel === 8, "Table puts a table in and its first heading is ready to type over", cell);
  await p.type("Item");
  await K("Tab", "Tab", 9);
  await p.type("Qty");
  await K("Tab", "Tab", 9);
  await p.type("Apples");
  await K("Tab", "Tab", 9);
  await p.type("3");
  await K("Tab", "Tab", 9);
  await p.type("Pears");
  await K("Tab", "Tab", 9);
  await p.type("2|4");
  await K("Escape", "Escape", 27);
  await p.type("Done.");
  const tdoc = await doc(tb);
  check(tdoc.includes("| Item | Qty |\n| --- | --- |\n| Apples | 3 |\n| Pears | 2\\|4 |\n\nDone."), "cells are typed in place; Tab moves on and adds a row at the end; Esc leaves below", tdoc);
  // The cell menu: a column, a row away.
  await p.click(`${tb.sel} .cm-table td[data-r="0"][data-c="1"]`);
  await sleep(150);
  await p.click(`${tb.sel} .cm-cell-menu`);
  await sleep(250);
  const tm = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-label')].map(e => e.textContent)`);
  check(["Row above", "Row below", "Column left", "Column right", "Delete row", "Delete column", "Delete table"].every((x) => tm.includes(x)), "a cell's ⋯ adds and removes rows and columns", tm);
  await p.clickText(".menu-item", "Column right");
  await sleep(300);
  await p.type("Shop");
  await p.click(`${tb.sel} .cm-table td[data-r="1"][data-c="0"]`);
  await sleep(150);
  await p.click(`${tb.sel} .cm-cell-menu`);
  await sleep(250);
  await p.clickText(".menu-item", "Delete row");
  await sleep(300);
  const tdoc2 = await doc(tb);
  check(tdoc2.includes("| Item | Qty |  |\n| --- | --- | --- |\n| Apples | 3 | Shop |\n\nDone."), "…and each change touches only those cells in the markdown", tdoc2);
  await saved(tb.id, (n) => n.content === tdoc2);
  await tileShot(tb, "notes-table-desktop", p, 320);
  await ws(p, `m.closePane('${tb.pane}')`);

  // ── pasting ───────────────────────────────────────────────────────────
  console.log("— pasting");
  const ps = await show("Pasted", "Notes:");
  const paste = (t, data) => p.eval(`(() => { const dt = new DataTransfer(); for (const [k, v] of Object.entries(${JSON.stringify(data)})) dt.setData(k, v); ${V(t)}.contentDOM.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true })); })()`);
  await caretEnd(ps);
  await enter();
  const gdocs = `<meta charset="utf-8"><b style="font-weight:normal;" id="docs-internal-guid-9"><h2 dir="ltr"><span style="font-weight:700">Agenda</span></h2><p dir="ltr"><span style="font-weight:700">Bold</span><span style="font-weight:400"> and </span><span style="font-style:italic">italic</span><span> and </span><span style="text-decoration:line-through">gone</span><span> and </span><a href="https://example.com/x"><span>a link</span></a></p><ul><li aria-level="1"><p>one</p></li><li aria-level="1"><p>two</p></li><ul><li aria-level="2"><p>nested</p></li></ul></ul><ol><li><p>first</p></li><li><p>second</p></li></ol><table><tbody><tr><td><p>Name</p></td><td><p>Role</p></td></tr><tr><td><p>Ana</p></td><td><p>A|B</p></td></tr></tbody></table></b>`;
  await paste(ps, { "text/html": gdocs, "text/plain": "Agenda\nBold and italic" });
  await sleep(200);
  const pd = await doc(ps);
  check(pd.includes("## Agenda\n\n**Bold** and *italic* and ~~gone~~ and [a link](https://example.com/x)\n\n- one\n- two\n  - nested\n\n1. first\n2. second\n\n| Name | Role |\n| --- | --- |\n| Ana | A\\|B |"), "Google Docs text comes in as markdown: heading, styles, link, nested list, numbers, table", pd);
  await caretEnd(ps);
  const web = `<article><h3>Install</h3><p>Run <code>npm i</code> then:</p><pre><code class="language-sh">npm run build\nnpm test</code></pre><ul><li><input type="checkbox" checked disabled> done</li><li><input type="checkbox" disabled> todo</li></ul><blockquote><p>Quoted <em>words</em></p></blockquote><script>alert(1)</script><img src="https://tracker.example/p.gif" alt="pixel"></article>`;
  await paste(ps, { "text/html": web, "text/plain": "Install" });
  await sleep(200);
  const pwText = await doc(ps);
  check(pwText.includes("### Install\n\nRun `npm i` then:\n\n```sh\nnpm run build\nnpm test\n```\n\n- [x] done\n- [ ] todo\n\n> Quoted *words*") && !pwText.includes("alert"), "a web page's code, tasks and quotes too (its scripts never)", pwText.slice(pd.length));
  check(await p.eval(`!!document.querySelector('${ps.sel} .cm-pic-missing')`), "a picture from the web isn't fetched (shown as a note of it)");
  await caretEnd(ps);
  await enter();
  await paste(ps, { "text/plain": "**already** markdown\n\n- [ ] item" });
  await sleep(150);
  check((await doc(ps)).endsWith("**already** markdown\n\n- [ ] item") && (await p.eval(`!!document.querySelectorAll('${ps.sel} .cm-task').length`)), "pasted markdown goes in as it is and is drawn", (await doc(ps)).slice(-36));
  await caretEnd(ps);
  await enter();
  await p.type("our site");
  await K("ArrowLeft", "ArrowLeft", 37, CTRL | SHIFT);
  await paste(ps, { "text/plain": "https://tessera.example/" });
  await sleep(100);
  check((await doc(ps)).endsWith("our [site](https://tessera.example/)"), "a web address pasted over words links them", (await doc(ps)).slice(-36));
  await tileShot(ps, "notes-paste-desktop");
  await ws(p, `m.closePane('${ps.pane}')`);

  // ── pictures ──────────────────────────────────────────────────────────
  console.log("— pictures");
  const pic = await show("Album", "Our trip:");
  /** A picture made here: `w`×`h`, one colour (PNG). */
  const makePng = (w, h, color) => `await new Promise((res) => { const c = document.createElement('canvas'); c.width = ${w}; c.height = ${h}; const g = c.getContext('2d'); g.fillStyle = '${color}'; g.fillRect(0, 0, ${w}, ${h}); g.fillStyle = '#fff'; g.fillRect(${w} / 4, ${h} / 4, ${w} / 2, ${h} / 2); c.toBlob((b) => res(new File([b], 'beach.png', { type: 'image/png' })), 'image/png'); })`;
  await caretEnd(pic);
  await enter();
  await p.eval(`(async () => { const f = ${makePng(320, 200, "#3a7")}; const dt = new DataTransfer(); dt.items.add(f); ${V(pic)}.contentDOM.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true })); })()`);
  await until(async () => /!\[[^\]]*\]\(assets\/[0-9a-f]{16}\.png\)/.test(await doc(pic)), 8000);
  const pd1 = await doc(pic);
  const name1 = /assets\/([0-9a-f]{16}\.png)/.exec(pd1)?.[1];
  check(!!name1 && /!\[beach\]\(assets\/[0-9a-f]{16}\.png\)/.test(pd1), "a pasted picture goes to the server and into the note as ![…](assets/…)", pd1);
  await until(() => p.eval(`(() => { const i = document.querySelector('${pic.sel} .cm-pic img'); return !!i && i.complete && i.naturalWidth > 0; })()`), 6000);
  const im = await p.eval(`(() => { const i = document.querySelector('${pic.sel} .cm-pic img'); return i ? { src: i.src.slice(0, 5), w: i.naturalWidth } : null; })()`);
  check(im?.src === "blob:" && im.w === 320, "…shown from what came over the connection (a blob:, no picture address)", im);
  const asset = (await probe.rpc("read_note_asset", { name: name1, offset: 0 })).result;
  check(asset?.size > 100 && asset.eof, "the server serves it to a signed-in device, in pieces", asset && { size: asset.size, eof: asset.eof });
  // Picked with the header's picture button (its one place on a computer; ⋯ no longer repeats it), a big one: made smaller first.
  await caretEnd(pic);
  await p.eval(`(() => { const i = document.querySelector('${pic.sel} input[type=file]:not([capture])'); i.click = () => { window.__picked = true; }; })()`);
  await p.click(`${pic.sel} .note-add-pic`);
  await sleep(200);
  check(await p.eval(`window.__picked === true`), "the header's picture button opens the picker");
  await p.eval(`(async () => { const f = ${makePng(3000, 1500, "#a53")}; const i = document.querySelector('${pic.sel} input[type=file]:not([capture])'); const dt = new DataTransfer(); dt.items.add(f); i.files = dt.files; i.dispatchEvent(new Event('change', { bubbles: true })); })()`);
  await until(async () => ((await doc(pic)).match(/assets\//g) ?? []).length === 2, 8000);
  const name2 = [...(await doc(pic)).matchAll(/assets\/([0-9a-f]{16}\.\w+)/g)].map((m) => m[1]).find((n) => n !== name1);
  const big = name2 && (await probe.rpc("read_note_asset", { name: name2, offset: 0, len: 64 })).result;
  const dims = await p.eval(`(async () => { const u = await (await __mod('lib/noteAssets.ts')).assetUrl(${JSON.stringify(name2)}); const i = new Image(); i.src = u; await i.decode(); return [i.naturalWidth, i.naturalHeight]; })()`);
  check(!!big && dims[0] === 2560 && dims[1] === 1280, "a picked screenshot (PNG) is scaled to at most 2560 px before it is sent (photos: 2048)", dims);
  // A dropped one.
  await p.eval(`(async () => { const f = ${makePng(120, 120, "#36c")}; const dt = new DataTransfer(); dt.items.add(f); const r = document.querySelector('${pic.sel} .cm-content').getBoundingClientRect(); ${V(pic)}.contentDOM.dispatchEvent(new DragEvent('drop', { dataTransfer: dt, clientX: r.x + 20, clientY: r.y + 10, bubbles: true, cancelable: true })); })()`);
  await until(async () => ((await doc(pic)).match(/assets\//g) ?? []).length === 3, 8000);
  check(((await doc(pic)).match(/assets\//g) ?? []).length === 3, "a dropped picture goes in where it was dropped");
  await saved(pic.id, (n) => (n.content.match(/assets\//g) ?? []).length === 3);
  await tileShot(pic, "notes-pictures-desktop");
  // Its menu: smaller, a description, removed.
  await p.click(`${pic.sel} .cm-pic img`);
  await sleep(250);
  const pm = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-label')].map(e => e.textContent)`);
  check(["View", "Small", "Medium", "Full width", "Description…", "Save picture", "Remove"].every((x) => pm.includes(x)), "a click on a picture: view, size, description, save, remove", pm);
  await p.clickText(".menu-item", "Small");
  await sleep(200);
  check(/!\[[^\]|]*\|240\]\(assets\//.test(await doc(pic)), "Small writes the width the Obsidian way (alt|240)", (await doc(pic)).match(/!\[[^\]]*\]/g));
  const sizes = await p.eval(`[...document.querySelectorAll('${pic.sel} .cm-pic')].map(e => Math.round(e.getBoundingClientRect().width))`);
  check(sizes.includes(240), "…and it is drawn that wide", sizes);
  // Export: the note and its pictures.
  await p.eval(`(() => { window.__dl = null; const make = URL.createObjectURL; URL.createObjectURL = (b) => { window.__blob = b; return make.call(URL, b); }; HTMLAnchorElement.prototype.click = function () { if (this.download) window.__dl = { name: this.download }; }; })()`);
  await menu(pic, "Download (.zip)");
  await until(() => p.eval(`!!window.__dl`), 6000);
  const z = await p.eval(`window.__dl && window.__blob.arrayBuffer().then(b => { const u = new Uint8Array(b); const names = []; for (let i = 0; i + 30 < u.length; ) { if (u[i] !== 0x50 || u[i+1] !== 0x4b || u[i+2] !== 3 || u[i+3] !== 4) break; const size = u[i+18] | u[i+19] << 8 | u[i+20] << 16 | u[i+21] << 24; const nl = u[i+26] | u[i+27] << 8; names.push(new TextDecoder().decode(u.slice(i + 30, i + 30 + nl))); i += 30 + nl + size; } return { name: window.__dl.name, names }; })`);
  check(z?.name === "Album.zip" && z.names[0] === "Album.md" && z.names.filter((n) => n.startsWith("assets/")).length === 3, "Download gives a .zip: the note, and its pictures in assets/ as the note refers to them", z);
  await p.click(`${pic.sel} .cm-pic img`);
  await sleep(250);
  await p.clickText(".menu-item", "Remove");
  await sleep(200);
  check(((await doc(pic)).match(/assets\//g) ?? []).length === 2, "Remove takes one out");
  // Version history keeps the pictures: an earlier version still refers to it, and the server still has it.
  check((await probe.rpc("read_note_asset", { name: name1 })).result?.size > 0, "a picture taken out is still on the server (versions refer to it)");
  await ws(p, `m.closePane('${pic.pane}')`);

  // ── changes from elsewhere ────────────────────────────────────────────
  console.log("— changes from elsewhere");
  const b = await show("Trip notes", "Line one\n\nLine two\n\nLine three");
  await clickText(b, "Line one", 0.99);
  await K("End", "End", 35);
  await p.type(" (mine)");
  let n = await server(b.id);
  await probe.rpc("save_note", { id: b.id, title: "Trip notes", content: "Line one\n\nLine two\n\nLine three (theirs)", base_version: n.version });
  await saved(b.id, (n) => n.content.includes("(mine)") && n.content.includes("(theirs)"));
  n = await server(b.id);
  check(!(await warn(b)) && n.content === "Line one (mine)\n\nLine two\n\nLine three (theirs)", "a change to another line is combined with what's typed, no question", n.content);
  check((await doc(b)) === n.content && (await head(b)) === "Line one (mine)".length, "…it shows up here and the caret stays where it was", [await doc(b), await head(b)]);
  await p.type(" again");
  n = await server(b.id);
  await probe.rpc("save_note", { id: b.id, title: "Trip notes", content: n.content.replace("Line one (mine)", "Line one (other device)"), base_version: n.version });
  await until(() => warn(b));
  check(await warn(b), "a change to the same line asks which to keep");
  check((await doc(b)).startsWith("Line one (mine) again"), "…and what was typed here is still there", await doc(b));
  await p.clickText(`${b.sel} .note-bar button`, "Compare");
  await sleep(300);
  check(await p.eval(`(() => { const d = document.querySelector('${b.sel} .note-diff'); return !!d && d.querySelector('del')?.textContent.includes('other device') && [...d.querySelectorAll('ins')].some(e => e.textContent.includes('mine')); })()`), "Compare shows theirs → yours");
  await tileShot(b, "notes-conflict-desktop", p, 420);
  await p.clickText(`${b.sel} .note-bar button`, "Keep mine");
  await saved(b.id, (n) => n.content.startsWith("Line one (mine) again"));
  n = await server(b.id);
  check(n.content.startsWith("Line one (mine) again") && n.content.endsWith("(theirs)") && !(await warn(b)), "“Keep mine” saves it over theirs", n.content);
  await ws(p, `m.closePane('${b.pane}')`);

  // ── an agent's changes ────────────────────────────────────────────────
  console.log("— an agent's changes");
  const d = await show("Launch plan", "## Launch\n\n- Thursday at 16:00\n- Keep the day calm");
  await probe.rpc("mock_agent_edit", { id: d.id, find: "Keep the day calm", replace: "Keep the day calm\n- Send the announcement at 15:00" });
  await until(async () => (await bar(d)) !== null);
  check((await doc(d)).includes("Send the announcement"), "the tile follows an agent's change");
  check(/changed this note/.test((await bar(d)) ?? ""), "…and says an agent changed it", await bar(d));
  await until(() => p.eval(`!!document.querySelector('${d.sel} .cm-agent')`), 3000);
  const tinted = await p.eval(`[...document.querySelectorAll('${d.sel} .cm-agent')].map(e => e.textContent).join('')`);
  check(tinted.includes("Send the announcement at 15:00") && !tinted.includes("Thursday"), "what it added is tinted in the note itself", tinted);
  check((await p.eval(`document.querySelector('${d.sel} .note-by')?.textContent`))?.startsWith("Edited by"), "the header says who edited it last");
  await p.clickText(`${d.sel} .note-bar button`, "See changes");
  await sleep(300);
  check(await p.eval(`[...document.querySelectorAll('${d.sel} .note-diff ins')].some(e => e.textContent.includes('announcement'))`), "“See changes” marks what it added");
  await tileShot(d, "notes-agent-changes-desktop", p, 420);
  await p.clickText(`${d.sel} .np-actions button`, "Undo them");
  await saved(d.id, (n) => !n.content.includes("announcement"));
  check(!(await server(d.id)).content.includes("announcement") && !(await bar(d)) && !(await p.eval(`!!document.querySelector('${d.sel} .cm-agent')`)), "Undo takes them out again (and saves), the tint goes", (await server(d.id)).content);
  await caret(d, 0);
  await K("z", "KeyZ", 90, CTRL);
  check((await doc(d)).includes("announcement"), "…and Ctrl+Z brings them back (undo of the undo)");
  await K("Z", "KeyZ", 90, CTRL | SHIFT);
  // Undo keeps what you typed after the agent's change.
  await probe.rpc("mock_agent_edit", { id: d.id, append: "Agent: book the room." });
  await until(async () => (await doc(d)).includes("book the room"));
  await caret(d, (await doc(d)).indexOf("Launch") + 6);
  await p.type(" (v2)");
  await p.clickText(`${d.sel} .note-bar button`, "Undo");
  await saved(d.id, (n) => !n.content.includes("book the room"));
  n = await server(d.id);
  check(n.content.includes("## Launch (v2)") && !n.content.includes("book the room"), "Undo keeps your own edits made since", n.content);
  // Version history.
  await menu(d, "Version history");
  await until(() => p.eval(`!!document.querySelector('${d.sel} button.nh-row')`));
  const rows = await p.eval(`[...document.querySelectorAll('${d.sel} .nh-row')].map(e => e.textContent)`);
  check(rows.length >= 3, "Version history lists the kept versions", rows);
  await tileShot(d, "notes-history-desktop", p, 420);
  await p.click(`${d.sel} button.nh-row`);
  await until(() => p.eval(`!!document.querySelector('${d.sel} .note-diff')`));
  const before = (await server(d.id)).version;
  await p.clickText(`${d.sel} .np-actions button`, "Restore this version");
  await saved(d.id, (n) => n.version > before);
  check(!(await p.eval(`!!document.querySelector('${d.sel} .note-panel')`)) && (await doc(d)) === (await server(d.id)).content, "Restore brings it back as the newest version");
  // Opened later: the changes are offered once.
  await probe.rpc("mock_agent_edit", { id: d.id, append: "Agent: invite the press." });
  await ws(p, `m.closePane('${d.pane}')`);
  await sleep(400);
  await p.eval(`__mod('lib/notes.ts').then(m => m.openNote('${d.id}', { newPane: true }))`);
  await until(() => p.eval(`!!document.querySelector('.note-bar:not(.warn)')`));
  const d2 = { id: d.id, pane: await paneOf(p, d.id) };
  d2.sel = `[data-pane-id="${d2.pane}"]`;
  check(/changed this note/.test((await bar(d2)) ?? ""), "opened after an agent changed it, its changes are offered", await bar(d2));
  await p.click(`${d2.sel} .note-bar [aria-label="Dismiss"]`);
  await ws(p, `m.closePane('${d2.pane}')`);
  await sleep(300);
  await p.eval(`__mod('lib/notes.ts').then(m => m.openNote('${d.id}', { newPane: true }))`);
  await sleep(1200);
  check(!(await p.eval(`!!document.querySelector('.note-bar:not(.warn)')`)), "…once: dismissed, they aren't offered again");
  for (const id of await ws(p, `return m.workspace.order().filter(id => m.workspace.panes[id].content.type === 'note')`)) await ws(p, `m.closePane('${id}')`);

  // ── read only, find, outline, export ──────────────────────────────────
  console.log("— read only, find, outline, export");
  const c = await show("Shopping list", "# Shopping list\n\nBuy bread and [good](https://example.com) cheese today.\n\n- [ ] milk\n- [x] eggs\n\n## Later\n\nWine.\n\n## Much later\n\nCake.");
  await menu(c);
  const mi = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-label')].map(e => e.textContent)`);
  check(["Find in note", "Outline", "Read only", "Version history", "Copy as Markdown", "Download (.md)"].every((x) => mi.includes(x)) && !mi.includes("Markdown source"), "⋯ offers find, outline, read only, history and export (no source view)", mi);
  check(!mi.includes("Add a picture"), "…and not a second “Add a picture” (the header has it)", mi);
  await K("Escape", "Escape", 27);
  await menu(c, "Read only");
  await clickText(c, "cheese");
  await p.type("zzz");
  await sleep(300);
  check(!(await doc(c)).includes("zzz") && (await p.eval(`!!document.querySelector('${c.sel} .note-ro')`)), "read only: typing changes nothing, the header says so");
  await p.click(`${c.sel} .cm-task[aria-checked="false"]`);
  await sleep(500);
  check((await server(c.id)).content.includes("- [ ] milk"), "…and task boxes don't change");
  await p.click(`${c.sel} .note-ro`);
  await sleep(300);
  await clickText(c, "cheese", 0);
  await p.type("Swiss ");
  await saved(c.id, (n) => n.content.includes("Swiss cheese"));
  check((await server(c.id)).content.includes("Swiss cheese"), "the header's “Read only” turns it off again");
  await menu(c, "Find in note");
  await sleep(200);
  check(await p.eval(`!!document.querySelector('${c.sel} .cm-search input[name=search]')`), "Find in note opens a search over the note");
  await p.type("later");
  await K("End", "End", 35);
  await sleep(200);
  const found = await p.eval(`document.querySelectorAll('${c.sel} .cm-searchMatch').length`);
  check(found === 2, "…marking each place it is found", found);
  await K("Escape", "Escape", 27);
  await menu(c, "Outline");
  const ol = await p.eval(`[...document.querySelectorAll('${c.sel} .note-outline .nh-row')].map(e => e.textContent)`);
  check(JSON.stringify(ol) === JSON.stringify(["Shopping list", "Later", "Much later"]), "Outline lists the headings", ol);
  await p.clickText(`${c.sel} .note-outline .nh-row`, "Much later");
  await sleep(300);
  check((await head(c)) === (await doc(c)).indexOf("## Much later") + 3, "…and a heading there jumps to it (the caret at its text)", await head(c));
  await p.eval(`(() => { window.__copied = null; navigator.clipboard.writeText = async (t) => { window.__copied = t; }; window.__dl = null; })()`);
  await menu(c, "Copy as Markdown");
  check((await p.eval(`window.__copied`))?.startsWith("# Shopping list\n\nBuy"), "Copy as Markdown copies it (its heading already there)", (await p.eval(`window.__copied`))?.slice(0, 30));
  await menu(c, "Download (.md)");
  const dl = await p.eval(`window.__dl && window.__blob.text().then(t => ({ name: window.__dl.name, t }))`);
  check(dl?.name === "Shopping list.md" && dl.t.includes("- [x] eggs"), "Download saves Shopping list.md", dl?.name);
  await ws(p, `m.closePane('${c.pane}')`);

  // ── a long note stays quick ───────────────────────────────────────────
  console.log("— a long note");
  const para = (i) => `Paragraph ${i}: ${"some words that make a line of a decent length, ".repeat(2)}and **bold** too.`;
  const long = Array.from({ length: 1200 }, (_, i) => (i % 40 === 0 ? `## Part ${i / 40}\n\n- [ ] item ${i}\n- [x] done ${i}\n\n| a | b |\n| --- | --- |\n| ${i} | x |` : para(i))).join("\n\n");
  const t0 = Date.now();
  const L = await show("Long", long);
  const openMs = Date.now() - t0;
  await caret(L, long.indexOf("Paragraph 1100:"));
  // Per keystroke: the change, the page drawn again and the layout the browser then does (forced here).
  const perKey = await p.eval(`(() => { const v = ${V(L)}; let best = 1e9; for (let k = 0; k < 3; k++) { const t = performance.now(); for (let i = 0; i < 20; i++) { const at = v.state.selection.main.head; v.dispatch({ changes: { from: at, insert: 'x' }, selection: { anchor: at + 1 }, userEvent: 'input.type' }); void v.contentDOM.getBoundingClientRect().height; } best = Math.min(best, (performance.now() - t) / 20); } return best; })()`);
  console.log(`  ${Buffer.byteLength(long) >> 10} KB, 30 tables: opened in ${openMs} ms, ${perKey.toFixed(2)} ms per keystroke`);
  check(perKey < 10, "typing in a ~170 KB note costs little per keystroke", perKey);
  await saved(L.id, (n) => n.content.includes("x".repeat(60) + "Paragraph 1100:"), 10000);
  check((await server(L.id)).content.includes("x".repeat(60) + "Paragraph 1100:"), "…and it is all saved");
  await ws(p, `m.closePane('${L.pane}')`);

  // ── HTML preview ──────────────────────────────────────────────────────
  console.log("— previews of HTML");
  const h = await show("Widget", "A counter:\n\n```html\n<button id=b>0</button>\n<p id=out></p>\n<script>\nlet n = 0; b.onclick = () => b.textContent = ++n;\nlet seen = 'blocked'; try { seen = parent.document.title; } catch (e) {}\nlet net = 'blocked'; fetch('/api/health').then(() => { net = 'reached'; }).catch(() => {}).finally(() => { out.textContent = 'parent:' + seen + ' net:' + net; });\n</script>\n```\n\nAfter.");
  await p.click(`${h.sel} .cm-code-run`);
  await until(() => p.eval(`!!document.querySelector('${h.sel} .nb-frame.ready iframe')`));
  const frame = await p.eval(`(() => { const f = document.querySelector('${h.sel} .nb-frame iframe'); return f ? { sandbox: f.getAttribute('sandbox'), src: f.src } : null; })()`);
  check(frame?.sandbox === "allow-scripts" && /\/preview\/[0-9a-f]+\//.test(frame.src), "Preview runs the HTML in a sandboxed frame on the preview page", frame);
  const { targetInfos } = await p.conn.send("Target.getTargets");
  const ft = targetInfos.find((t) => t.type === "iframe" && t.url === frame?.src);
  let inside = null;
  if (ft) {
    const { sessionId } = await p.conn.send("Target.attachToTarget", { targetId: ft.targetId, flatten: true });
    for (let i = 0; i < 20 && !inside?.includes("net:"); i++) {
      inside = (await p.conn.send("Runtime.evaluate", { expression: "document.getElementById('out')?.textContent ?? null", returnByValue: true }, sessionId)).result.value;
      await sleep(150);
    }
  }
  check(inside === "parent:blocked net:blocked", "…it runs, but can't reach the app or the network", inside);
  await tileShot(h, "notes-preview-desktop", p, 560);
  console.log("desktop errors:", p.errors);
  check(!p.errors.length, "no page errors (desktop)");
  await p.close();

  // ── phone: swiping over a note, the format bar, the camera ────────────
  console.log("— phone");
  await mock("/__mock/mosaics/reset");
  const phone = await device();
  await phone.viewport(375, 812, true, 2);
  await signIn(phone, "Phone");
  const pn = (await probe.rpc("save_note", { title: "Packing", content: "# Packing\n\nPassport, charger and the blue jacket for the evenings.\n\n- [ ] tickets\n- [ ] adapter\n\nMore words here so the note has a little length to it." })).result;
  const pw = (js) => phone.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
  await pw(`m.openSession('ses_e0b6d4a29c1f', { pane: m.workspace.focused() })`);
  await sleep(300);
  await phone.eval(`__mod('lib/notes.ts').then(m => m.openNote('${pn.id}', { newPane: true }))`);
  await sleep(500);
  await pw(`m.openSession('ses_5a3e8f10c2d9', { newPane: true })`);
  await sleep(500);
  const order = await pw(`return m.workspace.order()`);
  const NP = await pw(`return m.workspace.order().find(id => m.workspace.panes[id].content.type === 'note')`);
  const T = { sel: `[data-pane-id="${NP}"]` };
  const idx = () => pw(`return m.workspace.order().indexOf(m.workspace.focused())`);
  await pw(`m.focusPane('${NP}')`);
  await sleep(900);
  const at = order.indexOf(NP);
  const step = at < order.length - 1 ? 1 : -1;
  const away = (x0, y, o) => (step > 0 ? swipe(phone, x0, y, x0 - 260, o) : swipe(phone, x0 - 260, y, x0, o));
  const back2 = () => (step > 0 ? swipe(phone, 60, 400, 320) : swipe(phone, 320, 400, 60));
  const focusedOn = () => phone.eval(`document.activeElement?.matches('${T.sel} .cm-content') ?? false`);
  await phone.shot(`${SHOTS}/notes-phone.png`);
  let s = await spotOf(T, "charger", 0.5, phone);
  await away(330, s.y);
  check((await idx()) === at + step, "a swipe over a note's text turns the page", [await idx(), at]);
  await back2();
  check((await idx()) === at && (await phone.eval(`location.pathname`)) === "/", "…and back (the browser never takes the stroke)");
  s = await spotOf(T, "charger", 0.5, phone);
  await phone.tap(s.x, s.y);
  await sleep(300);
  check(await focusedOn(), "a tap on its text starts writing there (no mode to switch)");
  await phone.shot(`${SHOTS}/notes-phone-writing.png`);
  const tool = (name) => phone.eval(`(() => { const r = document.querySelector('${T.sel} .nb-tool[data-tool="${name}"]')?.getBoundingClientRect(); return r ? { x: r.x + r.width / 2, y: r.y + r.height / 2 } : null; })()`);
  const tt = await tool("Task");
  check(!!tt && !!(await tool("Heading")) && !!(await tool("Picture")) && !!(await tool("Bold")), "while writing on a phone, a format bar shows (heading, bold, list, task, picture…)");
  if (tt) {
    await phone.tap(tt.x, tt.y);
    await sleep(200);
    check((await doc(T, phone)).includes("- [ ] Passport") && (await focusedOn()), "its Task button makes the line a task, and the text keeps the focus (and the keyboard)", (await doc(T, phone)).slice(0, 40));
    await phone.tap(tt.x, tt.y);
    await sleep(200);
    check((await doc(T, phone)).includes("\nPassport"), "…and a second tap takes it back");
    const hb = await tool("Heading");
    await phone.tap(hb.x, hb.y);
    await sleep(200);
    check((await doc(T, phone)).includes("# Passport") && (await phone.eval(`document.querySelector('${T.sel} .nb-tool[data-tool="Heading"]').textContent`)) === "H1", "Heading makes it H1 (and says so); again: H2, H3, text", await phone.eval(`document.querySelector('${T.sel} .nb-tool[data-tool="Heading"]').textContent`));
    await phone.tap(hb.x, hb.y);
    await phone.tap(hb.x, hb.y);
    await phone.tap(hb.x, hb.y);
    await sleep(200);
    check((await doc(T, phone)).includes("\nPassport,"), "…back to text after H3");
    // On a new, empty line: the caret goes after the new box, ready to type.
    await phone.eval(`(() => { const v = ${V(T)}; v.dispatch({ selection: { anchor: v.state.doc.length } }); })()`);
    await K("Enter", "Enter", 13, 0, phone);
    await K("Enter", "Enter", 13, 0, phone);
    await phone.tap(tt.x, tt.y);
    await sleep(150);
    await phone.type("call the hotel");
    check((await doc(T, phone)).endsWith("\n- [ ] call the hotel"), "Task on an empty line: the box, then what's typed", (await doc(T, phone)).slice(-30));
  }
  // Android's WebView puts zero-width images around drawn markers: they must stay inline (else every bullet breaks its line).
  const buf = await phone.eval(`(() => { const i = document.createElement('img'); i.className = 'cm-widgetBuffer'; const l = document.querySelector('${T.sel} .cm-line.cm-li'); l.prepend(i); const d = getComputedStyle(i).display; i.remove(); return d; })()`);
  check(buf === "inline", "the editor's helper images stay inline whatever the app's rule for images", buf);
  const sameLine = await phone.eval(`(() => { const l = document.querySelector('${T.sel} .cm-line.cm-li'); const box = l.querySelector('.cm-mark').getBoundingClientRect(); const r = document.createRange(); r.selectNodeContents(l.lastChild); const t = r.getBoundingClientRect(); return Math.abs(box.top - t.top) < t.height; })()`);
  check(sameLine, "a task's box and its text are on one line");
  // A photo from the camera: the Picture button asks, Take photo uses the camera input.
  const pb = await tool("Picture");
  await phone.eval(`(() => { const i = document.querySelector('${T.sel} input[capture]'); window.__cam = i ? { accept: i.accept, capture: i.getAttribute('capture') } : null; i.click = () => { window.__camOpened = true; }; })()`);
  await phone.tap(pb.x, pb.y);
  await sleep(300);
  const pm2 = await phone.eval(`[...document.querySelectorAll('.menu-pop .menu-label')].map(e => e.textContent)`);
  check(pm2.includes("Take photo") && pm2.includes("Photos"), "the Picture button offers the camera and the photos", pm2);
  await phone.clickText(".menu-item", "Take photo");
  await sleep(200);
  check((await phone.eval(`window.__camOpened === true`)) && (await phone.eval(`window.__cam?.capture`)) === "environment", "Take photo opens the camera (capture=environment)", await phone.eval(`window.__cam`));
  await phone.eval(`(async () => { const f = ${makePng(1200, 1600, "#c84")}; const i = document.querySelector('${T.sel} input[capture]'); const dt = new DataTransfer(); dt.items.add(new File([f], 'IMG_20261002_101500.png', { type: 'image/png' })); i.files = dt.files; i.dispatchEvent(new Event('change', { bubbles: true })); })()`);
  await saved(pn.id, (n) => /!\[Picture\]\(assets\/[0-9a-f]{16}\.png\)/.test(n.content), 10000);
  check(/!\[Picture\]\(assets\/[0-9a-f]{16}\.png\)/.test((await server(pn.id)).content), "the photo goes into the note where the caret was (a camera's file name isn't used as its description)", (await server(pn.id)).content.match(/!\[[^\]]*\]\([^)]*\)/)?.[0]);
  await sleep(600);
  await phone.shot(`${SHOTS}/notes-phone-picture.png`);
  s = await spotOf(T, "charger", 0.5, phone);
  // A finger that rests first is selecting text: no page turn.
  await away(330, s.y, { hold: 520 });
  check((await idx()) === at, "a press-and-drag on the text doesn't turn the page", await idx());
  // A quick stroke turns the page even while writing, and writing stops.
  s = await spotOf(T, "charger", 0.5, phone);
  await away(330, s.y);
  check((await idx()) === at + step, "a quick swipe over the text being written turns the page", await idx());
  check(!(await focusedOn()), "…and writing stops (the keyboard goes)");
  await back2();
  // Ticking a task by tap.
  await phone.eval(`document.querySelector('${T.sel} .cm-task').scrollIntoView({ block: 'center' })`);
  s = await phone.eval(`(() => { const r = document.querySelector('${T.sel} .cm-task').getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; })()`);
  await phone.tap(s.x, s.y);
  await saved(pn.id, (n) => n.content.includes("- [x] tickets"));
  check((await server(pn.id)).content.includes("- [x] tickets"), "a tap ticks a task on the phone");
  await phone.theme("light");
  await sleep(300);
  await phone.shot(`${SHOTS}/notes-phone-light.png`);
  console.log("phone errors:", phone.errors);
  check(!phone.errors.length, "no page errors (phone)");
  await phone.close();

  // ── the Fold: two tiles, a note in one ───────────────────────────────
  console.log("— fold");
  await mock("/__mock/mosaics/reset");
  const fold = await device();
  await fold.viewport(673, 841, true, 2.625);
  await signIn(fold, "Fold");
  await sleep(800);
  const fw = (js) => fold.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
  await fold.eval(`__mod('lib/notes.ts').then(m => m.openNote('${pn.id}', { newPane: true }))`);
  await sleep(1200);
  const FP = await fw(`return m.workspace.order().find(id => m.workspace.panes[id].content.type === 'note')`);
  const F = { sel: `[data-pane-id="${FP}"]` };
  check((await fw(`return m.workspace.mode()`)) === "dual" && (await fold.eval(`!!document.querySelector('${F.sel} .cm-content')`)), "on an unfolded Fold the note opens beside the chat, ready to write in");
  await fold.theme("dark");
  await sleep(300);
  await fold.shot(`${SHOTS}/notes-fold.png`);
  await fold.theme("light");
  await sleep(300);
  await fold.shot(`${SHOTS}/notes-fold-light.png`);
  console.log("fold errors:", fold.errors);
  check(!fold.errors.length, "no page errors (fold)");
  await fold.close();
  probe.close();
  finish();
});
