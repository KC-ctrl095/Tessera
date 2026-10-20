// The Settings Anthropic key form must never reuse a custom compatible endpoint.
// Only the mock receives the synthetic key. Desktop, 344 px and Fold, light/dark.
import { SHOTS, check, device, finish, mock, run, signIn } from "./cdp.mjs";

const form = ".settings-body .form-card";
async function saveKey(p) {
  await p.clickText(".settings-body .models-connected .section-head button", "Connect");
  await p.waitFor(`!!document.querySelector('.settings-body .connect-card-btn')`, 4000);
  await p.clickText(".settings-body .connect-card-btn", "Anthropic API key");
  await p.waitFor(`!!document.querySelector('${form} input[type=password]')`, 4000);
  await p.click(`${form} input[type=password]`);
  await p.type("fixture-official-key");
  check(await p.eval(`(() => { const e = document.querySelector('${form}'); return e.scrollWidth <= e.clientWidth + 1 && e.getBoundingClientRect().right <= innerWidth + 1; })()`), "key form fits");
  const before = await p.eval("window.__presetSpecs.length");
  await p.click(`${form} button[type=submit]`);
  await p.waitFor(`window.__presetSpecs.length === ${before + 1} && !document.querySelector('${form}')`, 5000);
  return p.eval("window.__presetSpecs.at(-1)");
}

run(async () => {
  for (const [label, w, h, mobile] of [["desktop", 1440, 900, false], ["cover", 344, 882, true], ["fold", 673, 841, true]]) {
    for (const theme of ["dark", "light"]) {
      const tag = `${label}-${theme}`;
      await mock("/__mock/reset");
      const p = await device();
      await p.viewport(w, h, mobile, mobile ? 2.625 : 1);
      await signIn(p, `Presets ${tag}`);
      await p.theme(theme);
      await p.eval(`__mod('lib/store.ts').then(async (m) => {
        for (const provider of m.state.providers.filter((x) => x.kind === 'anthropic')) await m.api.removeProvider(provider.id);
        await m.api.saveProvider({ id: 'anthropic', name: 'Custom Claude', kind: 'anthropic', base_url: 'https://custom.example', api_key: 'fixture-custom-key', discover: true, models: [] });
        await m.refreshProviders();
        window.__presetSpecs = [];
        const save = m.api.saveProvider;
        m.api.saveProvider = async (spec) => { window.__presetSpecs.push(JSON.parse(JSON.stringify(spec))); return save(spec); };
      })`);
      await p.eval(`__mod('lib/ui.ts').then((u) => u.openSettings('providers'))`);
      await p.waitFor(`!!document.querySelector('.settings-body .models-connected .section-head button')`, 8000);
      const added = await saveKey(p);
      check(added.id === "anthropic-2", `${tag}: a new provider avoids the custom ID`, added);
      check(added.base_url === "https://api.anthropic.com", `${tag}: the new key goes only to the official address`);
      const custom = await p.eval(`__mod('lib/store.ts').then(m => JSON.parse(JSON.stringify(m.state.providers.find(x => x.id === 'anthropic'))))`);
      check(custom?.name === "Custom Claude" && custom?.base_url === "https://custom.example", `${tag}: the custom provider is unchanged`, custom);
      const reused = await saveKey(p);
      check(reused.id === added.id && reused.base_url === added.base_url, `${tag}: an official provider is reused, not duplicated`, reused);
      check(await p.eval(`__mod('lib/store.ts').then(m => m.state.providers.filter(x => x.kind === 'anthropic').length === 2)`), `${tag}: both providers remain`);
      await p.shot(`${SHOTS}/provider-presets-${tag}.png`);
      check(p.errors.length === 0, `${tag}: no page errors`, p.errors);
      await p.close();
    }
  }
  finish();
});
