import test from "node:test";
import assert from "node:assert/strict";
import {handleNativeStickerAction} from "./native-sticker-send.ts";
import {wechatMediaActions} from "./outbound-options.ts";

function fixture() {
  const cfg:any={channels:{"agent-wechat":{enabled:true,native:{mode:"external",endpoint:"http://127.0.0.1:6174",accountId:"a",token:"fixture-secret",consumerStateDir:"/private/fixture"}}}};
  let sends=0;const keys:string[]=[];
  const ctx:any={action:"sticker",channel:"agent-wechat",cfg,senderIsOwner:true,params:{to:"wxid_peer",stickerId:["a".repeat(32)],idempotencyKey:"host-key"},onPlatformSendDispatch:async()=>{},assertDirectAdapterHandoff:()=>{}};
  const client:any={sendSticker:async(_account:string,request:any,key:string)=>{sends++;keys.push(key);assert.deepEqual(request,{kind:"sticker",threadId:"wxid_peer",source:{kind:"saved",stickerId:"a".repeat(32)}});return {id:"job",accountId:"a",threadId:"wxid_peer",kind:"sticker_send",state:"queued",result:{}};}};
  return {ctx,client,keys,sends:()=>sends,run:()=>handleNativeStickerAction(ctx,{host:async()=>({client}) as any})};
}
test("only the owner discovers standard sticker sends; management remains separate",()=> {
  const f=fixture();
  assert.deepEqual(wechatMediaActions.describeMessageTool({cfg:f.ctx.cfg} as any)?.actions,["send"]);
  assert.deepEqual(wechatMediaActions.describeMessageTool({cfg:f.ctx.cfg,senderIsOwner:true} as any)?.actions,["send","sticker"]);
});
test("pending native jobs are not delivered-message facts and retries keep their bounded key",async()=> {
  const f=fixture();f.ctx.params.idempotencyKey="host:"+"x".repeat(1000);
  const result=await f.run();await f.run();
  assert.ok("status" in result.details && "job" in result.details);
  assert.equal(result.details.status,"pending");assert.equal(result.details.job.id,"job");assert.equal(Object.hasOwn(result.details,"messageId"),false);
  assert.equal(f.keys[0],f.keys[1]);assert.equal(f.keys[0].length,64);
});
test("dry runs, delegated identities and lost authority never submit",async()=> {
  const f=fixture();f.ctx.dryRun=true;await f.run();assert.equal(f.sends(),0);
  f.ctx.dryRun=false;f.ctx.senderIsOwner=false;await assert.rejects(f.run(),/owner/);
  f.ctx.senderIsOwner=true;f.ctx.assertDirectAdapterHandoff=undefined;await assert.rejects(f.run(),/authority/);
  f.ctx.assertDirectAdapterHandoff=()=>{throw Error("superseded")};await assert.rejects(f.run(),/superseded/);assert.equal(f.sends(),0);
});
test("unverified completed jobs retain inspection status without exposing a fabricated receipt",async()=> {
  const f=fixture();f.client.sendSticker=async()=>({id:"job",accountId:"a",threadId:"wxid_peer",kind:"sticker_send",state:"available",result:{messageId:"wrong",serverMessageId:"42"}});
  f.client.message=async()=>({id:"wrong",accountId:"other",threadId:"wxid_peer",direction:"outgoing",serverId:"42"});
  const result=await f.run();assert.ok("status" in result.details && "job" in result.details);assert.equal(result.details.status,"needs_review");assert.equal(Object.hasOwn(result.details.job,"result"),false);assert.equal(Object.hasOwn(result.details,"delivery"),false);
});
