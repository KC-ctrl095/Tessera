// Offline tests of the real onDemand state machine/probe, transpiled without changing it.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const scratch = mkdtempSync(new URL('../.on-demand-test-', import.meta.url));
const running = './assets/index-old.js';
globalThis.document = { querySelector: () => ({ getAttribute: () => running }) };
globalThis.location = { href: 'http://tessera.test/', origin: 'http://tessera.test' };
globalThis.DOMParser = class { parseFromString(html) { return { querySelector: () => { const src = html.match(/src="([^"]+)"/)?.[1]; return src ? { getAttribute: () => src } : null; } }; } };
for (const name of ['retryImport', 'onDemand']) {
  const source = readFileSync(new URL(`../src/lib/${name}.ts`, import.meta.url), 'utf8').replace('"./retryImport"', '"./retryImport.mjs"');
  writeFileSync(join(scratch, name + '.mjs'), ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText);
}
const { onDemand, updatedBuild } = await import(join(scratch, 'onDemand.mjs'));
const fresh = src => async () => new Response(`<script type="module" src="${src}"></script>`);
try {
  await test('probe distinguishes updated, unchanged, and unknown', async () => {
    globalThis.fetch = fresh(running); assert.equal(await updatedBuild(), 'unchanged');
    globalThis.fetch = fresh('./assets/index-new.js'); assert.equal(await updatedBuild(), 'updated');
    globalThis.fetch = fresh(''); assert.equal(await updatedBuild(), 'unknown');
    globalThis.fetch = async () => { throw Error('offline'); }; assert.equal(await updatedBuild(), 'unknown');
  });
  await test('mismatch followed by failed probe never erases the updated/Reload latch', async () => {
    let imports = 0;
    const part = onDemand(async () => { imports++; throw Error('missing old hash'); });
    globalThis.fetch = fresh('./assets/index-new.js'); await part.load();
    assert.equal(part.error().build, 'updated');
    globalThis.fetch = async () => { throw Error('offline after mismatch'); }; await part.retry();
    assert.equal(part.error().build, 'updated'); assert.equal(imports, 1, 'no futile old import');
  });
  await test('parts metadata mismatch is typed stale evidence even when HTML probe is unknown', async () => {
    globalThis.fetch = async () => { throw Error('offline'); };
    const part = onDemand(async () => { throw Object.assign(Error('updated parts'), { build: 'updated' }); });
    await part.load(); await part.retry(); assert.equal(part.error().build, 'updated');
  });
  await test('unknown is transient and may recover locally', async () => {
    globalThis.fetch = async () => { throw Error('offline'); };
    let attempt = 0;
    const part = onDemand(async () => { if (!attempt++) throw Error('transient'); return { ok: true }; });
    await part.load(); assert.equal(part.error().build, 'unknown');
    assert.deepEqual(await part.retry(), { ok: true }); assert.equal(part.error(), null);
  });
  await test('hanging HTML/body probe is bounded by an abort timeout', async () => {
    globalThis.fetch = (_url, { signal }) => new Promise((_resolve, reject) => signal.addEventListener('abort', () => reject(signal.reason)));
    const start = Date.now();
    // Keep Node alive: browser fetch itself holds the event loop until cancellation.
    const keepAlive = setInterval(() => {}, 100);
    try { assert.equal(await updatedBuild(), 'unknown'); } finally { clearInterval(keepAlive); }
    assert.ok(Date.now() - start >= 2400 && Date.now() - start < 5000);
  });
} finally { rmSync(scratch, { recursive: true, force: true }); }
