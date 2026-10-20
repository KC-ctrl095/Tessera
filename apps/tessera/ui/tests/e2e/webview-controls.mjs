// Phone controls share the exact input path with a desktop tile/drive mode.
import {device,signIn,sleep,check,finish,SHOTS,MOCK,wsClient,menuPick} from './cdp.mjs';
for(const [w,h]of [[344,780],[412,915],[673,841],[1440,900]])for(const theme of ['light','dark']){
const mobile=w<1000;
console.log(`T12 controls ${w} ${theme}`);
const p=await device();await p.viewport(w,h,mobile,mobile?2:1);await signIn(p,`T12 controls ${w} ${theme}`);await p.theme(theme);
const c=await wsClient('T12 fixtures');const rpc=async(method,params={})=>{const r=await c.rpc(method,params);if(r.error)throw Error(r.error.message);return r.result;};
const b=await rpc('create_board',{session_id:'ses_7d3a91e5c4b2',title:'T12 controls',content:{type:'url',url:'https://example.test/homework'},placed:true});
const browser=async()=> (await fetch(`${MOCK}/__mock/browser`)).json();
const id=b.id,open=async()=>{await p.eval(`__mod('lib/workspace.ts').then(m=>m.openBoard('${id}',{pane:m.workspace.focused()}))`);await p.waitFor(`document.querySelector('.wv-canvas.shown')`);await sleep(300);};await open();
const keyboard=async()=>{if(mobile)await p.click('[aria-label="Show page keyboard"]');else await p.click('.wv-canvas');};
const dismissSheet=async()=>{if(await p.eval(`!!document.querySelector('.sheet-backdrop')`))await p.key('Escape','Escape',0,27);await p.waitFor(`!document.querySelector('.sheet-backdrop')`);if(mobile&&await p.eval(`!!document.querySelector('.drawer-layer.open')`)){await p.tap(w-12,90);await p.waitFor(`!document.querySelector('.drawer-layer.open')`);} };
const pick=async(label)=>{await menuPick(p,label,mobile);await p.waitFor(`!document.querySelector('.sheet-backdrop')`);check(true,`${label} closes its menu sheet`);};
const menu=async()=>{await dismissSheet();await p.click('[aria-label="Web view options"]');await sleep(100);};
if(mobile){
await menu();await pick('Desktop site');await sleep(400);
let st=await browser();check(!st.tabs.find(t=>t.board_id===id).size.mobile,'Desktop site turns off mobile metrics without identity overrides');check(st.tabs.find(t=>t.board_id===id).size.w>=1024,'Desktop site requests readable desktop width');
await menu();await pick('Mobile site');await sleep(400);st=await browser();check(st.tabs.find(t=>t.board_id===id).size.mobile,'Mobile site restores phone touch viewport');
}
let st;
if(mobile){
 await fetch(`${MOCK}/__mock/browser/focus?board=${id}`,{method:'POST'});
 await p.waitFor(`document.querySelector('.wv-kbd')`);
 const pill=await p.box('.wv-kbd');check(pill.w>=44&&pill.h>=44,`${w}: Type pill is at least 44 px`);
 const k=await p.box('.wv-keyboard');check(k.w>=44&&k.h>=44,`${w}: keyboard target is at least 44 px`);
}
await p.eval(`window.__copied='';Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async text=>window.__copied=text,readText:async()=>{throw Error('not allowed')}}})`);
await keyboard();check(await p.eval(`document.activeElement?.classList.contains('wv-keys')`),'Keyboard control focuses the one shared keyboard bridge');
await p.key('f','KeyF',2,70);await p.waitFor(`document.querySelector('[aria-label="Find in page"]')`);await p.type('Homework');await p.key('Enter');await sleep(100);check(await p.eval(`document.querySelector('.wv-find [role=status]')?.textContent==='Found'`),'Find feedback visible');check(await p.eval(`(()=>{const status=getComputedStyle(document.querySelector('.wv-find [role=status]')),bar=getComputedStyle(document.querySelector('.wv-find'));const rgb=s=>s.match(/[\\d.]+/g).slice(0,3).map(Number);const lum=s=>rgb(s).map(v=>{v/=255;return v<=.04045?v/12.92:((v+.055)/1.055)**2.4}).reduce((n,v,i)=>n+v*[.2126,.7152,.0722][i],0);const a=lum(status.color),b=lum(bar.backgroundColor);return bar.columnGap==='6px'&&bar.backgroundColor!=='rgba(0, 0, 0, 0)'&&bar.borderBottomWidth==='1px'&&(Math.max(a,b)+.05)/(Math.min(a,b)+.05)>=4.5})()`),`${w} ${theme}: Find resolves theme colors and readable contrast`);
await p.key('Escape');await p.waitFor(`!document.querySelector('.wv-find')`);st=await browser();check(st.log.some(e=>e.board_id===id&&e.page?.type==='clear_find'),'Escape clears Find selection through the same close routine');
const setQuery=async(text,submit=true)=>p.eval(`(()=>{const e=document.querySelector('[aria-label="Find in page"]');e.value=${JSON.stringify(text)};e.dispatchEvent(new Event('input',{bubbles:true}));${submit?"e.closest('form').requestSubmit()":""}})()`);
await keyboard();await p.key('f','KeyF',2,70);await p.waitFor(`document.querySelector('.wv-find')`);await setQuery('T12-delayed');await setQuery('missing-word');await sleep(650);check(await p.eval(`document.querySelector('.wv-find [role=status]')?.textContent==='Not found'`),'Old query reply cannot overwrite a newer Find generation');
await setQuery('T12-delayed');await rpc('browser_nav',{board_id:id,action:{type:'go',url:'https://example.test/homework'}});await sleep(650);check(await p.eval(`document.querySelector('.wv-find [role=status]')?.textContent===''`),'Navigation invalidates pending Find reply even at the same address');await p.click('[aria-label="Close find"]');
// Paste fallback stays local until the user explicitly submits it.
await menu();await pick('Paste');await p.waitFor(`document.querySelector('[aria-label="Text to paste"]')`);await p.type('phone pasted homework');await p.click('.wv-dialog .btn-primary');await sleep(100);st=await browser();check(st.log.some(e=>e.board_id===id&&e.event?.type==='text'&&e.event.text==='phone pasted homework'),'Phone paste arrives as text, not keys or a host clipboard request');
const r=await p.box('.wv-canvas');
if(mobile){await p.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[{x:r.x+100,y:r.y+100,id:0}]});await sleep(650);await p.send('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});}
else {for(const type of ['mousePressed','mouseReleased'])await p.send('Input.dispatchMouseEvent',{type,x:r.x+100,y:r.y+100,button:'right',clickCount:1});}
await sleep(100);
check(await p.eval(`[...document.querySelectorAll('.menu-item')].some(e=>e.textContent.includes('Copy text'))`),'Page context offers safe text/link/paste commands');
st=await browser();check(st.log.some(e=>e.board_id===id&&e.page?.type==='context'),'Context uses the canvas coordinate');
if(mobile){check(st.log.some(e=>e.board_id===id&&e.page?.select_word),'Long press asks for a word');check(st.log.some(e=>e.board_id===id&&e.event?.type==='touch'&&e.event.kind==='cancel'),'Long press cancels pending page tap');}
await pick('Copy text');check(await p.eval(`window.__copied==='Homework selection'`),'Copy writes selection to device clipboard, not host clipboard');
// Tab groups switch pages in the same tile; inactive pages detach.
const next=await rpc('browser_new_tab',{board_id:id,url:'https://example.test/lecture'});
await p.waitFor(`document.querySelectorAll('.wv-tabs [role=tab]').length===2`);await sleep(300);
check(await p.eval(`document.querySelectorAll('.wv-tabs [role=tab]').length===2`),'Independent pages have visible tabs');
check(await p.eval(`(()=>{const e=document.querySelector('.wv-tab:last-of-type .wv-tab-title');e.scrollIntoView({block:'nearest'});const r=e.getBoundingClientRect();return r.width>=44&&r.height>=44&&e.contains(document.elementFromPoint(r.x+Math.min(r.width/2,20),r.y+r.height/2))})()`),`${w} ${theme}: tab title is 44 px and wins the hit test`);
if(mobile){await p.click('.wv-tab-count');await p.waitFor(`document.querySelector('.menu-pop')`);const row=await p.box('.menu-pop .menu-item:last-child');await p.tap(row.cx,row.cy);await p.waitFor(`!document.querySelector('.sheet-backdrop')`);check(true,'Tab switch action closes its sheet');}
await p.click('.wv-tab:last-of-type .wv-tab-title');await sleep(300);st=await browser();check(st.attached[next.id]>0,'Switching tab attaches that saved page');
await keyboard();await p.key('f','KeyF',2,70);await p.waitFor(`document.querySelector('.wv-find')`);await setQuery('T12-delayed');await p.click('.wv-tab:first-child .wv-tab-title');await sleep(650);check(await p.eval(`document.querySelector('.wv-find [role=status]')?.textContent!=='Found'`),'Page switch invalidates a pending Find reply');if(await p.eval(`!!document.querySelector('.wv-find')`))await p.click('[aria-label="Close find"]');
await p.click('.wv-tab:first-child .wv-tab-title');await sleep(400);st=await browser();check(st.attached[next.id]===0,'Inactive page has no frame subscription');
await keyboard();await p.key('Tab','Tab',2,9);await sleep(250);check(await p.eval(`__mod('lib/workspace.ts').then(m=>m.workspace.panes[m.workspace.focused()].content.boardId==='${next.id}')`),'Ctrl+Tab switches to next saved page');
await keyboard();await p.key('Tab','Tab',10,9);await sleep(250);check(await p.eval(`__mod('lib/workspace.ts').then(m=>m.workspace.panes[m.workspace.focused()].content.boardId==='${id}')`),'Ctrl+Shift+Tab switches back');
await keyboard();await p.key('t','KeyT',2,84);await p.waitFor(`document.querySelector('.dialog-body input')`);await p.type('https://example.test/shortcut');await p.click('.dialog-body .btn-primary');await p.waitFor(`document.querySelectorAll('.wv-tabs [role=tab]').length===3`);check(true,'Ctrl+T opens another saved page');
await keyboard();await p.key('w','KeyW',2,87);await p.waitFor(`document.querySelectorAll('.wv-tabs [role=tab]').length===2`);check(true,'Ctrl+W closes the active saved page');
await p.click('.wv-tab:first-child .wv-tab-title');await sleep(200);
await p.click('.wv-tab:last-of-type .icon-btn');await sleep(200);
check(!await p.eval(`!!document.querySelector('.drawer-layer.open')`),'Tab close leaves no side drawer overlay');if(mobile){await dismissSheet();await rpc('browser_nav',{board_id:id,action:{type:'go',url:'https://example.test/next'}});await sleep(300);const e=await p.box('.wv-canvas');check(await p.eval(`document.elementFromPoint(${e.x+8},${e.y+90})===document.querySelector('.wv-canvas')`),'Back swipe starts on canvas, not a sheet backdrop');await p.eval(`(()=>{window.__edge=[];for(const type of ['pointerdown','pointermove','pointerup','pointercancel'])document.querySelector('.wv-canvas').addEventListener(type,e=>window.__edge.push({type,x:e.clientX,y:e.clientY}),{once:true});window.__edgeTarget=document.elementFromPoint(${e.x+8},${e.y+90})?.className;})()`);await p.send('Input.dispatchTouchEvent',{type:'touchStart',touchPoints:[{x:e.x+8,y:e.y+90,id:0}]});await sleep(50);await p.send('Input.dispatchTouchEvent',{type:'touchMove',touchPoints:[{x:e.x+105,y:e.y+92,id:0}]});await sleep(50);await p.send('Input.dispatchTouchEvent',{type:'touchEnd',touchPoints:[]});await sleep(300);st=await browser();const back=st.log.some(l=>l.board_id===id&&l.nav?.type==='back');if(!back)console.log('edge diagnostic',await p.eval(`({events:__edge,target:__edgeTarget})`),st.tabs.find(t=>t.board_id===id));check(back,'Left-edge phone swipe invokes Back, not page drag');}
await rpc('mock_file_chooser',{board_id:id,multiple:false});await p.waitFor(`document.querySelector('.wv-file-dialog input')`);
await p.eval(`(()=>{const d=new DataTransfer();d.items.add(new File([new Uint8Array(20*1024*1024)],'lecture.pdf'));const e=document.querySelector('.wv-file-dialog input');e.files=d.files;e.dispatchEvent(new Event('change',{bubbles:true}))})()`);
await p.waitFor(`!document.querySelector('.wv-file-dialog')`,30000);st=await browser();const chunks=st.log.filter(e=>e.board_id===id&&e.upload==='chunk');check(chunks.length===80&&chunks.every(e=>e.bytes<=256*1024),'20 MB selected file travels in exactly 80 bounded chunks');
check(st.log.some(e=>e.board_id===id&&e.upload==='finish'),'20 MB upload completes explicitly');
await menu();await pick('Bookmark this page');check(await p.eval(`__mod('lib/webViewPrefs.ts').then(m=>m.webBookmarks('${id}').includes('${id}'))`),'Bookmark stores only saved board id');await menu();await pick('Remove bookmark');check(await p.eval(`__mod('lib/webViewPrefs.ts').then(m=>!m.webBookmarks('${id}').includes('${id}'))`),'Bookmark can be removed through the menu');
if(!mobile){
for(const [button,modifiers,label]of [['middle',0,'Middle'],['left',2,'Ctrl'],['left',4,'Cmd']]){
 const q=await p.box('.wv-canvas');for(const type of ['mousePressed','mouseReleased'])await p.send('Input.dispatchMouseEvent',{type,x:q.x+70,y:q.y+90,button,modifiers,clickCount:1});
 await p.waitFor(`document.querySelectorAll('.wv-tabs [role=tab]').length===2`);check(true,`${label}+click opens the resolved link as a saved page`);
 await keyboard();await p.key('w','KeyW',2,87);await p.waitFor(`document.querySelectorAll('.wv-tabs [role=tab]').length===1`);
}
}
await menu();await pick('Save page as PDF');await sleep(100);st=await browser();check(st.log.some(e=>e.board_id===id&&e.page?.type==='print_pdf'),'PDF export is an explicit device action');
await p.shot(`${SHOTS}/webview-controls-${w}-${theme}.png`);check(await p.eval(`document.body.scrollWidth<=innerWidth+1`),`${w} ${theme}: no horizontal overflow`);
if(w===1440&&theme==='light'){
const privacy=await p.eval(`(async()=>{const prefs=await __mod('lib/webViewPrefs.ts'),store=await __mod('lib/store.ts');const {state,setState,privateIds}=store;const normal='${id}',sid='T12-private-session',pid='T12-private-board',savedId=state.conn.serverId,savedState=state.conn.state;const source=JSON.parse(JSON.stringify(state.sessions[state.boards[normal].session_id]));source.info={...source.info,id:sid,private:true};const install=()=>{setState('sessions',sid,source);setState('boards',pid,{...state.boards[normal],id:pid,session_id:sid,title:'Private fixture'});privateIds.add(sid)};const tick=()=>new Promise(r=>setTimeout(r,20));
prefs.bookmarkPage(normal);install();prefs.bookmarkPage(pid);prefs.rememberTabGroup([pid,normal]);await tick();const scoped=prefs.webBookmarks(pid).join()===pid&&prefs.webBookmarks(normal).includes(normal)&&!prefs.webBookmarks(normal).includes(pid)&&prefs.webTabGroup(pid).join()===pid;const noDisk=Object.keys(localStorage).filter(k=>k.startsWith('tessera-web-pages:')).every(k=>!localStorage.getItem(k).includes(pid));
setState('sessions',sid,undefined);await tick();install();const close=prefs.webBookmarks(pid).length===0&&prefs.webTabGroup(pid).length===0;
prefs.bookmarkPage(pid);setState('conn','state','disconnected');await tick();setState('conn','state',savedState);await tick();const reconnect=prefs.webBookmarks(pid).length===0&&prefs.webBookmarks(normal).includes(normal);
setState('conn','serverId','T12-other-server');await tick();const switched=prefs.webBookmarks(normal).length===0;setState('conn','serverId',savedId);await tick();const restored=prefs.webBookmarks(normal).includes(normal);prefs.bookmarkPage(normal);setState('boards',pid,undefined);setState('sessions',sid,undefined);return{scoped,noDisk,close,reconnect,switched,restored};})()`);
for(const [key,value]of Object.entries(privacy))check(value,`Bookmarks/groups privacy isolation: ${key}`);
}
await p.close();await rpc('delete_board',{id});await c.close();
}
finish();process.exit(process.exitCode??0);
