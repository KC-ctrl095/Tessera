// Built UI, a real throwaway encrypted server, then the mock for failure fixtures.
// Never serves dev TS modules; deployment B has genuinely different emitted hashes.
import { build } from 'vite';
import { createServer, request } from 'node:http';
import { spawn, spawnSync } from 'node:child_process';
import { readFileSync, existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve, join, extname } from 'node:path';
import { fileURLToPath } from 'node:url';
const root = resolve(fileURLToPath(new URL('..', import.meta.url)));
const scratch = mkdtempSync(join(tmpdir(), 'built-ui-'));
const children = [];
const port = Number(process.env.VITE_PORT || 6591), backendPort = Number(process.env.MOCK_PORT || 9291);
let active = resolve(root, 'dist');
const original = active, newer = join(scratch, 'updated');
process.env.E2E_BROWSER_PID_FILE = join(scratch, 'browser.json');
let server;
const wait = async fn => { for(let i=0;i<200;i++){if(await fn())return;await new Promise(r=>setTimeout(r,100));}throw Error('server startup timeout'); };
const child = (command,args,env={}) => { const p=spawn(command,args,{cwd:root,env:{...process.env,...env},stdio:['ignore','pipe','pipe']});children.push(p);p.stdout.on('data',d=>process.stdout.write(d));p.stderr.on('data',d=>process.stderr.write(d));return p; };
const completion = p => new Promise((res,rej)=>{p.on('exit',code=>code===0?res():rej(Error(`child exit ${code}`)));p.on('error',rej);});
try {
  await build({root,configFile:resolve(root,'vite.config.ts'),build:{outDir:newer,manifest:true},plugins:[{name:'test-real-deployment-B',renderChunk(code,chunk){if(chunk.isEntry || chunk.facadeModuleId?.endsWith('/chat/RecallCard.tsx'))return {code:code+'\n;globalThis.__B=1;',map:null};}}]});
  const entryA=readFileSync(join(original,'index.html'),'utf8').match(/<script[^>]+src="([^"]+)"/)[1];
  const entryB=readFileSync(join(newer,'index.html'),'utf8').match(/<script[^>]+src="([^"]+)"/)[1];
  if(entryA===entryB)throw Error('test deployment must have a genuinely new emitted entry hash');
  const a=JSON.parse(readFileSync(join(original,'parts.json'))),b=JSON.parse(readFileSync(join(newer,'parts.json')));
  if(a.RecallCard.file===b.RecallCard.file || existsSync(join(newer,a.RecallCard.file)))throw Error('deployment must delete the actual old RecallCard hash');
  server=createServer((req,res)=>{
    const path=new URL(req.url,'http://built.test').pathname;
    if(path==='/__test/update'){active=newer;res.end('updated');return;}
    if(path==='/__test/reset'){active=original;res.end('original');return;}
    if(path.startsWith('/api/')||path.startsWith('/web/api/')||path.startsWith('/__mock')){
      const proxy=request({hostname:'127.0.0.1',port:backendPort,path:req.url,method:req.method,headers:{...req.headers,...(req.headers.origin?{origin:req.headers.origin.replace(/^http:/,'https:')}:{}) ,'x-forwarded-proto':'https','x-forwarded-for':'198.51.100.23'}},r=>{res.writeHead(r.statusCode,r.headers);r.pipe(res);});proxy.on('error',()=>{res.writeHead(502);res.end();});req.pipe(proxy);return;
    }
    const relative=path.replace(/^\/web\//,'').replace(/^\//,'')||'index.html';
    const file=resolve(active,relative);
    if(!file.startsWith(active+'/')||!existsSync(file)){res.writeHead(404);res.end('missing emitted file');return;}
    res.setHeader('Cache-Control','no-store');
    res.setHeader('Content-Type',({'.js':'text/javascript','.css':'text/css','.html':'text/html','.json':'application/json'})[extname(file)]||'application/octet-stream');
    res.end(readFileSync(file));
  });
  server.on('upgrade',(req,socket,head)=>{
    socket.on('error',()=>socket.destroy());
    const proxy=request({hostname:'127.0.0.1',port:backendPort,path:req.url,headers:{...req.headers,...(req.headers.origin?{origin:req.headers.origin.replace(/^http:/,'https:')}:{}) ,'x-forwarded-proto':'https','x-forwarded-for':'198.51.100.23'}});
    proxy.on('upgrade',(r,upstream,h)=>{upstream.on('error',()=>upstream.destroy());socket.on('close',()=>upstream.destroy());upstream.on('close',()=>socket.destroy());socket.write(`HTTP/1.1 ${r.statusCode} ${r.statusMessage}\r\n`+Object.entries(r.headers).map(([k,v])=>`${k}: ${v}`).join('\r\n')+'\r\n\r\n');if(h.length)socket.write(h);if(head.length)upstream.write(head);upstream.pipe(socket);socket.pipe(upstream);});proxy.on('error',()=>socket.destroy());proxy.end();
  });
  await new Promise(r=>server.listen(port,'127.0.0.1',r));
  const home=join(scratch,'home');
  const {mkdirSync}=await import('node:fs');mkdirSync(home,{recursive:true});
  const env={HOME:home,TESSERA_HOME:home,XDG_CONFIG_HOME:join(home,'config'),XDG_DATA_HOME:join(home,'data'),XDG_CACHE_HOME:join(home,'cache'),RUST_LOG:'warn'};
  const bin=process.env.TESSERA_BIN||join(process.env.HOME,'.local/bin/tessera');
  const real=child(bin,['serve','--bind',`127.0.0.1:${backendPort}`,'--web',original,'--no-scheduler'],env);
  await wait(()=>fetch(`http://127.0.0.1:${backendPort}/api/health`).then(r=>r.ok).catch(()=>false));
  spawnSync(bin,['sessions','--plain'],{env:{...process.env,...env},timeout:30000,stdio:'ignore'});
  const common={APP:`http://127.0.0.1:${port}/web/`,MOCK:`http://127.0.0.1:${backendPort}`,BUILT_DIST:original,BUILT_UPDATED:newer};
  await completion(child(process.execPath,['tests/e2e/built-ui.mjs'],{...common,BUILT_SCENARIO:'cold',REAL_TOKEN:join(home,'cli-token')}));
  real.kill('SIGTERM');await new Promise(r=>real.once('exit',r));
  const mock=child(process.execPath,['dev/mock-server.mjs'],{PORT:String(backendPort),QUIET:'1'});
  await wait(()=>fetch(`http://127.0.0.1:${backendPort}/api/health`).then(r=>r.ok).catch(()=>false));
  await completion(child(process.execPath,['tests/e2e/built-ui.mjs'],{...common,APP:`http://127.0.0.1:${port}/`,BUILT_SCENARIO:'recovery',REAL_TOKEN:''}));
  mock.kill('SIGTERM');
  console.log('BUILT_OUTPUT_OK: real paired network set and built CSS/transient/stale reload recovery');
} finally {
  for(const p of children)if(p.exitCode===null)p.kill('SIGTERM');
  server?.close();
  try { const {pid,start}=JSON.parse(readFileSync(process.env.E2E_BROWSER_PID_FILE,'utf8'));const now=readFileSync(`/proc/${pid}/stat`,'utf8').split(') ').pop().split(' ')[19];if(now===start)process.kill(pid,'SIGTERM'); }catch{}
  rmSync(scratch,{recursive:true,force:true});
}
