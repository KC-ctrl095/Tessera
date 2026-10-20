// Real WAV Recorder, including late permission and cancelled/empty payloads.
// The T0 lifecycle contracts remain after T8 replaces MediaRecorder with PCM.
import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import ts from 'typescript';
const scratch = mkdtempSync(new URL('../.voice-intent-test-', import.meta.url));
const source = readFileSync(new URL('../src/lib/voice.ts', import.meta.url), 'utf8');
writeFileSync(join(scratch, 'voice.mjs'), ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } }).outputText);
const { startRecording: recording, voiceError } = await import(join(scratch, 'voice.mjs'));
let stopped, contexts, processor, closed;
const globals = new Map(['window', 'navigator', 'AudioContext', 'MediaRecorder'].map(name => [name, Object.getOwnPropertyDescriptor(globalThis, name)]));
const stream = () => ({ getTracks: () => [{ stop() { stopped++; } }] });
function environment(getUserMedia = async () => stream()) {
  stopped = contexts = closed = 0; processor = undefined;
  globalThis.window = {};
  Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { mediaDevices: { getUserMedia } } });
  globalThis.AudioContext = class {
    constructor() { contexts++; this.sampleRate = 16000; this.destination = {}; }
    async resume() {} async close() { closed++; }
    createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
    createScriptProcessor() { return processor = { connect() {}, disconnect() {}, onaudioprocess: null }; }
    createGain() { return { gain: { value: 1 }, connect() {}, disconnect() {} }; }
  };
  window.AudioContext = globalThis.AudioContext;
}
try {
  await test('Cancel during microphone permission discards a later grant before capture starts', async () => {
    let grant; environment(() => new Promise(resolve => { grant = resolve; }));
    const abort = new AbortController(), pending = recording(abort.signal);
    abort.abort(); grant(stream());
    await assert.rejects(pending, { name: 'AbortError' });
    assert.equal(stopped, 1); assert.equal(contexts, 0);
  });
  await test('Cancel immediately after capture starts stops every track once and disconnects PCM', async () => {
    environment(); const recorder = await recording(); recorder.cancel(); recorder.cancel();
    assert.equal(stopped, 1); assert.equal(closed, 1); assert.equal(processor.onaudioprocess, null);
    await assert.rejects(recorder.stop(), /already finished/);
  });
  await test('empty capture refuses transcription instead of offering a header-only WAV', async () => {
    environment(); const recorder = await recording();
    await assert.rejects(recorder.stop(), /No speech was recorded/);
    assert.equal(stopped, 1);
  });
  await test('unsupported MediaRecorder choices cannot prevent private WAV capture', async () => {
    environment(); globalThis.MediaRecorder = class { constructor() { throw Error('unused codec'); } };
    const recorder = await recording();
    processor.onaudioprocess({ inputBuffer: { getChannelData: () => new Float32Array([.2, -.2, .1, -.1]) } });
    const result = await recorder.stop(); assert.equal(result.mime, 'audio/wav');
    assert.equal(Buffer.from(result.audio, 'base64').subarray(0, 4).toString(), 'RIFF'); assert.equal(stopped, 1);
  });
  await test('platform-default recorder availability is not required for WAV capture', async () => {
    environment(); delete globalThis.MediaRecorder;
    const recorder = await recording(); recorder.cancel(); assert.equal(stopped, 1); assert.equal(contexts, 1);
  });
  await test('named permission failures have a visible plain-language meaning', () => {
    assert.match(voiceError(new DOMException('blocked', 'NotAllowedError')), /permission was denied/);
    assert.match(voiceError(new DOMException('gone', 'NotFoundError')), /No microphone/);
    assert.match(voiceError(new DOMException('busy', 'NotReadableError')), /in use/);
    assert.match(voiceError(Error('localhost only')), /localhost only/);
  });
  await test('capture entry retains both cancellation checks around the permission prompt', () => {
    assert.match(source, /if \(signal\?\.aborted\)/);
    assert.match(source, /getUserMedia[\s\S]*signal\?\.aborted[\s\S]*getTracks/);
    const host = readFileSync(new URL('../src/lib/voice-host.ts', import.meta.url), 'utf8');
    assert.match(host, /voicePart\.load\(\)[\s\S]*disposed \|\| gen !== generation[\s\S]*new part\.Conversation/);
  });
} finally {
  for (const [name, descriptor] of globals) {
    if (descriptor) Object.defineProperty(globalThis, name, descriptor);
    else delete globalThis[name];
  }
  rmSync(scratch, { recursive: true, force: true });
}
