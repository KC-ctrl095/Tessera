// Real workspace memory/Open/Discard functions with a small deterministic pane fixture.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';
const scratch=mkdtempSync(new URL('../.reload-identity-test-',import.meta.url));
const source=readFileSync(new URL('../src/lib/workspace.ts',import.meta.url),'utf8');
const body=source.slice(source.indexOf('export type ComposerMemory'),source.indexOf('export const getDraft'));
const staging=source.slice(source.indexOf('const [staged, setStaged]'),source.indexOf('/** A tile showing a private chat'));
const prelude=`import {createStore} from 'solid-js/store'; import {batch} from 'solid-js';
const [drafts,setDrafts]=createStore({});let draftsChanged=false;const saveDrafts=()=>{};const draftListeners=new Set();const privateDrafts=new Set();const state={sessions:{same:{info:{title:'Same chat'}}}};
const ws={panes:{},serverKey:()=> 'server'};let focused=null;const workspace={focused:()=>focused};let warning='';
const toast=message=>{warning=message.message};const focusPane=id=>{focused=id};const focusComposer=()=>{};
const draftIdentity=id=>JSON.stringify(['server',id,ws.panes[id]?.content.sessionId??null]);
const isPrivatePane=id=>!!ws.panes[id]?.content.draft?.private;
const getDraft=(id,identity=draftIdentity(id))=>drafts[identity]?.text??'';
const setPaneContent=(id,content)=>{ws.panes[id].content=content};
const openPane=(content,target,zone,id)=>{ws.panes[id]={id,content};focused=id;return id};
export const fixture={ws,privateDrafts,drafts,focused:()=>focused,warning:()=>warning,draftIdentity,
 pane(id,chat=null,privateChat=false){ws.panes[id]={id,content:{type:'chat',sessionId:chat,draft:{private:privateChat}}};return draftIdentity(id)},
 text(identity,text){const [,paneId]=JSON.parse(identity);setDrafts(identity,{paneId,text,private:true});privateDrafts.add(identity)}};
`;
let count=0;
async function fresh(){const file=join(scratch,`fixture-${count++}.mjs`);writeFileSync(file,ts.transpileModule(prelude+body+staging,{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText);return import(file);}
const attachment={id:'file',name:'private.bin',mime:'application/octet-stream',data:'QQ=='};
try{
 for(const retained of [false,true])await test(`${retained?'retained':'mounted'} Discard clears text, staged attachments, memory and marker`,async()=>{
  const m=await fresh(),id=m.fixture.pane('own',null,true);m.fixture.text(id,'private text');m.setComposerAttachments(id,[attachment]);m.stageAttachments('own',[attachment]);
  if(retained)delete m.fixture.ws.panes.own;
  assert.equal(m.privateReloadDrafts().length,1);m.discardPrivateDraft(id);
  assert.equal(m.fixture.drafts[id],undefined);assert.equal(m.composerAttachments(id).length,0);assert.equal(m.fixture.privateDrafts.has(id),false);assert.equal(m.privateReloadDrafts().length,0);assert.equal(m.reloadWarning(),'');
 });
 await test('Discard during preparation cannot resurrect the identity on completion',async()=>{
  const m=await fresh(),id=m.fixture.pane('own',null,true);const pending=m.prepareAttachments(id);assert.match(m.reloadWarning(),/Still preparing/);
  m.discardPrivateDraft(id);pending.append(attachment);pending.finish();assert.equal(m.composerAttachments(id).length,0);assert.equal(m.reloadWarning(),'');
 });
 await test('Open refuses an attachment-only destination and changes neither draft',async()=>{
  const m=await fresh(),id=m.fixture.pane('own',null,true);m.fixture.text(id,'private original');m.fixture.ws.panes.own.content={type:'chat',sessionId:'other'};const destination=m.fixture.draftIdentity('own');m.setComposerAttachments(destination,[attachment]);
  const before=JSON.stringify(m.fixture.ws.panes);m.openPrivateDraft(id);assert.equal(JSON.stringify(m.fixture.ws.panes),before);assert.equal(m.fixture.drafts[id].text,'private original');assert.equal(m.composerAttachments(destination).length,1);assert.match(m.fixture.warning(),/Nothing was moved/);
 });
 await test('Open refuses pending preparation in the original pane',async()=>{
  const m=await fresh(),id=m.fixture.pane('own',null,true);m.fixture.text(id,'private original');m.fixture.ws.panes.own.content={type:'chat',sessionId:'other'};const destination=m.fixture.draftIdentity('own');const pending=m.prepareAttachments(destination);
  m.openPrivateDraft(id);assert.equal(m.fixture.draftIdentity('own'),destination);assert.match(m.fixture.warning(),/Nothing was moved/);pending.finish();
 });
 await test('same-session collision reopens only the source identity and preserves both drafts',async()=>{
  const m=await fresh(),id=m.fixture.pane('own','same',true),other=m.fixture.pane('other','same',true);m.fixture.text(id,'first');m.fixture.text(other,'second');m.setComposerAttachments(id,[attachment]);delete m.fixture.ws.panes.own;
  m.openPrivateDraft(id);assert.equal(m.fixture.draftIdentity('own'),id);assert.equal(m.fixture.drafts[id].text,'first');assert.equal(m.fixture.drafts[other].text,'second');assert.equal(m.composerAttachments(id).length,1);assert.equal(m.fixture.focused(),'own');
 });
 await test('attachment-only private identity opens without a text-transfer requirement',async()=>{
  const m=await fresh(),id=m.fixture.pane('own',null,true);m.setComposerAttachments(id,[attachment]);delete m.fixture.ws.panes.own;
  m.openPrivateDraft(id);assert.equal(m.fixture.draftIdentity('own'),id);assert.equal(m.composerAttachments(id).length,1);assert.equal(m.fixture.drafts[id],undefined);
 });
}finally{rmSync(scratch,{recursive:true,force:true});}
