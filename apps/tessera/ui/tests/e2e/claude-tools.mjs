// Your Claude plan as a first-class engine: Settings → Models walks anyone through it (install Claude Code on
// the server, sign in once there, Tessera's tools come built in) without ever asking for a Claude login, and a
// chat run by Claude Code shows Tessera's tools as their own cards (never `mcp__…`), with Claude Code's own
// subagent as a subagent card.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const CHAT = "ses_cc4b2e81d9f3"; // "Homelab sweep on Claude" (dev/mock-server.mjs, CLAUDE-CODE)

run(async () => {
  await mock("/__mock/reset");
  await mock("/__mock/claude-code?state=missing");
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, "Claude tools");
  await sleep(1000);
  const open = async (tab) => {
    await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings(${JSON.stringify(tab)}))`);
    await sleep(800);
  };
  /** Models › Connect: the ways to connect a service (folded behind Connect once one is connected). */
  const showConnect = async () => {
    await p.waitFor(`document.querySelector('.models-connected .section-head .btn')`, 15000);
    await p.eval(`document.querySelector('.models-connected .section-head .btn[aria-expanded="false"]')?.click()`);
    await p.waitFor(`document.querySelectorAll('.connect-card-btn').length >= 3`, 15000);
  };
  const panel = () =>
    p.eval(`(() => {
      const s = document.querySelector('.claude-setup');
      if (!s) return null;
      return {
        title: s.querySelector('.claude-setup-title').textContent,
        steps: [...s.querySelectorAll('.claude-steps li')].map((l) => ({ done: l.dataset.done === 'true', text: l.textContent })),
        next: s.querySelector('.claude-next')?.textContent ?? null,
        code: [...s.querySelectorAll('code')].map((c) => c.textContent),
        fields: s.querySelectorAll('input, textarea').length,
        guide: !!s.querySelector('.claude-guide'),
      };
    })()`);

  // ── Settings → Models: three plain steps, the server's own next step ──
  await open("providers");
  await showConnect();
  await sleep(300);
  const offered = await p.eval(`[...document.querySelectorAll('.connect-card-btn')].map((b) => b.textContent)`);
  check(offered.some((t) => /Your Claude plan/.test(t) && /Claude Code/.test(t)), "Models offers “Your Claude plan” (through your own Claude Code)", offered);
  await p.clickText(".connect-card-btn", "Your Claude plan");
  await p.waitFor(`document.querySelector('.claude-setup .claude-steps li')`, 6000);
  await sleep(500);
  let st = await panel();
  check(st && st.steps.length === 3 && st.steps.every((s) => !s.done), "three steps, none done yet", st);
  check(/homelab/.test(st.next ?? "") && st.code.some((c) => /install\.sh/.test(c)), "not installed: the server says where, with the install command to copy", st);
  check(st.fields === 0, "no password or key field anywhere: Tessera never takes the Claude login", st);
  check(st.guide, "…and a link to what Tessera does with the Claude account", st);
  await p.shot(`${SHOTS}/claude-setup-missing.png`);

  await mock("/__mock/claude-code?state=signed_out");
  await sleep(700); // the providers list re-renders first
  await p.clickText(".claude-setup .btn", "Check again");
  await sleep(1000);
  st = await panel();
  check(st.steps[0].done && !st.steps[1].done, "installed: step 1 done, step 2 next", st.steps);
  check(/Sign in by running claude once/.test(st.next ?? "") && /homelab/.test(st.next) && st.code.includes("claude"), "…“Sign in by running `claude` once on homelab”, the command as code", st);
  await p.shot(`${SHOTS}/claude-setup-signed-out.png`);

  await mock("/__mock/claude-code?state=ready");
  await sleep(700); // the providers list re-renders first
  await p.clickText(".claude-setup .btn", "Check again");
  await sleep(1200);
  st = await panel();
  check(st.steps.every((s) => s.done) && /Claude is ready/.test(st.title) && st.next === null, "signed in: all three done, no next step", st);
  check(["Notes", "Whiteboards", "Memory", "Passwords & keys"].every((t) => st.steps[2].text.includes(t)), "…Tessera's tools listed as built in, nothing to set up", st.steps[2].text);
  const card = await p.eval(`(() => { const c = [...document.querySelectorAll('.provider')].find((c) => c.textContent.includes('Your Claude Code')); return c && { pill: c.querySelector('.auth-pill').textContent, built: c.querySelector('.provider-built-in')?.textContent ?? null, signIn: [...c.querySelectorAll('button')].some((b) => b.textContent.trim() === 'Sign in') }; })()`);
  check(!!card && /Signed in/.test(card.pill) && /built into its chats/.test(card.built ?? "") && !card.signIn, "its provider card: signed in, Tessera's tools built in, no Tessera sign-in", card);
  const stillOffered = await p.eval(`[...document.querySelectorAll('.connect-card-btn')].some((b) => /Your Claude plan/.test(b.textContent))`);
  check(!stillOffered, "once ready, the “Your Claude plan” card makes way", stillOffered);
  await p.shot(`${SHOTS}/claude-setup-ready.png`);

  // The guide says plainly what Tessera does and doesn't do with the account.
  await p.clickText(".claude-setup .claude-guide", "What Tessera does");
  await p.waitFor(`document.querySelector('.guide-modal .guide-body')`, 8000);
  await sleep(400);
  const guide = await p.eval(`document.querySelector('.guide-modal .guide-body').textContent`);
  check(/doesn.t do with your Claude account/i.test(guide) && /never/i.test(guide) && /mcp-config|in-process|its own pipe|own pipe/i.test(guide), "the guide: what Tessera does and doesn't do with your Claude account", guide.slice(0, 200));
  await p.key("Escape", "Escape", 0, 27);
  await sleep(300);
  await p.key("Escape", "Escape", 0, 27);
  await sleep(500);

  // Phone width: the steps (and the install command) fit the screen.
  await mock("/__mock/claude-code?state=missing");
  await sleep(700);
  await p.viewport(390, 844, true, 2);
  await open("providers");
  await showConnect();
  await p.clickText(".connect-card-btn", "Your Claude plan");
  await p.waitFor(`document.querySelector('.claude-setup .claude-cmd')`, 6000);
  await sleep(400);
  const fits = await p.eval(`(() => { const s = document.querySelector('.claude-setup'); const r = s.getBoundingClientRect(); return { ok: r.left >= -1 && r.right <= innerWidth + 1 && s.scrollWidth <= s.clientWidth + 1, page: document.documentElement.scrollWidth <= innerWidth + 1 }; })()`);
  check(fits.ok && fits.page, "on a phone the steps fit the screen", fits);
  await p.shot(`${SHOTS}/claude-setup-phone.png`);
  await p.key("Escape", "Escape", 0, 27);
  await sleep(300);
  await p.viewport(1440, 900);
  await mock("/__mock/claude-code?state=ready");
  await sleep(300);

  // ── A chat run by Claude Code: Tessera's tools as their own cards ──
  await p.eval(`__mod('lib/workspace.ts').then((m) => m.openSession('${CHAT}', { pane: m.workspace.focused() }))`);
  await p.waitFor(`document.querySelector('.pane.focused .tool, .pane.focused .steps')`, 8000);
  await sleep(600);
  // Open folded runs of steps so every card is on screen.
  await p.eval(`[...document.querySelectorAll('.pane.focused .steps-head[aria-expanded="false"]')].forEach((b) => b.click())`);
  await sleep(500);
  const names = await p.eval(`[...document.querySelectorAll('.pane.focused .tool-name')].map((e) => e.textContent.trim())`);
  // A note the agent writes is a card of its own (AGENT-TILES), not a tool row.
  const noteCard = await p.eval(`!!document.querySelector('.pane.focused .note-card')`);
  check(["bash", "notify"].every((n) => names.includes(n)) && noteCard, "bridged calls show as Tessera's own tools", { names, noteCard });
  const raw = await p.eval(`document.querySelector('.pane.focused').textContent`);
  check(!/mcp__/.test(raw), "no `mcp__…` names anywhere in the chat");
  const sub = await p.eval(`(() => { const c = [...document.querySelectorAll('.pane.focused .subagents .subagent')].find(x => /Review the NAS logs/.test(x.textContent)); return c && { name: c.querySelector('.subagent-name').textContent, kind: c.querySelector('.subagent-role')?.textContent, stats: c.querySelector('.subagent-stats').textContent, activity: c.querySelector('.subagent-activity').textContent, status: c.dataset.status }; })()`);
  check(!!sub && /Review the NAS logs/.test(sub.name) && sub.kind === "Explore", "Claude Code's own subagent: a subagent card with what it was asked and its kind", sub);
  check(!!sub && /6 steps/.test(sub.stats) && /42s|42 s/.test(sub.stats) && sub.status === "ok", "…its steps and time, finished", sub);
  // One with a session of its own (newer servers): the same card as Tessera's subagents, and its chat opens.
  const helper = await p.eval(`(() => { const c = [...document.querySelectorAll('.pane.focused .subagents .subagent')].find((x) => /Check the backups/.test(x.textContent)); return c && { name: c.querySelector('.subagent-name').textContent, role: c.querySelector('.subagent-role')?.textContent, stats: c.querySelector('.subagent-stats').textContent, activity: c.querySelector('.subagent-activity').textContent, status: c.dataset.status, open: !!c.querySelector('.subagent-actions .btn') }; })()`);
  check(!!helper && helper.status === "ok" && /1 step/.test(helper.stats) && helper.activity === "Finished" && helper.open, "a Claude Code subagent with a session: Tessera's subagent card, finished, with Open", helper);
  check(!!helper && !helper.role, "…an everyday subagent (Claude Code's general-purpose type) has no label", helper);
  await p.eval(`[...document.querySelectorAll('.pane.focused .subagents .subagent')].find((x) => /Check the backups/.test(x.textContent)).querySelector('.subagent-actions .btn').click()`);
  await p.waitFor(`document.querySelector('.pane.focused .dm-note')`, 6000).catch(() => {});
  await sleep(600);
  const child = await p.eval(`({ title: document.querySelector('.pane.focused .pane-head')?.textContent ?? '', note: document.querySelector('.pane.focused .dm-note')?.textContent ?? null, composer: !!document.querySelector('.pane.focused .composer textarea'), tools: [...document.querySelectorAll('.pane.focused .tool-name')].map((e) => e.textContent.trim()) })`);
  check(/Check the backups/.test(child.title) && child.tools.includes("bash"), "…its chat opens: what it was asked and what it ran", child);
  check(!!child.note && /A helper/.test(child.note) && !child.composer, "…watch only: a note instead of the message box, pointing to the main chat", child);
  await p.shot(`${SHOTS}/claude-helper-chat.png`);
  await p.eval(`__mod('lib/workspace.ts').then((m) => m.closePane(m.workspace.focused()))`);
  await sleep(500);
  // At work again: Stop stops that subagent alone (no "stop the whole turn?" question), the chat goes on.
  await mock("/__mock/claude-helper?status=running");
  const stopBtn = `[...document.querySelectorAll('.pane.focused .subagents .subagent')].find((x) => /Check the backups/.test(x.textContent))?.querySelector('[aria-label^="Stop"]')`;
  await p.waitFor(`!!(${stopBtn})`, 6000).catch(() => {});
  const canStop = await p.eval(`!!(${stopBtn})`);
  check(canStop, "a Claude Code subagent at work has its own Stop", canStop);
  if (canStop) await p.eval(`(${stopBtn}).click()`);
  await sleep(800);
  const stopped = await p.eval(`(() => { const c = [...document.querySelectorAll('.pane.focused .subagents .subagent')].find((x) => /Check the backups/.test(x.textContent)); return { asked: !!document.querySelector('.modal-backdrop'), activity: c?.querySelector('.subagent-activity')?.textContent, status: c?.dataset.status }; })()`);
  check(!stopped.asked && stopped.activity === "Stopped" && stopped.status === "error", "…it stops alone, without asking to stop the chat's whole turn", stopped);
  const meta = await p.eval(`__mod('lib/tools.ts').then((m) => ({ task: m.toolMeta('Task').verb, ledger: m.toolMeta('task').verb }))`);
  check(meta.task === "Delegated" && meta.ledger === "Updated", "Claude Code's Task is a delegation, Tessera's task still the plan", meta);
  await p.shot(`${SHOTS}/claude-chat-cards.png`);

  // Wave 2: secure entry stays outside collapsed steps; values go to vault only.
  await mock(`/__mock/secret-entry?session_id=${CHAT}`);
  const secretCard = `[aria-label="Secure password entry"]`;
  await p.waitFor(`Boolean(document.querySelector('${secretCard} input[type="password"]'))`);
  for (const [width, height, label] of [[1440,900,"desktop"], [344,780,"folded"], [720,860,"fold"]]) for (const theme of ["dark", "light"]) {
    await p.viewport(width,height,label !== "desktop",2);
    await p.theme(theme);
    await p.eval(`document.querySelector(${JSON.stringify(secretCard)}).scrollIntoView({block:'center'})`);
    await sleep(300);
    check(await p.eval(`document.documentElement.scrollWidth <= innerWidth + 1`), `Claude secure entry ${label} fits`);
    await p.shot(`${SHOTS}/claude-secure-entry-${label}-${theme}.png`);
  }
  await p.viewport(1440,900);
  await p.theme("dark");
  await p.eval(`document.querySelector('${secretCard} input:not([type="password"]):not([type="checkbox"])').focus()`);
  await p.type("fixture-user");
  await p.eval(`document.querySelector('${secretCard} input[type="password"]').focus()`);
  await p.type("ui-fixture-only-password");
  await p.click(`${secretCard} button[type="submit"]`);
  await p.waitFor(`document.querySelector(${JSON.stringify(secretCard)})?.innerText.includes('Saved securely')`);
  await p.waitFor(`document.body.innerText.includes('Saved securely in Tessera.')`);
  check(await p.eval(`!document.body.innerText.includes("starts from Tessera's sanitized history")`), "secure-card-only save keeps Claude history");
  check(await p.eval(`!document.querySelector('.pane.focused').innerText.includes('ui-fixture-only-password')`), "password not in transcript");
  await p.shot(`${SHOTS}/claude-secure-entry-saved-desktop.png`);

  // Tessera's agents bridge: other-model helpers, including queued ones,
  // show at the top and on the call card, across every screen and theme.
  const probe = await wsClient("Claude bridge helpers");
  for (const [label, w, h, mobile] of [["desktop", 1440, 900, false], ["phone", 344, 812, true], ["fold", 841, 701, true]]) {
    for (const theme of ["dark", "light"]) {
      const tag = `${label}-${theme}`;
      await p.viewport(w, h, mobile, mobile ? 2 : 1);
      await p.theme(theme);
      await probe.rpc("set_helpers", { at_once: 1 });
      const chat = (await probe.rpc("create_session", { title: "Claude bridge helpers", model: { provider: "claude-code", model: "sonnet" } })).result;
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${chat.id}', { newPane: true }))`);
      try {
        await p.waitFor(`document.querySelector('.pane.focused .pane-head')?.textContent.includes('Claude bridge helpers')`, 8000);
        await probe.rpc("send", { session_id: chat.id, content: [{ type: "text", text: "delegate two reconnect investigations in parallel and wait" }] });
        await p.waitFor(`document.querySelector('.pane.focused .agents-bar')`, 25000);
      } catch (e) {
        console.log("bridge snapshot", JSON.stringify(await probe.rpc("get_session", { session_id: chat.id })));
        console.log("bridge UI", await p.eval(`({ text: document.querySelector('.pane.focused')?.textContent })`), p.errors);
        await p.shot(`${SHOTS}/claude-bridge-helpers-failure.png`);
        throw e;
      }
      await p.waitFor(`document.querySelectorAll('.pane.focused .subagents .subagent').length === 2`, 12000);
      const snap = (await probe.rpc("get_session", { session_id: chat.id })).result;
      const kids = snap.children;
      check(kids.length === 2 && kids.every(c => c.parent_call && c.model.provider === "openai-codex"), `${tag}: bridged helpers use GPT and link to the call`, kids);
      await p.waitFor(`(() => { const b = document.querySelector('.pane.focused .agents-bar'); const h = document.querySelector('.pane.focused .pane-head'); return b && h && Math.abs(b.getBoundingClientRect().top - h.getBoundingClientRect().bottom) <= 1; })()`, 4000);
      const bar = await p.eval(`(() => { const b = document.querySelector('.pane.focused .agents-bar'); const h = document.querySelector('.pane.focused .pane-head'); return { sum: b.textContent, top: b.getBoundingClientRect().top, header: h.getBoundingClientRect().bottom, fits: b.getBoundingClientRect().right <= innerWidth + 1 }; })()`);
      check(/working/.test(bar.sum) && /waiting/.test(bar.sum) && Math.abs(bar.top - bar.header) <= 1 && bar.fits, `${tag}: working and queued helpers pinned under header and fit`, bar);
      const cards = await p.eval(`[...document.querySelectorAll('.pane.focused .subagents .subagent')].map(c => ({ status: c.dataset.status, text: c.textContent }))`);
      check(cards.some(c => c.status === "waiting") && cards.some(c => c.status === "running"), `${tag}: agents start card shows running and queued helpers`, cards);
      await p.shot(`${SHOTS}/claude-bridge-helpers-${tag}.png`);
      if (label === "desktop" && theme === "dark") {
        const kid = kids.find(c => c.status === "running");
        await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('${kid.id}', { newPane: true }))`);
        await p.waitFor(`document.querySelector('.pane.focused .composer textarea')`, 6000);
        check((await p.eval(`!!document.querySelector('.pane.focused .composer textarea')`)), "bridged GPT helper can continue normally");
        await probe.rpc("abort", { session_id: kid.id });
        const parent = (await probe.rpc("get_session", { session_id: chat.id })).result;
        check(parent.info.status === "running", "stopping a bridged helper leaves Claude's waiting call running");
        await p.eval(`__mod('lib/workspace.ts').then(m => m.closePane(m.workspace.focused()))`);
      }
      await probe.rpc("abort", { session_id: chat.id });
      await p.waitFor(`!document.querySelector('.pane.focused .agents-bar')`, 8000);
      check(!(await p.eval(`!!document.querySelector('.pane.focused .agents-bar')`)), `${tag}: Stop chat clears the helpers strip`);
      const stopped = (await probe.rpc("get_session", { session_id: chat.id })).result;
      const ownInput = stopped.entries.filter(e => e.kind === "assistant").reduce((n, e) => n + (e.usage?.input ?? 0), 0);
      const childInput = stopped.children.reduce((n, c) => n + c.usage.input, 0);
      check(stopped.info.usage.input === ownInput + childInput, `${tag}: the chat counts its own usage and its bridged helpers once`, { total: stopped.info.usage.input, ownInput, childInput });
      await probe.rpc("delete_session", { session_id: chat.id });
    }
  }
  await probe.rpc("set_helpers", { at_once: 4 });
  probe.close();
  console.log("errors", p.errors);
  check(p.errors.length === 0, "no page errors", p.errors);
  await p.close();
  finish();
});
