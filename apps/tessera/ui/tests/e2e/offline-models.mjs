// Local models offline, not gone: when a model server of the user's (llama.cpp on the desk PC here)
// isn't answering, the model menu keeps its models, marked "Offline — <host> isn't answering", and
// they can't be picked for a chat (a setting such as a model role still can). The chat's chip marks
// a chat whose model is offline, and a message sent anyway ends in the engine's plain error rather
// than a raw one. When the server answers again everything is normal, without a reload.
// Desktop, the folded phone (344 px), the unfolded Fold; dark and light. ONLY=phone-dark runs one.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const SID = "ses_7d3a91e5c4b2"; // "Add a /healthz endpoint" (Claude Sonnet 5.5)
const ws = (p, js) => p.eval(`__mod('lib/workspace.ts').then(m => { ${js} })`);
const open = (p, sid) => ws(p, `return m.openSession('${sid}', { pane: m.workspace.focused() })`);
const until = (p, expr, ms = 4000) => p.waitFor(expr, ms).then(() => true, () => false);
const OFF = "Offline — 192.168.1.20:8080 isn't answering";

/** The chip, or on phones the Model line in the title's sheet. */
async function openModelMenu(p) {
  const direct = await p.eval(`(() => { const b = [...document.querySelectorAll('.pane.focused .model-chip, .pane.focused .model-sub')].find((e) => e.offsetParent); b?.click(); return !!b; })()`);
  if (!direct) {
    await p.click(".pane.focused .title-menu");
    await p.waitFor(`document.querySelector('.tile-opts .opt-model')`, 3000);
    await p.click(".tile-opts .opt-model");
  }
  await p.waitFor(`document.querySelector('.pop-model .model-menu')`, 3000);
  await sleep(350);
}
async function closeMenu(p) {
  await p.key("Escape", "Escape", 0, 27);
  await sleep(250);
}
/** The llama.cpp group in the open menu: its tag, its rows. */
const localGroup = (p) =>
  p.eval(`(() => {
    const g = [...document.querySelectorAll('.pop-model .model-group')].find((x) => x.querySelector('.model-group-name')?.textContent === 'llama.cpp (desk)');
    if (!g) return null;
    const tag = g.querySelector('.model-group-off');
    return {
      tag: tag ? { text: tag.textContent.trim(), color: getComputedStyle(tag).color, bg: getComputedStyle(tag).backgroundColor } : null,
      empty: g.querySelector('.model-offline-empty')?.textContent ?? null,
      rows: [...g.querySelectorAll('.model-item')].map((b) => {
        const d = b.querySelector('.menu-desc');
        const r = b.getBoundingClientRect(), dr = d.getBoundingClientRect();
        return { name: b.querySelector('.menu-label').textContent, desc: d.textContent, disabled: b.disabled, offline: b.classList.contains('offline'), opacity: +getComputedStyle(b).opacity, fits: dr.right <= r.right + 0.5 && d.scrollWidth <= d.clientWidth + 1, title: b.title };
      }),
    };
  })()`);
const chatModel = (p) => p.eval(`__mod('lib/actions.ts').then((m) => m.paneSettings(document.querySelector('.pane.focused').dataset.paneId).model?.model ?? null)`);

run(async () => {
  for (const [label, w, h, mobile, dpr] of [["desktop", 1440, 900, false, 1], ["phone", 344, 882, true, 2.625], ["fold", 673, 841, true, 2.625]]) {
    for (const theme of ["dark", "light"]) {
      const tag = `${label}-${theme}`;
      if (process.env.ONLY && !tag.includes(process.env.ONLY)) continue;
      console.log(`— ${label} ${theme}`);
      await mock("/__mock/reset");
      await mock("/__mock/mosaics/reset");
      const p = await device();
      await p.viewport(w, h, mobile, dpr);
      await signIn(p, `Offline ${label}`);
      await p.theme(theme);
      await open(p, SID);
      await sleep(1200);

      // ── the desk PC switches off: its models stay, marked ──
      await mock("/__mock/provider-offline?id=local");
      check(await until(p, `__mod('lib/store.ts').then((m) => m.state.providers.find((x) => x.id === 'local')?.offline === true)`), `${tag}: the app hears the server is offline`);
      await openModelMenu(p);
      let g = await localGroup(p);
      check(g?.rows.length === 2, `${tag}: its two models are still listed`, g?.rows.map((r) => r.name));
      check(g?.tag?.text === "Offline", `${tag}: the group says Offline`, g?.tag);
      check(!!g && g.rows.every((r) => r.offline && r.disabled && r.desc === OFF), `${tag}: each row says why it can't be picked`, g?.rows);
      check(!!g && g.rows.every((r) => r.opacity >= 0.7 && r.fits), `${tag}: readable and within the menu`, g?.rows.map((r) => [r.opacity, r.fits]));
      check(!!g && g.rows.every((r) => r.title === OFF), `${tag}: the reason is also the row's tooltip`);
      const others = await p.eval(`[...document.querySelectorAll('.pop-model .model-item')].filter((b) => !b.classList.contains('offline')).every((b) => !b.disabled || b.closest('.model-group').querySelector('.link-btn'))`);
      check(others, `${tag}: other providers' models are unaffected`);
      await p.shot(`${SHOTS}/offline-menu-${tag}.png`);
      // A tap on one does nothing.
      await p.clickText(".pop-model .model-item", "Gemma 4 12B");
      await sleep(300);
      check((await chatModel(p)) === "claude-sonnet-5-5", `${tag}: picking an offline model leaves the chat's model as it is`);
      check(await p.eval(`!!document.querySelector('.pop-model .model-menu')`), `${tag}: …and the menu stays open`);

      // ── it answers again: normal at once, while the menu is open ──
      await mock("/__mock/provider-offline?id=local&offline=0");
      check(await until(p, `!document.querySelector('.pop-model .model-group-off') && !document.querySelector('.pop-model .model-item.offline')`), `${tag}: the marks go without a reload`);
      g = await localGroup(p);
      check(!!g && g.rows.every((r) => !r.disabled && r.desc !== OFF), `${tag}: its models can be picked again`, g?.rows);
      await p.clickText(".pop-model .model-item", "Gemma 4 12B");
      check(await until(p, `!document.querySelector('.pop-model')`), `${tag}: picking one works`);
      check((await chatModel(p)) === "gemma-4-12b", `${tag}: the chat is on Gemma now`);

      // ── off again with the chat on it: the chip says so, a message fails plainly ──
      await mock("/__mock/provider-offline?id=local");
      await until(p, `__mod('lib/store.ts').then((m) => m.state.providers.find((x) => x.id === 'local')?.offline === true)`);
      await sleep(200);
      const chip = await p.eval(`(() => { const c = [...document.querySelectorAll('.pane.focused .model-chip')].find((e) => e.offsetParent); return c ? { off: !!c.querySelector('.model-chip-off'), tip: c.dataset.tip } : null; })()`);
      if (chip) {
        check(chip.off && chip.tip.includes(OFF), `${tag}: the model chip marks the offline model`, chip);
        await p.shot(`${SHOTS}/offline-chip-${tag}.png`, await p.eval(`(() => { const r = document.querySelector('.pane.focused .pane-head, .pane.focused .chat-head, .pane.focused header').getBoundingClientRect(); return { x: r.x, y: r.y, width: r.width, height: r.height }; })()`));
      }
      await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
      await p.type("Are you there?");
      if (mobile) await p.click(".pane.focused .send-btn");
      else await p.key("Enter", "Enter", 0, 13);
      const said = await until(p, `[...document.querySelectorAll('.pane.focused .notice-error')].some((n) => n.textContent.includes("llama.cpp (desk) isn't answering at 192.168.1.20:8080 — start it, or pick another model for this chat."))`, 6000);
      check(said, `${tag}: a message sent anyway ends in plain words`);
      check(await p.eval(`[...document.querySelectorAll('.pane.focused .notice-warning')].some((n) => /isn't answering at 192\\.168\\.1\\.20:8080 — retrying/.test(n.textContent))`), `${tag}: …after the engine's retry note`);
      check(!(await p.eval(`/network error|error sending request|tcp connect/i.test(document.querySelector('.pane.focused .transcript')?.textContent ?? '')`)), `${tag}: no raw network error`);
      await sleep(300);
      await p.shot(`${SHOTS}/offline-turn-${tag}.png`);

      // ── a setting may still name an offline model ──
      if (label !== "phone" || theme === "dark") {
        await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings('providers'))`);
        // The models for titles and memory are under Models' More.
        await p.waitFor(`[...document.querySelectorAll('.settings-body .disclosure')].some(d => /models for titles/.test(d.textContent))`, 6000);
        await p.eval(`[...document.querySelectorAll('.settings-body .disclosure')].find(d => /models for titles/.test(d.textContent)).click()`);
        await p.waitFor(`document.querySelector('.role-row[data-role="utility"] .role-pick')`, 6000);
        await sleep(500);
        // (A click can land while Settings is still sliding in.)
        let menu = false;
        for (let k = 0; k < 3 && !menu; k++) {
          await p.click(`.role-row[data-role="utility"] .role-pick`);
          menu = await until(p, `document.querySelector('.pop-model .model-menu')`, 1500);
        }
        check(menu, `${tag}: the role's model menu opens`);
        await sleep(300);
        g = await localGroup(p);
        check(!!g && g.rows.every((r) => r.offline && !r.disabled && r.desc === OFF), `${tag}: a model role can still be set to one (marked)`, g?.rows);
        await p.clickText(".pop-model .model-item", "Qwen3.8 27B");
        check(await until(p, `document.querySelector('.role-row[data-role="utility"] .role-pick-main')?.textContent === 'Qwen3.8 27B'`), `${tag}: …and it is`);
        await closeMenu(p);
        await closeMenu(p);
      }

      // ── a server that never listed a model, off: the group stays with the reason ──
      await mock("/__mock/provider-offline?id=local&empty");
      await until(p, `__mod('lib/store.ts').then((m) => m.state.providers.find((x) => x.id === 'local')?.models.length === 0)`);
      await openModelMenu(p);
      g = await localGroup(p);
      check(g?.empty === OFF && g?.tag?.text === "Offline", `${tag}: an empty offline server still shows, with the reason`, g);
      await closeMenu(p);
      check(p.errors.length === 0, `${tag}: no page errors`, p.errors);
      await p.close();
    }
  }
  finish();
});
