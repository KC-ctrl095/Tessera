// Private chats stay on the device that made them (batch 17, the user's "private chats only on the
// device they were made on"). One device starts a private chat; another (a phone, a computer, a
// script standing in for the terminal app) never lists it, never hears of it, can't open it or write
// in it. The device that made it still has it after reconnecting. The private chat's own page says
// so. Desktop, folded phone (344 px) and the unfolded Fold, light and dark.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

run(async () => {
  await mock("/__mock/reset");
  const term = await wsClient("Terminal on my-server");
  const st = (p, js) => p.eval(`__mod('lib/store.ts').then((st) => ${js})`);

  // ── the computer starts a private chat ──
  const a = await device();
  await a.viewport(1280, 860);
  await signIn(a, "Laptop");
  await a.theme("dark");
  await a.eval(`__mod('lib/actions.ts').then((m) => m.newPrivateChat())`);
  await a.waitFor(`document.querySelector('.private-hero')`, 6000);
  await sleep(300);
  const facts = await a.eval(`[...document.querySelectorAll('.private-facts li')].map((l) => l.textContent.trim())`);
  check(facts.some((f) => /Only on this device/.test(f)), "the private chat's page says it stays on this device", facts);
  for (const theme of ["dark", "light"]) {
    await a.theme(theme);
    await sleep(200);
    await a.shot(`${SHOTS}/private-device-hero-desktop-${theme}.png`);
  }
  await a.theme("dark");
  const priv = await st(a, `(async () => { const i = await st.api.createSession({ private: true }); st.upsertSession(i); st.setState('sessions', i.id, 'loaded', true); await st.sendMessage(i.id, [{ type: 'text', text: 'a private question' }], 'auto'); return i.id; })()`);
  check(typeof priv === "string" && priv.startsWith("ses_"), "the private chat is made", priv);
  const normal = (await term.rpc("create_session", { title: "Shared plans" })).result;

  // ── the folded phone and the unfolded Fold: never see it ──
  for (const [label, w, h] of [["phone", 344, 760], ["fold", 884, 1104]]) {
    const b = await device();
    await b.viewport(w, h, true, 2);
    await signIn(b, `${label} device`);
    await sleep(600);
    const known = await st(b, `({ priv: !!st.state.sessions['${priv}'], normal: !!st.state.sessions['${normal.id}'] })`);
    check(!known.priv && known.normal, `${label}: the computer's private chat isn't on this device (its normal chats are)`, known);
    const open = await st(b, `st.call({ method: 'get_session', params: { session_id: '${priv}' } }).then(() => 'opened', (e) => e.code)`);
    check(open === "not_found", `${label}: opening it fails as if it didn't exist`, open);
    const sent = await st(b, `st.call({ method: 'send', params: { session_id: '${priv}', content: [{ type: 'text', text: 'let me in' }] } }).then(() => 'sent', (e) => e.code)`);
    check(sent === "not_found", `${label}: nor can it write in it`, sent);
    // The computer keeps writing in it: nothing of that reaches this device.
    await st(a, `st.sendMessage('${priv}', [{ type: 'text', text: 'one more private line' }], 'auto')`);
    await sleep(1500);
    check(!(await st(b, `!!st.state.sessions['${priv}']`)), `${label}: …and hears nothing of it as it goes on`);
    // The side list (the phone's drawer) shows no private chats at all.
    await b.eval(`__mod('lib/ui.ts').then((u) => u.setDrawerOpen?.(true))`);
    await sleep(500);
    check(!(await b.eval(`!!document.querySelector('.sb-sub.private')`)), `${label}: no "Private" group in the chat list`);
    for (const theme of ["dark", "light"]) {
      await b.theme(theme);
      await sleep(200);
      await b.shot(`${SHOTS}/private-device-${label}-list-${theme}.png`);
    }
    check(!b.errors.length, `${label}: no page errors`, b.errors);
    await b.close();
  }

  // ── the terminal app (a script with its own login): neither ──
  const listed = (await term.rpc("list_sessions", {})).result.map((s) => s.id);
  check(!listed.includes(priv) && listed.includes(normal.id), "the terminal app doesn't list it", listed.length);
  const peek = await term.rpc("get_session", { session_id: priv });
  check(peek.error?.code === "not_found", "…and can't open it", peek.error);
  check(!term.events.some((e) => (e.session?.id ?? e.session_id) === priv), "…and never heard of it", term.events.filter((e) => (e.session?.id ?? e.session_id) === priv).length);

  // ── the computer, reconnected: still its own ──
  await a.goto(await a.eval(`location.href`), 1500);
  await a.waitFor(`document.querySelector('.workspace')`, 10000);
  await sleep(800);
  const back = await st(a, `!!st.state.sessions['${priv}']`);
  check(back, "the computer, reconnected, still has its private chat");
  await a.eval(`__mod('lib/workspace.ts').then((w) => w.openSession('${priv}', { pane: w.workspace.focused() }))`);
  await sleep(800);
  check(await a.eval(`!!document.querySelector('.private-badge')`), "…and opens it (marked private)");
  check(!a.errors.length, "computer: no page errors", a.errors);
  await a.close();
  term.close();
  await mock("/__mock/reset");
  finish();
});
