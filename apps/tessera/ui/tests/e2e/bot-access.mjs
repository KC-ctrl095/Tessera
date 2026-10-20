// Who wrote each message, and what each bot may do on the user's machines.
// - A plain chat's agent asking a bot, a subagent's task, a scheduled job's
//   instructions and an older server's bot DM never show as the user's own
//   words: each names its sender (a bot, the chat by its title, the job).
// - The bot editor's "Machines": per machine No access / Read only / As
//   allowed, new bots with none, older bots as each machine allows; the
//   choice is saved, refused in the bot's chats, and listed under the
//   machine in Settings → Devices.
// Desktop, folded phone (cover screen) and unfolded Fold, dark and light.
// ONLY=folded-light runs one of them.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

const NOVA = "bot_nova01", FORGE = "bot_forge1", SCOUT = "bot_scout1";
const ROOM = "ses_4b7d2e9a0c15"; // "Launch planning"
const BRIEFING = "ses_2f8a6c1e9d47"; // Nova's "Morning briefing" duty run
const AUDIT_CHILD = "ses_c2e8a4f61b37"; // a subagent of "Audit the reconnect logic"

const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const open = (p, sid) => ws(p, `return m.openSession('${sid}', { pane: m.workspace.focused() })`);
const until = (p, expr, ms = 6000) => p.waitFor(expr, ms).then(() => true, () => false);
/** What the focused chat shows: user bubbles, authored messages (who, what kind, tag), speakers. */
const look = (p) =>
  p.eval(`(() => {
    const pane = document.querySelector('.pane.focused');
    return {
      users: [...pane.querySelectorAll('.msg-user .bubble-text')].map((e) => e.textContent),
      authored: [...pane.querySelectorAll('.msg-authored')].map((m) => ({
        kind: m.querySelector('.speaker')?.dataset.author ?? null,
        name: m.querySelector('.speaker-name')?.textContent ?? null,
        tag: m.querySelector('.speaker-handle')?.textContent ?? null,
        link: m.querySelector('button.speaker-name') ? true : false,
        text: m.querySelector('.authored-body')?.textContent.trim().slice(0, 80) ?? '',
      })),
      speakers: [...pane.querySelectorAll('.item-speaker .speaker-name')].map((e) => e.textContent),
      title: pane.querySelector('.pane-head')?.textContent ?? '',
      overflowX: [...pane.querySelectorAll('.speaker')].some((s) => s.getBoundingClientRect().right > pane.getBoundingClientRect().right + 1),
    };
  })()`);
/** The editor's machine rows: id → the picked choice. */
const rows = (p) =>
  p.eval(`Object.fromEntries([...document.querySelectorAll('.be-machines .be-mach')].map((r) => [r.dataset.machine || 'other', r.querySelector('[role=radio][aria-checked=true]')?.textContent.trim() ?? null]))`);

async function sessions(c) {
  return (await c.rpc("list_sessions", { archived: false })).result;
}
async function idle(c, sid, ms = 20000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const s = (await c.rpc("get_session", { session_id: sid })).result;
    if (s.info.status === "idle" && s.entries.some((e) => e.kind === "assistant")) return s;
    await sleep(250);
  }
  return (await c.rpc("get_session", { session_id: sid })).result;
}

run(async () => {
  for (const [label, w, h, mobile, dpr] of [["desktop", 1440, 900, false, 1], ["folded", 344, 882, true, 3], ["unfolded", 884, 1104, true, 2.5]]) {
    for (const theme of ["dark", "light"]) {
      const tag = `${label}-${theme}`;
      if (process.env.ONLY && !tag.includes(process.env.ONLY)) continue;
      const full = label === "desktop" && theme === "dark"; // the behaviour, once; layout everywhere
      console.log(`— ${label} ${theme}`);
      await mock("/__mock/reset");
      const c = await wsClient(`bot-access ${tag}`);
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Bot access ${tag}`);
      await p.theme(theme);

      // ── a plain chat's agent asks a bot: the DM shows the chat, not "You" ──
      const plain = (await c.rpc("create_session", { title: "Server upkeep" })).result;
      await c.rpc("send", { session_id: plain.id, content: [{ type: "text", text: "ask @forge whether main is green" }] });
      await idle(c, plain.id);
      const dm = (await sessions(c)).find((s) => s.kind === "channel" && s.bot_id === FORGE && s.members.length === 1);
      check(!!dm && dm.title === "Your chats → @forge", `${tag}: the plain chat's DM with Forge is "Your chats → @forge"`, dm?.title);
      // The hand-off runs in the background (batch 17): Forge's reply is in once its conversation is idle.
      await idle(c, dm.id);
      const dmEntries = (await c.rpc("get_session", { session_id: dm.id })).result.entries;
      check(dmEntries[0]?.kind === "user" && dmEntries[0].author === `chat:${plain.id}`, `${tag}: the message records the chat as its author`, dmEntries[0]?.author);
      await open(p, dm.id);
      await until(p, `document.querySelector('.pane.focused .msg-authored')`);
      await sleep(400);
      let v = await look(p);
      check(v.users.length === 0, `${tag}: nothing in the DM shows as the user's own message`, v.users);
      check(v.authored[0]?.kind === "chat" && v.authored[0].name === "Server upkeep" && v.authored[0].tag === "agent" && v.authored[0].link, `${tag}: it names the chat by its title, as its agent, with a way to it`, v.authored[0]);
      check(v.speakers.includes("Forge"), `${tag}: Forge's reply is Forge's`, v.speakers);
      check(!v.overflowX, `${tag}: the names fit`);
      await p.shot(`${SHOTS}/access-dm-${tag}.png`);
      if (full) {
        await p.click(".pane.focused .msg-authored button.speaker-name");
        await sleep(700);
        const shown = await ws(p, `return Object.values(m.workspace.panes).map((x) => x.content?.sessionId)`);
        check(shown.includes(plain.id), "the chat's name opens that chat", shown);
      }

      // ── an older server's DM: the message without an author is one of the user's chats' ──
      const legacy = (await c.rpc("mock_session", {
        title: "Tessera → @scout", kind: "channel", bot_id: SCOUT, members: [SCOUT],
        entries: [
          { kind: "user", text: "Hi Scout, which self-hosted note apps sync without a cloud account? A short answer is fine." },
          { kind: "assistant", text: "Joplin (WebDAV or a folder) and Obsidian with folder sync.", author: SCOUT },
        ],
      })).result;
      await open(p, legacy.id);
      await until(p, `document.querySelector('.pane.focused .msg-authored')`);
      await sleep(300);
      v = await look(p);
      check(v.users.length === 0 && v.authored[0]?.name === "One of your chats" && !v.authored[0].link, `${tag}: an older DM's message reads "One of your chats", never "You"`, v);
      check(/Your chats → @scout/.test(v.title), `${tag}: its header reads "Your chats → @scout"`, v.title);

      // ── a duty's instructions are the job's ──
      await open(p, BRIEFING);
      await until(p, `document.querySelector('.pane.focused .msg-authored')`);
      await sleep(300);
      v = await look(p);
      check(v.users.length === 0 && v.authored[0]?.kind === "job" && v.authored[0].name === "Morning briefing" && v.authored[0].tag === "scheduled", `${tag}: a duty run starts with the job's instructions, named as the job`, v.authored[0]);
      await p.shot(`${SHOTS}/access-duty-${tag}.png`);

      // ── a subagent's task is the chat's that started it ──
      await open(p, AUDIT_CHILD);
      await until(p, `document.querySelector('.pane.focused .msg-authored')`);
      await sleep(300);
      v = await look(p);
      check(v.users.length === 0 && v.authored[0]?.kind === "chat" && v.authored[0].name === "Audit the reconnect logic", `${tag}: a subagent's task names the chat that started it`, v.authored[0]);

      // ── a channel: the user's messages are "You", the bots' are theirs ──
      await open(p, ROOM);
      await until(p, `document.querySelectorAll('.pane.focused .item-speaker').length >= 3`);
      await sleep(300);
      v = await look(p);
      check(v.users.length === 2 && v.authored.length === 0, `${tag}: in a channel only the user's own two messages are the user's`, v);
      check(["Scout", "Forge", "Nova"].every((n) => v.speakers.includes(n)), `${tag}: each bot's reply names it`, v.speakers);
      if (full) {
        const gone = (await c.rpc("mock_session", { title: "Old room", kind: "channel", members: [NOVA], entries: [{ kind: "user", text: "From a bot that is gone.", author: "bot_gone99" }] })).result;
        await open(p, gone.id);
        await until(p, `document.querySelector('.pane.focused .msg-authored')`);
        v = await look(p);
        check(v.authored[0]?.name === "Removed bot" && v.users.length === 0, "a bot since removed reads “Removed bot”", v.authored[0]);
      }

      // ── the bot editor's Machines ──
      await p.eval(`__mod('lib/ui.ts').then(m => m.openBotEditor('${FORGE}'))`);
      await until(p, `document.querySelector('.be-machines .be-mach[data-machine="dev_desktop"]')`, 8000);
      await sleep(500);
      let r = await rows(p);
      check(r.dev_desktop === "As allowed" && r.dev_nas === "As allowed" && r.dev_mbp === "No access" && r.other === "No access", `${tag}: Forge: Desktop and nas as allowed, the rest none`, r);
      check(!(await p.eval(`!!document.querySelector('.be-mach-legacy')`)), `${tag}: no "made before" note for a bot that has the setting`);
      const g = await p.eval(`(() => {
        const sec = document.querySelector('.be-machines'); sec.scrollIntoView({ block: 'center' });
        const form = document.querySelector('.be-form').getBoundingClientRect();
        const bad = [...sec.querySelectorAll('.be-mach, .be-mach-seg, .be-mach-seg button, .be-mach-name')].filter((e) => { const b = e.getBoundingClientRect(); return b.right > form.right + 1 || b.left < form.left - 1; }).map((e) => e.className);
        const seg = sec.querySelector('.be-mach[data-machine="dev_desktop"] .be-mach-seg').getBoundingClientRect();
        const head = sec.querySelector('.be-mach[data-machine="dev_desktop"] .be-mach-head').getBoundingClientRect();
        const btn = [...sec.querySelectorAll('.be-mach-seg button')].map((b) => b.scrollWidth <= b.clientWidth + 1);
        return { bad, stacked: seg.top >= head.bottom - 1, fits: btn.every(Boolean), sideScroll: document.querySelector('.be-form').scrollWidth > document.querySelector('.be-form').clientWidth + 1 };
      })()`);
      await sleep(300);
      check(g.bad.length === 0 && !g.sideScroll, `${tag}: the Machines rows fit the form`, g);
      check(g.fits, `${tag}: every choice's label fits its button`, g);
      if (label === "folded") check(g.stacked, `${tag}: on the cover screen the choice sits under the machine's name`, g);
      if (label === "desktop") check(!g.stacked, `${tag}: on a desktop the choice sits beside the machine's name`, g);
      await p.shot(`${SHOTS}/access-editor-${tag}.png`);
      await p.eval(`__mod('lib/ui.ts').then(m => m.setBotEditor(null))`);
      await sleep(300);

      if (full) {
        // Older bots: as each machine allows, said plainly.
        await p.eval(`__mod('lib/ui.ts').then(m => m.openBotEditor('${NOVA}'))`);
        await until(p, `document.querySelector('.be-machines .be-mach[data-machine="dev_desktop"]')`, 8000);
        r = await rows(p);
        check(Object.values(r).every((x) => x === "As allowed") && (await p.eval(`!!document.querySelector('.be-mach-legacy')`)), "a bot made before the setting: as each machine allows, and it says so", r);
        await p.eval(`__mod('lib/ui.ts').then(m => m.setBotEditor(null))`);
        await sleep(300);
        // New bots start with none.
        await p.eval(`__mod('lib/ui.ts').then(m => m.openBotEditor(null))`);
        // A new bot's computers are under "More settings".
        await until(p, `document.querySelector('.bot-editor .be-more')`, 8000);
        await p.eval(`document.querySelector('.bot-editor .be-more').click()`);
        await until(p, `document.querySelector('.be-machines .be-mach[data-machine="dev_desktop"]')`, 8000);
        r = await rows(p);
        check(Object.values(r).every((x) => x === "No access"), "a new bot starts with no access to any machine", r);
        await p.eval(`__mod('lib/ui.ts').then(m => m.setBotEditor(null))`);
        await sleep(300);

        // Scout may only read on Desktop: a build there is refused in its chat.
        const sc = (await c.rpc("create_session", { bot_id: SCOUT })).result;
        await c.rpc("send", { session_id: sc.id, content: [{ type: "text", text: "build it on Desktop" }] });
        let s = await idle(c, sc.id);
        let res = s.entries.find((e) => e.kind === "tool_result");
        check(res?.is_error && /may only read files and list folders there/.test(res.content[0].text), "read only: a command on Desktop is refused, saying why", res?.content[0]?.text);
        // Taken away in the editor: saved, and refused as no access.
        await p.eval(`__mod('lib/ui.ts').then(m => m.openBotEditor('${SCOUT}'))`);
        await until(p, `document.querySelector('.be-machines .be-mach[data-machine="dev_desktop"]')`, 8000);
        check((await rows(p)).dev_desktop === "Read only", "Scout's editor shows read only on Desktop");
        await p.eval(`[...document.querySelectorAll('.be-machines .be-mach[data-machine="dev_desktop"] [role=radio]')].find((b) => b.textContent.trim() === 'No access').click()`);
        await sleep(200);
        check((await p.eval(`document.querySelector('.be-machines .be-mach[data-machine="dev_desktop"] .field-help').textContent`)) === "Can't use Desktop.", "the line under it says what that means");
        await p.click(".be-actions .btn-primary");
        await sleep(900);
        const scout = (await c.rpc("list_bots")).result.find((b) => b.id === SCOUT);
        check(scout.machines?.each?.dev_desktop === "no_access" && scout.machines.default === "no_access", "saved with the bot", scout.machines);
        await c.rpc("send", { session_id: sc.id, content: [{ type: "text", text: "build it on Desktop" }] });
        await sleep(600);
        s = await idle(c, sc.id);
        res = s.entries.filter((e) => e.kind === "tool_result").pop();
        check(res?.is_error && /@scout has no access to it/.test(res.content[0].text), "no access: refused, pointing to the bot's settings", res?.content[0]?.text);
        // A client that leaves it out of an edit keeps it.
        await c.rpc("save_bot", { bot: { ...scout, machines: undefined, role: "Researcher" } });
        const kept = (await c.rpc("list_bots")).result.find((b) => b.id === SCOUT);
        check(JSON.stringify(kept.machines) === JSON.stringify(scout.machines), "an edit without it keeps what was set", kept.machines);

        // Settings → Devices: who may use each machine, by bot.
        await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings('devices'))`);
        await until(p, `document.querySelector('.dev-list')`, 8000);
        await p.eval(`[...document.querySelectorAll('.dev-item')].find(li => li.querySelector('.dev-name').textContent.startsWith('Desktop')).querySelector('.dev-row').click()`);
        await until(p, `document.querySelector('.mach-bots')`);
        const line = await p.eval(`document.querySelector('.mach-bots')?.textContent ?? ''`);
        check(/Bots that may use it: Forge, Nova\./.test(line) && !/Scout/.test(line), "Desktop lists the bots that may use it (Forge, and Nova from before the setting)", line);
        await p.shot(`${SHOTS}/access-devices-${tag}.png`);
        await p.eval(`__mod('lib/ui.ts').then(m => m.setSettingsTab(null))`);
      }
      await p.close();
      c.close?.();
    }
  }
  finish();
});
