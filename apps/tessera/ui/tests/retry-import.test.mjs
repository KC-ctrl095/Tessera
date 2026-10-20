// The already-loaded bootstrap uses the emitted URL even for URL-less errors.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';
import ts from 'typescript';
const scratch=mkdtempSync(new URL('../.retry-import-test-',import.meta.url));
mkdirSync(join(scratch,'assets'));
const helper=join(scratch,'assets/chunkRecovery-test.js');writeFileSync(helper,'export const recovered=true;');
const original=readFileSync(new URL('../src/lib/retryImport.ts',import.meta.url),'utf8');
let seq=0;
const fixture=async source=>{
 const file=join(scratch,`retry-${seq++}.mjs`);
 writeFileSync(file,ts.transpileModule(original.replace('"__TESSERA_RECOVERY_URL__"',JSON.stringify(source)),{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText);
 return (await import(file)).retryImport;
};
try{
 await test('URL-less helper failure retries via bundler metadata, not the error message',async()=>{
  const source=pathToFileURL(helper).href;globalThis.location={href:source,origin:'null'};
  const retry=await fixture(source);let calls=0;const load=async()=>{calls++;throw Error('URL-less synthetic failure')};
  await assert.rejects(retry(load),/URL-less/);
  assert.equal((await retry(load)).recovered,true);assert.equal(calls,1);
 });
 await test('foreign-origin and wrong-path helper metadata fail closed',async()=>{
  globalThis.location={href:'https://tessera.test/web/',origin:'https://tessera.test'};
  for(const source of ['https://foreign.test/assets/chunkRecovery-test.js','https://tessera.test/user-content/chunkRecovery-test.js']){
   const retry=await fixture(source);const load=async()=>{throw Error('URL-less failure')};
   await assert.rejects(retry(load),/URL-less/);await assert.rejects(retry(load),/Unsafe recovery path/);
  }
 });
}finally{rmSync(scratch,{recursive:true,force:true});}
