import assert from 'node:assert/strict';
import { test } from 'node:test';
import { webcrypto } from 'node:crypto';
import vm from 'node:vm';
import { photoBytes } from './fixtures/photo-bytes.mjs';

function page(bytes, name = 'image.jpg', alter = value => value) {
  const context = vm.createContext({ window: { __photos: { [name]: new Uint8Array(bytes) } }, crypto: webcrypto,
    btoa: value => Buffer.from(value, 'binary').toString('base64') });
  const replies = [];
  return { replies, async eval(source) { const value = await vm.runInContext(source, context); replies.push(value); return alter(value, replies.length); } };
}

test('camera-sized bytes cross bounded CDP replies byte-for-byte, including tail and quoted filename', async () => {
  const bytes = Buffer.alloc(5 * 1024 * 1024 + 17);
  for (let i = 0; i < bytes.length; i++) bytes[i] = (i * 31 + i % 17) % 256;
  const name = 'image "quote" \\ sideways.jpg', p = page(bytes, name);
  const result = await photoBytes(p, name, bytes.length);
  assert.deepEqual(result, bytes);
  assert.equal(p.replies.length, 22); // 21 chunks, then a separate original-byte digest.
  assert.ok(p.replies.every(value => value.length < 400_000));
  assert.ok(Buffer.from(bytes).toString('base64').length > 4 * 1024 * 1024);
});

test('a shortened chunk fails before the next chunk', async () => {
  const p = page(Buffer.alloc(300_000), 'image.jpg', value => value.slice(4));
  await assert.rejects(photoBytes(p, 'image.jpg', 300_000), /Incomplete fixture photo chunk/);
  assert.equal(p.replies.length, 1);
});

test('same-length byte corruption fails the independent page digest', async () => {
  const p = page(Buffer.from([1, 2, 3]), 'image.jpg', (value, call) => call === 1 ? Buffer.from([1, 2, 4]).toString('base64') : value);
  await assert.rejects(photoBytes(p, 'image.jpg', 3), /hash mismatch/);
});

test('a source length change fails without returning a partial file', async () => {
  const p = page(Buffer.from([1, 2, 3]));
  await assert.rejects(photoBytes(p, 'image.jpg', 4), /changed during transfer/);
});

test('invalid or unbounded fixture sizes are rejected before any browser call', async () => {
  const p = page(Buffer.from([1]));
  for (const length of [0, -1, 1.5, NaN, Infinity, 64 * 1024 * 1024 + 1]) {
    await assert.rejects(photoBytes(p, 'image.jpg', length), /Invalid fixture photo length/);
  }
  assert.equal(p.replies.length, 0);
});
