// A web page never names a file on the server. Only files picked on this
// device are copied, with a chooser belonging to the current page.
import {SHOTS,MOCK,check,device,finish,mock,run,signIn,sleep,wsClient} from './cdp.mjs';
const sid='ses_7d3a91e5c4b2';
const ws=(p,js)=>p.eval(`__mod('lib/workspace.ts').then(m=>{${js}})`);
const uploads=async()=>((await (await fetch(`${MOCK}/__mock/browser`)).json()).log||[]).filter(l=>l.files);
run(async()=>{
 await mock('/__mock/reset');await mock('/__mock/mosaics/reset');
 const probe=await wsClient('probe');
 const p=await device();await p.viewport(1440,900);await signIn(p,'Desktop');
 await ws(p,`m.openSession('${sid}',{pane:m.workspace.focused()})`);
 const b=(await probe.rpc('create_board',{session_id:sid,title:'Homework',placed:false,content:{type:'url',url:'https://example.org/homework'}})).result;
 const pane=await ws(p,`return m.openBoard('${b.id}')`);await sleep(1000);
 const picker=`[data-pane-id="${pane}"] input[type=file]`;
 await probe.rpc('mock_file_chooser',{board_id:b.id,multiple:true});
 await p.waitFor(`!!document.querySelector('${picker}')`,6000);
 check(await p.eval(`document.querySelector('${picker}').multiple`),'multiple chooser matches the page');
 check((await uploads()).length===0,'a page asking does not upload anything');
 check(await p.eval(`document.querySelector('[aria-label="Choose files for this page"]').textContent.includes('Only files you choose')`),'clear consent and size limit');
 await p.shot(`${SHOTS}/webview-files-desktop.png`);
 await p.eval(`(()=>{const d=new DataTransfer();d.items.add(new File(['my homework'],'homework.txt',{type:'text/plain'}));const e=document.querySelector('${picker}');e.files=d.files;e.dispatchEvent(new Event('change',{bubbles:true}))})()`);
 await p.waitFor(`!document.querySelector('${picker}')`,4000);
 const log=await uploads();check(log.length===1&&log[0].board_id===b.id,'one upload to the exact page');
 check(log[0]?.files?.[0]?.name==='homework.txt'&&Buffer.from(log[0]?.files?.[0]?.data||'','base64').toString()==='my homework','picked filename and exact bytes, no path');
 check(Object.keys(log[0]?.files?.[0]||{}).sort().join(',')==='data,mime,name','no host path is accepted');
 await probe.rpc('mock_file_chooser',{board_id:b.id,multiple:false});
 await p.waitFor(`!!document.querySelector('${picker}')`,4000);
 check(!(await p.eval(`document.querySelector('${picker}').multiple`)),'single-file chooser stays single');
 await p.clickText(`[data-pane-id="${pane}"] .wv-dialog button`,'Cancel');await sleep(300);
 check((await uploads()).at(-1)?.files?.length===0,'cancel sends no selected bytes');
 await probe.rpc('mock_file_chooser',{board_id:b.id,multiple:true});
 await p.waitFor(`!!document.querySelector('${picker}')`,4000);
 await p.eval(`(()=>{const d=new DataTransfer();d.items.add(new File([new Uint8Array(200*1024*1024+1)],'too-big.txt'));const e=document.querySelector('${picker}');e.files=d.files;e.dispatchEvent(new Event('change',{bubbles:true}))})()`);
 await sleep(300);check((await uploads()).length===2,'oversized file refused before upload');
 check(await p.eval(`document.body.textContent.includes('200 MB')`),'limit is explained');
 for (const [label,w,h,touch,dpr] of [['desktop',1440,900,false,1],['phone',344,882,true,2.625],['fold-open',673,841,true,2.625]]) {
   await p.viewport(w,h,touch,dpr);await sleep(500);
   for (const theme of ['dark','light']) {
     await p.theme(theme);await sleep(200);
     check(await p.eval(`document.documentElement.scrollWidth<=innerWidth+1`),`picker fits ${label} ${theme}`);
     check(await p.eval(`(()=>{const r=document.querySelector('[aria-label="Choose files for this page"]').getBoundingClientRect();return r.left>=0&&r.right<=innerWidth+1})()`),`dialog contained on ${label} ${theme}`);
     await p.shot(`${SHOTS}/webview-files-${label}-${theme}.png`);
   }
 }
 await probe.rpc('browser_nav',{board_id:b.id,action:{type:'go',url:'https://example.org/next'}});await sleep(300);
 check(!(await p.eval(`!!document.querySelector('${picker}')`)),'navigation dismisses the expired chooser');
 await p.close();probe.close?.();finish();
});
