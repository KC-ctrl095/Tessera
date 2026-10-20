// Offline: final HTML/file fixtures, no browser, network or user state.
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { emittedFirstLoad, emittedLaunchPaths, firstLoadBudget } from "../build/first-load-budget.mjs";
import { startupCallsites, startupSource, validateStartupCalls } from '../build/startup-audit.mjs';

const fixture = JSON.parse(readFileSync(new URL("./fixtures/first-load-budget.json", import.meta.url), "utf8"));
const context = { error: message => { throw new Error(message); } };
const temporary = fn => {
  const directory = mkdtempSync(join(tmpdir(), "tessera-first-load-"));
  const bundle = Object.fromEntries([
    ...fixture.chunks.map(chunk => [chunk.fileName, { type: "chunk", ...structuredClone(chunk) }]),
    ...fixture.assets.map(asset => [asset.fileName, { type: "asset", ...asset }]),
  ]);
  writeFileSync(join(directory, "index.html"), fixture.html);
  for (const chunk of fixture.chunks) writeFileSync(join(directory, chunk.fileName), chunk.written);
  for (const asset of fixture.assets) writeFileSync(join(directory, asset.fileName), asset.source.repeat(1000));
  try { return fn(directory, bundle); } finally { rmSync(directory, { recursive: true, force: true }); }
};
const files = result => result.files.map(file => file.fileName);
const guard = (directory, bundle, staticKiB = 610, pairedKiB = 670) => firstLoadBudget(staticKiB, pairedKiB).writeBundle.handler.call(context, { dir: directory }, bundle);

test("fixed caps document the measured paired startup and small headroom", () => {
  assert.equal(fixture.budgets.staticKiB, 610);
  assert.equal(fixture.budgets.pairedKiB, 670);
  assert.equal(fixture.budgets.pairedKiB * 1024, fixture.budgets.pairedMeasuredBytes + fixture.budgets.pairedHeadroomBytes);
  assert.ok(fixture.budgets.pairedHeadroomBytes / fixture.budgets.pairedMeasuredBytes < 0.01);
  assert.match(fixture.budgets.reason, /lib\/e2e.*pre-chat/);
});
test("emitted HTML seeds scripts, preloads, inline JS and a JS asset; cycles/shared imports count once", () => temporary((directory, bundle) => {
  const measured = emittedFirstLoad(bundle, directory);
  assert.deepEqual(files(measured), fixture.expected);
  assert.equal(measured.bytes, measured.files.reduce((n, f) => n + (f.fileName.includes('#') ? Buffer.byteLength("var theme='é';") : statSync(join(directory, f.fileName)).size), 0));
  assert.ok(!files(measured).some(name => /lazy|\.css|\.map/.test(name)));
  bundle["unrequested.js"] = { type: "chunk", fileName: "unrequested.js", isEntry: true };
  assert.deepEqual(files(emittedFirstLoad(bundle, directory)), fixture.expected, "unrequested Rollup entry flags do not seed the page");
  assert.notEqual(measured.bytes, fixture.chunks.filter(c => fixture.expected.includes(c.fileName)).reduce((n, c) => n + Buffer.byteLength(c.code), 0));
}));

test("a modulepreload of an otherwise dynamic chunk increases the gated set", () => temporary((directory, bundle) => {
  const before = emittedFirstLoad(bundle, directory);
  writeFileSync(join(directory, "index.html"), fixture.html + "<link rel='modulepreload' href='./lazy.js'>");
  const after = emittedFirstLoad(bundle, directory);
  assert.ok(files(after).includes("lazy-only.js"));
  assert.equal(after.bytes - before.bytes, statSync(join(directory, "lazy.js")).size + statSync(join(directory, "lazy-only.js")).size);
}));

test("exact 610 KiB passes, one final byte over fails with tiny pre-hook source", () => temporary((directory, bundle) => {
  const initial = emittedFirstLoad(bundle, directory).bytes;
  const main = statSync(join(directory, "main.js")).size;
  writeFileSync(join(directory, "main.js"), " ".repeat(610 * 1024 - initial + main));
  assert.doesNotThrow(() => guard(directory, bundle, 610, 1000));
  writeFileSync(join(directory, "main.js"), " ".repeat(610 * 1024 - initial + main + 1));
  assert.throws(() => guard(directory, bundle, 610, 1000), /624641 emitted bytes; budget 610 KiB/);
  assert.equal(bundle["main.js"].code, "x");
  const plugin = firstLoadBudget(610, 670);
  assert.equal(plugin.writeBundle.order, "post");
  assert.equal(plugin.writeBundle.sequential, true);
}));

test("encrypted startup includes e2e, all its pre-chat imports, and fails its own cap while static passes", () => temporary((directory, bundle) => {
  const paths = emittedLaunchPaths(bundle, directory);
  assert.deepEqual(files(paths.paired), [...fixture.expected, 'crypto.js', 'crypto-helper.js'].sort((a,b) => a.localeCompare(b)));
  assert.ok(paths.onboarding.bytes > paths.static.bytes && paths.fileWindowShell.bytes > paths.static.bytes);
  const encrypted = statSync(join(directory, "crypto.js")).size;
  writeFileSync(join(directory, "crypto.js"), " ".repeat(670 * 1024 - paths.paired.bytes + encrypted + 1));
  assert.throws(() => guard(directory, bundle), /paired.*686081 emitted bytes; budget 670 KiB/);
}));

test("unknown/missing HTML scripts and external imports fail closed", () => temporary((directory, bundle) => {
  for (const addition of ["<script src='./missing.js'></script>", "<link rel='modulepreload' href='./missing.js'>", "<script src='https://foreign.invalid/app.js'></script>", "<body onload='import(\"./hidden.js\")'>", "<a href='javascript:run()'>", "<base href='/other/'>", "<script src='./app.css'></script>", "<script type='importmap'>{}</script>"]) {
    writeFileSync(join(directory, "index.html"), fixture.html + addition);
    assert.throws(() => emittedFirstLoad(bundle, directory), /Uncounted/);
  }
  writeFileSync(join(directory, "index.html"), fixture.html);
  for (const dependency of ['missing.js', 'https://foreign.invalid/app.js']) {
    bundle['main.js'].imports = [dependency];
    assert.throws(() => emittedFirstLoad(bundle, directory), /Uncounted/);
  }
}));

test("post-hook imports not in chunk metadata and JS-asset dependencies cannot disappear", () => temporary((directory, bundle) => {
  writeFileSync(join(directory, "main.js"), "import './missing-late.js';");
  assert.throws(() => emittedFirstLoad(bundle, directory), /Uncounted.*missing-late/);
  writeFileSync(join(directory, "main.js"), fixture.chunks[0].written);
  writeFileSync(join(directory, "worker.js"), "import './lazy-only.js';");
  assert.ok(files(emittedFirstLoad(bundle, directory)).includes('lazy-only.js'));
  writeFileSync(join(directory, "worker.js"), "import './missing-asset-dependency.js';");
  assert.throws(() => emittedFirstLoad(bundle, directory), /Uncounted.*missing-asset/);
}));

test("workers are counted, and an unresolvable worker fails closed", () => temporary((directory, bundle) => {
  const original = fixture.chunks[0].written;
  writeFileSync(join(directory, "main.js"), original + "new Worker(new URL('./lazy-only.js', import.meta.url));");
  assert.ok(files(emittedFirstLoad(bundle, directory)).includes('lazy-only.js'));
  writeFileSync(join(directory, "main.js"), original + "new window.Worker('./lazy-only.js');");
  assert.ok(files(emittedFirstLoad(bundle, directory)).includes('lazy-only.js'));
  writeFileSync(join(directory, "worker.js"), "importScripts('./lazy-only.js');");
  assert.ok(files(emittedFirstLoad(bundle, directory)).includes('lazy-only.js'));
  writeFileSync(join(directory, "worker.js"), "importScripts('./missing-worker-dependency.js');");
  assert.throws(() => emittedFirstLoad(bundle, directory), /Uncounted.*missing-worker/);
  writeFileSync(join(directory, "worker.js"), fixture.assets[2].source);
  writeFileSync(join(directory, "main.js"), original + "new Worker(runtimeUrl);");
  assert.throws(() => emittedFirstLoad(bundle, directory), /Uncounted JavaScript dependency/);
}));

test("missing disk files, missing startup modules, and absent HTML fail rather than measuring zero", () => temporary((directory, bundle) => {
  delete bundle['crypto.js'];
  assert.throws(() => emittedLaunchPaths(bundle, directory), /Uncounted startup module src\/lib\/e2e/);
  rmSync(join(directory, 'shared.mjs'));
  assert.throws(() => emittedFirstLoad(bundle, directory), { code: 'ENOENT' });
  rmSync(join(directory, 'index.html'));
  assert.throws(() => emittedFirstLoad(bundle, directory), { code: 'ENOENT' });
  assert.throws(() => firstLoadBudget(610,670).writeBundle.handler.call(context, {}, bundle), /output directory/);
}));

test("onboarding and FileWindow are reported without imposing either launch cap", () => temporary((directory, bundle) => {
  writeFileSync(join(directory, 'onboarding.js'), ' '.repeat(700 * 1024));
  writeFileSync(join(directory, 'setup.js'), ' '.repeat(700 * 1024));
  writeFileSync(join(directory, 'file-window.js'), ' '.repeat(700 * 1024));
  const paths = emittedLaunchPaths(bundle, directory);
  assert.ok(paths.onboarding.bytes > 670 * 1024 && paths.setup.bytes > 670 * 1024 && paths.fileWindowShell.bytes > 670 * 1024);
  assert.doesNotThrow(() => guard(directory, bundle));
}));

test("a late startup dynamic import cannot evade the paired gate via missing metadata", () => temporary((directory, bundle) => {
  bundle['crypto.js'].dynamicImports = [];
  writeFileSync(join(directory, 'crypto.js'), "import('./crypto-helper.js');");
  assert.ok(files(emittedLaunchPaths(bundle, directory).paired).includes('crypto-helper.js'));
  writeFileSync(join(directory, 'crypto.js'), "import('./missing-startup.js');");
  assert.throws(() => emittedLaunchPaths(bundle, directory), /Uncounted.*missing-startup/);
  writeFileSync(join(directory, 'crypto.js'), 'import(runtimeUrl);');
  assert.throws(() => emittedLaunchPaths(bundle, directory), /Uncounted JavaScript dependency/);
}));

for (const mode of ['static', 'paired']) test(`${mode}: startup dynamic edges on a static dependency are counted through the whole closure`, () => temporary((directory, bundle) => {
  // Not on the HTML entry or selected e2e root: the former blind spot.
  const owner = mode === 'static' ? 'shared.mjs' : 'crypto-helper.js';
  writeFileSync(join(directory, owner), "import(`./lazy-only.js`);");
  const measured = emittedLaunchPaths(bundle, directory)[mode];
  assert.ok(files(measured).includes('lazy-only.js'));
  writeFileSync(join(directory, 'lazy-only.js'), ' '.repeat((mode === 'static' ? 610 : 670) * 1024));
  assert.throws(() => guard(directory, bundle), new RegExp(`${mode}.*emitted bytes; budget`));
  writeFileSync(join(directory, owner), "import('./missing-transitive-startup.js');");
  assert.throws(() => emittedLaunchPaths(bundle, directory), /Uncounted.*missing-transitive/);
}));

test('deferred targets do not exempt another importing call site', async () => {
  const resolve = async () => ({ id: '/fixture/src/components/Settings.tsx' });
  const audited = 'const SettingsModal = lazy(() => import("./components/Settings").then((m) => ({ default: m.SettingsModal })));';
  assert.equal((await startupCallsites(audited, '/fixture/src/App.tsx', resolve)).length, 1);
  await assert.rejects(startupCallsites('import("./components/Settings");', '/fixture/src/App.tsx', resolve), /Unaudited.*call site/);
  await assert.rejects(startupCallsites(audited + '\nimport("./components/Settings");', '/fixture/src/App.tsx', resolve), /Unaudited.*call site/);
  await assert.rejects(startupCallsites(audited, '/fixture/src/other.tsx', resolve), /Unaudited.*call site/);
});
for (const mode of ['static', 'paired']) test(`${mode}: emitted unconditional import of deferred target counts`, () => temporary((directory, bundle) => {
  writeFileSync(join(directory, 'shared.mjs'), fixture.chunks[2].written + "import('./lazy.js');");
  bundle['shared.mjs'].dynamicImports = ['lazy.js'];
  assert.ok(files(emittedLaunchPaths(bundle, directory)[mode]).includes('lazy-only.js'));
  // Even a second late import in the audited owning chunk must not inherit its exemption.
  writeFileSync(join(directory, 'main.js'), fixture.chunks[0].written + "import('./lazy.js');import('./lazy.js');");
  assert.ok(files(emittedLaunchPaths(bundle, directory)[mode]).includes('lazy-only.js'));
}));

test('audited imports must be function-contained and loader/preload calls cannot run at module top level', async () => {
  const resolver = async () => ({ id: '/fixture/src/components/Settings.tsx' });
  const deferred = 'const SettingsModal = lazy(() => import("./components/Settings"));';
  for (const call of ['SettingsModal.preload();', 'void SettingsModal.preload();', '(() => SettingsModal.preload())();', 'void function(){SettingsModal.preload()}();']) {
    await assert.rejects(startupCallsites(deferred + call, '/fixture/src/App.tsx', resolver), /top level/);
  }
  for (const code of ['import("./components/Settings");', 'const SettingsModal = (() => import("./components/Settings"))();']) {
    await assert.rejects(startupCallsites(code, '/fixture/src/App.tsx', resolver), /top-level/);
  }
  const arrow = await startupSource('export const loadSettings = () => import("./components/Settings");', '/fixture/src/App.tsx', resolver);
  assert.doesNotThrow(() => validateStartupCalls(new Map([['/fixture/src/App.tsx', arrow]])));
  for (const code of ['import {loadSettings as boot} from "./App";void boot();', 'import * as app from "./App";(() => app.loadSettings())();']) {
    const other = await startupSource(code, '/fixture/src/other.ts', async () => ({ id: '/fixture/src/App.tsx' }));
    assert.throws(() => validateStartupCalls(new Map([['/fixture/src/App.tsx', arrow], ['/fixture/src/other.ts', other]])), /top level/);
  }
});

test('named function expressions bind their importing loader to both variable and internal name', async () => {
  const resolve = async () => ({ id: '/fixture/src/components/Settings.tsx' });
  const source = 'const loadSettings = function inner(){ return import("./components/Settings"); };';
  const edges = await startupCallsites(source, '/fixture/src/App.tsx', resolve);
  assert.deepEqual(edges[0].loaders, ['loadSettings', 'inner']);
  await assert.rejects(startupCallsites(source + 'void loadSettings();', '/fixture/src/App.tsx', resolve), /top level/);
});
test('optional lazy preload member and call are top-level loader calls', async () => {
  const resolve = async () => ({ id: '/fixture/src/components/Settings.tsx' });
  const source = 'const SettingsModal = lazy(() => import("./components/Settings"));';
  for (const call of ['SettingsModal?.preload();', 'SettingsModal.preload?.();', 'SettingsModal?.preload?.();']) {
    await assert.rejects(startupCallsites(source + call, '/fixture/src/App.tsx', resolve), /top level/);
  }
});
