// Offline regression: execute the actual transport module with deterministic
// timers and fetch/socket doubles. No server, browser, or user settings.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import ts from '../node_modules/typescript/lib/typescript.js';

const source = fs.readFileSync(process.argv[2] ?? new URL('../src/lib/transport.ts', import.meta.url), 'utf8');
const js = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } }).outputText;
let now = 0, next = 1;
const timers = new Map();
let fetcher;
const exports = {};
vm.runInNewContext(js, {
  exports, require: (id) => {
    assert.equal(id, './util');
    return { lsGet: (_k, fallback) => fallback, lsSet() {}, uid: () => 'test' };
  },
  window: { addEventListener() {} }, document: { addEventListener() {} },
  location: { origin: 'http://fixture.invalid', host: 'fixture.invalid' },
  AbortController, Date, Math,
  setTimeout: (fn, ms) => { const id = next++; timers.set(id, { fn, at: now + ms }); return id; },
  clearTimeout: (id) => timers.delete(id),
  fetch: (...args) => fetcher(...args),
});
const settle = async () => { for (let i = 0; i < 20; i++) await Promise.resolve(); };
async function advance(ms) {
  now += ms;
  for (const [id, timer] of [...timers]) if (timer.at <= now) { timers.delete(id); timer.fn(); }
  await settle();
}
const server = { id: 'fixture', endpoints: ['http://fixture.invalid'], headers: {}, token: '', deviceName: 'test' };
let checks = 0;
const watchdog = setTimeout(() => { throw new Error('diagnostics blocked retry scheduling'); }, 3000);
for (const Base of [exports.WsTransport, exports.WebTransport]) {
  for (const mode of ['fetch never answers', 'body never answers']) {
    timers.clear();
    let signal;
    fetcher = async (_url, options) => {
      signal = options.signal;
      if (mode === 'fetch never answers') return new Promise(() => {}); // deliberately ignores abort
      return { status: 403, json: () => new Promise(() => {}) };
    };
    const t = new Base(true);
    let retries = 0;
    t.openSocket = async () => false;
    // Capture scheduling without actually attempting the next socket yet.
    t.scheduleRetry = function () { retries++; this.attempt++; this.setStatus({ state: 'offline', retryAt: now + 700, error: 'socket failed' }); };
    await t.loop(0, server);
    assert.equal(retries, 1, `${Base.name}: retry scheduled despite ${mode}`); checks++;
    assert.ok(signal && !signal.aborted); checks++;
    const diagnosis = t.diagnoseWsFailure(server.endpoints[0], {});
    await settle();
    if (Base === exports.WebTransport && mode === 'body never answers') {
      // The health diagnostic intentionally reads no body. It must finish
      // immediately, rather than needlessly waiting for/aborting JSON.
      assert.equal(await diagnosis, null, 'WebTransport: no body read'); checks++;
      assert.ok(!signal.aborted, 'WebTransport: already-completed diagnostic'); checks++;
    } else {
      await advance(2000);
      assert.equal(await diagnosis, null, `${Base.name}: diagnostic deadline`); checks++;
      assert.ok(signal.aborted, `${Base.name}: abort includes body reads`); checks++;
    }
    await t.disconnect();
  }
  timers.clear();
  let signal;
  let respond;
  fetcher = (_url, options) => {
    signal = options.signal;
    return new Promise((resolve) => { respond = resolve; });
  };
  const t = new Base(true);
  t.openSocket = async () => false;
  t.scheduleRetry = function () { this.attempt++; this.setStatus({ state: 'offline', error: 'old attempt' }); };
  await t.loop(0, server);
  await t.disconnect();
  assert.ok(signal.aborted, `${Base.name}: disconnect cancels diagnostic`); checks++;
  respond({ status: 403, json: async () => ({ message: 'stale diagnostic' }) });
  await settle();
  assert.equal(t.status.state, 'disconnected'); checks++;
  assert.equal(t.status.error, null, `${Base.name}: old diagnostic cannot update new generation`); checks++;

  // Exercise the REAL retry timer too: neither a hung fetch nor its body
  // may stop the next socket attempt, and that attempt aborts stale diagnostics.
  timers.clear();
  const signals = [];
  fetcher = async (_url, options) => { signals.push(options.signal); return new Promise(() => {}); };
  const retrying = new Base(true);
  let socketAttempts = 0;
  retrying.openSocket = async () => { socketAttempts++; return false; };
  await retrying.loop(0, server);
  assert.equal(retrying.status.state, 'offline'); checks++;
  assert.ok(retrying.status.retryAt); checks++;
  await advance(1000);
  assert.equal(socketAttempts, 2, `${Base.name}: real retry is not blocked`); checks++;
  assert.ok(signals[0].aborted, `${Base.name}: next attempt cancels stale diagnostic`); checks++;
  await retrying.disconnect();
}
clearTimeout(watchdog);
console.log(`transport diagnostics: ${checks} checks passed`);
