// Rewind, end to end against the mock: the action on your messages, the dry-run
// preview, confirming (rows fade, transcript trimmed without re-mounting what
// stays, composer refilled, toast with Open, "(rewound)" branch in the sidebar),
// the event on a second client, busy/private/subagent rules, files-only rewinds,
// images coming back, touch long-press, and the sheet at phone/foldable/desktop
// sizes in both themes.
import zlib from "node:zlib";
import { SHOTS, check, device, finish, mock, readEarlier, run, signIn, sleep, wsClient } from "./cdp.mjs";
import { offlineRewindCases } from "./rewind-offline.mjs";

const SID = "ses_7d3a91e5c4b2"; // "Add a /healthz endpoint"
const MSG1 = "Add a `/healthz` endpoint";
const MSG2 = "Also add a readiness probe";
/** A small mosaic of gold and stone tiles, as a real PNG (base64). */
function mosaicPng(n = 48, cell = 8) {
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(zlib.crc32(td) >>> 0);
    return Buffer.concat([len, td, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(n, 0);
  ihdr.writeUInt32BE(n, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 2; // RGB
  const rows = [];
  for (let y = 0; y < n; y++) {
    const row = [0];
    for (let x = 0; x < n; x++) {
      const gold = (Math.floor(x / cell) * 7 + Math.floor(y / cell) * 3) % 5 === 0;
      row.push(...(gold ? [212, 175, 95] : [60 + ((x / cell) | 0) * 6, 55 + ((y / cell) | 0) * 5, 49]));
    }
    rows.push(Buffer.from(row));
  }
  const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  return Buffer.concat([sig, chunk("IHDR", ihdr), chunk("IDAT", zlib.deflateSync(Buffer.concat(rows))), chunk("IEND", Buffer.alloc(0))]).toString("base64");
}
const PNG = mosaicPng();

/** The tile under test (by id: the shared mosaic can move focus to another device's tile). */
const R = `(document.querySelector('[data-pane-id="' + window.__paneFor + '"]') || document.querySelector('.pane.focused'))`;
async function openChat(p, sid) {
  await p.eval(`__mod('lib/workspace.ts').then(m => { window.__paneFor = m.openSession('${sid}', { pane: window.__paneFor || m.workspace.focused() }); })`);
  // A subagent's task is written by the chat that started it (an authored message), not the user.
  await p.waitFor(`${R}.querySelector('.transcript .msg-user, .transcript .msg-authored')`, 8000);
  await sleep(600);
  const pane = await p.eval(`${R}.dataset.paneId`);
  await readEarlier(p, `[data-pane-id="${pane}"] .transcript`);
}
const msgEl = (text) => `[...${R}.querySelectorAll('.msg-user')].find(e => e.textContent.includes(${JSON.stringify(text)}))`;
const bubble = async (p, text) => {
  const pane = await p.eval(`${R}.dataset.paneId`);
  await readEarlier(p, `[data-pane-id="${pane}"] .transcript`);
  return p.eval(`(() => { const el = ${msgEl(text)}; if (!el) return null; el.scrollIntoView({ block: 'center' }); const r = el.querySelector('.bubble').getBoundingClientRect(); return { x: r.x + Math.min(60, r.width / 2), y: r.y + r.height / 2 }; })()`);
};
const rewindBtn = (p, text) =>
  p.eval(`(() => { const el = ${msgEl(text)}; const b = el?.querySelector('.msg-rewind'); if (!b) return null; const r = b.getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2, disabled: b.getAttribute('aria-disabled'), tip: b.dataset.tip, shown: getComputedStyle(el.querySelector('.msg-actions')).opacity }; })()`);
const hover = async (p, x, y) => {
  await p.send("Input.dispatchMouseEvent", { type: "mouseMoved", x, y });
  await sleep(120);
};
const tooltip = (p) => p.eval(`document.querySelector('.tooltip')?.textContent ?? null`);
const dialogState = (p) =>
  p.eval(`(() => { const d = document.querySelector('.rewind-dialog'); if (!d) return null; const r = d.getBoundingClientRect(); return {
    quote: d.querySelector('.rewind-quote-text')?.textContent ?? '',
    toggles: [...d.querySelectorAll('.rewind-opt .toggle')].map(t => t.getAttribute('aria-checked') + (t.disabled ? '/disabled' : '')),
    desc: [...d.querySelectorAll('.rewind-opt-desc')].map(x => x.textContent),
    groups: Object.fromEntries([...d.querySelectorAll('.rewind-group')].map(g => [g.querySelector('.rewind-group-label').textContent, [...g.querySelectorAll('.rewind-file')].map(f => f.title)])),
    reasons: [...d.querySelectorAll('.rewind-why')].map(x => x.textContent),
    notes: [...d.querySelectorAll('.rewind-notes li')].map(x => x.textContent),
    commands: [...d.querySelectorAll('.rewind-notice li')].map(l => l.textContent),
    kept: d.querySelector('.rewind-kept')?.textContent ?? null,
    moved: d.querySelector('.rewind-moved')?.textContent ?? null,
    more: [...d.querySelectorAll('.rewind-more')].map(b => b.textContent),
    blocked: d.querySelector('.rewind-blocked')?.textContent ?? null,
    go: { disabled: d.querySelector('.rewind-go').disabled, focused: document.activeElement === d.querySelector('.rewind-go') },
    rect: { top: r.top, bottom: r.bottom, left: r.left, right: r.right, vw: innerWidth, vh: innerHeight },
  }; })()`);
const openDialogFor = async (p, sid, text) => {
  const seq = await p.eval(`Number(${msgEl(text)}?.dataset.seq)`);
  await p.eval(`(async () => { const w = await __mod('lib/workspace.ts'); (await __mod('lib/rewind.ts')).openRewind('${sid}', ${seq}, window.__paneFor || w.workspace.focused()); })()`);
  await p.waitFor(`document.querySelector('.rewind-dialog .rewind-preview')`, 6000);
  await sleep(450);
  return seq;
};
/** Watch a tile's transcript: rows marked leaving, skeletons, and a kept row's identity. */
const watch = (p, keepSeq) =>
  p.eval(`(() => {
    const inner = ${R}.querySelector('.transcript-inner');
    inner.querySelector('[data-seq="${keepSeq}"]').closest('.item').dataset.keep = '1';
    window.__rw = { leaving: 0, skeleton: false };
    new MutationObserver(ms => { for (const m of ms) if (m.target.classList?.contains('leaving')) window.__rw.leaving++; })
      .observe(inner, { attributes: true, subtree: true, attributeFilter: ['class'] });
    new MutationObserver(() => { if (${R}.querySelector('.skeleton')) window.__rw.skeleton = true; })
      .observe(document.body, { childList: true, subtree: true });
  })()`);
const tileState = (p, sid) =>
  p.eval(`(async () => {
    const w = await __mod('lib/workspace.ts'); const st = await __mod('lib/store.ts');
    const s = st.state.sessions['${sid}'];
    const f = window.__paneFor || w.workspace.focused();
    return {
      last: s?.entries.at(-1)?.seq ?? 0, count: s?.entries.length ?? 0, rewind: s?.rewind ?? null,
      msgs: [...${R}.querySelectorAll('.msg-user')].map(e => e.textContent.slice(0, 48)),
      harness: [...${R}.querySelectorAll('.harness')].map(e => e.textContent.slice(0, 80)),
      draft: w.getDraft(f),
      ta: ${R}.querySelector('.composer textarea')?.value ?? null,
      focusedTa: document.activeElement === ${R}.querySelector('.composer textarea'),
      atts: [...${R}.querySelectorAll('.composer .attachment img, .composer .file-chip')].length,
      hero: !!${R}.querySelector('.hero'),
      toasts: [...document.querySelectorAll('.toast')].map(t => t.querySelector('.toast-msg').textContent),
      kept: !!${R}.querySelector('[data-keep="1"]'),
      rw: window.__rw ?? null,
      branch: [...document.querySelectorAll('.sidebar .srow')].filter(r => r.textContent.includes('(rewound)')).map(r => ({ text: r.querySelector('.srow-title').textContent, fork: r.querySelector('.srow-lead svg')?.innerHTML.includes('circle') ?? false, title: r.querySelector('.srow-main').title })),
    };
  })()`);

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/mosaics/reset");

  // ── desktop: preview, confirm, trim, composer, toast, branch; a second client gets the event ──
  const a = await device();
  await a.viewport(1440, 900);
  await signIn(a, "Desk A");
  await openChat(a, SID);
  await sleep(1500); // the mosaic is shared: B gets the same tile
  const b = await device();
  await b.viewport(1440, 900);
  await signIn(b, "Desk B");
  await sleep(1500);
  // B's sign-in adds its own empty tile to the shared mosaic: close it, so both show just this chat.
  await b.eval(`__mod('lib/workspace.ts').then(m => { for (const id of m.workspace.order()) { const c = m.workspace.panes[id].content; if (c.type === 'chat' && !c.sessionId) m.closePane(id); } })`);
  await sleep(1500);
  for (const p of [a, b]) await p.eval(`__mod('lib/workspace.ts').then(m => { window.__paneFor = m.paneShowing('${SID}'); m.focusPane(window.__paneFor); })`);
  await b.waitFor(`${msgEl(MSG2)}`, 8000);
  await sleep(600);

  const bx = await bubble(a, MSG2);
  await hover(a, bx.x, bx.y);
  await sleep(400);
  const hidden = await a.eval(`getComputedStyle(${msgEl(MSG1)}.querySelector('.msg-actions')).opacity`);
  const btn = await rewindBtn(a, MSG2);
  await hover(a, btn.x, btn.y);
  await sleep(700);
  const shown = await rewindBtn(a, MSG2);
  check(hidden === "0" && shown.shown === "1" && shown.disabled === "false", "hovering your message shows “Rewind to here” next to copy and fork (others stay quiet)", { hidden, shown });
  check((await tooltip(a)) === "Rewind to here", "its tooltip says “Rewind to here”", await tooltip(a));
  await a.shot(`${SHOTS}/desktop-message-action-dark.png`, { x: 280, y: Math.max(0, btn.y - 120), width: 900, height: 220 });
  await a.mouse(btn.x, btn.y);
  await a.waitFor(`document.querySelector('.rewind-dialog')`, 3000);
  const loading = await a.eval(`!!document.querySelector('.rewind-chunk [aria-busy="true"]') || !!document.querySelector('.rewind-dialog .rewind-loading') || !!document.querySelector('.rewind-dialog .rewind-preview')`);
  check(loading, "the dialog opens at once (loading code, checking, then the preview)");
  await a.waitFor(`document.querySelector('.rewind-dialog .rewind-preview')`, 5000);
  await sleep(450);
  const d = await dialogState(a);
  check(d.quote.startsWith(MSG2), "it quotes the message it goes back before", d.quote);
  check(d.toggles.join() === "true,true", "both toggles start on: this preview has file changes", d.toggles);
  check(/everything after it: 11 messages/.test(d.desc[0]), "it says how many messages go", d.desc[0]);
  check(d.groups.Restored?.join() === "crates/server/src/router.rs" && d.groups.Removed?.join() === "crates/server/tests/ready.rs", "files to restore and to delete, grouped", d.groups);
  check(d.groups["Left as they are"]?.join() === "crates/server/src/ready.rs" && /changed since it wrote it/.test(d.reasons[0] ?? ""), "skipped files with their reason", { g: d.groups, r: d.reasons });
  check(d.notes.length === 1 && /rewinding would overwrite/.test(d.notes[0]), "the server's note about later changes", d.notes);
  check(d.commands.length === 2 && d.commands[0].includes("rustfmt") && d.commands[1].includes("cargo test"), "shell commands that won't be undone", d.commands);
  check(/“Add a \/healthz endpoint \(rewound\)”/.test(d.kept ?? ""), "it explains the full chat is kept as “… (rewound)”", d.kept);
  check(!d.go.disabled && d.go.focused, "Rewind is ready and focused (Enter confirms)", d.go);
  await a.shot(`${SHOTS}/desktop-dialog-dark.png`);
  await a.theme("light");
  await sleep(250);
  await a.shot(`${SHOTS}/desktop-dialog-light.png`);
  await a.theme("dark");

  // Turning "Rewind conversation" off changes the words; the button still works for files.
  await a.clickText(".rewind-dialog .rewind-opt-title", "Rewind conversation");
  await sleep(150);
  const off = await dialogState(a);
  check(off.toggles[0] === "false" && /Keeps the conversation/.test(off.desc[0]) && off.kept === null && !off.go.disabled, "conversation off: it keeps the chat and still offers the files", off);
  await a.clickText(".rewind-dialog .rewind-opt-title", "Restore files");
  await sleep(150);
  check((await dialogState(a)).go.disabled, "both off: nothing to do, Rewind is disabled");
  await a.clickText(".rewind-dialog .rewind-opt-title", "Rewind conversation");
  await a.clickText(".rewind-dialog .rewind-opt-title", "Restore files");
  await sleep(150);

  await watch(a, 1);
  await watch(b, 1);
  await a.click(".rewind-dialog .rewind-go");
  // Looked at right away: the fade lasts REWIND_FADE_MS (200 ms), and on a slow
  // test machine a fixed pause plus the look itself can outlast it.
  const mid = await a.eval(`({ label: document.querySelector('.rewind-go')?.textContent, leaving: ${R}.querySelectorAll('.item.leaving').length })`);
  check(mid.leaving > 0 && mid.label === "Rewinding…", "while it works, the rows being rewound fade out", mid);
  await a.waitFor(`!document.querySelector('.rewind-dialog')`, 6000);
  await sleep(900);
  const A1 = await tileState(a, SID);
  check(A1.last === 10 && !A1.msgs.some((m) => m.includes(MSG2)) && A1.msgs.length === 1, "the transcript is trimmed to just before the message", A1);
  check(A1.kept && !A1.rw.skeleton && A1.rewind === null, "what stays is never re-rendered (no flicker, no skeleton)", A1.rw);
  check(A1.draft.startsWith(MSG2) && A1.ta?.startsWith(MSG2) && A1.focusedTa, "your message is back in the composer, focused, to edit and send", { draft: A1.draft, focused: A1.focusedTa });
  const toastMsg = A1.toasts.find((t) => t.startsWith("Rewound")) ?? "";
  check(toastMsg === "Rewound · 1 file restored · 1 new file removed · kept the full chat as “Add a /healthz endpoint (rewound)”", "the toast says what happened", toastMsg);
  check(A1.branch.length === 1 && A1.branch[0].fork && /Branched from “Add a \/healthz endpoint”/.test(A1.branch[0].title), "the “(rewound)” chat is in the sidebar, marked as branched from the original", A1.branch);
  await a.shot(`${SHOTS}/desktop-after-dark.png`);

  await b.waitFor(`!${msgEl(MSG2)}`, 5000);
  await sleep(700);
  const B1 = await tileState(b, SID);
  check(B1.last === 10 && B1.rw.leaving > 0 && B1.kept && !B1.rw.skeleton, "the other device fades the rows out, trims and keeps the rest mounted", B1);
  check(B1.draft === "" && B1.branch.length === 1, "the other device gets the branch but not the draft", { draft: B1.draft, branch: B1.branch });
  await b.shot(`${SHOTS}/desktop-second-client-dark.png`);
  console.log("errors B", b.errors);
  await b.close();

  // "Open" in the toast opens the full history in a new tile.
  await a.clickText(".toast .btn", "Open");
  await sleep(1200);
  const opened = await a.eval(`(async () => { const w = await __mod('lib/workspace.ts'); const pane = w.paneShowing(w.paneSession(w.workspace.focused())?.info.id ?? ''); const s = w.paneSession(w.workspace.focused()); return { title: s?.info.title, parent: s?.info.parent_id, n: s?.entries.length, other: pane !== window.__paneFor, has: [...document.querySelectorAll('.pane.focused .msg-user')].some(e => e.textContent.includes(${JSON.stringify(MSG2)})) }; })()`);
  check(opened.title === "Add a /healthz endpoint (rewound)" && opened.parent === SID && opened.n === 21 && opened.other && opened.has, "Open shows the kept full chat in a new tile beside it", opened);
  await a.shot(`${SHOTS}/desktop-branch-open-dark.png`);
  await a.eval(`__mod('lib/workspace.ts').then(m => { m.closePane(m.workspace.focused()); m.focusPane(window.__paneFor); })`);
  await sleep(400);

  // ── busy: disabled with "Stop the agent first"; the server refuses too ──
  await a.eval(`(async () => { const w = await __mod('lib/workspace.ts'); w.setDraft(w.workspace.focused(), ''); })()`);
  await a.eval(`__mod('lib/store.ts').then(m => m.sendMessage('${SID}', [{ type: 'text', text: 'stream a long answer please' }], 'auto'))`);
  await a.waitFor(`${R}.querySelector('.composer.busy')`, 5000);
  await sleep(500);
  // The view follows the streaming answer; a notch up stops that, as a reader reaching back would.
  const bx1 = await bubble(a, MSG1);
  await a.send("Input.dispatchMouseEvent", { type: "mouseWheel", x: bx1.x, y: bx1.y, deltaX: 0, deltaY: -40 });
  await sleep(400);
  const bx2 = await bubble(a, MSG1);
  await hover(a, bx2.x, bx2.y);
  const busyBtn = await rewindBtn(a, MSG1);
  await hover(a, busyBtn.x, busyBtn.y);
  await sleep(700);
  check(busyBtn.disabled === "true" && (await tooltip(a)) === "Stop the agent first", "while the agent works: disabled, “Stop the agent first”", { busyBtn, tip: await tooltip(a) });
  await a.shot(`${SHOTS}/desktop-busy-tooltip-dark.png`, { x: 280, y: Math.max(0, busyBtn.y - 120), width: 900, height: 220 });
  await a.mouse(busyBtn.x, busyBtn.y);
  await sleep(300);
  check(!(await a.eval(`!!document.querySelector('.rewind-dialog')`)), "clicking it then opens nothing");
  const ws = await wsClient("probe");
  const refused = await ws.rpc("rewind", { session_id: SID, to_seq: 0, files: true, conversation: true, dry_run: true });
  check(refused.error?.code === "busy", "the server refuses with `busy`", refused.error);
  await a.eval(`__mod('lib/actions.ts').then(m => m.abortSession('${SID}'))`);
  await a.waitFor(`!${R}.querySelector('.composer.busy')`, 6000);
  await sleep(600);

  // A dialog left open while another device starts a turn: it says so and offers Stop; the preview returns after.
  await openDialogFor(a, SID, MSG1);
  await ws.rpc("send", { session_id: SID, content: [{ type: "text", text: "stream something" }], delivery: "auto" });
  await a.waitFor(`document.querySelector('.rewind-dialog .rewind-blocked')`, 5000);
  await sleep(300);
  const blocked = await dialogState(a);
  const dupError = await a.eval(`!!document.querySelector('.rewind-dialog .form-error')`);
  check(/stop it first/.test(blocked.blocked ?? "") && blocked.go.disabled && !dupError, "a turn starting elsewhere blocks the open dialog calmly, with a Stop button", { blocked: blocked.blocked, dupError });
  await a.shot(`${SHOTS}/desktop-dialog-blocked-dark.png`);
  await a.clickText(".rewind-blocked .btn", "Stop");
  await a.waitFor(`!document.querySelector('.rewind-dialog .rewind-blocked')`, 6000);
  await sleep(900);
  const unblocked = await dialogState(a);
  check(!unblocked.go.disabled && unblocked.desc.length === 2, "stopped: the preview is fresh and Rewind works again", unblocked);
  await a.key("Escape", "Escape", 0, 27);
  await sleep(300);
  check(!(await a.eval(`!!document.querySelector('.rewind-dialog')`)), "Escape closes the dialog");

  // ── files only: the conversation stays, the agent gets a note, the composer is untouched ──
  await openChat(a, "ses_8c1f2a9d4e70");
  await openDialogFor(a, "ses_8c1f2a9d4e70", "The session store is still a pile");
  await a.clickText(".rewind-dialog .rewind-opt-title", "Rewind conversation");
  await sleep(150);
  const fo = await dialogState(a);
  check(fo.groups.Restored?.join() === "crates/core/src/store.rs" && fo.commands.length === 1, "an older chat: one file to restore, one command not undone", fo);
  await a.click(".rewind-dialog .rewind-go");
  await a.waitFor(`!document.querySelector('.rewind-dialog')`, 6000);
  await sleep(900);
  const F1 = await tileState(a, "ses_8c1f2a9d4e70");
  check(F1.msgs.length === 2 && F1.harness.some((h) => /Rewind/.test(h) && /put files back/.test(h)), "files only: the conversation stays and a “Rewind” note tells the agent", F1.harness);
  check(F1.draft === "" && F1.toasts.some((t) => t === "Files rewound · 1 file restored"), "files only: the composer is left alone; the toast says “Files rewound”", { draft: F1.draft, toasts: F1.toasts });
  await a.shot(`${SHOTS}/desktop-files-only-dark.png`);

  // ── /rewind and the command palette: back to before your last message ──
  await a.eval(`${R}.querySelector('.composer textarea').focus()`);
  await a.type("/rew");
  await sleep(250);
  const slash = await a.eval(`[...${R}.querySelectorAll('.slash-item')].map(e => e.textContent)`);
  check(slash.length === 1 && slash[0].startsWith("/rewind"), "/rewind is in the command list", slash);
  await a.type("ind");
  await a.key("Enter", "Enter", 0, 13);
  await a.waitFor(`document.querySelector('.rewind-dialog .rewind-preview')`, 5000);
  await sleep(300);
  const sd = await dialogState(a);
  check(sd.quote.startsWith("Nice. Also migrate"), "/rewind opens the dialog for your last message", sd.quote);
  await a.key("Escape", "Escape", 0, 27);
  await sleep(300);
  await a.key("k", "KeyK", 2, 75);
  await a.waitFor(`document.activeElement && document.activeElement === document.querySelector('.palette input')`, 3000);
  await a.type("rewind");
  await sleep(300);
  const pal = await a.eval(`[...document.querySelectorAll('.palette .palette-item .palette-label')].map(e => e.textContent)`);
  check(pal.includes("Rewind to before your last message"), "the command palette offers it too", pal);
  await a.key("Escape", "Escape", 0, 27);
  await sleep(300);

  // ── private chats and subagent sessions have no Rewind ──
  const priv = await a.eval(`(async () => { const st = await __mod('lib/store.ts'); const i = await st.api.createSession({ private: true }); st.upsertSession(i); st.setState('sessions', i.id, 'loaded', true); await st.sendMessage(i.id, [{ type: 'text', text: 'hello private' }], 'auto'); return i.id; })()`);
  await openChat(a, priv);
  await a.waitFor(`!${R}.querySelector('.composer.busy')`, 15000);
  await sleep(300);
  check(!(await a.eval(`!!${R}.querySelector('.msg-rewind')`)), "a private chat shows no Rewind");
  const child2 = "ses_c2e8a4f61b37"; // a seeded subagent of "Audit the reconnect logic"
  {
    await openChat(a, child2);
    check(!(await a.eval(`!!${R}.querySelector('.msg-rewind')`)), "a subagent session shows no Rewind");
  }
  // Asked from the device that made it (another device doesn't even find it: batch 17).
  const refusedPriv = await a.eval(`__mod('lib/store.ts').then((st) => st.call({ method: 'rewind', params: { session_id: '${priv}', to_seq: 0, files: true, conversation: true, dry_run: true } }).then(() => null, (e) => ({ code: e.code, message: e.message })))`);
  check(refusedPriv?.code === "bad_request", "the server refuses private chats", refusedPriv);
  const hiddenPriv = await ws.rpc("rewind", { session_id: priv, to_seq: 0, files: true, conversation: true, dry_run: true });
  check(hiddenPriv.error?.code === "not_found", "…and to another device it doesn't exist", hiddenPriv.error);

  // ── a first message with an image: back to an empty chat, text and image in the composer ──
  const img = await a.eval(`(async () => { const st = await __mod('lib/store.ts'); const w = await __mod('lib/workspace.ts'); const i = await st.api.createSession({}); st.upsertSession(i); st.setState('sessions', i.id, 'loaded', true); window.__paneFor = w.openSession(i.id, { pane: window.__paneFor || w.workspace.focused() }); await st.sendMessage(i.id, [{ type: 'image', mime: 'image/png', data: '${PNG}' }, { type: 'text', text: 'What is in this picture?' }], 'auto'); return i.id; })()`);
  await a.waitFor(`${R}.querySelector('.msg-user .bubble-img')`, 6000);
  await a.waitFor(`!${R}.querySelector('.composer.busy')`, 20000);
  await sleep(500);
  await openDialogFor(a, img, "What is in this picture?");
  const imgD = await a.eval(`document.querySelector('.rewind-quote-att')?.textContent`);
  check(imgD === "1 image", "the quote mentions the image", imgD);
  await a.click(".rewind-dialog .rewind-go");
  await a.waitFor(`!document.querySelector('.rewind-dialog')`, 6000);
  await sleep(900);
  const I1 = await tileState(a, img);
  check(I1.count === 0 && I1.hero && I1.draft === "What is in this picture?" && I1.atts === 1, "the first message: an empty chat again, its text and image back in the composer", I1);
  await a.shot(`${SHOTS}/desktop-image-back-dark.png`);

  // ── long lists fold ("Show all"), and a moved cut point is explained ──
  await openChat(a, SID);
  await a.eval(`(async () => {
    const st = await __mod('lib/store.ts');
    const real = st.api.rewind;
    st.api.rewind = async (id, to, o) => {
      if (!o.dryRun) return real(id, to, o);
      const r = await real(id, to, o);
      const files = Array.from({ length: 9 }, (_, i) => 'apps/tessera/ui/src/components/chat/Part' + (i + 1) + '.tsx');
      return { ...r, to_seq: Math.max(0, to - 1), restored: files, deleted: ['docs/NOTES.md'], not_undone: Array.from({ length: 7 }, (_, i) => 'npm run build --prefix apps/tessera/ui # ' + (i + 1)) };
    };
    window.__restoreRewind = () => (st.api.rewind = real);
  })()`);
  await openDialogFor(a, SID, "stream a long answer please");
  const fold = await dialogState(a);
  check(fold.groups.Restored?.length === 4 && fold.more.some((m) => m === "Show all 9 files") && fold.commands.length === 4 && fold.more.some((m) => m === "Show all 7 commands"), "long lists start folded", fold.more);
  check(/a little further/.test(fold.moved ?? ""), "a cut point moved back is explained", fold.moved);
  await a.shot(`${SHOTS}/desktop-dialog-folded-dark.png`);
  await a.clickText(".rewind-dialog .rewind-more", "Show all 9 files");
  await sleep(200);
  const unfolded = await dialogState(a);
  check(unfolded.groups.Restored?.length === 9 && unfolded.more.includes("Show fewer"), "Show all unfolds them", unfolded.more);
  await a.key("Escape", "Escape", 0, 27);
  await a.eval(`window.__restoreRewind()`);
  console.log("errors A", a.errors);
  await a.close();
  ws.close();

  // ── phone: long press → menu → bottom sheet; confirm refills the composer ──
  for (const theme of ["dark", "light"]) {
    await mock("/__mock/reset");
    await mock("/__mock/mosaics/reset");
    const p = await device();
    await p.viewport(375, 812, true, 2);
    await signIn(p, `Phone ${theme}`);
    await p.theme(theme);
    await openChat(p, SID);
    const pt = await bubble(p, MSG2);
    await p.send("Input.dispatchTouchEvent", { type: "touchStart", touchPoints: [{ x: pt.x, y: pt.y }] });
    await sleep(650);
    await p.send("Input.dispatchTouchEvent", { type: "touchEnd", touchPoints: [] });
    await sleep(500);
    const menu = await p.eval(`[...document.querySelectorAll('.menu-pop .menu-item')].map(m => m.textContent)`);
    check(menu.some((m) => m.startsWith("Copy message")) && menu.some((m) => m.startsWith("Branch from here")) && menu.some((m) => m.startsWith("Rewind to here")), `phone (${theme}): a long press offers Copy, Branch and Rewind`, menu);
    await p.shot(`${SHOTS}/phone-longpress-menu-${theme}.png`);
    const item = await p.eval(`(() => { const m = [...document.querySelectorAll('.menu-pop .menu-item')].find(m => m.textContent.startsWith('Rewind to here')); const r = m.getBoundingClientRect(); return [r.x + r.width / 2, r.y + r.height / 2]; })()`);
    await p.tap(item[0], item[1]);
    await p.waitFor(`document.querySelector('.rewind-dialog .rewind-preview')`, 6000);
    await sleep(600);
    const ps = await dialogState(p);
    check(Math.abs(ps.rect.bottom - ps.rect.vh) < 2 && ps.rect.left === 0 && ps.rect.right === ps.rect.vw, `phone (${theme}): the dialog is a bottom sheet`, ps.rect);
    await p.shot(`${SHOTS}/phone-sheet-${theme}.png`);
    await p.eval(`document.querySelector('.rewind-dialog .modal-body').scrollTop = 1e6`);
    await sleep(200);
    await p.shot(`${SHOTS}/phone-sheet-scrolled-${theme}.png`);
    const go = await p.box(".rewind-dialog .rewind-go");
    await p.tap(go.cx, go.cy);
    await p.waitFor(`!document.querySelector('.rewind-dialog')`, 6000);
    await sleep(1000);
    const P1 = await tileState(p, SID);
    check(P1.last === 10 && P1.ta?.startsWith(MSG2), `phone (${theme}): trimmed, and the message is back in the composer`, { last: P1.last, ta: P1.ta });
    await p.shot(`${SHOTS}/phone-after-${theme}.png`);
    console.log(`errors phone ${theme}`, p.errors);
    await p.close();
  }

  // ── foldable and desktop: a centred sheet ──
  for (const [w, h] of [[673, 841], [1440, 900]]) {
    for (const theme of ["dark", "light"]) {
      await mock("/__mock/reset");
      const p = await device();
      await p.viewport(w, h, w < 700, w < 700 ? 2 : 1);
      await signIn(p, `Sheet ${w}`);
      await p.theme(theme);
      await openChat(p, SID);
      await openDialogFor(p, SID, MSG2);
      const s = await dialogState(p);
      const centred = s.rect.bottom < s.rect.vh - 8 && s.rect.left > 8 && Math.abs(s.rect.left - (s.rect.vw - s.rect.right)) < 2;
      check(centred, `${w}×${h} (${theme}): a centred sheet`, s.rect);
      await p.shot(`${SHOTS}/sheet-${w}x${h}-${theme}.png`);
      console.log(`errors ${w} ${theme}`, p.errors);
      await p.close();
    }
  }
  await offlineRewindCases();
  finish();
});
