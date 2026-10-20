// Pure voice logic in Node, no audio devices, servers, downloads or paid calls.
import fs from 'node:fs';
import assert from 'node:assert/strict';
import ts from 'typescript';
const load = async path => {
  const source = fs.readFileSync(new URL(path, import.meta.url), 'utf8');
  const code = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText;
  return import(`data:text/javascript;base64,${Buffer.from(code).toString('base64')}`);
};
const { compose, Pauses, sentence, spoken, echo } = await load('../src/lib/voice/flow.ts');
const voice = await load('../src/lib/voice.ts');
const { wav, startRecording } = voice;
let count = 0;
const check = (name, fn) => { fn(); count++; console.log(`ok ${name}`); };
check('silence never sends', () => { const p = new Pauses(); assert.deepEqual(p.tick(0, 2000), { interrupt: false, finish: false }); });
check('short click cannot barge in', () => { const p = new Pauses(); p.tick(.5, 1000); assert.equal(p.tick(0, 1100).interrupt, false); });
check('sustained speech barges in', () => { const p = new Pauses(); p.tick(.5, 1000); assert.equal(p.tick(.5, 1500).interrupt, true); });
check('pause ends the utterance', () => { const p = new Pauses(); p.tick(.5, 1000); assert.equal(p.tick(0, 1999).finish, false); assert.equal(p.tick(0, 2000).finish, true); });
check('reset prevents repeat sends', () => { const p = new Pauses(); p.tick(.5, 1000); p.reset(); assert.equal(p.tick(0, 4000).finish, false); });
check('long turns are bounded', () => { const p = new Pauses(); p.tick(.5, 1000); assert.equal(p.tick(.5, 116000).finish, true); });
check('partial sentence waits', () => assert.equal(sentence('An unfinished answer', false), null));
check('stream extracts one sentence', () => assert.deepEqual(sentence('First answer. Another answer', false), { text: 'First answer. ', used: 14 }));
check('final tail is spoken', () => assert.deepEqual(sentence('unfinished answer', true), { text: 'unfinished answer', used: 17 }));
check('chunks bounded to engine limit', () => assert.ok(sentence('word '.repeat(1000), false).used <= 600));
check('markdown not read literally', () => assert.equal(spoken('**Read** [this](https://example.com) ```secret code```'), 'Read this'));
check('playback echo ignored', () => assert.equal(echo('this is the answer', 'Here, this is the answer.'), true));
check('new question not echo', () => assert.equal(echo('stop now I have a question', 'Here is the answer.'), false));
check('tiny echo match not overfiltered', () => assert.equal(echo('no', 'No problem'), false));
check('WAV mono PCM header and downsample', () => { const b = Buffer.from(wav(new Float32Array(48000).fill(.5), 48000), 'base64'); assert.equal(b.length, 32044); assert.equal(b.subarray(0, 4).toString(), 'RIFF'); assert.equal(b.readUInt32LE(24), 16000); assert.equal(b.readUInt16LE(22), 1); assert.equal(b.readUInt16LE(34), 16); assert.equal(b.readInt16LE(44), 16384); });
check('WAV clamps samples', () => { const b = Buffer.from(wav(new Float32Array([-2, 2]), 16000), 'base64'); assert.equal(b.readInt16LE(44), -32768); assert.equal(b.readInt16LE(46), 32767); });
const flowSource = fs.readFileSync(new URL('../src/lib/voice/flow.ts', import.meta.url), 'utf8');
const flowCode = ts.transpileModule(flowSource, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText;
const flowUrl = `data:text/javascript;base64,${Buffer.from(flowCode).toString('base64')}`;
let controllerSource = fs.readFileSync(new URL('../src/lib/voice/conversation.ts', import.meta.url), 'utf8');
controllerSource = controllerSource.replace('import { api, type SessionState } from "../store";', 'const api = globalThis.__voiceFixture.api;');
controllerSource = controllerSource.replace('from "./flow";', `from ${JSON.stringify(flowUrl)};`);
controllerSource = controllerSource.replace('import { listen, playback, support, type Input, type Playback } from "./engines";', 'const { listen, playback, support } = globalThis.__voiceFixture;');
const code = ts.transpileModule(controllerSource, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText;
const defer = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };
const settle = () => new Promise(r => setTimeout(r, 0));
async function scenario(name, body, expectedErrors = 0) {
  const fixture = { engine: 'auto', calls: [], views: [], drafts: [], sends: [], errors: [], canceled: 0, interrupted: 0, plays: [], words: null, local: { listen: true, speak: true } };
  const config = () => ({ engine: fixture.engine, enabled: false, openai_key: false, keep_audio: false, language: 'en', server: { state: 'ready', listen: true, speak: true, live: false } });
  fixture.api = {
    getVoice: async () => fixture.configWait ? fixture.configWait.promise : config(),
    installVoice: async () => { fixture.calls.push('install'); return config(); },
    transcribe: async (...args) => { fixture.calls.push(['transcribe', ...args]); fixture.atTranscribe = fixture.released; return fixture.transcribeWait ? fixture.transcribeWait.promise : { text: 'Recognized speech' }; },
    speak: async (...args) => { fixture.calls.push(['speak', ...args]); return fixture.speakWait ? fixture.speakWait.promise : { audio: 'fake', mime: 'audio/wav' }; },
  };
  fixture.api.cancelVoice = () => {};
  fixture.support = async () => fixture.local;
  fixture.listen = async (lang, device, words, failed, signal) => { if (fixture.listenError) throw new Error(fixture.listenError); fixture.inputSignal = signal; fixture.calls.push(['listen', device]); fixture.words = words; const input = { cancel: () => fixture.canceled++, finish: () => words('Final words', true) }; if (!device) input.recorder = { level: () => 0, reset() {}, snapshot: () => ({ audio: 'fake', mime: 'audio/wav', ms: 1000 }), async stop() { fixture.released = true; return this.snapshot(); }, cancel() {} }; return fixture.inputWait ? fixture.inputWait.promise : input; };
  fixture.playback = () => ({ async play(text) { fixture.plays.push(text); const wait = defer(); fixture.playWait = wait; return wait.promise; }, cancel() { fixture.playWait?.resolve(); } });
  globalThis.__voiceFixture = fixture;
  // A fresh module captures one isolated set of fake engines; all controller code is the real file.
  const { Conversation } = await import(`data:text/javascript;base64,${Buffer.from(code + `\n// fixture ${count}`).toString('base64')}`);
  const c = new Conversation({ view: v => fixture.views.push(v), draft: t => fixture.drafts.push(t), send: async t => fixture.sends.push(t), interrupt: () => fixture.interrupted++, error: e => fixture.errors.push(String(e)) });
  fixture.current = () => fixture.views.at(-1)?.phase;
  try { await body(c, fixture, config); await settle(); assert.equal(fixture.errors.length, expectedErrors, fixture.errors.join('; ')); count++; console.log(`ok ${name}`); } finally { c.stop(); }
}
const session = (text, ended = false, entries = [], status = 'running') => ({ info: { id: 'test', status }, entries, streaming: text === null ? null : { startedAt: 1, content: [{ type: 'text', text }], ended } });
await scenario('offline default uses no server speech', async (c, f) => { await c.start(true); assert.deepEqual(f.calls, [['listen', true]]); f.words('Hello', true); await settle(); assert.deepEqual(f.sends, ['Hello']); assert.equal(f.current(), 'waiting'); });
await scenario('dictation interim words never send', async (c, f) => { await c.start(false, undefined, 'Prefix ', ' suffix'); f.words('live words', false); assert.equal(f.drafts.at(-1), 'Prefix live words suffix'); assert.equal(f.sends.length, 0); c.finish(); assert.equal(f.current(), 'idle'); assert.equal(f.sends.length, 0); });
await scenario('hold to send uses final words', async (c, f) => { await c.start(false); c.finish(true); await settle(); assert.deepEqual(f.sends, ['Final words']); });
await scenario('start canceled before microphone never opens', async (c, f, cfg) => { f.configWait = defer(); const pending = c.start(true); c.stop(); f.configWait.resolve(cfg()); await pending; assert.equal(f.calls.length, 0); assert.equal(f.current(), 'idle'); });
await scenario('late microphone is canceled', async (c, f) => { f.inputWait = defer(); const pending = c.start(true); await settle(); c.stop(); assert.equal(f.inputSignal.aborted, true); f.inputWait.resolve({ cancel: () => f.canceled++ }); await pending; assert.equal(f.canceled, 1); assert.equal(f.current(), 'idle'); });
await scenario('existing streamed answer never replays on voice start', async (c, f) => { const existing = session('An earlier answer.', true, [], 'idle'); await c.start(true, existing); c.update(existing); assert.equal(f.plays.length, 0); f.words('New question', true); await settle(); c.update(existing); assert.equal(f.plays.length, 0); });
await scenario('only new streamed answer spoken once', async (c, f) => { const old = { seq: 1, kind: 'assistant', content: [{ type: 'text', text: 'Old history.' }] }; await c.start(true, session(null, false, [old], 'idle')); c.update(session('First answer. More')); await settle(); assert.deepEqual(f.plays, ['First answer.']); c.update(session('First answer. More', true, [old, { seq: 2, kind: 'assistant', content: [{ type: 'text', text: 'First answer. More' }] }], 'idle')); f.playWait.resolve(); await settle(); assert.deepEqual(f.plays, ['First answer.', 'More']); f.playWait.resolve(); await settle(); assert.equal(f.current(), 'listening'); });
await scenario('barge-in cancels audio and suppresses old tail', async (c, f) => { await c.start(true); c.update(session('An answer.')); await settle(); f.words('Wait I have another question', false); assert.equal(f.current(), 'listening'); assert.equal(f.interrupted, 1); c.update(session('An answer. Stale tail.', true)); await settle(); assert.deepEqual(f.plays, ['An answer.']); });
await scenario('late server TTS is never played after End', async (c, f) => { f.local = { listen: true, speak: false }; f.speakWait = defer(); await c.start(true); c.update(session('An answer.')); await settle(); c.stop(); f.speakWait.resolve({ audio: 'fake', mime: 'audio/wav' }); await settle(); assert.equal(f.plays.length, 0); });
await scenario('late transcription never edits another chat', async (c, f) => { f.local = { listen: false, speak: false }; f.transcribeWait = defer(); await c.start(false); c.finish(); c.stop(); f.transcribeWait.resolve({ text: 'late words' }); await settle(); assert.equal(f.drafts.length, 0); assert.equal(f.sends.length, 0); });
await scenario('Automatic records privately when device unsupported', async (c, f) => { f.local = { listen: false, speak: false }; await c.start(false); c.finish(); await settle(); assert.equal(f.calls[0][1], false); assert.equal(f.calls[1][0], 'transcribe'); assert.equal(f.calls[1][3], 'server'); });
await scenario('final entry adds tail without repeating streamed sentences', async (c, f) => { await c.start(true); c.update(session('First answer. Partial')); await settle(); const entry = { seq: 1, kind: 'assistant', content: [{ type: 'text', text: 'First answer. Partial tail.' }] }; c.update(session(null, false, [entry], 'idle')); f.playWait.resolve(); await settle(); assert.deepEqual(f.plays, ['First answer.', 'Partial tail.']); });
await scenario('Off never opens a microphone', async (c, f) => { f.engine = 'off'; await c.start(true); assert.equal(f.current(), 'idle'); assert.equal(f.calls.length, 0); assert.ok(f.errors[0].includes('Voice is off')); }, 1);
await scenario('device-only does not silently fall back to a server', async (c, f) => { f.engine = 'device'; f.local = { listen: false, speak: false }; await c.start(true); assert.equal(f.current(), 'idle'); assert.equal(f.calls.length, 0); assert.ok(f.errors[0].includes('no private speech engine')); }, 1);
await scenario('missing OpenAI key does not fall back or start capture', async (c, f) => { f.engine = 'openai'; await c.start(true); assert.equal(f.current(), 'idle'); assert.equal(f.calls.length, 0); assert.ok(f.errors[0].includes('API key')); }, 1);
await scenario('blocked microphone is an actionable error not stuck listening', async (c, f) => { f.listenError = 'Microphone access was blocked'; await c.start(true); assert.equal(f.current(), 'idle'); assert.ok(f.errors[0].includes('Microphone access was blocked')); }, 1);
await scenario('dictation releases capture before pending transcription', async (c, f) => { f.local = { listen: false, speak: false }; f.transcribeWait = defer(); await c.start(false); c.finish(); await settle(); assert.equal(f.current(), 'sending'); assert.equal(f.atTranscribe, true); assert.equal(f.released, true); f.transcribeWait.resolve({ text: 'words' }); });
for (const [prefix, suffix] of [['abc', 'def'], ['abc ', ' def'], ['', 'suffix'], ['prefix', ''], ['before\n', '\nafter']]) {
  check(`shared composition ${JSON.stringify([prefix,suffix])}`, () => assert.equal(compose(prefix, 'words', suffix), `${prefix}${prefix && !/\s$/.test(prefix) ? ' ' : ''}words${suffix && !/^\s/.test(suffix) ? ' ' : ''}${suffix}`));
  await scenario(`native display/send identical ${JSON.stringify([prefix,suffix])}`, async (c, f) => { await c.start(false, undefined, prefix, suffix); c.finish(true); await settle(); assert.equal(f.sends[0], f.drafts.at(-1)); assert.equal(f.sends[0], compose(prefix, 'Final words', suffix)); });
  await scenario(`server display/send identical ${JSON.stringify([prefix,suffix])}`, async (c, f) => { f.local = { listen: false, speak: false }; await c.start(false, undefined, prefix, suffix); c.finish(true); await settle(); assert.equal(f.sends[0], f.drafts.at(-1)); assert.equal(f.sends[0], compose(prefix, 'Recognized speech', suffix)); });
}
await scenario('native no-final fallback preserves insertion', async (c, f) => { await c.start(false, undefined, 'before ', ' after'); f.words('heard words', false); c.input.finish = () => {}; c.finish(true); await new Promise(r => setTimeout(r, 1250)); assert.equal(f.sends[0], f.drafts.at(-1)); assert.equal(f.sends[0], 'before heard words after'); });
await scenario('conversation display/send shares insertion only on the first turn', async (c, f) => { await c.start(true, undefined, 'before', 'after'); f.words('first words', true); await settle(); assert.equal(f.sends[0], f.drafts.at(-1)); assert.equal(f.sends[0], 'before first words after'); f.words('second words', true); await settle(); assert.equal(f.sends[1], f.drafts.at(-1)); assert.equal(f.sends[1], 'second words'); });
await scenario('Stop aborts final server transcription signal', async (c, f) => { f.local = { listen: false, speak: false }; f.transcribeWait = defer(); await c.start(false); c.finish(); await settle(); const signal = f.calls.find(x => x[0] === 'transcribe').at(-1); assert.equal(signal.aborted, false); c.stop(); assert.equal(signal.aborted, true); f.transcribeWait.resolve({ text: 'late' }); });
await scenario('barge-in aborts server TTS signal', async (c, f) => { f.local = { listen: true, speak: false }; f.speakWait = defer(); await c.start(true); c.update(session('An answer.')); await settle(); const signal = f.calls.find(x => x[0] === 'speak').at(-1); assert.equal(signal.aborted, false); f.words('A new question', false); assert.equal(signal.aborted, true); f.speakWait.resolve({ audio: 'fake', mime: 'audio/wav' }); });

async function asyncCheck(name, test) { await test(); count++; console.log(`ok ${name}`); }
function captureFixture(permission, resume = Promise.resolve()) {
  let stopped = 0, closed = 0;
  const stream = { getTracks: () => [{ stop: () => stopped++ }] };
  const node = () => ({ connect() {}, disconnect() {}, gain: { value: 1 } });
  class Context { sampleRate = 16000; destination = {}; createMediaStreamSource() { return node(); } createScriptProcessor() { return node(); } createGain() { return node(); } resume() { return resume; } async close() { closed++; } }
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { mediaDevices: { getUserMedia: () => permission ?? Promise.resolve(stream) }, userAgent: 'Android', language: 'en-US' } });
  globalThis.window = { AudioContext: Context, isSecureContext: true };
  return { stream, stopped: () => stopped, closed: () => closed };
}
await asyncCheck('permission grant after Stop immediately stops late tracks', async () => {
  const permission = defer(), f = captureFixture(permission.promise), abort = new AbortController();
  const pending = startRecording(abort.signal); const rejected = assert.rejects(pending, { name: 'AbortError' });
  abort.abort(); await rejected; permission.resolve(f.stream); await settle(); assert.equal(f.stopped(), 1);
});
await asyncCheck('Stop releases stream and AudioContext while resume is still pending', async () => {
  const resume = defer(), f = captureFixture(undefined, resume.promise), abort = new AbortController();
  const pending = startRecording(abort.signal); const rejected = assert.rejects(pending, { name: 'AbortError' });
  await settle(); abort.abort(); await rejected; assert.equal(f.stopped(), 1); assert.equal(f.closed(), 1); resume.resolve();
});
const voiceCode = ts.transpileModule(fs.readFileSync(new URL('../src/lib/voice.ts', import.meta.url), 'utf8'), { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText;
const voiceUrl = `data:text/javascript;base64,${Buffer.from(voiceCode).toString('base64')}`;
let engines = fs.readFileSync(new URL('../src/lib/voice/engines.ts', import.meta.url), 'utf8').replace('import { isTauri } from "../util";', 'const isTauri = true;').replace('from "../voice";', `from ${JSON.stringify(voiceUrl)};`).replaceAll('await import("@tauri-apps/api/core")', 'await globalThis.__nativeModule()');
const engineCode = ts.transpileModule(engines, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText;
const nativeEngines = await import(`data:text/javascript;base64,${Buffer.from(engineCode).toString('base64')}`);
await asyncCheck('Stop during native import never invokes microphone start', async () => {
  captureFixture(); const loading = defer(), calls = [], abort = new AbortController(); globalThis.__nativeModule = () => loading.promise;
  const pending = nativeEngines.listen('en', true, () => {}, () => {}, abort.signal); const rejected = assert.rejects(pending, { name: 'AbortError' });
  abort.abort(); loading.resolve({ Channel: class {}, invoke: async command => calls.push(command) }); await rejected;
  assert.deepEqual(calls, []);
});
await asyncCheck('old caller generation cannot cancel a replacement native microphone', async () => {
  captureFixture(); const calls = [], operations = []; globalThis.__nativeModule = async () => ({ Channel: class {}, invoke: async (command, args) => { calls.push(command); operations.push(args?.operation); } });
  const first = new AbortController(), second = new AbortController();
  await nativeEngines.listen('en', true, () => {}, () => {}, first.signal); const current = await nativeEngines.listen('en', true, () => {}, () => {}, second.signal);
  first.abort(); assert.deepEqual(calls, ['voice_listen', 'voice_listen']); current.cancel(); assert.equal(calls.at(-1), 'voice_cancel');
  assert.notEqual(operations[0], operations[1]); assert.equal(operations[1], operations[2]);
});
await asyncCheck('Stop during native speech import never invokes speech', async () => {
  captureFixture(); const loading = defer(), calls = []; globalThis.__nativeModule = () => loading.promise;
  const output = nativeEngines.playback('en', true), pending = output.play('words'); const rejected = assert.rejects(pending, { name: 'AbortError' });
  output.cancel(); loading.resolve({ invoke: async command => calls.push(command) }); await rejected; assert.deepEqual(calls, []);
});
console.log(`ALL OK: ${count} voice unit tests`);
