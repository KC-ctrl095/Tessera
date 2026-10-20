// Real Vite pipeline negative: relocating one audited dynamic import to module
// scope keeps ownership and import count unchanged, but must reject the build.
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { build } from 'vite';
import { parse } from '@babel/parser';
import { createHash } from 'node:crypto';
const imports = code => {
  const found = [];
  const visit = node => {
    if (!node || typeof node !== 'object') return;
    if (node.type === 'ImportExpression' || (node.type === 'CallExpression' && node.callee.type === 'Import')) {
      const source = node.source ?? node.arguments[0];
      const value = source?.type === 'StringLiteral' ? source.value : source?.type === 'TemplateLiteral' && !source.expressions.length ? source.quasis[0].value.cooked : null;
      found.push({ start: node.start, end: node.end, value });
    }
    for (const value of Object.values(node)) {
      if (Array.isArray(value)) value.forEach(visit);
      else if (value && typeof value === 'object') visit(value);
    }
  };
  visit(parse(code, { sourceType: 'module' }));
  return found;
};
const hash = code => createHash('sha256').update(code).digest('hex');
const root = fileURLToPath(new URL('..', import.meta.url));
const outDir = await mkdtemp(path.join(os.tmpdir(), 'tessera-eager-proof-'));
let moved;
try {
  await assert.rejects(build({ root, configFile: path.join(root, 'vite.config.ts'), build: { outDir }, plugins: [{
    name: 'negative-late-eager-import', enforce: 'post',
    generateBundle: { order: 'post', sequential: true, handler(_options, bundle) {
      const asset = bundle['startup-edges.json'];
      assert.equal(asset?.type, 'asset', 'audit emitted its native persisted proof asset');
      const proof = JSON.parse(typeof asset.source === 'string' ? asset.source : Buffer.from(asset.source).toString('utf8'));
      for (const chunk of Object.values(bundle)) {
        if (chunk.type !== 'chunk' || !proof[chunk.fileName]) continue;
        for (const match of imports(chunk.code)) {
          if (match.value === null) continue;
          const target = path.posix.normalize(path.posix.join(path.posix.dirname(chunk.fileName), match.value));
          const sites = proof[chunk.fileName][target];
          if (!sites?.length || !sites.every(site => !site.modes.includes('static'))) continue;
          const before = chunk.code, owners = [...chunk.moduleIds];
          chunk.code = `${before.slice(match.start, match.end)};${before.slice(0, match.start)}Promise.resolve({})${before.slice(match.end)}`;
          assert.equal(imports(before).length, imports(chunk.code).length);
          assert.notEqual(hash(before), hash(chunk.code));
          assert.deepEqual(chunk.moduleIds, owners);
          moved = { fileName: chunk.fileName, target, imports: imports(before).length, beforeHash: hash(before), afterHash: hash(chunk.code) };
          return;
        }
      }
      throw new Error('No audited import found for negative build');
    } },
  }] }), /Changed startup audit code/);
  assert.ok(moved, 'negative plugin relocated a real excluded edge');
  console.log('EAGER_IMPORT_REJECTED', JSON.stringify(moved));
} finally { await rm(outDir, { recursive: true, force: true }); }
