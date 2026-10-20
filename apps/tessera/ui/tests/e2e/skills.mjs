// SKILLS: Settings → Skills lists skills by where they live and deletes them
// (with a confirm). Desktop and phone, dark and light.
import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from "./cdp.mjs";

run(async () => {
  for (const [label, w, h, mobile] of [["desktop", 1440, 900, false], ["phone", 375, 812, true]]) {
    for (const theme of ["dark", "light"]) {
      await mock("/__mock/reset");
      const ws = await wsClient("skills script");
      const serverSkills = async () => (await ws.rpc("list_skills", { cwd: null })).result.map((s) => s.name);
      const first = label === "desktop" && theme === "dark";
      const p = await device();
      await p.viewport(w, h, mobile, mobile ? 2 : 1);
      await signIn(p, `Skills ${label}`);
      await p.theme(theme);
      await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings('skills'))`);
      await p.waitFor(`document.querySelector('.skill-list')`, 8000);
      await sleep(300);
      const dialog = () => p.eval(`document.querySelector('.dialog-body') ? { title: [...document.querySelectorAll('.modal h2')].pop()?.textContent ?? '', body: document.querySelector('.dialog-text')?.textContent ?? '', danger: !!document.querySelector('.dialog-actions .btn-danger') } : null`);
      const answer = async (ok) => {
        await p.click(ok ? ".dialog-actions button[type=submit]" : ".dialog-actions .btn-ghost");
        await sleep(500);
      };
      const cards = () => p.eval(`[...document.querySelectorAll('.skill[data-skill]')].map(c => c.dataset.skill)`);
      const groups = () => p.eval(`[...document.querySelectorAll('.skill-group')].map(g => g.querySelector('h3').textContent + '|' + g.querySelector('.skill-group-head span').textContent.split(' · ')[0])`);
      const pressDelete = async (name) => {
        await p.click(`.skill[data-skill="${name}"] button[aria-label="Delete ${name}"]`);
        await sleep(350);
      };

      // The listing: grouped by where the skills live, counted, with folder and age.
      const g = await groups();
      check(JSON.stringify(g) === JSON.stringify(["Saved on this server|3 skills", "Project folder|1 skill", "Other folders|2 skills"]), `${label} ${theme}: skills are grouped by where they live`, g);
      const all = await cards();
      check(all.length === 6, `${label} ${theme}: six skills listed`, all);
      const row = await p.eval(`(() => { const c = document.querySelector('.skill[data-skill="rust-release"]'); return { desc: c.querySelector('.skill-desc').textContent, where: c.querySelector('.skill-where').textContent, btn: !!c.querySelector('button[aria-label="Delete rust-release"]') }; })()`);
      check(/Cut a release/.test(row.desc) && /\.config\/tessera\/skills\/rust-release/.test(row.where) && /changed 3d ago/.test(row.where) && row.btn, `${label} ${theme}: a card shows what it does, its folder, when it changed, and a delete button`, row);
      check(!/SKILL\.md/.test(row.where), `${label} ${theme}: the folder is shown, not the file`, row.where);
      if (mobile) {
        const over = await p.eval(`({ page: document.documentElement.scrollWidth - innerWidth, card: Math.max(...[...document.querySelectorAll('.skill')].map(c => c.scrollWidth - c.clientWidth)) })`);
        check(over.page <= 0 && over.card <= 0, `${label} ${theme}: long skill names wrap, nothing scrolls sideways`, over);
      }
      await p.shot(`${SHOTS}/skills-list-${label}-${theme}.png`);

      if (!first) {
        ws.close();
        await p.close();
        continue;
      }

      // What a skill says can be read before deciding.
      await p.click(`.skill[data-skill="rust-release"] .skill-view`);
      await p.waitFor(`document.querySelector('.skill[data-skill="rust-release"] .skill-body')`, 4000);
      const body = await p.eval(`document.querySelector('.skill[data-skill="rust-release"] .skill-body').textContent`);
      check(/name: rust-release/.test(body) && /1\. Check the working tree/.test(body), "View steps shows the skill's text", body);
      await p.shot(`${SHOTS}/skills-view-${label}-${theme}.png`);
      await p.click(`.skill[data-skill="rust-release"] .skill-view`);
      check(!(await p.eval(`!!document.querySelector('.skill[data-skill="rust-release"] .skill-body')`)), "Hide steps puts it away");

      // Cancel keeps it.
      await pressDelete("commit-message");
      let d = await dialog();
      check(!!d && d.danger && /Delete .commit-message.\?/.test(d.title), "deleting asks first, as a dangerous choice", d);
      check(/\.config\/tessera\/skills\/commit-message/.test(d?.body ?? "") && /can't be undone/.test(d?.body ?? "") && !/another tool/.test(d?.body ?? ""), "the confirm names the folder and says it can't be undone", d);
      await p.shot(`${SHOTS}/skills-confirm-${label}-${theme}.png`);
      await answer(false);
      check((await cards()).includes("commit-message") && (await serverSkills()).includes("commit-message"), "Cancel: the skill stays, here and on the server");

      // Confirm deletes it.
      await pressDelete("commit-message");
      await answer(true);
      await p.waitFor(`!document.querySelector('.skill[data-skill="commit-message"]')`, 4000);
      check(!(await serverSkills()).includes("commit-message"), "confirmed: the server no longer has it");
      check((await groups())[0] === "Saved on this server|2 skills", "the group count follows", await groups());
      check(await p.eval(`!![...document.querySelectorAll('.toast')].find(t => /Deleted commit-message/.test(t.textContent))`), "a toast says it was deleted");

      // Another tool's folder says so.
      await pressDelete("pdf");
      d = await dialog();
      check(/another tool/.test(d?.body ?? "") && /\.claude\/skills\/pdf/.test(d?.body ?? ""), "a skill from another tool's folder says it is removed there too", d);
      await answer(true);
      await p.waitFor(`!document.querySelector('.skill[data-skill="pdf"]')`, 4000);

      // Already gone on the server: an error, and the list catches up.
      await ws.rpc("delete_skill", { name: "tessera-proto", cwd: null });
      await pressDelete("tessera-proto");
      await answer(true);
      await sleep(700);
      check(await p.eval(`!![...document.querySelectorAll('.toast-error')].length`), "deleting what is already gone shows an error");
      check(!(await cards()).includes("tessera-proto"), "and the list drops it", await cards());
      check(!(await groups()).some((x) => x.startsWith("Project folder")), "an empty group disappears", await groups());

      // Delete the rest: the empty state.
      for (const name of await cards()) {
        await pressDelete(name);
        await answer(true);
        await p.waitFor(`!document.querySelector('.skill[data-skill="${name}"]')`, 4000);
      }
      check(await p.eval(`!!document.querySelector('.empty-state') && /No skills yet/.test(document.querySelector('.empty-state').textContent)`), "with none left it says so");
      check((await serverSkills()).length === 0, "and the server has none");
      await p.shot(`${SHOTS}/skills-empty-${label}-${theme}.png`);
      console.log(label, theme, "errors", p.errors);
      ws.close();
      await p.close();
    }
  }
  finish();
});
