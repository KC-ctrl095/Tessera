// How much freedom new chats get: one choice for all the user's devices, held by the server
// (`default_permission`, lib/freedom.ts). Picked on one device, it is the server's and new chats
// everywhere start with it; a device whose own earlier choice differs asks once which to keep (and
// never again); one that never chose just follows. Desktop and the folded phone; dark and light.
import { APP, SHOTS, check, device, finish, mock, run, signIn, sleep } from "./cdp.mjs";

const ui = (p, js) => p.eval(`__mod('lib/ui.ts').then((u) => { ${js} })`);
const server = (p) => p.eval(`__mod('lib/store.ts').then((m) => m.call({ method: 'get_security' })).then((i) => i.default_permission ?? null)`);
const newChatPermission = (p) =>
  p.eval(`Promise.all([__mod('lib/workspace.ts'), __mod('lib/actions.ts')]).then(([w, a]) => { w.newChat(); return a.paneSettings(w.workspace.focused()).permission; })`);
const dialog = (p) => p.eval(`(() => { const m = document.querySelector('.modal .dialog-body'); if (!m) return null; return { title: document.querySelector('.modal .modal-title, .modal h2')?.textContent ?? '', text: m.querySelector('.dialog-text')?.textContent ?? '', buttons: [...m.querySelectorAll('.dialog-actions button')].map((b) => b.textContent.trim()) }; })()`);
/** A device with its own earlier choice on it (set before it connects). */
async function deviceWith(name, mode, w, h, mobile, theme) {
  const p = await device();
  await p.viewport(w, h, mobile, mobile ? 2.6 : 1);
  await p.goto(APP, 400);
  if (mode) await p.eval(`localStorage.setItem('tessera.prefs', JSON.stringify({ defaultPermission: ${JSON.stringify(mode)}, yoloAck: true, theme: ${JSON.stringify(theme)} }))`);
  await signIn(p, name);
  return p;
}

run(async () => {
  for (const [label, w, h, mobile] of [["desktop", 1440, 900, false], ["phone", 344, 760, true]]) {
    for (const theme of ["dark", "light"]) {
      const tag = `${label}-${theme}`;
      console.log(`— ${tag}`);
      await mock("/__mock/reset");

      // Nobody chose yet: the server has none, a device keeps its own (as before).
      const a = await deviceWith(`Freedom A ${tag}`, null, w, h, mobile, theme);
      await a.theme(theme);
      check((await server(a)) === null, `${tag}: the server has no choice yet`);
      check((await newChatPermission(a)) === "default", `${tag}: new chats start with Default`);

      // Picked in Privacy & safety: the server's, for every device.
      await ui(a, `u.openSettings('safety')`);
      check(await a.waitFor(`document.querySelector('.perm-card')`, 8000).then(() => true, () => false), `${tag}: the choice is in Privacy & safety`);
      const help = await a.eval(`[...document.querySelectorAll('.setting-title')].find((e) => /freedom new chats/.test(e.textContent))?.parentElement.querySelector('.setting-help')?.textContent ?? ''`);
      check(/all your devices/.test(help), `${tag}: …saying it is for all your devices`, help);
      await a.eval(`[...document.querySelectorAll('.perm-card')].find((b) => /Ask me/.test(b.textContent)).click()`);
      await sleep(500);
      check((await server(a)) === "ask", `${tag}: picking Ask me makes it the server's`);
      check((await a.eval(`document.querySelector('.perm-card[aria-checked=true]')?.textContent`))?.includes("Ask me"), `${tag}: …and shows it picked`);
      await a.key("Escape", "Escape", 0, 27);
      await sleep(300);
      check((await newChatPermission(a)) === "ask", `${tag}: new chats here start with it`);
      await a.close();

      // A device that never chose: follows, no question.
      const b = await deviceWith(`Freedom B ${tag}`, "default", w, h, mobile, theme);
      await sleep(800);
      check((await dialog(b)) === null, `${tag}: a device that never chose isn't asked`);
      check((await newChatPermission(b)) === "ask", `${tag}: …and starts new chats with the shared choice`);
      await b.close();

      // A device whose own earlier choice differs: asked once, which to keep.
      const c = await deviceWith(`Freedom C ${tag}`, "read_only", w, h, mobile, theme);
      const asked = await c.waitFor(`document.querySelector('.modal .dialog-body')`, 6000).then(() => true, () => false);
      const d = await dialog(c);
      check(asked && /all your devices/i.test(d?.title ?? "") && /Ask me/.test(d?.text ?? "") && /Read-only/.test(d?.text ?? ""), `${tag}: a device with its own earlier choice is asked which to keep`, d);
      check(JSON.stringify(d?.buttons) === JSON.stringify(["Keep “Ask me”", "Use “Read-only”"]), `${tag}: …two plain choices`, d?.buttons);
      await c.shot(`${SHOTS}/freedom-ask-${tag}.png`);
      await c.eval(`[...document.querySelectorAll('.modal .dialog-actions button')].find((x) => /Use/.test(x.textContent)).click()`);
      await sleep(600);
      check((await server(c)) === "read_only", `${tag}: "Use Read-only" makes it everyone's`);
      check((await newChatPermission(c)) === "read_only", `${tag}: …and new chats start with it`);
      await c.reload(1500);
      await sleep(800);
      check((await dialog(c)) === null, `${tag}: asked only once`);
      check(c.errors.length === 0, `${tag}: no errors`, c.errors);
      await c.close();
    }
  }
  finish();
});
