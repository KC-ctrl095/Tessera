import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import { Page } from './e2e/cdp.mjs';

const page = (context) => ({ eval: async (expression) => vm.runInNewContext(expression, context) });
test('waitFor polls an asynchronously false predicate until it becomes true', async () => {
  let calls = 0;
  await Page.prototype.waitFor.call(page({ ready: () => ++calls >= 3 }), 'Promise.resolve(ready())', 1000);
  assert.equal(calls, 3);
});
test('waitFor does not mistake a Promise for a successful predicate', async () => {
  await assert.rejects(Page.prototype.waitFor.call(page({}), 'Promise.resolve(false)', 100), /waitFor timeout/);
});
test('waitFor retries an asynchronous rejection', async () => {
  let calls = 0;
  await Page.prototype.waitFor.call(page({ ready: () => ++calls === 1 ? Promise.reject(new Error('not ready')) : Promise.resolve(true) }), 'ready()', 1000);
  assert.equal(calls, 2);
});
test('waitFor still accepts a synchronous predicate', async () => {
  await Page.prototype.waitFor.call(page({}), 'true', 1000);
});
