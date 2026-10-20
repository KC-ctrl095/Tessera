// Fault-inject only the popup's mandatory setup in a disposable browser pipe.
import fs from 'node:fs';
import {spawn} from 'node:child_process';
const [mode,bin,spki,marker,...args]=process.argv.slice(2);
const chrome=spawn(bin,[`--ignore-certificate-errors-spki-list=${spki}`,...args],{stdio:['ignore','ignore','inherit','pipe','pipe']});
const popup=new Set(), ids=new Map();
function frames(stream,consume){let b=Buffer.alloc(0);stream.on('data',c=>{b=Buffer.concat([b,c]);let i;while((i=b.indexOf(0))>=0){const v=JSON.parse(b.subarray(0,i));b=b.subarray(i+1);consume(v)}})}
const input=fs.createReadStream(null,{fd:3,autoClose:false});
const output=fs.createWriteStream(null,{fd:4,autoClose:false});
let pdfBytes=0;
frames(input,v=>{
 // A CDP error needs both code and message; an incomplete envelope is ignored.
 if(mode==='upload-failure' && v.method==='DOM.setFileInputFiles') { fs.writeFileSync(marker,'handoff-failed');output.write(JSON.stringify({id:v.id,sessionId:v.sessionId,error:{code:-32000,message:'fixture file input failure'}})+'\0');return; }
 if(mode==='pdf-oversize' || mode==='pdf-cancel') {
   if(v.method==='Page.printToPDF') {const reply=()=>output.write(JSON.stringify({id:v.id,sessionId:v.sessionId,result:{stream:'T12Oversized'}})+'\0');if(mode==='pdf-cancel'){fs.writeFileSync(marker,'generating');setTimeout(reply,200);}else reply();return;}
   if(v.params?.handle==='T12Oversized') {
     if(v.method==='IO.close') {fs.writeFileSync(marker,`closed:${pdfBytes}`);output.write(JSON.stringify({id:v.id,sessionId:v.sessionId,result:{}})+'\0');return;}
     if(v.method==='IO.read') {const size=v.params.size;if(!Number.isInteger(size)||size<1||size>65536)throw Error('unbounded read');pdfBytes+=size;output.write(JSON.stringify({id:v.id,sessionId:v.sessionId,result:{data:'x'.repeat(size),eof:false}})+'\0');return;}
   }
 }
 if(popup.has(v.sessionId)) {
   const guard=v.method==='Page.addScriptToEvaluateOnNewDocument'&&v.params?.source?.includes('__tsrNoKeys');
   const fetch=v.method==='Fetch.enable';
   if(guard&&mode==='guard-error'){v.params.source=123;fs.writeFileSync(marker,'guard-error');}
   if(guard&&mode==='guard-id'){ids.set(v.id,'guard-id');}
   if(fetch&&((mode==='fetch-error'&&v.params?.patterns?.[0]?.urlPattern!=='*')||(mode==='network-hold-error'&&v.params?.patterns?.[0]?.urlPattern==='*'))){v.params.patterns='invalid';fs.writeFileSync(marker,mode);}
 }
 chrome.stdio[3].write(JSON.stringify(v)+'\0');
});
frames(chrome.stdio[4],v=>{
 if(v.method==='Target.attachedToTarget'&&v.params?.targetInfo?.type==='page') {
   if(v.params.targetInfo.openerId) popup.add(v.params.sessionId);
 }
 if(ids.has(v.id)&&v.result){delete v.result.identifier;ids.delete(v.id);fs.writeFileSync(marker,'guard-id');}
 output.write(JSON.stringify(v)+'\0');
});
chrome.on('exit',code=>process.exit(code??1));
process.on('SIGTERM',()=>{chrome.kill('SIGTERM');});
