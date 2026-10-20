// Launch review captures use fake offline speech and generated files, never a model service.
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from './cdp.mjs';

const C = '.pane.focused .composer';
const fakeVoice = `(() => {
  window.__voice = {};
  window.SpeechRecognition = class {
    static async available() { return 'available'; }
    start() { if (!this.processLocally) throw Error('Expected offline recognition'); __voice.current = this; }
    stop() { this.onend?.(); }
    abort() {}
  };
})()`;
const draft = (p, text) => p.eval(`__mod('lib/workspace.ts').then(m => m.setDraft(m.workspace.focused(), ${JSON.stringify(text)}))`);
const shot = async (p, tag, state) => {
  await sleep(300);
  await p.shot(`${SHOTS}/launch-composer/${tag}-${state}.png`);
  const b = await p.box('.pane.focused .composer-wrap');
  await p.shot(`${SHOTS}/launch-composer/${tag}-${state}-detail.png`, { x: b.x, y: b.y, width: b.w, height: b.h });
};

run(async () => {
  for (const [label, width, height, mobile] of [['desktop', 1440, 900, false], ['phone', 390, 844, true], ['cover', 344, 882, true], ['fold', 880, 1000, true]]) {
    for (const theme of ['light', 'dark']) {
      const tag = `${label}-${theme}`;
      await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
      const p = await device(); await p.viewport(width, height, mobile); await signIn(p, `Launch visual ${tag}`); await p.theme(theme);
      await p.eval(fakeVoice);
      await p.eval(`__mod('lib/workspace.ts').then(m => m.openSession('ses_2b7e91c04d5a', { pane: m.workspace.focused() }))`);
      await p.waitFor(`document.querySelector('.pane.focused .composer-dock')`);
      await shot(p, tag, 'idle');
      await draft(p, 'Please review the launch checklist.');
      await p.waitFor(`document.querySelector('${C} [aria-label="Send"]')`);
      await shot(p, tag, 'text');
      await draft(p, '');
      await p.click(`${C} [aria-label="Voice input"]`);
      await p.waitFor(`document.querySelector('${C} .voice-status')?.dataset.phase === 'listening'`);
      await p.eval(`__voice.current.onresult?.({results:[{0:{transcript:'A private draft to review'},isFinal:false}]})`);
      await shot(p, tag, 'dictation');
      await p.click(`${C} [aria-label="Finish recording"]`);
      await p.waitFor(`!document.querySelector('${C} .voice-status')`);
      await draft(p, '');
      await p.click(`${C} [aria-label="Start voice conversation"]`);
      await p.waitFor(`document.querySelector('${C} .voice-status')?.dataset.phase === 'listening'`);
      await shot(p, tag, 'conversation');
      await p.click(`${C} .voice-status button`);
      await p.waitFor(`!document.querySelector('${C} .voice-status')`);
      await p.eval(`(async () => {
        const files = new DataTransfer();
        for (const [name, w, h, color] of [['landscape', 960, 120, '#468d98'], ['portrait', 120, 960, '#bd7753'], ['square', 240, 240, '#7b78a4'], ['plan', 360, 240, '#658661']]) {
          const c = document.createElement('canvas'); c.width = w; c.height = h;
          const x = c.getContext('2d'); x.fillStyle = color; x.fillRect(0, 0, w, h);
          x.strokeStyle = '#f6edda'; x.lineWidth = 8; x.strokeRect(15, 15, w - 30, h - 30);
          files.items.add(new File([await new Promise(r => c.toBlob(r))], name + '.png', { type: 'image/png' }));
        }
        files.items.add(new File(['Project notes'], 'Project-notes-for-the-next-release-review.md', { type: 'text/markdown' }));
        const input = document.querySelector('${C} input[multiple]'); input.files = files.files;
        input.dispatchEvent(new Event('change', { bubbles: true }));
      })()`);
      await p.waitFor(`document.querySelectorAll('${C} .attachment, ${C} .file-chip').length === 5`);
      await shot(p, tag, 'images');
      await p.eval(`document.querySelector('${C} .file-chip').scrollIntoView({block:'nearest',inline:'nearest'})`);
      await shot(p, tag, 'file');
      check(await p.eval(`document.documentElement.scrollWidth <= innerWidth`), `${tag} composer states do not overflow the screen`);
      check(p.errors.length === 0, `${tag} capture has no page errors`, p.errors);
      await p.close();
    }
  }
  finish();
});
