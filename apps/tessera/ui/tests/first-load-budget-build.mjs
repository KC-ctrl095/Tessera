// Offline real-build proof: compare every launch-path file/byte set with emitted
// HTML + written Vite manifest, not just entry flags or the plugin's own graph.
import assert from "node:assert/strict";
import { readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parse } from "parse5";
import { build } from "vite";
import { emittedLaunchPaths, LAUNCH_MODULES } from "../build/first-load-budget.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
let observed;
let failure;
try {
  await build({
    root,
    configFile: resolve(root, "vite.config.ts"),
    build: { manifest: true },
    plugins: [{
      name: "first-load-budget-proof",
      configResolved(config) {
        const guard = config.plugins.find(plugin => plugin.name === "tessera-first-load-budget");
        assert.ok(guard?.writeBundle?.handler, "real final-write budget hook present");
        const original = guard.writeBundle.handler;
        guard.writeBundle.handler = async function(options, bundle) {
          // The guard restores audited metadata inside this hook on Rolldown.
          // Observe that exact graph even when the fixed byte cap rejects it.
          try { return await original.call(this, options, bundle); }
          finally { observed = emittedLaunchPaths(bundle, options.dir); }
        };
      },
    }],
  });
} catch (error) { failure = error; }
assert.ok(observed, `guard reached final emitted files: ${failure?.message ?? "no measurement"}`);
const directory = resolve(root, "dist");
const manifest = JSON.parse(readFileSync(resolve(directory, ".vite/manifest.json"), "utf8"));
const byFile = new Map(Object.entries(manifest).map(([key, entry]) => [entry.file, key]));
const audited = JSON.parse(readFileSync(resolve(directory, 'startup-edges.json'), 'utf8'));
const seeds = new Set();
const inline = [];
function htmlWalk(node) {
  const attrs = Object.fromEntries((node.attrs ?? []).map(a => [a.name, a.value]));
  if (node.tagName === 'script' && (!attrs.type || attrs.type === 'module')) {
    if (attrs.src) seeds.add(attrs.src.replace(/^\.\//, ''));
    else inline.push({ fileName: `index.html#script-${inline.length}`, bytes: Buffer.byteLength((node.childNodes ?? []).map(n => n.value ?? '').join('')) });
  }
  if (node.tagName === 'link' && (attrs.rel === 'modulepreload' || (attrs.rel === 'preload' && attrs.as === 'script'))) seeds.add(attrs.href.replace(/^\.\//, ''));
  node.childNodes?.forEach(htmlWalk);
}
htmlWalk(parse(readFileSync(resolve(directory, 'index.html'), 'utf8')));
function independent(mode) {
  const seen = new Set(), files = new Set();
  const visit = key => {
    if (seen.has(key)) return;
    seen.add(key);
    const entry = manifest[key];
    assert.ok(entry, `manifest dependency ${key} exists`);
    files.add(entry.file);
    for (const dep of entry.imports ?? []) visit(dep);
    for (const dep of entry.dynamicImports ?? []) {
      const sites = audited[entry.file]?.[manifest[dep]?.file];
      if (!sites?.length || sites.some(site => site.modes.includes(mode))) visit(dep);
    }
  };
  for (const seed of seeds) {
    if (byFile.has(seed)) visit(byFile.get(seed));
    else files.add(seed); // a direct HTML JS asset is not required to be a Rollup chunk
  }
  if (mode !== 'static') visit(LAUNCH_MODULES[mode]);
  return [...inline, ...[...files].map(fileName => ({ fileName, bytes: statSync(resolve(directory, fileName)).size }))].sort((a,b) => a.fileName.localeCompare(b.fileName));
}
const fixture = JSON.parse(readFileSync(resolve(root, 'tests/fixtures/first-load-budget.json'), 'utf8'));
const limits = { static: fixture.budgets.staticKiB, paired: fixture.budgets.pairedKiB };
assert.equal(limits.static, 610, 'native static cap is not loosened');
assert.equal(limits.paired * 1024, fixture.budgets.pairedMeasuredBytes + fixture.budgets.pairedHeadroomBytes);
const paths = {};
for (const mode of Object.keys(observed)) {
  const files = independent(mode);
  const bytes = files.reduce((n, f) => n + f.bytes, 0);
  assert.deepEqual(observed[mode].files, files, `${mode} files/bytes equal independent HTML/manifest closure`);
  assert.equal(observed[mode].bytes, bytes);
  paths[mode] = { bytes, kib: bytes / 1024, limitKiB: limits[mode] ?? null, files };
}
const rejected = Object.entries(limits).some(([mode, limit]) => paths[mode].bytes > limit * 1024);
assert.equal(!!failure, rejected, "real verdict follows both fixed launch budgets");
if (failure) assert.match(failure.message, /emitted bytes; budget/);
const report = { paths, buildRejected: !!failure, manifestMatched: true };
if (process.argv[2]) writeFileSync(resolve(process.argv[2]), JSON.stringify(report, null, 2) + "\n");
console.log(`BUDGET_PROOF_OK: static ${paths.static.bytes} bytes, paired ${paths.paired.bytes} bytes, HTML/manifest matched, build ${failure ? "rejected" : "accepted"}`);
