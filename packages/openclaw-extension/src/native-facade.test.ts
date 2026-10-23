import test from "node:test";
import assert from "node:assert/strict";
import { createServer, request, type IncomingMessage, type ServerResponse } from "node:http";
import { once } from "node:events";
import { NATIVE_MAX_SOURCE_BYTES } from "@agent-wechat/shared";
import { createNativeFacade, facadePrefix, registerNativeFacade } from "./native-facade.ts";
import type { WeChatNativeConfig } from "./types.ts";
const config:WeChatNativeConfig={mode:"external",accountId:"a",endpoint:"http://127.0.0.1:1",token: "synthetic-native-token-".repeat(3),consumerStateDir:"/unused",gatewayFacade:{enabled:true}};
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
test("facade admits 512 MiB upload/content headers while JSON, previews and over-limit sources stay restricted", async t => {
  assert.equal(NATIVE_MAX_SOURCE_BYTES, 512 * 1024 * 1024);
  let calls = 0, responseBytes = 1;
  const handler = createNativeFacade(config, { endpoint: "http://127.0.0.1:1", token: "native-fixture", fetch: async () => {
    calls++; return new Response(new Uint8Array([1]), { headers: { "content-length": String(responseBytes) } });
  } });
  const base = await server(t, async (req, res) => { await handler(req, res); }); t.after(() => handler.stop());
  const headersOnly = (path: string, method: string, length?: number): Promise<{ status: number; length?: string }> => new Promise((resolve, reject) => {
    const req = request(`${base}/plugins/agent-wechat/native/v1/accounts/a/${path}`, { method, headers: length == null ? {} : { "content-length": length } }, res => {
      resolve({ status: res.statusCode!, length: res.headers["content-length"] }); res.destroy(); req.destroy();
    }); req.on("error", reject); req.setTimeout(3000, () => req.destroy(Error("Header fixture timed out"))); req.end();
  });
  assert.equal((await headersOnly("uploads", "POST", NATIVE_MAX_SOURCE_BYTES + 64 * 1024)).status, 200); assert.equal(calls, 1);
  assert.equal((await headersOnly("uploads", "POST", NATIVE_MAX_SOURCE_BYTES + 64 * 1024 + 1)).status, 413); assert.equal(calls, 1);
  assert.equal((await headersOnly("sends", "POST", 128 * 1024 * 1024 + 1)).status, 413); assert.equal(calls, 1);
  responseBytes = NATIVE_MAX_SOURCE_BYTES;
  const content = await headersOnly("media/asset/content", "GET"); assert.equal(content.status, 200); assert.equal(Number(content.length), NATIVE_MAX_SOURCE_BYTES);
  assert.equal((await headersOnly("messages/message", "GET")).status, 502);
  assert.equal((await headersOnly("media/asset/content?representation=preview", "GET")).status, 502);
  responseBytes++;
  assert.equal((await headersOnly("media/asset/content", "GET")).status, 502);
});

test("facade forwards notification PUT bodies and stable mutation keys", async t => {
  const upstream = await server(t, async (req, res) => {
    assert.equal(req.method, "PUT");
    assert.equal(req.url, "/v1/accounts/a/threads/room%40chatroom/notifications");
    assert.equal(req.headers["idempotency-key"], "notification-fixture");
    let body = ""; for await (const chunk of req) body += chunk;
    assert.deepEqual(JSON.parse(body), { muted: true });
    res.writeHead(202, { "content-type": "application/json" }); res.end('{"id":"operation"}');
  });
  const handler = createNativeFacade(config, { endpoint: upstream, token: "native-fixture" });
  const base = await server(t, async (req, res) => { await handler(req, res); }); t.after(() => handler.stop());
  const result = await fetch(`${base}/plugins/agent-wechat/native/v1/accounts/a/threads/room%40chatroom/notifications`, {
    method: "PUT", headers: { "Content-Type": "application/json", "Idempotency-Key": "notification-fixture" }, body: JSON.stringify({ muted: true }),
  });
  assert.equal(result.status, 202); assert.deepEqual(await result.json(), { id: "operation" });
});
