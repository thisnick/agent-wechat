import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { NativeMessage, NativeWeChatClient, MediaAsset } from "@agent-wechat/shared";
import { prepareNativeAttachment, sendNativeAttachment } from "./native-media.ts";
import { resolveWeChatAccount } from "./types.ts";
const account=resolveWeChatAccount({channels:{"agent-wechat":{backend:"native",native:{mode:"external",accountId:"a",endpoint:"http://127.0.0.1:6174",tokenFile:"/unused",consumerStateDir:"/unused"}}}})!;
const message={id:"m",accountId:"a",threadId:"u",type:3} as NativeMessage;
const bytes=new Uint8Array([71,73,70,56,57,97,1,2,3]);
const hash=createHash("sha256").update(bytes).digest("hex");
function asset(overrides:Partial<MediaAsset>={}):MediaAsset{return {id:"asset",accountId:"a",messageId:"m",kind:"image",variant:"original",actualVariant:"original",state:"available",contentType:"image/gif",byteLength:bytes.length,sha256:hash,filename:"animated.gif",expiresAt:null,reason:null,retryable:false,jobId:null,contentUrl:null,animationFrames:4,officialWriteback:"pending",...overrides};}
test("native original media preserves exact bytes, managed path and explicit quality",async()=>{
  let saved=0;const client={mediaAssets:async()=>({items:[asset()]}),mediaContent:async()=>({bytes,contentType:"image/gif"})}as unknown as NativeWeChatClient;
  const result=await prepareNativeAttachment({client,account,message,core:{channel:{media:{saveMediaBuffer:async(buffer:Buffer,mime:string,_subdir:string,max:number,name:string)=>{saved++;assert.deepEqual(new Uint8Array(buffer),bytes);assert.equal(mime,"image/gif");assert.equal(name,"animated.gif");assert.equal(max,50*1024*1024);return{path:"/managed/animated.gif"};}}}}});
  assert.equal(saved,1);assert.equal(result.mediaPath,"/managed/animated.gif");assert.equal(result.attachment?.quality,"full");assert.equal(result.pending,false);
});
test("expired native media never requests a transfer or invents a local path",async()=>{
  const client={mediaAssets:async()=>({items:[asset({state:"expired"})]}),transferMedia:async()=>{throw Error("must not fetch");}}as unknown as NativeWeChatClient;
  const result=await prepareNativeAttachment({client,account,message,core:{}});assert.equal(result.attachment?.status,"expired");assert.equal(result.mediaPath,undefined);assert.match(result.body,/expired/);
});
test("native media remains pending and unread while durable transfer is queued",async()=>{
  const client={mediaAssets:async()=>({items:[asset({state:"pending"})]}),transferMedia:async()=>({id:"job",state:"queued"})}as unknown as NativeWeChatClient;
  const result=await prepareNativeAttachment({client,account,message,core:{},waitMs:0});assert.equal(result.pending,true);assert.equal(result.mediaPath,undefined);
});
test("native attachment hash failure occurs before OpenClaw managed storage",async()=>{
  const client={mediaAssets:async()=>({items:[asset()]}),mediaContent:async()=>({bytes:new Uint8Array([0]),contentType:"image/gif"})}as unknown as NativeWeChatClient;
  await assert.rejects(prepareNativeAttachment({client,account,message,core:{}}),/integrity/);
});
test("native outbound ordinary audio stays file and explicit voice stays voice",async t=>{
  const directory=await mkdtemp(join(tmpdir(),"native-audio-fixture-"));t.after(()=>rm(directory,{recursive:true,force:true}));const path=join(directory,"fixture.wav");await writeFile(path,bytes);
  const kinds:string[]=[];let submissions=0;const client={upload:async()=>({id:"upload"}),sendMedia:async(_account:string,input:any,key:string)=>{submissions++;kinds.push(input.kind);assert.equal(key,"stable-key");return{id:"job",state:"accepted"};}}as unknown as NativeWeChatClient;
  await sendNativeAttachment(client,account,"filehelper",path,false,"stable-key");await sendNativeAttachment(client,account,"filehelper",path,true,"stable-key");assert.deepEqual(kinds,["file","voice"]);assert.equal(submissions,2);
});
