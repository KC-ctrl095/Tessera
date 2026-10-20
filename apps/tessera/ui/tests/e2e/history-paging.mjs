import { SHOTS, check, device, finish, mock, run, signIn, sleep, wsClient } from './cdp.mjs';

run(async () => {
  await mock('/__mock/reset');
  await mock('/__mock/mosaics/reset');
  const probe = await wsClient('History paging fixture');
  const seeded = (await probe.rpc('mock_phone_lag', { calls: 500, helpers: 0 })).result;
  const id = seeded.parents[0].id;
  const p = await device();
  await p.viewport(1440, 900);
  await signIn(p, 'History paging');
  const initial = await p.eval(`__mod('lib/store.ts').then(async s => {
    const startFull = performance.now();
    const full = await s.api.getSession('${id}');
    const fullMs = performance.now()-startFull;
    const startPage = performance.now();
    await s.ensureLoaded('${id}');
    const pageMs = performance.now()-startPage;
    const current = s.state.sessions['${id}'];
    window.__historyFull = full.entries;
    return { fullMs, pageMs, all:full.entries.length, loaded:current.entries.length, older:current.hasOlderHistory, last:current.entries.at(-1)?.seq, expected:full.entries.at(-1)?.seq };
  })`);
  console.log('HISTORY browser snapshot', JSON.stringify(initial));
  check(initial.all > 1000 && initial.loaded <= 120 && initial.older && initial.last === initial.expected, 'huge opens install only the newest bounded page', initial);
  const prepend = await p.eval(`__mod('lib/store.ts').then(async s => {
    const current = s.state.sessions['${id}'];
    const retained = [...current.entries], count = current.entries.length;
    const a = s.loadOlderHistory('${id}'), b = s.loadOlderHistory('${id}');
    const shared = a === b;
    await Promise.all([a,b]);
    return { shared, count:current.entries.length, before:count, identity:retained.every((entry,i) => entry === current.entries[current.entries.length-retained.length+i]), sorted:current.entries.every((entry,i) => !i || entry.seq > current.entries[i-1].seq), loading:current.loadingOlderHistory };
  })`);
  check(prepend.shared && prepend.count > prepend.before && prepend.identity && prepend.sorted && !prepend.loading, 'concurrent older requests share a page and preserve installed entry identity', prepend);
  const live = await p.eval(`__mod('lib/store.ts').then(async s => {
    const current = s.state.sessions['${id}'];
    const seq = current.entries.at(-1).seq+1;
    s.applyEvent({type:'entry_added',session_id:'${id}',entry:{seq,ts:Date.now(),kind:'notice',level:'info',text:'New fixture entry'}});
    await s.loadOlderHistory('${id}');
    return current.entries.at(-1).seq === seq;
  })`);
  check(live, 'loading an older page retains a concurrently appended entry');
  const complete = await p.eval(`__mod('lib/store.ts').then(async s => {
    const current = s.state.sessions['${id}'];
    let pages = 0;
    while (current.hasOlderHistory && pages++ < 100) await s.loadOlderHistory('${id}');
    const expected = window.__historyFull.map(e=>e.seq);
    const actual = current.entries.map(e=>e.seq);
    return { pages, complete:!current.hasOlderHistory, matching:expected.every((seq,i)=>actual[i]===seq), unique:new Set(actual).size === actual.length, count:actual.length, expected:expected.length };
  })`);
  check(complete.complete && complete.matching && complete.unique && complete.count === complete.expected + 1, 'all pages reconstruct history with no missing, duplicated or reordered entries', complete);
  const stale = await p.eval(`__mod('lib/store.ts').then(async s => {
    await s.loadSession('${id}', false);
    const original = s.api.getSession;
    let release, ready;
    const started = new Promise(r=>ready=r);
    s.api.getSession = async (id,since,page) => {
      const result = await original(id,since,page);
      if (id === '${id}' && page?.before_seq) { ready(); await new Promise(r=>release=r); }
      return result;
    };
    const older = s.loadOlderHistory('${id}');
    await started;
    await s.loadSession('${id}', false);
    const count = s.state.sessions['${id}'].entries.length;
    release(); await older; s.api.getSession = original;
    return { count, after:s.state.sessions['${id}'].entries.length, loading:s.state.sessions['${id}'].loadingOlderHistory };
  })`);
  check(stale.count === stale.after && !stale.loading, 'a late older page cannot overwrite a newer baseline', stale);
  const edited = await p.eval(`__mod('lib/store.ts').then(async s => {
    const original = s.api.getSession;
    let release, ready;
    const started = new Promise(r=>ready=r);
    s.api.getSession = async (id,since,page) => {
      const result = await original(id,since,page);
      if (id === '${id}' && page?.before_seq) { ready(); await new Promise(r=>release=r); }
      return result;
    };
    const current = s.state.sessions['${id}'], count = current.entries.length;
    const older = s.loadOlderHistory('${id}');
    await started;
    s.applyEvent({type:'entries_changed',session_id:'${id}',entries:[]});
    release(); await older; s.api.getSession = original;
    return {count, after:current.entries.length, loading:current.loadingOlderHistory};
  })`);
  check(edited.count === edited.after && !edited.loading, 'an in-place history edit invalidates a pending older page', edited);
  await p.eval(`__mod('lib/workspace.ts').then(w=>w.openSession('${id}',{pane:w.workspace.focused()}))`);
  await p.waitFor(`document.querySelector('.pane.focused .transcript')`);
  // Screenshots and anchoring use the real reader, not direct loader calls.
  for (const [label, width, height, mobile] of [['desktop',1440,900,false],['cover',344,780,true],['fold',841,701,true]]) {
    for (const theme of ['light','dark']) {
      await p.viewport(width,height,mobile,mobile?2:1); await p.theme(theme);
      for (const anchoring of ['none', 'auto']) {
        await p.eval(`document.querySelector('.pane.focused .transcript').style.overflowAnchor = '${anchoring}'`);
        await p.eval(`__mod('lib/store.ts').then(async s => {
          await s.loadSession('${id}', false);
          window.__pageStarted = false;
          window.__pageBefore = s.state.sessions['${id}'].entries.length;
          const original = s.api.getSession;
          window.__restorePage = () => { s.api.getSession = original; };
          s.api.getSession = async (id, since, page) => {
            const result = await original(id, since, page);
            if (page?.before_seq) { window.__pageStarted = true; await new Promise(r => window.__releasePage = r); }
            return result;
          };
        })`);
        await sleep(200);
        check(await p.eval(`!window.__pageStarted`), `${label} ${theme} does not preload older pages on mount or resize`);
        for (let i=0;i<8 && !await p.eval('window.__pageStarted');i++) {
          await p.eval(`(() => { const t=document.querySelector('.pane.focused .transcript'); t.dispatchEvent(new WheelEvent('wheel',{deltaY:-500,bubbles:true})); t.scrollTop=0; if (t.scrollHeight<=t.clientHeight+1) t.querySelector('.load-more button')?.click(); })()`);
          await sleep(150);
        }
        await p.waitFor('window.__pageStarted');
        await p.eval(`(() => { const t=document.querySelector('.pane.focused .transcript'), r=t.getBoundingClientRect(); const el=[...t.querySelectorAll('.transcript-inner > .item')].find(e=>e.querySelector('.msg-assistant') && e.getBoundingClientRect().top>r.top+35); window.__pageAnchor={el,top:el?.getBoundingClientRect().top}; })()`);
        check(await p.eval(`document.querySelector('.load-more [disabled]')?.textContent.includes('Loading earlier')`), `${label} ${theme} shows quiet loading feedback`);
        check(await p.eval(`document.documentElement.scrollWidth <= innerWidth+1 && !!document.querySelector('.pane.focused .composer textarea')`), `${label} ${theme} keeps paged chat usable`);
        await p.shot(`${SHOTS}/history-paging-${label}-${theme}-${anchoring}.png`);
        await p.eval('window.__restorePage(); window.__releasePage()');
        await p.waitFor(`__mod('lib/store.ts').then(s=>!s.state.sessions['${id}'].loadingOlderHistory)`);
        await sleep(100);
        const anchored=await p.eval(`({connected:__pageAnchor.el?.isConnected,jump:Math.abs(__pageAnchor.el?.getBoundingClientRect().top-__pageAnchor.top)})`);
        console.log('HISTORY anchor', label, theme, anchoring, JSON.stringify(anchored));
        check(anchored.connected && anchored.jump<=2, `${label} ${theme} prepends without a visual jump (native anchoring ${anchoring})`, anchored);
        // Grow a mounted row above the retained anchor in separate layouts.
        // This covers late images and the same deferred code-colouring path as chat.
        await p.eval(`(async () => {
          const host = __pageAnchor.el.previousElementSibling;
          host.style.contentVisibility = 'visible';
          const box = document.createElement('div'); box.className = 'md';
          box.innerHTML = '<pre><code class="language-js">const ready = true;\\nconsole.log(ready);</code></pre>';
          const img = document.createElement('img'); img.style.width = '100px';
          box.append(img); host.append(box);
          (await __mod('lib/markdown.ts')).highlightCodes([box.querySelector('code')]);
          const canvas = document.createElement('canvas'); canvas.width = 100; canvas.height = 120;
          window.__lateImage = false;
          img.onload = () => { window.__lateImage = true; };
          setTimeout(() => { img.src = canvas.toDataURL(); }, 100);
        })()`);
        await p.waitFor('window.__lateImage'); await sleep(250);
        const late = await p.eval(`({connected:__pageAnchor.el.isConnected,jump:Math.abs(__pageAnchor.el.getBoundingClientRect().top-__pageAnchor.top)})`);
        check(late.connected && late.jump<=2, `${label} ${theme} keeps the anchor through late image and code layout (native anchoring ${anchoring})`, late);
      }
    }
  }
  // A page that arrives under a held finger must wait, then capture the new
  // reading position immediately before writing the store, not before fetching.
  await p.viewport(344,780,true,2);
  await p.eval(`__mod('lib/store.ts').then(async s => {
    await s.loadSession('${id}',false);
    window.__pageStarted = false;
    const original = s.api.getSession;
    window.__restorePage = () => { s.api.getSession = original; };
    s.api.getSession = async (id,since,page) => {
      const snap = await original(id,since,page);
      if (page?.before_seq) { window.__pageStarted = true; await new Promise(r => window.__releasePage = r); }
      return snap;
    };
    window.__heldCount = s.state.sessions['${id}'].entries.length;
  })`);
  for(let i=0;i<8 && !await p.eval('window.__pageStarted');i++) {
    await p.eval(`(() => { const t=document.querySelector('.pane.focused .transcript');t.dispatchEvent(new WheelEvent('wheel',{deltaY:-500,bubbles:true}));t.scrollTop=0;t.querySelector('.load-more button')?.click(); })()`);
    await sleep(150);
  }
  await p.waitFor('window.__pageStarted');
  const finger = await p.eval(`(() => { const r=document.querySelector('.pane.focused .transcript').getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2}; })()`);
  await p.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[finger]});
  await p.eval('window.__restorePage(); window.__releasePage()');
  await sleep(200);
  check(await p.eval(`__mod('lib/store.ts').then(s => s.state.sessions['${id}'].loadingOlderHistory && s.state.sessions['${id}'].entries.length===window.__heldCount)`), 'a fetched page waits while a finger is on the scroller');
  await p.eval(`(() => { const t=document.querySelector('.pane.focused .transcript');t.scrollTop=80;const el=t.querySelector('.msg-assistant').closest('.item');window.__heldAnchor={el,top:el.getBoundingClientRect().top}; })()`);
  await sleep(60);
  await p.send('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});
  await p.waitFor(`__mod('lib/store.ts').then(s=>!s.state.sessions['${id}'].loadingOlderHistory)`);
  await sleep(150);
  const held = await p.eval(`({connected:__heldAnchor.el.isConnected,jump:Math.abs(__heldAnchor.el.getBoundingClientRect().top-__heldAnchor.top)})`);
  check(held.connected && held.jump<=2, 'the commit captures the reading position reached during the pending fetch', held);
  // A hidden-only page advances the cursor but paints nothing. It must not leave
  // the top-reader stuck waiting for a scroll event that cannot happen.
  const hidden = await p.eval(`__mod('lib/store.ts').then(async s => {
    await s.loadSession('${id}',false);
    const original=s.api.getSession; let calls=0;
    s.api.getSession=async (id,since,page)=>{const snap=await original(id,since,page); if(page?.before_seq && ++calls===1) return {...snap,entries:[]}; return snap;};
    window.__hiddenCalls=()=>calls; window.__restoreHidden=()=>s.api.getSession=original;
  })`);
  for(let i=0;i<8 && await p.eval('__hiddenCalls()')<2;i++) {
    await p.eval(`(() => {const t=document.querySelector('.pane.focused .transcript');t.dispatchEvent(new WheelEvent('wheel',{deltaY:-500,bubbles:true}));t.scrollTop=0;t.querySelector('.load-more button')?.click();})()`); await sleep(200);
  }
  check(await p.eval('__hiddenCalls()>=2'), 'an empty visible page allows the next near-top page');
  await p.eval('__restoreHidden()');
  await p.eval(`__mod('lib/store.ts').then(s=>s.loadSession('${id}',false))`);
  const countBeforeJump = await p.eval(`__mod('lib/store.ts').then(s=>s.state.sessions['${id}'].entries.length)`);
  await p.eval(`(async()=>{const w=await __mod('lib/workspace.ts'),u=await __mod('lib/ui.ts');u.setJumpTarget({paneId:w.workspace.focused(),sessionId:'${id}',seq:1});})()`);
  await p.waitFor(`!!document.querySelector('.pane.focused [data-seq="1"].flash')`,30000);
  check(countBeforeJump<=120 && await p.eval(`!!document.querySelector('.pane.focused [data-seq="1"].flash')`), 'search and recall jumps fetch missing pages before locating the target');
  await p.eval(`__mod('lib/store.ts').then(s=>s.loadSession('${id}',false))`);
  await p.eval(`document.querySelector('.pane.focused .composer textarea').focus()`);
  await p.key('f','KeyF',2,70);
  await p.waitFor(`document.querySelector('[aria-label="Find in chat"]')`);
  await p.eval(`(() => {const i=document.querySelector('[aria-label="Find in chat"]');i.value='Read these synthetic project files';i.dispatchEvent(new Event('input',{bubbles:true}));i.closest('form').dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));})()`);
  await p.waitFor(`document.querySelector('.chat-find [role="status"]')?.textContent==='Found'`,30000);
  check(true, 'in-chat find reaches a message outside the opened page');
  await p.eval(`document.querySelector('[aria-label="Close find"]').click()`);
  check(!p.errors.length, 'history paging has no uncaught browser errors', p.errors);
  probe.close(); await p.close(); finish();
});
