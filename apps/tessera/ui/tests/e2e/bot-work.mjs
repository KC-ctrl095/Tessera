// Work handed to a bot in the background (batch 17, item 4): `message_bot`
// returns at once, the chat stays free, and the chat shows plainly that a bot
// is working on something for it (a live card on the call, a row in the pinned
// helpers bar) with Stop and Open. The bot's reply comes back into the chat as
// a quiet card ("@forge replied · 3 s"), and the chat carries on with it. Stop
// on the card or the bar stops the bot (its reply then says so, and the chat
// waits for the user); stopping the chat stops its bots too. "… and wait" still
// waits in the call. Desktop, a folded phone (344 px) and the unfolded Fold,
// dark and light.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const FORGE = "bot_forge1", SCOUT = "bot_scout1", NOVA = "bot_nova01";

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const open = (p, sid) => ws(p, `return m.openSession('${sid}', { pane: m.workspace.focused() })`);
const waitFor = async (fn, ms = 10000) => {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const v = await fn().catch(() => null);
    if (v) return v;
    await sleep(120);
  }
  return null;
};
const session = async (c, sid) => (await c.rpc("get_session", { session_id: sid })).result;
const dmOf = async (c, bot) => (await c.rpc("list_sessions", { archived: false })).result.find((s) => s.kind === "channel" && s.bot_id === bot && s.members.length === 1);
/** The focused chat: the hand-off card, the bar, the reply cards. */
const look = (p) =>
  p.eval(`(() => {
    const pane = document.querySelector('.pane.focused');
    // The newest hand-off's card (a chat may have several).
    const card = [...pane.querySelectorAll('.bot-handoff')].pop() ?? null;
    const bar = pane.querySelector('.agents-bar');
    return {
      card: card && {
        name: card.querySelector('.subagent-name')?.textContent.trim(),
        status: card.querySelector('.subagent')?.dataset.status,
        activity: card.querySelector('.subagent-activity')?.textContent.trim(),
        stop: !!card.querySelector('[aria-label^="Stop"]'),
        open: [...card.querySelectorAll('button')].some((b) => b.textContent.trim() === 'Open'),
        right: card.getBoundingClientRect().right,
      },
      bar: bar && { sum: bar.querySelector('.agents-sum')?.textContent.trim(), names: bar.querySelector('.agents-names')?.textContent.trim() ?? null },
      replies: [...pane.querySelectorAll('.bot-reply')].map((r) => ({ name: r.querySelector('.report-name')?.textContent.trim(), meta: r.querySelector('.report-meta')?.textContent.trim(), failed: r.classList.contains('failed'), gist: r.querySelector('.report-gist')?.textContent.trim() ?? '' })),
      paneRight: pane.getBoundingClientRect().right,
    };
  })()`);

run(async () => {
  for (const [label, w, h, mobile, dpr] of [["desktop", 1440, 900, false, 1], ["folded", 344, 882, true, 3], ["unfolded", 884, 1104, true, 2.5]]) {
    for (const theme of ["dark", "light"]) {
      const tag = `${label}-${theme}`;
      if (process.env.ONLY && !tag.includes(process.env.ONLY)) continue;
      const full = label === "desktop" && theme === "dark"; // the behaviour, once; layout everywhere
      console.log(`— ${tag}`);
      await mock("/__mock/reset");
      const c = await wsClient(`bot-work ${tag}`);
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Bot work ${tag}`);
      await p.theme(theme);

      // ── a plain chat hands work to Forge: the call returns at once ──
      const chat = (await c.rpc("create_session", { title: "Server upkeep" })).result;
      await open(p, chat.id);
      await sleep(400);
      await c.rpc("send", { session_id: chat.id, content: [{ type: "text", text: "ask @forge whether main is green" }] });
      const working = await waitFor(async () => {
        const v = await look(p);
        return v.card?.status === "running" ? v : null;
      });
      check(!!working, `${tag}: the call shows Forge at work on it`, (await look(p)).card);
      check(working?.card.name === "@forge" && /^Working on it/.test(working?.card.activity ?? "") && working?.card.stop && working?.card.open, `${tag}: …named, what it does now, with Stop and Open`, working?.card);
      const freeAgain = await waitFor(async () => {
        const [me, dm] = [await session(c, chat.id), await dmOf(c, FORGE)];
        return me.info.status === "idle" && dm?.status === "running" ? { me: me.info.status, dm: dm.status } : null;
      });
      check(!!freeAgain, `${tag}: the chat is free again while Forge still works`, freeAgain);
      const dm = await dmOf(c, FORGE);
      const wf = (await session(c, dm.id)).info.working_for;
      check(wf?.session_id === chat.id && !!wf?.call_id, `${tag}: Forge's conversation says whom it works for`, wf);
      const bar = await waitFor(async () => {
        const v = await look(p);
        return v.bar && /1 bot working/.test(v.bar.sum ?? "") ? v.bar : null;
      });
      check(!!bar && /@forge/.test(bar.names ?? ""), `${tag}: the pinned bar says a bot is working, and which`, (await look(p)).bar);
      const said = (await session(c, chat.id)).entries.filter((e) => e.kind === "assistant").map((e) => e.content.filter((x) => x.type === "text").map((x) => x.text).join(" ")).join(" ");
      check(/handed this to Forge/.test(said), `${tag}: the agent told the user it handed it over`, said.slice(-160));
      const v1 = await look(p);
      check(v1.card.right <= v1.paneRight + 1, `${tag}: the card fits the tile`, { card: v1.card.right, pane: v1.paneRight });
      await p.shot(`${SHOTS}/bot-work-running-${tag}.png`);

      // ── the reply arrives as a quiet card, and the chat carries on ──
      const replied = await waitFor(async () => {
        const v = await look(p);
        return v.replies.length === 1 && v.card?.status === "ok" ? v : null;
      }, 20000);
      check(!!replied, `${tag}: Forge's reply arrives in the chat`, await look(p));
      check(replied?.replies[0].name === "@forge replied" && /^\d+ s$/.test(replied?.replies[0].meta ?? "") && /Green|Checked/.test(replied?.replies[0].gist ?? ""), `${tag}: "@forge replied · N s" and its first words`, replied?.replies[0]);
      check(replied?.card.activity === "Replied" && !replied?.card.stop, `${tag}: the card says it replied (no Stop any more)`, replied?.card);
      const relay = await waitFor(async () => {
        const s = await session(c, chat.id);
        const last = s.entries[s.entries.length - 1];
        return s.info.status === "idle" && last?.kind === "assistant" && /Forge replied/.test(last.content.map((x) => x.text ?? "").join(" ")) ? last : null;
      }, 15000);
      check(!!relay, `${tag}: the chat carried on with the reply by itself`);
      check(!(await look(p)).bar, `${tag}: the bar is gone once nobody works`);
      check(!(await session(c, dm.id)).info.working_for, `${tag}: Forge's conversation works for nobody now`);
      const entry = (await session(c, chat.id)).entries.find((e) => e.kind === "user" && e.author === FORGE);
      check(/^\[Reply from @forge · \d+ s · session /.test(entry?.content?.[0]?.text ?? ""), `${tag}: the reply is a message from Forge with its header`, entry?.content?.[0]?.text?.slice(0, 80));
      await p.shot(`${SHOTS}/bot-work-replied-${tag}.png`);

      if (full) {
        await p.click(".pane.focused .bot-reply .report-head");
        await sleep(300);
        const body = await p.eval(`document.querySelector('.pane.focused .bot-reply .report-body')?.textContent`);
        check(/Green|Checked/.test(body ?? "") && /Open conversation/.test(body ?? ""), "the reply opens to all of it, with a way to Forge's conversation", body?.slice(0, 120));
        await p.clickText(".pane.focused .bot-reply .report-open", "Open conversation");
        await sleep(700);
        const shown = await ws(p, `return Object.values(m.workspace.panes).map((x) => x.content?.sessionId)`);
        check(shown.includes(dm.id), "Open conversation shows Forge's conversation", shown);
        await open(p, chat.id);
        await sleep(400);

        // ── Stop on the card: the bot stops, and the chat waits for the user ──
        console.log("— stopped from the card");
        await c.rpc("send", { session_id: chat.id, content: [{ type: "text", text: "ask @scout about self-hosted note apps" }] });
        const scoutCard = await waitFor(async () => ((await look(p)).card?.status === "running" && (await look(p)).card?.name === "@scout" && (await p.eval(`document.querySelectorAll('.pane.focused .bot-handoff').length`)) === 2 ? true : null));
        check(!!scoutCard, "a second hand-off shows Scout at work");
        // Stopped while the chat itself is idle (in a running turn the note is simply read at its next step).
        const idleWhileScout = await waitFor(async () => ((await session(c, chat.id)).info.status === "idle" && (await dmOf(c, SCOUT))?.status === "running" ? true : null));
        check(!!idleWhileScout, "…while the chat itself is free");
        await p.eval(`[...document.querySelectorAll('.pane.focused .bot-handoff')].pop().querySelector('[aria-label^="Stop"]').click()`);
        const stopped = await waitFor(async () => {
          const v = await look(p);
          return v.replies.length === 2 && v.replies[1].failed ? v.replies[1] : null;
        }, 10000);
        check(stopped?.name === "@scout didn't reply" && /stopped before it replied/.test(stopped?.gist ?? ""), "Stop: Scout stops, and its reply says so", stopped);
        await sleep(1500);
        const after = await session(c, chat.id);
        const tail = after.entries[after.entries.length - 1];
        check(after.info.status === "idle" && tail.kind === "user" && tail.author === SCOUT, "…and the chat doesn't carry on by itself (it waits for the user)", { status: after.info.status, tail: tail.kind });
        await p.shot(`${SHOTS}/bot-work-stopped-${tag}.png`);

        // ── Stop in the bar ──
        console.log("— stopped from the bar");
        await c.rpc("send", { session_id: chat.id, content: [{ type: "text", text: "ask @nova to plan friday" }] });
        const barRow = await waitFor(() => p.eval(`!!document.querySelector('.pane.focused .agents-bar')`));
        check(barRow, "the bar is back while Nova works");
        await p.click(".pane.focused .agents-bar .agents-head");
        await sleep(300);
        const row = await p.eval(`document.querySelector('.pane.focused .agents-bar .bot-row')?.textContent`);
        check(/@nova/.test(row ?? ""), "the bar's row names Nova", row);
        await p.shot(`${SHOTS}/bot-work-bar-${tag}.png`);
        await p.click(".pane.focused .agents-bar .bot-row [aria-label^='Stop']");
        const novaStopped = await waitFor(async () => ((await look(p)).replies.find((r) => r.name === "@nova didn't reply") ? true : null));
        check(!!novaStopped, "Stop in the bar stops Nova too");

        // ── stopping the chat stops its bots ──
        console.log("— the chat stopped");
        await c.rpc("send", { session_id: chat.id, content: [{ type: "text", text: "ask @forge whether main is green" }] });
        await waitFor(async () => ((await dmOf(c, FORGE))?.status === "running" ? true : null));
        await waitFor(async () => ((await session(c, chat.id)).info.status === "idle" ? true : null));
        await c.rpc("abort", { session_id: chat.id });
        const forgeIdle = await waitFor(async () => ((await dmOf(c, FORGE))?.status === "idle" ? true : null));
        check(!!forgeIdle, "Stop on the chat stops the bot working for it");
        const forgeStopped = await waitFor(async () => ((await look(p)).replies.filter((r) => r.name === "@forge didn't reply").length === 1 ? true : null));
        check(!!forgeStopped, "…and its reply says it was stopped");

        // ── "… and wait": the call waits for the reply, as before ──
        console.log("— waiting in the call");
        const before = (await look(p)).replies.length;
        await c.rpc("send", { session_id: chat.id, content: [{ type: "text", text: "ask @forge whether main is green and wait for it" }] });
        const waited = await waitFor(async () => {
          const s = await session(c, chat.id);
          const call = s.entries.flatMap((e) => (e.kind === "assistant" ? e.content : [])).filter((x) => x.type === "tool_call" && x.name === "message_bot").pop();
          return s.info.status === "idle" && call?.arguments?.wait === true && s.entries[s.entries.length - 1].kind === "assistant" ? call : null;
        }, 20000);
        check(!!waited, "with “wait” the call carries wait: true and the turn ends with the reply relayed");
        check((await look(p)).replies.length === before, "…and no reply card arrives separately", (await look(p)).replies.length);
      }
      check(!p.errors.length, `${tag}: no page errors`, p.errors.slice(0, 2));
      await p.close?.();
    }
  }
  finish();
});
