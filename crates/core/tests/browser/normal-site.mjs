// Loopback-only homework/login fixtures. The certificate is generated for
// this throwaway run, never installed in the user's trust store.
import https from 'node:https';
import http from 'node:http';
import fs from 'node:fs';
import {execFileSync} from 'node:child_process';
const dir = process.argv[2];
execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost,IP:127.0.0.1', '-keyout', `${dir}/key.pem`, '-out', `${dir}/cert.pem`], {stdio:'ignore'});
const pub = execFileSync('openssl',['x509','-in',`${dir}/cert.pem`,'-pubkey','-noout']);
const der = execFileSync('openssl',['pkey','-pubin','-outform','DER'],{input:pub});
const spki = execFileSync('openssl',['dgst','-sha256','-binary'],{input:der}).toString('base64');
let a,b,protectedUrl,protectedPort,hits=0;
const protectedServer=http.createServer((req,res)=>{res.setHeader('Access-Control-Allow-Origin','*');if(req.url==='/own-hit')hits++;res.end(String(hits));});
await new Promise(r=>protectedServer.listen(0,'127.0.0.1',r));protectedPort=protectedServer.address().port;protectedUrl=`http://127.0.0.1:${protectedPort}`;
const button = 'position:fixed;left:10px;top:300px;width:220px;height:30px';
const inlineGuard = `<script>(async()=>{let ok=!!CredentialsContainer.prototype.__tsrNoKeys;for(const what of ['get','create'])try{await navigator.credentials[what]({publicKey:{challenge:new Uint8Array(32)}});ok=false}catch(e){ok&&=e.message==='Security keys and passkeys are not available in this browser. Choose another way to sign in.'}document.title=ok?'First inline guard ready':'First inline guard missing';if(window.opener)window.opener.postMessage(document.title,location.origin)})()</script>`;
const server = https.createServer({key:fs.readFileSync(`${dir}/key.pem`),cert:fs.readFileSync(`${dir}/cert.pem`)},(req,res)=>{
 const u = new URL(req.url,a), cookie=req.headers.cookie||'';
 res.setHeader('Content-Type','text/html');
 const html = (title,body)=>res.end(`<!doctype html><meta name="viewport" content="width=device-width"><title>${title}</title>${body}`);
 switch(u.pathname){
 case '/fault-opener': return html('Fault opener',`<button style="${button}" onclick="window.open('${b}/fault-popup','fault','width=480,height=640')">Open fault</button><script>addEventListener('message',e=>document.title=e.data)</script>`);
 case '/fault-noopener': return html('Fault opener',`<button style="${button}" onclick="window.open('${b}/fault-popup','fault','noopener,width=480,height=640')">Open fault</button>`);
 case '/fault-popup': hits++; return html('Fault loaded',`<script>fetch('${protectedUrl}/own-hit');window.opener.postMessage('Fault ran',${JSON.stringify(a)});document.title='Fault ran'</script>`);
 case '/fetch-opener': return html('Fetch opener',`<button style="${button}" onclick="window.open('${b}/fetch-guard','fetch','width=480,height=640')">Open checked page</button>`);
 case '/fetch-guard': return html('Fetch guard',`<script>fetch('${protectedUrl}/own-hit').then(()=>document.title='Own port reachable',()=>document.title='Own port blocked')</script>`);
 case '/login': return html('Login',`<button style="${button}" onclick="window.open('${b}/authorize','provider','width=480,height=640')">Sign in with Provider</button><script>addEventListener('message',e=>{if(e.origin===${JSON.stringify(b)}&&e.data==='ann')document.title='Logged in across origins'})</script>`);
 case '/authorize': return html('Provider',`<form method=post action=/approve><button style="${button}">Allow</button></form>`);
 case '/approve': res.writeHead(303,{'Location':`${b}/callback`});return res.end();
 case '/callback': return html('Callback',`<script>window.opener.postMessage('ann',${JSON.stringify(a)});setTimeout(()=>window.close(),200)</script>`);
 case '/remember': res.setHeader('Set-Cookie','remember=ann; Max-Age=3600; SameSite=None; Secure; Path=/; HttpOnly');return html('Saving','<script>localStorage.setItem("name","Ann");const o=indexedDB.open("homework",1);o.onupgradeneeded=e=>e.target.result.createObjectStore("notes").put("saved homework","draft");o.onsuccess=()=>{o.result.close();document.title="Saved"}</script>');
 case '/remember-check': return html(cookie.includes('remember=ann')?'Cookie kept':'Cookie missing','<script>const db=indexedDB.open("homework",1);db.onsuccess=()=>{try{db.result.transaction("notes").objectStore("notes").get("draft").onsuccess=e=>document.title+=(localStorage.name==="Ann"&&e.target.result==="saved homework")?" and storage kept":" and storage missing"}catch(e){document.title+=" and storage missing"}}</script>');
 case '/embed': return html('Checking iframe',`<iframe src="${b}/cookie-probe"></iframe><script>addEventListener('message',e=>{if(e.origin===${JSON.stringify(b)})document.title=e.data})</script>`);
 case '/cookie-probe': return html('Cookie probe',`<script>parent.postMessage(${JSON.stringify(cookie.includes('remember=ann')?'Cross-site cookie sent':'Cross-site cookie blocked')},${JSON.stringify(a)})</script>`);
 case '/lax-set': res.setHeader('Set-Cookie','lax=ann; Max-Age=3600; SameSite=Lax; Secure; Path=/; HttpOnly');return html('Lax set','');
 case '/lax-link': return html('Lax link',`<a style="${button}" href="${b}/lax-check">Continue</a>`);
 case '/lax-check': res.writeHead(302,{'Location':`${a}/lax-done?sent=${cookie.includes('lax=ann')}`});return res.end();
 case '/lax-done': return html(u.searchParams.get('sent')==='true'?'Lax redirect signed in':'Lax redirect missing','');
 case '/tabs': return html('Tabs',`<a style="${button}" target=_blank href="${b}/new-tab">Open homework tab</a>`);
 case '/new-tab': return html('Homework tab','<script>document.title=window.opener===null?"Homework tab without opener":"Homework tab with opener"</script>');
 case '/post-tab': return html('Post tab',`<form method=post target=_blank action="${b}/post-target"><input name=homework value=ann><button style="${button}">Submit homework</button></form>`);
 case '/post-target': {let body='';req.on('data',c=>body+=c);req.on('end',()=>{const good=req.method==='POST'&&body==='homework=ann'&&req.headers.origin===a&&req.headers.referer===`${a}/`;html('Post reply',inlineGuard.replace('First inline guard ready',good?'Post and referrer kept and guard ready':'Post or referrer lost'))});return;}
 case '/frame-store': return html('Frame store',`<iframe src="${b}/embedded-store"></iframe><script>addEventListener('message',e=>{if(e.origin===${JSON.stringify(b)})document.title=e.data})</script>`);
 case '/frame-read': return html('Frame read',`<iframe src="${b}/embedded-read"></iframe><script>addEventListener('message',e=>{if(e.origin===${JSON.stringify(b)})document.title=e.data})</script>`);
 case '/embedded-store': return html('Embedded store',`<script>localStorage.setItem('embedded','saved');parent.postMessage('Embedded storage saved',${JSON.stringify(a)})</script>`);
 case '/embedded-read': return html('Embedded read',`<script>parent.postMessage(localStorage.embedded==='saved'?'Embedded storage kept':'Embedded storage missing',${JSON.stringify(a)})</script>`);
 case '/identity': return html('Identity',`<script>document.title=JSON.stringify({chrome:navigator.userAgent.includes('Chrome/'),headless:navigator.userAgent.includes('HeadlessChrome'),android:navigator.userAgent.includes('Android'),header:navigator.userAgent===${JSON.stringify(req.headers['user-agent'])},platform:navigator.platform,webdriver:navigator.webdriver,guard:!!CredentialsContainer.prototype.__tsrNoKeys});</script>`);
 case '/blank-guard': return html('Blank guard opener',`<button style="${button}" onclick="openBlank()">Open blank sign in</button><script>addEventListener('message',e=>document.title=e.data);function openBlank(){const w=window.open('about:blank','blank','width=480,height=640');w.document.write(${JSON.stringify(inlineGuard).replaceAll('</script>','<\\/script>')});w.document.close()}</script>`);
 case '/guard-popup': return html('Guard opener',`<button style="${button}" onclick="window.open('${b}/guard','guard','width=480,height=640')">Open sign in</button><script>addEventListener('message',e=>document.title=e.data)</script>`);
 case '/guard': return html('Guard',`<script>(async()=>{let ok=!!CredentialsContainer.prototype.__tsrNoKeys;for(const what of ['get','create'])try{await navigator.credentials[what]({publicKey:{challenge:new Uint8Array(32)}});ok=false}catch(e){ok&&=e.message==='Security keys and passkeys are not available in this browser. Choose another way to sign in.'}document.title=ok?'First inline guard ready':'First inline guard missing';if(window.opener)window.opener.postMessage(document.title,${JSON.stringify(a)})})()</script>`);
 case '/context-frames': return html('Context frames',`<iframe style="position:fixed;left:20px;top:20px;width:300px;height:150px;border:4px solid" src="${a}/context-inner"></iframe><iframe style="position:fixed;left:20px;top:220px;width:300px;height:150px;border:4px solid" src="${b}/context-inner"></iframe><div id=shadow style="position:fixed;left:400px;top:20px;width:300px;height:150px"></div><script>document.querySelector('#shadow').attachShadow({mode:'open'}).innerHTML='<a href="${a}/type" style="display:block;padding:20px">Shadow embedded selection</a>'</script>`);
 case '/context-inner': return html('Embedded context',`<a href="${a}/type" style="position:fixed;left:10px;top:10px;padding:10px">Frame embedded selection</a>`);
 case '/type': return html('Type','<p>Long text to select and find</p><input aria-label="Answer"><textarea aria-label="Long answer"></textarea>');
 case '/password': return html('password-hidden','<input type=password style="position:fixed;left:10px;top:20px;width:220px;height:30px">');
 case '/file': {
   const painted=u.searchParams.get('paint');
   // Readiness drains and acknowledges frames before the title arrives. Keep
   // painting so the subsequent paint check cannot wait on an unchanged page.
   return html(painted?'File preparing':'File',`<input id=file type=file ${painted?'disabled':''} style="${button}" onchange="fetch('/uploaded',{method:'POST',body:this.files[0]}).then(r=>r.text()).then(n=>document.title='Uploaded '+n)">${painted?`<style>@keyframes ready-paint{from{background:#d0e8ff}to{background:#e8ffd0}}#file:enabled{animation:ready-paint 1s linear infinite alternate}</style><script>requestAnimationFrame(()=>requestAnimationFrame(()=>{document.querySelector('#file').disabled=false;document.title=${JSON.stringify('File painted '+painted)};}))</script>`:''}`);
 }
 case '/upload': return html('Upload',`<input type=file multiple style="${button}" onchange="document.title='Selected:'+Array.from(this.files,f=>f.name).join(',')+':'+event.isTrusted;fetch('/uploaded',{method:'POST',body:this.files[0]})">`);
 case '/uploaded': {let n=0;req.on('data',b=>n+=b.length);req.on('end',()=>res.end(String(n)));return;}
 case '/download-page': return html('Download',`<a style="${button}" href=/download download>Download homework</a>`);
 case '/download': res.setHeader('Content-Type','text/plain');res.setHeader('Content-Disposition','attachment; filename="homework.txt"');return res.end('Homework download\n');
 default:return html('Fixture','');
 }
});
server.listen(0,'127.0.0.1',()=>{const port=server.address().port;a=`https://127.0.0.1:${port}`;b=`https://localhost:${port}`;console.log(JSON.stringify({a,b,spki,protectedUrl,protectedPort}));});
