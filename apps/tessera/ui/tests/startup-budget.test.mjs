import assert from 'node:assert/strict';
import test from 'node:test';
import { startupBudget } from '../build/startup-budget.mjs';

const edge = { target: '/src/optional.ts', modes: [], loaders: ['optional'] };
function fixture() {
  const original = { type: 'chunk', fileName: 'entry.js', facadeModuleId: '/src/main.ts', moduleIds: ['/src/main.ts'], code: 'function optional(){return import("./optional.js");}' };
  let measured = false;
  const plugins = startupBudget({ generateBundle(_options, bundle) { bundle['entry.js'].tesseraStartupEdges = { 'optional.js': [edge] }; } }, {
    writeBundle: { order: 'post', sequential: true, handler(_options, bundle) {
      measured = true;
      assert.deepEqual(bundle['entry.js'].tesseraStartupEdges, { 'optional.js': [edge] });
    } },
  });
  return { plugins, original, measured: () => measured };
}

test('exact audited proof survives a fresh Rolldown write-hook chunk object', async () => {
  const { plugins, original, measured } = fixture();
  await plugins.audit.generateBundle.handler({}, { 'entry.js': original });
  const fresh = { ...original }; delete fresh.tesseraStartupEdges;
  plugins.budget.writeBundle.handler({}, { 'entry.js': fresh });
  assert.equal(measured(), true);
  assert.equal(plugins.audit.generateBundle.order, 'post');
  assert.equal(plugins.audit.generateBundle.sequential, true);
  assert.equal(plugins.budget.writeBundle.order, 'post');
  assert.equal(plugins.budget.writeBundle.sequential, true);
  assert.notEqual(fresh.tesseraStartupEdges['optional.js'][0], edge);
});

test('no generation proof cannot authorize a deferred edge', () => {
  const { plugins, original, measured } = fixture();
  assert.throws(() => plugins.budget.writeBundle.handler({}, { 'entry.js': original }), /Missing startup audit proof/);
  assert.equal(measured(), false);
});

test('changed or new chunk owners fail closed', async () => {
  for (const change of [{ facadeModuleId: '/src/other.ts' }, { moduleIds: ['/src/other.ts'] }, { fileName: 'other.js' }]) {
    const { plugins, original, measured } = fixture();
    await plugins.audit.generateBundle.handler({}, { 'entry.js': original });
    assert.throws(() => plugins.budget.writeBundle.handler({}, { 'entry.js': { ...original, ...change } }), /Changed startup audit owner/);
    assert.equal(measured(), false);
  }
});

test('later hook fields cannot overwrite the privately retained proof', async () => {
  const { plugins, original, measured } = fixture();
  await plugins.audit.generateBundle.handler({}, { 'entry.js': original });
  original.tesseraStartupEdges = { 'unreviewed.js': [{ modes: [] }] };
  plugins.budget.writeBundle.handler({}, { 'entry.js': original });
  assert.equal(measured(), true);
  assert.equal(original.tesseraStartupEdges['unreviewed.js'], undefined);
});

test('later eager relocation with unchanged owners and import count fails the code hash', async () => {
  const { plugins, original, measured } = fixture();
  await plugins.audit.generateBundle.handler({}, { 'entry.js': original });
  const changed = { ...original, code: 'import("./optional.js");function optional(){}' };
  assert.equal(changed.code.match(/import\(/g).length, original.code.match(/import\(/g).length);
  assert.deepEqual(changed.moduleIds, original.moduleIds);
  assert.throws(() => plugins.budget.writeBundle.handler({}, { 'entry.js': changed }), /Changed startup audit code/);
  assert.equal(measured(), false);
});

test('a normal generate mutation before post-capture cannot make the excluded import eager', async () => {
  const { plugins, original, measured } = fixture();
  original.code = 'import("./optional.js");function optional(){}';
  await assert.rejects(plugins.audit.generateBundle.handler({}, { 'entry.js': original }), /Eager startup import in final code/);
  assert.equal(measured(), false);
});

for (const code of [
  '(()=>import("./optional.js"))();function optional(){}',
  '(function(){return import("./optional.js")}).call(null);',
  '(function(){return import("./optional.js")})["call"](null);',
  '(function(){return import("./optional.js")})?.call(null);',
  'new (class { constructor(){import("./optional.js");} })();',
  'new (class { field=import("./optional.js"); })();',
  'import("./optional.js?eager");function optional(){}',
  'import("./optional.js#eager");function optional(){}',
  'import("./%6fptional.js");function optional(){}',
]) test(`pre-capture immediate/canonicalized eager edge fails: ${code}`, async () => {
  const { plugins, original, measured } = fixture();
  assert.equal(code.match(/import\(/g).length, original.code.match(/import\(/g).length);
  original.code = code;
  await assert.rejects(plugins.audit.generateBundle.handler({}, { 'entry.js': original }), /Eager startup import in final code/);
  assert.equal(measured(), false);
});

test('an immediately constructed class method stays deferred until it is called', async () => {
  const { plugins, original, measured } = fixture();
  original.code = 'new (class { optional(){return import("./optional.js");} })();';
  await plugins.audit.generateBundle.handler({}, { 'entry.js': original });
  plugins.budget.writeBundle.handler({}, { 'entry.js': original });
  assert.equal(measured(), true);
});
