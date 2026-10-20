// Platform-specific machine settings, with the app's local node shell stub.
import { APP, MOCK, SHOTS, check, device, finish, mock, run, sleep, wsClient } from "./cdp.mjs";
import { shellScript } from "./machines-shell.mjs";

run(async () => {
  await mock("/__mock/reset");
  const ws = await wsClient("platform settings");
  const joined = (await ws.rpc("mock_machines", { join: { name: "Windows PC", os: "windows x86_64" } })).result;
  const win = joined.find((m) => m.name === "Windows PC");
  check(win.os === "windows x86_64" && !win.sandbox_ok, "mock presents an honest Windows machine");
  const response = await fetch(MOCK + '/api/pair', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ code: '123456', device_name: 'Platform desktop app' }) });
  const { token } = await response.json();
  for (const [name, os, features, clientOs, sandboxOk] of [
    ["old-linux", "linux x86_64", null, "Win32", true],
    ["old-windows", "windows x86_64", null, "Linux x86_64", false],
    ["old-mac", "macos aarch64", null, "Win32", false],
    ["release-windows", "windows x86_64", ["desktop", "terminal"], "Win32", false],
    ["webview-windows", "windows x86_64", ["desktop", "terminal", "webviews"], "Win32", false],
    ["desktop-only-windows", "windows x86_64", ["desktop"], "Win32", false],
    ["future-windows", "windows x86_64", ["desktop", "admin", "sandbox", "terminal", "webviews"], "Win32", true],
    ["release-linux", "linux x86_64", ["desktop_session", "admin", "sandbox", "terminal", "webviews"], "Win32", true],
    ["empty-linux", "linux x86_64", [], "Linux x86_64", false],
    ["unknown", "", null, "Linux x86_64", false],
    ["old-linux-no-sandbox", "linux x86_64", null, "Win32", false],
  ]) {
    const linux = os.startsWith("linux"), windows = os.startsWith("windows"), mac = os.startsWith("macos");
    const unix = linux || mac;
    const has = (c, fallback) => features == null ? fallback : features.includes(c);
    const sandbox = has("sandbox", linux) && sandboxOk;
    const p = await device();
    await p.send("Page.addScriptToEvaluateOnNewDocument", { source: shellScript(token) + `
      Object.defineProperty(navigator, 'platform', { value: ${JSON.stringify(clientOs)} });
      Object.assign(window.__node, { os: ${JSON.stringify(os)}, features: ${JSON.stringify(features)}, enabled: true, running: true, connected: true, paired: true, sandbox_ok: ${sandboxOk}, scope: { folders: [], commands: false, ask: 'always', timeout_secs: 300 } });
      window.__chosen = [];` });
    await p.viewport(1280, 820);
    await p.goto(APP, 1800);
    await p.waitFor(`document.querySelector('.workspace')`, 12000);
    await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings('thiscomputer'))`);
    await p.waitFor(`document.querySelector('.mach-local-tab')`, 8000);
    await p.eval(`[...document.querySelectorAll('.dev-this .disclosure')].find(b => b.textContent.trim() === 'Advanced').click()`);
    for (const [layout, w, h, mobile] of [["desktop", 1280, 820, false], ["phone", 344, 882, true], ["fold", 884, 1104, true]]) {
      await p.viewport(w, h, mobile);
      for (const theme of ["dark", "light"]) {
        await p.theme(theme);
        await sleep(200);
        const state = await p.eval(`(() => {
          const panel = document.querySelector('.mach-local-tab');
          return { presets: [...panel.querySelectorAll('.mach-preset-name')].map(e => e.textContent), work: [...panel.querySelectorAll('.mach-preset')].find(e => e.textContent.includes('Work in folders'))?.querySelector('.mach-preset-desc')?.textContent, desktop: !!panel.querySelector('[aria-label="Desktop control"]'), admin: !!panel.querySelector('[data-mach-admin]'), webviews: !!panel.querySelector('[aria-label="Web views here"]'), terminal: [...panel.querySelectorAll('.mach-term button')].map(e => e.textContent), unavailable: panel.querySelector('[data-mach-terminal]').textContent, fits: panel.scrollWidth <= panel.clientWidth + 1 };
        })()`);
        const label = `${name} ${layout} ${theme}`;
        const desktop = has("desktop", linux) || has("desktop_session", false);
        check(state.presets.includes("Desktop control") === desktop && state.desktop === desktop, `${label}: desktop feature or safe legacy fallback`, state);
        check(state.presets.includes("Full access") === (!mac || has("sandbox", false)), `${label}: full-home write access only when supported`);
        check(state.presets.includes("Work in folders"), `${label}: checked file work is always offered`);
        check(sandbox ? state.work.includes('run commands there') : state.work.includes("Commands aren't available") && !state.work.includes('run commands'), `${label}: Work description matches command confinement`, state.work);
        check(state.admin === has("admin", unix), `${label}: administrator feature or Unix fallback`);
        check(state.webviews === has("webviews", unix), `${label}: webviews feature or Unix fallback`);
        check(has("terminal", unix) ? state.terminal.includes("Full access") : !state.terminal.length && state.unavailable.includes(windows ? "Not on Windows yet" : "Not available"), `${label}: terminal availability is known before clicking`, state.terminal);
        if (windows) check(!state.terminal.includes("Sandboxed"), `${label}: no Windows terminal sandbox promised`);
        check(state.fits, `${label}: controls fit the panel`);
        check(!(await p.eval(`document.querySelector('.toast-error')`)), `${label}: no refusal toast`);
        await p.shot(`${SHOTS}/platform-${name}-${layout}-${theme}.png`);
      }
    }
    check(!(await p.eval(`window.__calls.some(c => ['this_computer_scope', 'this_computer_preset', 'this_computer_admin_password'].includes(c.cmd))`)), `${os}: unavailable choices never sent a local change`);
    await p.viewport(1280, 820);
    // A truly fresh node has no remembered folders. Cancelling either the
    // picker or its access confirmation must not enable or change the node.
    await p.clickText('.mach-preset', 'Off');
    await p.waitFor(`window.__node.enabled === false && document.querySelector('.mach-preset[aria-checked=true] .mach-preset-name')?.textContent === 'Off'`, 5000);
    const before = await p.eval(`JSON.stringify(window.__node)`);
    const mutations = () => p.eval(`window.__calls.filter(c => ['this_computer_enable', 'this_computer_preset', 'this_computer_scope'].includes(c.cmd)).length`);
    const beforeCalls = await mutations();
    const pickWork = async () => {
      await p.clickText('.mach-preset', 'Work in folders');
      await p.waitFor(`document.querySelector('.dialog-text')`, 5000);
      const warning = await p.eval(`document.querySelector('.dialog-text').textContent`);
      check(sandbox ? warning.includes('run commands there') : warning.includes("Commands aren't available") && !warning.includes('run commands'), `${name}: Work confirmation matches its capabilities`, warning);
      await p.click('.dialog-actions button[type=submit]');
      await p.waitFor(`document.querySelector('.dirb-list .dirb-item')`, 5000);
      check(await p.eval(`JSON.stringify(window.__node)`) === before && await mutations() === beforeCalls, `${name}: first Work opens the local folder picker before changing anything`);
    };
    const firstFolder = async () => {
      await p.clickText('.dirb-item', 'code');
      await p.waitFor(`document.querySelector('.dirb-path input').value === '/home/me/code'`, 5000);
      await p.clickText('.dirb-foot .btn-primary', 'Use this folder');
      await p.waitFor(`document.querySelector('.dialog-text')`, 5000);
      const title = await p.eval(`[...document.querySelectorAll('.modal h2')].pop()?.textContent`);
      const warning = await p.eval(`document.querySelector('.dialog-text').textContent`);
      check(title === 'Let agents work in ~/code?' && warning.includes('read, change and delete') && (sandbox ? warning.includes('run commands there') : warning.includes("Commands aren't available")), `${name}: first folder asks for Work access, not read-only`, { title, warning });
    };
    await pickWork();
    await p.clickText('.dirb-foot .btn', 'Cancel');
    await p.waitFor(`!document.querySelector('.dirb')`, 5000);
    check(await p.eval(`JSON.stringify(window.__node)`) === before && await mutations() === beforeCalls, `${name}: cancelling the first-folder picker changes nothing`);
    await pickWork();
    await firstFolder();
    await p.clickText('.dialog-actions button', 'Cancel');
    await p.waitFor(`!document.querySelector('.dialog-text')`, 5000);
    check(await p.eval(`JSON.stringify(window.__node)`) === before && await mutations() === beforeCalls, `${name}: cancelling first-folder access changes nothing`);
    await pickWork();
    await firstFolder();
    await p.click('.dialog-actions button[type=submit]');
    await p.waitFor(`window.__node.preset === 'work_in_folders'`, 5000);
    const saved = await p.eval(`window.__node.scope`);
    check(saved.folders.length === 1 && saved.folders[0].path === '/home/me/code' && saved.folders[0].write && saved.commands === sandbox, `${name}: fresh Work keeps its first folder writable, commands only with sandbox`, saved);
    const applied = await p.eval(`window.__calls.filter(c => c.cmd === 'this_computer_preset').map(c => c.args)`);
    check(applied.length === 1 && applied[0].preset === 'work_in_folders' && JSON.stringify(applied[0].folders) === '["/home/me/code"]', `${name}: Work and the first folder are saved together, never an empty preset`, applied);
    check(await p.eval(`document.querySelector('.mach-preset[aria-checked=true] .mach-preset-name').textContent === 'Work in folders'`), `${name}: file-only Work remains selected after save`);
    check(!(await p.eval(`document.querySelector('.toast-error')`)), `${name}: saving Work has no refusal toast`);
    if (name === "release-windows") {
      await p.viewport(1280, 820);
      await p.clickText('.mach-term button', 'Full access');
      await p.waitFor(`document.querySelector('.dialog-text')`, 5000);
      const warning = await p.eval(`document.querySelector('.dialog-text').textContent`);
      check(/keys and saved passwords/.test(warning), 'Windows Full terminal warns about unconfined account access', warning);
      await p.click('.dialog-actions button[type=submit]');
      await p.waitFor(`window.__node.scope.terminal === 'full'`, 5000);
      check(await p.eval(`window.__calls.some(c => c.cmd === 'this_computer_scope' && c.args.scope.terminal === 'full')`), 'Windows terminal capability enables Full via the local scope command');
      await p.clickText('.mach-term button', 'Off');
      await p.waitFor(`window.__node.scope.terminal === 'off'`, 5000);
      check(!(await p.eval(`document.querySelector('.toast-error')`)), 'Windows Full and Off changes do not report an unsupported-option refusal');
    }
    check(!p.errors.length, `${os}: no page errors`, p.errors);
    await p.close();
  }
  ws.close();
  finish();
});
