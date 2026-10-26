import { SHOTS, check, device, finish, mock, run, signIn, sleep } from './cdp.mjs';

const fakeDevice = `(() => {
  window.__voice = { starts: 0, cancels: 0, utterances: [], stopped: 0 };
  class Recognition {
    static async available() { return 'available'; }
    start() { if (!this.processLocally) throw Error('Cloud recognition was started'); __voice.current = this; __voice.starts++; }
    stop() { if (this.onresult && __voice.words) this.onresult({ results: [{ 0: { transcript: __voice.words }, isFinal: true }] }); this.onend?.(); }
    abort() { __voice.cancels++; }
  }
  window.SpeechRecognition = Recognition;
  window.SpeechSynthesisUtterance = class { constructor(text) { this.text = text; } };
  Object.defineProperty(window, 'speechSynthesis', { configurable: true, value: {
    getVoices: () => [{ localService: true, lang: 'en-US', name: 'Offline test voice' }],
    speak: u => { __voice.utterances.push(u.text); __voice.playing = u; },
    cancel: () => { __voice.stopped++; __voice.playing = null; }, addEventListener() {}, removeEventListener() {}
  } });
  window.__words = (text, final = false) => { __voice.words = text; __voice.current.onresult?.({ results: [{ 0: { transcript: text }, isFinal: final }] }); };
})()`;
async function until(p, expression, note) {
  for (let i = 0; i < 100; i++) { if (await p.eval(expression)) return; await sleep(100); }
  console.log('voice timeout', note, await p.eval(`({ phase: document.querySelector('.voice-status')?.dataset.phase, note: document.querySelector('.voice-status')?.textContent, errors: [...document.querySelectorAll('.toast,.part-error,.form-error')].map(e => e.textContent), preview: window.__preview, engine: document.querySelector('[aria-label="Where speech is handled"]')?.value, buttons: [...document.querySelectorAll('#set-voice button')].map(e => [e.textContent, e.disabled]) })`), p.errors);
  throw Error(`Timed out: ${note}`);
}
const phase = (value) => `document.querySelector('.voice-status')?.dataset.phase === '${value}'`;
const hitArea = (p, selector) => p.eval(`(() => { const e = document.querySelector(${JSON.stringify(selector)}); if (!e) return false; const r = e.getBoundingClientRect(); return r.width >= 43.5 && r.height >= 43.5 && [[.1,.1],[.9,.9],[.5,.5]].every(([x,y]) => e.contains(document.elementFromPoint(r.x+r.width*x,r.y+r.height*y))); })()`);
const apiCall = (name, params = '{}') => `__mod('lib/store.ts').then(m => m.api.${name}(${params}))`;
run(async () => {
  for (const [label, w, h, mobile] of [['desktop', 1440, 900, false], ['phone', 344, 812, true], ['fold', 841, 701, true]]) {
    for (const theme of ['dark', 'light']) {
      await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
      const p = await device(); await p.viewport(w, h, mobile, mobile ? 2 : 1); await signIn(p, `Voice ${label}`); await p.theme(theme);
      await p.eval(fakeDevice);
      await until(p, `!!document.querySelector('[aria-label="Voice input"]')`, 'mic');
      check(await p.eval(`!!document.querySelector('[aria-label="Start voice conversation"]')`), `${label}/${theme}: both voice buttons default on`);
      check(await hitArea(p, '[aria-label="Voice input"]') && await hitArea(p, '[aria-label="Start voice conversation"]'), `${label}/${theme}: mic and conversation 44px rects and hit areas`);
      if (!mobile) {
        await p.eval(`document.documentElement.dataset.density = 'compact'`);
        check(await hitArea(p, '[aria-label="Voice input"]') && await hitArea(p, '[aria-label="Start voice conversation"]'), `${label}/${theme}: compact voice controls retain 44px hit areas`);
      }
      const cfg = await p.eval(apiCall('getVoice', ''));
      check(cfg.engine === 'auto' && !cfg.keep_audio && !cfg.openai_key, `${label}/${theme}: private default without recordings or paid key`);
      await p.eval(`__mod('lib/store.ts').then(m => m.setState('server', { ...m.state.server, voice_engine: undefined, voice: true }))`);
      check(await p.eval(`!!document.querySelector('[aria-label="Voice input"]') && !document.querySelector('[aria-label="Start voice conversation"]')`), `${label}/${theme}: older server offers dictation only`);
      await p.eval(`__mod('lib/store.ts').then(m => m.setState('server', { ...m.state.server, voice_engine: 'auto', voice: false }))`);
      await p.click('[aria-label="Voice input"]'); await until(p, phase('listening'), 'dictation starts');
      check(await hitArea(p, '.voice-status button'), `${label}/${theme}: Cancel retains 44px hit area`);
      await p.eval(`__words('A live private transcript')`);
      check(await p.eval(`document.querySelector('.composer textarea').value === 'A live private transcript'`), `${label}/${theme}: interim words in editable composer`);
      await p.key('Enter', 'Enter', 0, 13); await until(p, `!document.querySelector('.voice-status')`, 'dictation ends');
      check(await p.eval(`document.querySelector('.composer textarea').value === 'A live private transcript'`), `${label}/${theme}: dictation reviews before sending`);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.setDraft(m.workspace.focused(), ''))`);
      await p.click('[aria-label="Start voice conversation"]'); await until(p, phase('listening'), 'conversation starts');
      await p.eval(`__words('Please tell me a short story about the moon.', true)`);
      await until(p, `__voice.utterances.length > 0`, 'answer spoken');
      check(await p.eval(`!!document.querySelector('.composer-dock .voice-status')`), `${label}/${theme}: voice survives hero to normal chat`);
      check(await p.eval(`__voice.utterances.every(t => !t.includes('thinking'))`), `${label}/${theme}: speaks answer, not reasoning`);
      const before = await p.eval(`__voice.stopped`);
      const spoken = await p.eval(`__voice.utterances.at(-1)`);
      await p.eval(`__words(${JSON.stringify(spoken)})`);
      check(await p.eval(`__voice.stopped === ${before}`), `${label}/${theme}: echo does not interrupt`);
      await p.eval(`__words('Actually stop, I have another question')`);
      check(await p.eval(`__voice.stopped > ${before} && !__voice.playing`), `${label}/${theme}: talking cancels speech immediately`);
      await p.shot(`${SHOTS}/voice-${label}-${theme}.png`);
      check(await p.eval(`document.documentElement.scrollWidth <= innerWidth && document.querySelector('.voice-status').getBoundingClientRect().right <= innerWidth + 1`), `${label}/${theme}: status fits viewport`);
      check(await hitArea(p, '.voice-status button'), `${label}/${theme}: End retains 44px hit area`);
      await p.click('.voice-status button');
      check(await p.eval(`!document.querySelector('.voice-status') && __voice.cancels > 0`), `${label}/${theme}: End releases recognizer`);
      const requests = await p.eval(`__mod('lib/store.ts').then(m => m.call({ method: 'mock_voice', params: {} }))`);
      check(requests.requests.length === 0, `${label}/${theme}: offline device sends no audio or speech requests`);
      await p.click('[aria-label="Voice input"]'); await until(p, phase('listening'), 'second dictation');
      const canceled = await p.eval(`__voice.cancels`);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.setPaneContent(m.workspace.focused(), { type: 'chat', draft: {} }))`);
      await until(p, `__voice.cancels > ${canceled}`, 'chat switch cancels microphone');
      check(await p.eval(`!document.querySelector('.voice-status')`), `${label}/${theme}: switching chats ends voice`);
      await p.click('[aria-label="Voice input"]'); await until(p, phase('listening'), 'third dictation');
      await p.eval(`document.dispatchEvent(new Event('pagehide')); window.dispatchEvent(new Event('pagehide'))`);
      check(await p.eval(`!document.querySelector('.voice-status')`), `${label}/${theme}: leaving the page ends voice`);
      check(p.errors.length === 0, `${label}/${theme}: no browser errors (${p.errors.join(', ')})`);
      await p.close();
    }
  }
  await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
  const p = await device(); await signIn(p, 'Voice server fallback');
  // Never start a recognizer that doesn't prove processLocally availability.
  await p.eval(`window.SpeechRecognition = class { start() { throw Error('Cloud speech started'); } };
    window.AudioContext = class extends AudioContext { createMediaStreamSource() { const osc = this.createOscillator(), gain = this.createGain(); gain.gain.value = .05; window.__voiceGain = gain; osc.connect(gain); osc.start(); return gain; } };
    window.__voiceTracks = []; const media = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
    navigator.mediaDevices.getUserMedia = async (...args) => { const stream = await media(...args); __voiceTracks.push(...stream.getTracks()); return stream; };`);
  await p.click('[aria-label="Voice input"]'); await until(p, phase('listening'), 'server microphone after first-use install'); await sleep(4000);
  await p.eval(`__mod('lib/store.ts').then(m => { const original = m.api.transcribe; m.api.transcribe = async (...args) => { window.__sttPending = true; await new Promise(resolve => { window.__finishStt = resolve; }); m.api.transcribe = original; return original(...args); }; })`);
  await p.key('Enter', 'Enter', 0, 13);
  await until(p, `window.__sttPending && ${phase('sending')}`, 'transcription pending');
  check(await p.eval(`document.querySelector('.voice-status .rec-dot').classList.contains('quiet') && __voiceGain.context.state === 'closed' && __voiceTracks.length > 0 && __voiceTracks.every(track => track.readyState === 'ended')`), 'dictation microphone tracks are released during transcription');
  await p.eval(`__finishStt()`);
  await until(p, `!document.querySelector('.voice-status')`, 'server transcription');
  const canceledCall = await p.eval(`__mod('lib/store.ts').then(async m => { const abort = new AbortController(); const pending = m.api.transcribe('AAAA', 'audio/wav', 'server', true, abort.signal); abort.abort(); let canceled = false; try { await pending; } catch (e) { canceled = e.name === 'AbortError'; } await m.api.hello(); const fixture = await m.call({ method: 'mock_voice', params: {} }); return { canceled, sent: fixture.requests.some(r => r.method === 'cancel_voice' && r.operation), alive: !!m.state.server }; })`);
  check(canceledCall.canceled && canceledCall.sent && canceledCall.alive, 'Stop sends operation cancellation while chat connection stays open');
  const server = await p.eval(`__mod('lib/store.ts').then(m => m.call({ method: 'mock_voice', params: {} }))`);
  check(server.requests.some(r => r.method === 'install_voice'), 'server engines download automatically on first use');
  check(server.requests.some(r => r.method === 'transcribe' && r.wav && r.mime === 'audio/wav'), 'server input is real 16 kHz WAV capture');
  check(server.requests.every(r => !r.engine || r.engine === 'server'), 'automatic fallback never uses a paid provider');
  check(server.config.kept === 0, 'default server speech does not keep recordings');
  const privateRejected = await p.eval(`__mod('lib/store.ts').then(async m => { try { await m.api.transcribe('AAAA', 'audio/wav', 'openai'); return false; } catch { return true; } })`);
  check(privateRejected, 'server rejects paid route in Automatic');
  await p.eval(`__mod('lib/workspace.ts').then(m => m.setDraft(m.workspace.focused(), ''))`);
  await p.eval(`window.__audio = { played: 0, paused: 0, urls: [] }; window.Audio = class { constructor(url) { __audio.urls.push(url); } play() { __audio.played++; return Promise.resolve(); } pause() { __audio.paused++; } removeAttribute() {} load() {} };`);
  await p.click('[aria-label="Start voice conversation"]'); await until(p, phase('listening'), 'server conversation'); await sleep(1100);
  await p.eval(`__voiceGain.gain.value = 0`);
  await until(p, `__audio.played > 0`, 'Piper answer playback');
  const spokenServer = await p.eval(`__mod('lib/store.ts').then(m => m.call({ method: 'mock_voice', params: {} }))`);
  check(spokenServer.requests.some(r => r.method === 'speak' && r.engine === 'server'), 'server conversation plays private Piper speech');
  check(await p.eval(`__audio.urls.every(u => u.startsWith('blob:'))`), 'playback uses revocable local blob URLs');
  await sleep(700); await p.eval(`__voiceGain.gain.value = .05`);
  await until(p, `__audio.paused > 0`, 'server barge-in');
  check(true, 'sustained microphone audio interrupts server playback');
  await p.click('.voice-status button');
  await p.eval(`__mod('lib/workspace.ts').then(m => m.setDraft(m.workspace.focused(), ''))`);
  await p.eval(`__mod('lib/ui.ts').then(m => m.openSettings('voice'))`);
  await until(p, `!!document.querySelector('[aria-label="Where speech is handled"]')`, 'voice settings');
  await p.eval(`(() => { const s = document.querySelector('[aria-label="Where speech is handled"]'); s.value = 'openai'; s.dispatchEvent(new Event('change', { bubbles: true })); })()`);
  await p.eval(`document.querySelector('#set-voice form').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))`);
  await until(p, `document.querySelector('#set-voice').textContent.includes('Save your OpenAI API key')`, 'missing key validation');
  check((await p.eval(apiCall('getVoice', ''))).engine === 'auto', 'OpenAI cannot be selected without saved API key');
  await p.eval(`(() => { const s = document.querySelector('[aria-label="Where speech is handled"]'); s.value = 'openrouter'; s.dispatchEvent(new Event('change', { bubbles: true })); document.querySelector('#set-voice form').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); })()`);
  await until(p, `document.querySelector('#set-voice').textContent.includes('Save your OpenRouter API key')`, 'missing OpenRouter key validation');
  check((await p.eval(apiCall('getVoice', ''))).engine === 'auto', 'OpenRouter cannot be selected without a saved key');
  await p.eval(`(() => { const s = document.querySelector('[aria-label="Where speech is handled"]'); s.value = 'service'; s.dispatchEvent(new Event('change', { bubbles: true })); })()`);
  await until(p, `!!document.querySelector('#set-voice input[type=password]')`, 'service key field');
  await p.eval(`__mod('lib/store.ts').then(m => { const original = m.api.saveVoice; m.api.saveVoice = async (config, key) => { window.__submittedKey = key; await new Promise(resolve => { window.__finishSave = resolve; }); m.api.saveVoice = original; return original(config, key); }; const model = [...document.querySelectorAll('#set-voice label')].find(label => label.querySelector('.field-label')?.textContent === 'Model').querySelector('input'); model.value = 'fixture-model'; model.dispatchEvent(new Event('input', { bubbles: true })); const address = document.querySelector('#set-voice input[placeholder="http://127.0.0.1:8080/v1"]'); address.value = 'http://127.0.0.1:8080/v1'; address.dispatchEvent(new Event('input', { bubbles: true })); const key = document.querySelector('#set-voice input[type=password]'); key.value = 'fixture-submitted-key'; key.dispatchEvent(new Event('input', { bubbles: true })); document.querySelector('#set-voice form').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); })`);
  await until(p, `!!window.__finishSave`, 'held settings save');
  await p.eval(`(() => { const key = document.querySelector('#set-voice input[type=password]'); key.value = 'fixture-replacement-key'; key.dispatchEvent(new Event('input', { bubbles: true })); __finishSave(); })()`);
  await until(p, `document.querySelector('#set-voice button[type=submit]')?.disabled === false`, 'replacement key stays dirty');
  check(await p.eval(`__submittedKey === 'fixture-submitted-key' && document.querySelector('#set-voice input[type=password]').value === 'fixture-replacement-key'`), 'Save clears only submitted key, preserving replacement draft and baseline');
  await p.eval(`document.querySelector('#set-voice form').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))`);
  await until(p, `document.querySelector('#set-voice input[type=password]')?.value === '' && !!document.querySelector('#set-voice form') && !document.querySelector('#set-voice .form-actions')`, 'submitted replacement becomes clean');
  check(true, 'unchanged submitted key clears and Save baseline is clean');
  await p.eval(`(() => { const s = document.querySelector('[aria-label="Where speech is handled"]'); s.value = 'off'; s.dispatchEvent(new Event('change', { bubbles: true })); document.querySelector('#set-voice form').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })); })()`);
  await until(p, `__mod('lib/store.ts').then(m => m.state.server.voice_engine === 'off')`, 'Off saved');
  await p.key('Escape', 'Escape', 0, 27); await sleep(250);
  check(await p.eval(`!!document.querySelector('[aria-label="Voice input"]') && !!document.querySelector('[aria-label="Start voice conversation"]')`), 'Off keeps explicit turn-on controls available');
  await p.click('[aria-label="Start voice conversation"]'); await until(p, `!!document.querySelector('.voice-choice')`, 'voice off choice');
  check(await p.eval(`document.querySelector('.voice-choice').textContent.includes('Voice is off')`), 'Off offers private choices before any capture');
  await p.clickText('.voice-choice button', 'Not now');
  await p.eval(`__mod('lib/store.ts').then(async m => { await m.call({ method: 'mock_voice', params: { openai_key: true } }); const c = await m.api.getVoice(); await m.api.saveVoice({ ...c, engine: 'openai' }); m.setState('server', await m.api.hello()); })`);
  await p.click('[aria-label="Voice input"]'); await until(p, phase('listening'), 'explicit OpenAI mock choice'); await sleep(3300);
  await p.key('Enter', 'Enter', 0, 13); await until(p, `!document.querySelector('.voice-status')`, 'OpenAI mock transcription');
  const cloud = await p.eval(`__mod('lib/store.ts').then(m => m.call({ method: 'mock_voice', params: {} }))`);
  check(cloud.requests.filter(r => r.method === 'transcribe' && r.engine === 'openai').length === 1, 'explicit saved-key choice sends one cloud transcription (mock only)');
  check(!cloud.requests.some(r => r.engine === 'openai' && r.partial), 'cloud choice never uses repeated paid live-word requests');
  await p.eval(`__mod('lib/store.ts').then(async m => { await m.call({ method: 'mock_voice', params: { openrouter_key: true } }); const c = await m.api.getVoice(); await m.api.saveVoice({ ...c, engine: 'openrouter', openrouter_voice: 'george' }); m.setState('server', await m.api.hello()); });`);
  await p.eval(`__mod('lib/workspace.ts').then(m => m.setDraft(m.workspace.focused(), ''))`);
  const played = await p.eval(`__audio.played`);
  await p.click('[aria-label="Start voice conversation"]'); await until(p, phase('listening'), 'OpenRouter conversation'); await sleep(1100);
  await p.eval(`__voiceGain.gain.value = 0`);
  await until(p, `__audio.played > ${played}`, 'OpenRouter answer MP3 playback');
  const router = await p.eval(`__mod('lib/store.ts').then(m => m.call({ method: 'mock_voice', params: {} }))`);
  check(router.requests.filter(r => r.method === 'transcribe' && r.engine === 'openrouter').length === 1 && !router.requests.some(r => r.engine === 'openrouter' && r.partial), 'OpenRouter conversation transcribes once, with no paid partials');
  check(router.requests.some(r => r.method === 'transcribe' && r.engine === 'openrouter' && r.language === 'en'), 'cloud input carries a device-language hint without changing shared settings');
  check(router.requests.some(r => r.method === 'speak' && r.engine === 'openrouter' && r.voice === 'george'), 'OpenRouter conversation uses the saved voice for speech');
  await p.click('.voice-status button');
  await p.close();
  for (const [label, w, h, mobile] of [['desktop', 1440, 900, false], ['phone', 344, 812, true], ['fold', 841, 701, true]]) for (const theme of ['dark', 'light']) {
    await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
    const s = await device(); await s.viewport(w, h, mobile, mobile ? 2 : 1); await signIn(s, `Router ${label}`); await s.theme(theme);
    await s.eval(`window.SpeechRecognition = class {}; window.__preview = { played: 0, paused: 0 }; window.Audio = class { play() { __preview.played++; return Promise.resolve(); } pause() { __preview.paused++; } removeAttribute() {} load() {} }; __mod('lib/store.ts').then(async m => { await m.call({ method: 'mock_voice', params: { openrouter_key: true, live: false } }); });`);
    await s.eval(`__mod('lib/ui.ts').then(m => m.openSettings('voice'))`);
    await until(s, `document.querySelector('#set-voice')?.textContent.includes('For quicker, more natural conversations')`, 'private speech recommendation');
    check((await s.eval(apiCall('getVoice', ''))).engine === 'auto', `${label}/${theme}: recommendation leaves Automatic private`);
    await s.eval(`(() => { const e = document.querySelector('[aria-label="Where speech is handled"]'); e.value = 'openrouter'; e.dispatchEvent(new Event('change', { bubbles: true })); const v = document.querySelector('[aria-label="OpenRouter voice"]'); v.value = 'george'; v.dispatchEvent(new Event('change', { bubbles: true })); })()`);
    // DOM activation keeps this functional check independent of animated modal
    // scroll positioning. A separate hit-area assertion covers the control size.
    await s.eval(`[...document.querySelectorAll('#set-voice button')].find(e => e.textContent.includes('Save and preview voice')).click()`);
    await until(s, `__preview.played === 1`, 'explicit short voice preview');
    check((await s.eval(apiCall('getVoice', ''))).openrouter_voice === 'george', `${label}/${theme}: preview saves chosen voice explicitly`);
    await s.eval(`[...document.querySelectorAll('#set-voice button')].find(e => e.textContent.includes('Stop preview')).click()`);
    check(await s.eval(`__preview.paused > 0 && document.documentElement.scrollWidth <= innerWidth`), `${label}/${theme}: preview stops and settings fit`);
    check(await hitArea(s, '[aria-label="OpenRouter voice"]'), `${label}/${theme}: voice choice keeps a 44px hit area`);
    await s.shot(`${SHOTS}/voice-settings-${label}-${theme}.png`);
    await s.key('Escape', 'Escape', 0, 27);
    await s.eval(`__mod('lib/store.ts').then(async m => { await m.api.saveVoice({ ...await m.api.getVoice(), engine: 'off' }); m.setState('server', 'voice_engine', 'off'); window.__micStarts = 0; const original = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices); navigator.mediaDevices.getUserMedia = (...args) => { __micStarts++; return original(...args); }; })`);
    await s.click('[aria-label="Start voice conversation"]');
    await until(s, `!!document.querySelector('.voice-choice')`, 'off recovery inline choice');
    check(await s.eval(`__micStarts === 0 && document.querySelector('.voice-choice').textContent.includes('Voice is off')`), `${label}/${theme}: off entry offers choices without recording`);
    check(await hitArea(s, '.voice-choice .btn-primary'), `${label}/${theme}: one-tap OpenRouter choice keeps a 44px hit area`);
    await s.shot(`${SHOTS}/voice-choice-${label}-${theme}.png`);
    await s.clickText('.voice-choice button', 'Use OpenRouter (your key)');
    await until(s, phase('listening'), 'explicit engine choice starts listening');
    check(await s.eval(`__micStarts === 1 && !document.querySelector('.voice-choice')`), `${label}/${theme}: one choice starts the original conversation once`);
    await s.click('.voice-status button');
    check(!s.errors.length, `${label}/${theme}: voice settings have no browser errors`, s.errors); await s.close();
  }
  finish();
});
