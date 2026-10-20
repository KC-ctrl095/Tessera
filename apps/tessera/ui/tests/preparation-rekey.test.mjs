// Real preparation, moveDrafts and both public rekey operations. Only storage,
// geometry and server effects are stubbed for a deterministic two-mosaic fixture.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { registerHooks } from 'node:module';
import test from 'node:test';
import ts from 'typescript';
// This exercises reactive client memos, not Solid's non-reactive SSR exports.
// Resolve Solid (including its store's dependency) as the browser does even
// when the generic unit runner has no global --conditions=browser flag.
const browser = registerHooks({ resolve(specifier, context, nextResolve) {
 return nextResolve(specifier, specifier === 'solid-js' || specifier === 'solid-js/store'
  ? { ...context, conditions: [...context.conditions, 'browser'] } : context);
} });
const scratch=mkdtempSync(new URL('../.preparation-rekey-test-',import.meta.url));
const source=readFileSync(new URL('../src/lib/workspace.ts',import.meta.url),'utf8');
const ast=ts.createSourceFile('workspace.ts',source,ts.ScriptTarget.Latest,true);
const declaration=name=>{
 const node=ast.statements.find(node=>node.name?.text===name || node.declarationList?.declarations.some(d=>d.name.text===name));
 assert.ok(node,`real declaration ${name}`);return node.getText(ast);
};
const actual=['leaves','mapTree','removeLeaf','insertAt','makeWs','rekey','arriving','setMosaicLocal','moveToWorkspace','moveDrafts','clearDraft'].map(declaration).join('\n');
const memory=source.slice(source.indexOf('export type ComposerMemory'),source.indexOf('export const getDraft'));
const staging=source.slice(source.indexOf('const [staged, setStaged]'),source.indexOf('/** A tile showing a private chat'));
const prelude=`import {batch,createMemo,createSignal,createRoot} from 'solid-js';import {createStore,produce} from 'solid-js/store';
${declaration('arrEq')}
const [drafts,setDrafts]=createStore({});let draftsChanged=false;const saveDrafts=()=>{};const draftListeners=new Set();const privateDrafts=new Set();const state={sessions:{}};
const [panes,setPanes]=createStore({});const [active,setActive]=createSignal(0);const list=createRoot(()=>[makeWs(),makeWs()]);
const ws={panes,setPanes,list,active,setActive,serverKey:()=> 'server',cur:()=>list[active()]};const workspace={focused:()=>ws.cur().focused()};
const wsOf=id=>list.find(w=>w.order().includes(id));const WORKSPACES=2;const placeholders=new Set(),tileExtra=new Map(),remoteMeta=[{},{}];
let sequence=0;const uid=prefix=>prefix+(++sequence);const commit=()=>{};const document={activeElement:null};
const toast=()=>{};const focusComposer=()=>{};const focusPane=id=>wsOf(id)?.setFocused(id);const tooMany=()=>false;const pixelRectInTree=()=>({w:1000,h:500});const clamp=(n,lo,hi)=>Math.min(hi,Math.max(lo,n));
const draftIdentity=id=>JSON.stringify(['server',id,panes[id]?.content.sessionId??null]);const isPrivatePane=id=>!!panes[id]?.content.draft?.private;
const getDraft=(pane,identity=draftIdentity(pane))=>drafts[identity]?.text??'';const setPaneContent=(id,content)=>setPanes(id,'content',content);const openPane=()=>null;
export const fixture={ws,entry:identity=>attachmentDrafts[identity],identity:draftIdentity,
 addPane(){setPanes('old',{id:'old',content:{type:'chat',sessionId:null}});list[0].setRoot({t:'leaf',pane:'old'});list[0].setFocused('old');return draftIdentity('old')}};
`;
let count=0;
const fresh=async()=>{
 const file=join(scratch,`fixture-${count++}.mjs`);
 writeFileSync(file,ts.transpileModule(prelude+actual+memory+staging,{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText);
 return import(file);
};
const attachment={id:'delayed',name:'delayed.bin',mime:'application/octet-stream',data:'REVMSVZFUkVE'};
try{
 for(const path of ['Keep on this device','move into a device-local mosaic'])for(const fails of [false,true])await test(`${path}: delayed ${fails?'failure':'attachment'} follows the rekey and completes preparation`,async()=>{
  const m=await fresh(),old=m.fixture.addPane(),handle=m.prepareAttachments(old),token=m.fixture.entry(old).token;
  let resolve,reject;const decode=new Promise((yes,no)=>{resolve=yes;reject=no});
  const done=decode.then(value=>handle.append(value),()=>{}).finally(()=>handle.finish());
  assert.equal(m.fixture.entry(old).preparing,1);
  if(path==='Keep on this device')m.setMosaicLocal(0,true);
  else{m.setMosaicLocal(1,true);m.moveToWorkspace('old',1,true);}
  const pane=m.fixture.ws.cur().order()[0],moved=m.fixture.identity(pane);
  assert.notEqual(moved,old);assert.equal(m.fixture.entry(old),undefined);assert.equal(m.fixture.entry(moved).token,token);assert.equal(m.fixture.entry(moved).preparing,1);
  if(fails)reject(new Error('deterministic decode failure'));else resolve(attachment);await done;
  assert.equal(m.fixture.entry(moved).preparing,0);assert.equal(m.composerAttachments(moved).length,fails?0:1);
  if(!fails)assert.equal(m.composerAttachments(moved)[0].name,'delayed.bin');
  assert.doesNotMatch(m.reloadWarning(),/Still preparing/,'preparation must not strand Reload');
  // Unsent attachments still intentionally block Reload until sent/removed.
  m.setComposerAttachments(moved,[]);assert.equal(m.reloadWarning(),'','Reload unblocked after clearing the delivered attachment');
  handle.finish();assert.equal(m.fixture.entry(moved).preparing,0,'finish is idempotent');
 });
 await test('Discard removes the rekeyed token and stale completion cannot touch a recreated entry',async()=>{
  const m=await fresh(),old=m.fixture.addPane(),handle=m.prepareAttachments(old);m.setMosaicLocal(0,true);
  const moved=m.fixture.identity(m.fixture.ws.cur().order()[0]),token=m.fixture.entry(moved).token;m.discardPrivateDraft(moved);
  const next=m.prepareAttachments(moved);assert.notEqual(m.fixture.entry(moved).token,token);
  handle.append(attachment);handle.finish();assert.equal(m.composerAttachments(moved).length,0);assert.equal(m.fixture.entry(moved).preparing,1);
  next.finish();assert.equal(m.fixture.entry(moved).preparing,0);assert.equal(m.reloadWarning(),'');
 });
}finally{browser.deregister();rmSync(scratch,{recursive:true,force:true});}
