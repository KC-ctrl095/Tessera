// Composer geometry and real pointer states on desktop, phones and an open Fold.
// COMPOSER_CAPTURE=before records the original layout without the new geometry assertions.
import fs from 'node:fs';
import { SHOTS, check, device, finish, mock, run, signIn, sleep } from './cdp.mjs';
import { hitTarget } from './target-input.mjs';

const before = process.env.COMPOSER_CAPTURE === 'before';
const out = process.env.COMPOSER_SHOTS ?? `${SHOTS}/composer-polish`;
const C = '.pane.focused .composer';
const fakeVoice = `(() => {
  window.__voice = { starts: 0, cancels: 0 };
  window.SpeechRecognition = class {
    static async available() { if (__voice.wait) await new Promise(r => __voice.resume = r); return 'available'; }
    start() { if (!this.processLocally) throw Error('Expected offline recognition'); __voice.current = this; __voice.starts++; }
    stop() { this.onend?.(); }
    abort() { __voice.cancels++; }
  };
})()`;
const addFiles = (p) => p.eval(`(async () => {
  const dt = new DataTransfer();
  for (const [name, w, h, color] of [['landscape',960,120,'#468d98'],['portrait',120,960,'#bd7753'],['square',240,240,'#7b78a4'],['plan',360,240,'#658661']]) {
    const c = document.createElement('canvas'); c.width=w; c.height=h;
    const x = c.getContext('2d'); x.fillStyle=color; x.fillRect(0,0,w,h);
    x.strokeStyle='#f6edda'; x.lineWidth=8; x.strokeRect(15,15,w-30,h-30);
    x.beginPath(); x.moveTo(0,0); x.lineTo(w,h); x.moveTo(w,0); x.lineTo(0,h); x.stroke();
    dt.items.add(new File([await new Promise(r => c.toBlob(r))], name+'.png', {type:'image/png'}));
  }
  dt.items.add(new File(['Project notes'], 'Project-notes-with-a-very-long-name-for-the-next-release-review-and-final-changes.md', {type:'text/markdown'}));
  dt.items.add(new File(['Fixture document'], 'Release-checklist.pdf', {type:'application/pdf'}));
  const input=document.querySelector('${C} input[multiple]'); input.files=dt.files; input.dispatchEvent(new Event('change',{bubbles:true}));
})()`);
const measure = (p) => p.eval(`(() => {
  const c=document.querySelector('${C}'), row=c.querySelector('.comp-row'), ta=c.querySelector('textarea');
  const rect=e=>{const r=e.getBoundingClientRect();return {x:r.x,y:r.y,w:r.width,h:r.height,right:r.right,bottom:r.bottom,cy:r.y+r.height/2};};
  return { composer:rect(c), row:rect(row), field:rect(ta), controls:[...c.querySelectorAll('.comp-row button,.comp-bar > button')].map(e=>({label:e.getAttribute('aria-label'),...rect(e),bg:getComputedStyle(e).backgroundColor,visual:getComputedStyle(e,'::before').width})),
    strip:c.querySelector('.attachments') ? { ...rect(c.querySelector('.attachments')), scroll:c.querySelector('.attachments').scrollWidth, client:c.querySelector('.attachments').clientWidth } : null,
    thumbs:[...c.querySelectorAll('.attachment')].map(e=>({...rect(e),fit:getComputedStyle(e.querySelector('img')).objectFit})), removes:[...c.querySelectorAll('.attachment-x,.file-chip-x')].map(e=>({...rect(e),visual:getComputedStyle(e,'::before').width})),
    files:[...c.querySelectorAll('.file-chip')].map(e=>({...rect(e),name:!!e.querySelector('.file-chip-name'),icon:!!e.querySelector('.file-icon')})), overflow:document.documentElement.scrollWidth>innerWidth };
})()`);
const shot = async (p, tag, name) => {
  await sleep(250);
  await p.shot(`${out}/${tag}-${name}.png`);
  const b=await p.box('.pane.focused .composer-wrap');
  await p.shot(`${out}/${tag}-${name}-detail.png`, {x:b.x,y:b.y,width:b.w,height:b.h});
};
const clear = p => p.eval(`document.querySelectorAll('${C} .attachment-x,${C} .file-chip-x').forEach(e=>e.click())`);
const draft = (p,t) => p.eval(`__mod('lib/workspace.ts').then(m=>m.setDraft(m.workspace.focused(),${JSON.stringify(t)}))`);

run(async () => {
  fs.mkdirSync(out,{recursive:true});
  const metrics = {};
  for (const [name,w,h,touch] of [['desktop',1440,900,false],['phone',390,844,true],['cover',344,882,true],['fold',880,1000,true]]) for (const theme of ['dark','light']) {
    const tag=`${name}-${theme}`;
    if (process.env.COMPOSER_CASE && process.env.COMPOSER_CASE!==tag) continue;
    await mock('/__mock/reset'); await mock('/__mock/mosaics/reset');
    const p=await device(); await p.viewport(w,h,touch); await signIn(p,`Composer polish ${tag}`); await p.theme(theme); await p.eval(fakeVoice);
    await p.waitFor(`document.querySelector('${C} [aria-label="Start voice conversation"]')`);
    await shot(p,tag,'hero');
    await p.eval(`__mod('lib/workspace.ts').then(m=>m.openSession('ses_2b7e91c04d5a',{pane:m.workspace.focused()}))`);
    await p.waitFor(`document.querySelector('.pane.focused .composer-dock')`); await sleep(500);
    await shot(p,tag,'idle');
    const rest=await measure(p);
    if (!before) {
      check(rest.controls.every(b=>b.w>=44 && b.h>=44),`${tag}: uniform 44px idle controls`,rest);
      check(rest.controls.every(b=>Math.abs(b.cy-rest.field.cy)<1 && (b.right<=rest.field.x+1 || b.x>=rest.field.right-1)),`${tag}: controls align with the input without overlap`,rest);
      check(rest.composer.h<=44.5 && !rest.overflow,`${tag}: dock stays slim and fits`);
      check(rest.controls.find(b=>b.label==='Start voice conversation').bg==='rgba(0, 0, 0, 0)' && parseFloat(rest.controls.find(b=>b.label==='Start voice conversation').visual)===32,`${tag}: voice has a quiet 32px surface, not a heavy filled block`);
    }
    await addFiles(p); await p.waitFor(`document.querySelectorAll('${C} .attachments > .attachment,${C} .attachments > .file-chip').length===6`);
    await shot(p,tag,'attachments');
    const attached=await measure(p); metrics[tag]={rest,attached};
    if (!before) {
      check(attached.thumbs.every(r=>r.w===72 && r.h===72 && r.fit==='contain'),`${tag}: wide, tall and square images have consistent 72px previews`,attached);
      check(attached.thumbs.every(r=>Math.abs(r.y-attached.thumbs[0].y)<1) && attached.strip.scroll>attached.strip.client,`${tag}: multiple attachments stay in one scrollable row`);
      check(attached.strip.bottom<=attached.row.y && !attached.overflow,`${tag}: attachment row cannot overlap text or overflow viewport`);
      check(attached.removes.every(r=>r.w>=44 && r.h>=44 && parseFloat(r.visual)>=20 && parseFloat(r.visual)<=22),`${tag}: remove targets are 44px with a 20-22px visual`,attached.removes);
      check(attached.files.every(r=>r.h===72 && r.w<=260 && r.name && r.icon),`${tag}: non-images have tidy bounded name/icon chips`,attached.files);
      const strip=await p.box(`${C} .attachments`);
      const pane=await p.eval(`__mod('lib/workspace.ts').then(m=>m.workspace.focused())`);
      if (touch) {
        await p.eval(`window.__gesture=[]; for(const t of ['pointerdown','pointermove','pointercancel','pointerup']) document.querySelector('${C} .attachments').addEventListener(t,e=>__gesture.push({type:e.type,target:e.target.className,ta:getComputedStyle(e.currentTarget).touchAction,scroll:e.currentTarget.scrollLeft}),{passive:true})`);
        const x=strip.x+strip.w-24, y=strip.cy+16;
        await p.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[{x,y}]});
        for (let i=1;i<=8;i++) {
          await p.send('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[{x:x-i*18,y}]});
          await sleep(20);
        }
        await p.send('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});
      } else {
        await p.send('Input.dispatchMouseEvent',{type:'mouseWheel',x:strip.cx,y:strip.cy,deltaX:220,deltaY:0});
      }
      await sleep(400);
      check(await p.eval(`document.querySelector('${C} .attachments').scrollLeft>20`) && pane===await p.eval(`__mod('lib/workspace.ts').then(m=>m.workspace.focused())`),`${tag}: real horizontal gesture scrolls attachments, not the chat`, await p.eval(`({events:window.__gesture,scroll:document.querySelector('${C} .attachments').scrollLeft,mode:document.querySelector('.workspace').dataset.mode})`));
      await shot(p,tag,'files');
      // Let momentum settle before taking a deterministic removal target.
      await sleep(800); await p.eval(`document.querySelector('${C} .attachments').scrollLeft=0`);
      await hitTarget(p,`document.querySelector('${C} .attachment-x')`,touch,`${tag} remove landscape`,44);
      await p.waitFor(`document.querySelectorAll('${C} .attachment').length===3`);
      check(await p.eval(`!document.querySelector('${C} img[alt="landscape.png"]')`),`${tag}: pointer removes only the chosen image`);
      await p.eval(`document.querySelector('${C} .file-chip-x').scrollIntoView({block:'nearest',inline:'nearest'})`);
      await hitTarget(p,`document.querySelector('${C} .file-chip-x')`,touch,`${tag} remove long file name`,44);
      await p.waitFor(`document.querySelectorAll('${C} .file-chip').length===1`);
      check(true,`${tag}: file remove works after horizontal scrolling`);
    }
    await clear(p);
    if (!before) {
      await draft(p,'A short draft'); await sleep(200);
      let g=await measure(p);
      check(g.controls.some(b=>b.label==='Send') && !g.controls.some(b=>b.label==='Start voice conversation'),`${tag}: content replaces voice conversation with Send`);
      check(g.controls.every(b=>b.w>=44 && b.h>=44 && Math.abs(b.cy-g.field.cy)<1),`${tag}: Send has the same size and alignment`);
      await shot(p,tag,'send'); await draft(p,'');
      await p.eval(`__voice.wait=true`);
      await hitTarget(p,`document.querySelector('${C} [aria-label="Voice input"]')`,touch,`${tag} dictate`,44);
      await p.waitFor(`!!__voice.resume && document.querySelector('${C} .voice-status')?.dataset.phase==='starting'`);
      check(await p.eval(`!!document.querySelector('${C} .mic-btn .tess') && document.querySelector('${C} .mic-btn').getAttribute('aria-pressed')==='true'`),`${tag}: pending microphone shows progress and stays cancellable`);
      await shot(p,tag,'starting');
      await p.eval(`__voice.wait=false; __voice.resume()`);
      await p.waitFor(`document.querySelector('${C} .voice-status')?.dataset.phase==='listening'`);
      check(await p.eval(`document.querySelector('${C} [aria-label="Finish recording"]')?.getAttribute('aria-pressed')==='true'`),`${tag}: dictation has a clear active state`);
      await shot(p,tag,'dictation');
      await hitTarget(p,`document.querySelector('${C} [aria-label="Finish recording"]')`,touch,`${tag} finish dictation`,44);
      await p.waitFor(`!document.querySelector('${C} .voice-status')`);
      await p.eval(`document.querySelector('${C} [aria-label="Voice input"]').focus()`);
      await p.key('Enter','Enter',0,13);
      await p.waitFor(`document.querySelector('${C} .voice-status')?.dataset.phase==='listening'`);
      await p.key('Escape','Escape',0,27);
      await p.waitFor(`!document.querySelector('${C} .voice-status')`);
      check(true,`${tag}: keyboard activation and Escape cancellation still work`);
      await hitTarget(p,`document.querySelector('${C} [aria-label="Start voice conversation"]')`,touch,`${tag} conversation`,44);
      await p.waitFor(`document.querySelector('${C} .voice-status')?.dataset.phase==='listening'`);
      check(await p.eval(`document.querySelector('${C}').classList.contains('conversation') && !document.querySelector('${C} .mic-btn,${C} .voice-call,${C} .send-btn')`),`${tag}: conversation has one End action and no duplicate mic/send`);
      await shot(p,tag,'conversation');
      await hitTarget(p,`document.querySelector('${C} .voice-status button')`,touch,`${tag} end conversation`,44);
      await p.waitFor(`!document.querySelector('${C} .voice-status')`);
      // A long mock turn keeps busy stable and lets Stop exercise its real RPC.
      await p.eval(`__mod('lib/store.ts').then(m=>m.call({method:'send',params:{session_id:'ses_2b7e91c04d5a',content:[{type:'text',text:'stream the answer slowly please'}],attachments:[]}}))`);
      await p.waitFor(`document.querySelector('${C} [aria-label="Stop"]')`);
      await draft(p,'Queued draft'); await sleep(200); g=await measure(p);
      check(g.controls.some(b=>b.label==='Stop') && g.controls.some(b=>b.label==='Queue') && !g.controls.some(b=>b.label==='Send' || b.label==='Start voice conversation'),`${tag}: busy offers Stop and Queue, not duplicate Send/voice`);
      check(g.controls.every(b=>b.w>=44 && b.h>=44 && Math.abs(b.cy-g.field.cy)<1) && g.field.w>100 && !g.overflow,`${tag}: busy controls leave usable text space`);
      await shot(p,tag,'busy');
      await draft(p,'');
      await hitTarget(p,`document.querySelector('${C} [aria-label="Stop"]')`,touch,`${tag} stop agent`,44);
      await p.waitFor(`!document.querySelector('${C} [aria-label="Stop"]')`);
      if (!touch) {
        await p.eval(`__mod('lib/prefs.ts').then(m=>m.setPrefs('density','compact'))`); await sleep(200); g=await measure(p);
        check(g.controls.every(b=>b.w>=44 && b.h>=44 && Math.abs(b.cy-g.field.cy)<1),`${tag}: compact density preserves control targets and alignment`);
      }
      check(p.errors.length===0,`${tag}: no browser errors`,p.errors);
    }
    await p.close();
  }
  fs.writeFileSync(`${out}/measurements.json`,JSON.stringify(metrics,null,2));
  finish();
});
