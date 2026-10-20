import test from "node:test";
import assert from "node:assert/strict";
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { once } from "node:events";
import { createNativeFacade, facadePrefix, registerNativeFacade } from "./native-facade.ts";
import type { WeChatNativeConfig } from "./types.ts";
const config:WeChatNativeConfig={mode:"external",accountId:"a",endpoint:"http://127.0.0.1:1",tokenFile:"/unused",consumerStateDir:"/unused",gatewayFacade:{enabled:true}};
async function server(t:any,handler:(req:IncomingMessage,res:ServerResponse)=>void|Promise<void>){const server=createServer((req,res)=>{Promise.resolve(handler(req,res)).catch(()=>{res.writeHead(500);res.end();});});server.listen(0,"127.0.0.1");await once(server,"listening");t.after(()=>{server.closeAllConnections();return new Promise<void>(resolve=>server.close(()=>resolve()));});return `http://127.0.0.1:${(server.address()as any).port}`;}
test("optional facade registers explicit Gateway authentication and trusted operator scope",()=>{
  const routes:any[]=[];const api={registerHttpRoute:(route:any)=>routes.push(route)}as any;registerNativeFacade(api,config);assert.equal(routes[0].auth,"gateway");assert.equal(routes[0].gatewayRuntimeScopeSurface,"trusted-operator");assert.equal(routes[0].match,"prefix");registerNativeFacade(api,{...config,gatewayFacade:{enabled:false}});assert.equal(routes.length,1);assert.throws(()=>facadePrefix({...config,gatewayFacade:{enabled:true,prefix:"/"}}));
});
test("facade HTTP harness keeps Gateway/native credentials separate and preserves ranges",async t=>{
  let calls=0;const upstream=await server(t,async(req,res)=>{calls++;assert.equal(req.headers.authorization,"Bearer native-fixture");assert.equal(req.headers.range,"bytes=1-2");res.writeHead(206,{"content-type":"video/mp4","content-range":"bytes 1-2/3","etag":"fixture"});res.end(new Uint8Array([2,3]));});
  const handler=createNativeFacade(config,{endpoint:upstream,token:"native-fixture"});const gateway=await server(t,async(req,res)=>{if(req.headers.authorization!=="Bearer gateway-fixture"){res.writeHead(401);res.end();return;}await handler(req,res);});
  const path="/plugins/agent-wechat/native/v1/accounts/a/media/asset/content";assert.equal((await fetch(gateway+path)).status,401);assert.equal(calls,0);
  const result=await fetch(gateway+path,{headers:{Authorization:"Bearer gateway-fixture",Range:"bytes=1-2"}});assert.equal(result.status,206);assert.equal(result.headers.get("content-range"),"bytes 1-2/3");assert.equal(result.headers.get("x-content-type-options"),"nosniff");assert.deepEqual(new Uint8Array(await result.arrayBuffer()),new Uint8Array([2,3]));assert.equal(calls,1);
});
test("facade rejects foreign accounts, token management and encoded path escape before proxy",async t=>{
  let calls=0;const handler=createNativeFacade(config,{endpoint:"http://127.0.0.1:1",token:"fixture",fetch:async()=>{calls++;throw Error("unexpected");}});const base=await server(t,async(req,res)=>{await handler(req,res);});
  for(const path of ["v1/accounts/b/messages","v1/tokens","v1/accounts/a%2fother/messages"]){const response=await fetch(`${base}/plugins/agent-wechat/native/${path}`);assert.ok([400,403].includes(response.status));}assert.equal(calls,0);
});
test("facade HTTP harness streams SSE and client cancellation closes upstream",async t=>{
  let closed!:()=>void;const closure=new Promise<void>(resolve=>{closed=resolve;});const upstream=await server(t,async(req,res)=>{res.writeHead(200,{"content-type":"text/event-stream"});res.write('id: 1\ndata: {"cursor":"1","kind":"fixture","data":{}}\n\n');req.once("close",closed);});
  const handler=createNativeFacade(config,{endpoint:upstream,token:"native-fixture"});const base=await server(t,async(req,res)=>{await handler(req,res);});t.after(()=>handler.stop());
  const response=await fetch(`${base}/plugins/agent-wechat/native/v1/accounts/a/events/stream`);const reader=response.body!.getReader();const frame=await reader.read();assert.match(new TextDecoder().decode(frame.value),/cursor/);await reader.cancel();await Promise.race([closure,new Promise((_,reject)=>setTimeout(()=>reject(Error("upstream cancellation deadline")),2000))]);
});
