#!/usr/bin/env node
// Starts only an isolated, pinned OpenClaw Gateway and a synthetic loopback
// daemon. No account credentials, provider calls, installed service or messages.
import assert from 'node:assert/strict';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { mkdtemp, mkdir, writeFile, readFile, copyFile, rm, realpath } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
const root=resolve(dirname(new URL(import.meta.url).pathname),'..');
const sdkRoot=await realpath(join(root,'packages/openclaw-extension/node_modules/openclaw'));
const version=JSON.parse(await readFile(join(sdkRoot,'package.json'),'utf8')).version;
if(version!=='2026.10.2')throw Error(`Gateway smoke is pinned to OpenClaw 2026.10.2, got ${version}`);
const temporary=await mkdtemp(join(tmpdir(),'native-gateway-smoke-'));
let child;let log='';let streamClosed;const closed=new Promise(resolve=>{streamClosed=resolve;});let mediaRequests=0;
const token='synthetic-native-token-'.repeat(3), gatewayToken='synthetic-gateway-token-'.repeat(3);
const upstream=createServer((req,res)=>{assert.equal(req.headers.authorization,`Bearer ${token}`);
 if(req.url==='/v1/accounts/a/media/asset/content')mediaRequests++;
 if(req.url==='/v1/status'){res.setHeader('content-type','application/json');res.end(JSON.stringify({serverId:'fixture',dataDir:'/fixture',pid:process.pid,ownershipGeneration:'fixture',state:'running',version:{product:'agent-wechat',backend:'native',version:'0.16.0',apiVersion:1,capabilities:['serve','events','profile-read','text-send','native-login']}}));return;}
 if(req.url==='/v1/accounts/a/media/asset/content'){assert.equal(req.headers.range,'bytes=1-2');res.writeHead(206,{'content-type':'video/mp4','content-range':'bytes 1-2/3','etag':'fixture'});res.end(Buffer.from([2,3]));return;}
 if(req.url==='/v1/accounts/a/events/stream'){res.writeHead(200,{'content-type':'text/event-stream'});res.write('id: 1\ndata: {"cursor":"1","kind":"fixture","data":{}}\n\n');req.once('close',streamClosed);return;}
 res.writeHead(404,{'content-type':'application/json'});res.end(JSON.stringify({error:{code:'fixture_not_found'}}));
});
try{
 await mkdir(join(temporary,'plugin'),{mode:0o700});await mkdir(join(temporary,'workspace'),{mode:0o700});
 const plugin=join(temporary,'plugin');await mkdir(join(plugin,'dist'));for(const name of ['package.json','openclaw.plugin.json','dist/index.js'])await copyFile(join(root,'packages/openclaw-extension',name),join(plugin,name));
 await writeFile(join(temporary,'native-token'),token,{mode:0o600});upstream.listen(0,'127.0.0.1');await once(upstream,'listening');const nativePort=upstream.address().port;
 const probe=createServer();probe.listen(0,'127.0.0.1');await once(probe,'listening');const port=probe.address().port;await new Promise(resolve=>probe.close(resolve));
 const config={gateway:{mode:'local',bind:'loopback',port,auth:{mode:'token',token:gatewayToken},controlUi:{enabled:false}},browser:{enabled:false},cron:{enabled:false},discovery:{mdns:{mode:'off'}},update:{checkOnStart:false},agents:{defaults:{workspace:join(temporary,'workspace')}},plugins:{allow:['agent-wechat'],load:{paths:[plugin]},entries:{'agent-wechat':{enabled:true,config:{backend:'native',native:{mode:'external',accountId:'a',endpoint:`http://127.0.0.1:${nativePort}`,tokenFile:join(temporary,'native-token'),consumerStateDir:join(temporary,'consumer'),gatewayFacade:{enabled:true}}}}}},logging:{file:join(temporary,'gateway.log')},channels:{'agent-wechat':{enabled:true}}};
 const path=join(temporary,'openclaw.json');await writeFile(path,JSON.stringify(config),{mode:0o600});
 const environment={PATH:process.env.PATH,HOME:temporary,TMPDIR:temporary,OPENCLAW_STATE_DIR:join(temporary,'state'),OPENCLAW_CONFIG_PATH:path,OPENCLAW_SKIP_CHANNELS:'1',NO_COLOR:'1'};
 const imported=await promisify(execFile)(process.execPath,[join(sdkRoot,'openclaw.mjs'),'config','get','plugins','--json'],{env:environment,timeout:20000,maxBuffer:262144});if(!imported.stdout.includes(plugin))throw Error(`Temporary config was not loaded: ${imported.stdout} ${imported.stderr}`);
 child=spawn(process.execPath,[join(sdkRoot,'openclaw.mjs'),'gateway','run'],{env:environment,stdio:['ignore','pipe','pipe']});
 for(const stream of [child.stdout,child.stderr])stream.on('data',chunk=>{log=(log+chunk.toString()).slice(-131072);});
 const base=`http://127.0.0.1:${port}/plugins/agent-wechat/native/`;let ready=false;
 for(let i=0;i<300;i++){if(child.exitCode!==null||child.signalCode)throw Error(`Pinned Gateway exited during startup: ${log}`);try{const response=await fetch(base+'v1/accounts/a/media/asset/content',{signal:AbortSignal.timeout(1000)});await response.body?.cancel();if(response.status===401){ready=true;break;}}catch{}await new Promise(resolve=>setTimeout(resolve,100));}
 if(!ready)throw Error(`Pinned Gateway readiness deadline: ${log}`);
 // The independently starting service may perform its /v1/status preflight
 // concurrently. Rejection must prevent forwarding this protected media read.
 const before=mediaRequests;assert.equal((await fetch(base+'v1/accounts/a/media/asset/content',{headers:{Authorization:'Bearer invalid'},signal:AbortSignal.timeout(2000)})).status,401);assert.equal(mediaRequests,before,'An invalid token reached the protected upstream media route');
 const headers={Authorization:`Bearer ${gatewayToken}`,Range:'bytes=1-2'};
 const ranged=await fetch(base+'v1/accounts/a/media/asset/content',{headers,signal:AbortSignal.timeout(3000)});assert.equal(ranged.status,206,`Gateway plugin response: ${await (ranged.status!==206?ranged.text():Promise.resolve(''))}; logs: ${log}`);assert.equal(ranged.headers.get('content-range'),'bytes 1-2/3');assert.deepEqual(new Uint8Array(await ranged.arrayBuffer()),new Uint8Array([2,3]));
 const foreign=await fetch(base+'v1/accounts/b/media/asset/content',{headers,signal:AbortSignal.timeout(3000)});assert.equal(foreign.status,403);const admin=await fetch(base+'v1/tokens',{headers,signal:AbortSignal.timeout(3000)});assert.equal(admin.status,403);
 const sse=await fetch(base+'v1/accounts/a/events/stream',{headers:{Authorization:`Bearer ${gatewayToken}`},signal:AbortSignal.timeout(5000)});assert.equal(sse.status,200);assert.ok(sse.headers.get('content-type').startsWith('text/event-stream'));const reader=sse.body.getReader();assert.match(new TextDecoder().decode((await reader.read()).value),/cursor/);await reader.cancel();await Promise.race([closed,new Promise((_,reject)=>setTimeout(()=>reject(Error('Gateway did not cancel upstream SSE')),3000))]);
 console.log(`Pinned OpenClaw ${version} Gateway smoke passed: actual plugin loading, unauthenticated/invalid-token rejection, separate native bearer, range/status forwarding, account restriction and upstream SSE cancellation. No model handoff, native login or send.`);
}finally{
 if(child&&child.exitCode===null&&child.signalCode===null){child.kill('SIGTERM');await Promise.race([once(child,'exit'),new Promise(resolve=>setTimeout(resolve,8000))]);if(child.exitCode===null&&child.signalCode===null){child.kill('SIGKILL');await once(child,'exit');}}
 upstream.closeAllConnections();await new Promise(resolve=>upstream.close(resolve));await rm(temporary,{recursive:true,force:true});
}
