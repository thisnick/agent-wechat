import test from "node:test";
import assert from "node:assert/strict";
import {createHash} from "node:crypto";
import {createNativeStickerTools} from "./native-sticker-tool.ts";
function fixture(extra:Record<string,unknown>={}) {
  const cfg:any={channels:{"agent-wechat":{native:{mode:"external",endpoint:"http://127.0.0.1:6174",accountId:"a",token:"fixture-secret",consumerStateDir:"/private/fixture"}}}};
  const context:any={senderIsOwner:true,getRuntimeConfig:()=>cfg,assertInvocationCurrent:()=>{},...extra};
  const png=Buffer.from("89504e470d0a1a0a","hex"),gif=Buffer.from("GIF89a synthetic");
  const asset=(id:string,bytes:Buffer,mime:string)=>({id,accountId:"a",jobId:"prep",state:"available",sha256:createHash("sha256").update(bytes).digest("hex"),byteLength:bytes.length,contentType:mime});
  const original=asset("original",gif,"image/gif"),sheet=asset("sheet",png,"image/png");
  const client:any={
    customStickers:async()=>({items:[],revision:"revision",freshness:"profile"}),
    stickerPreparation:async()=>({id:"prep",accountId:"a",kind:"sticker_prepare",state:"available",result:{id:"prep",original,contactSheet:sheet,width:2,height:2,durationMs:100,transformations:[]}}),
    mediaContent:async(_account:string,id:string)=>({bytes:id==="original"?gif:png,contentType:id==="original"?"image/gif":"image/png"}),
  };
  const writes:unknown[]=[];
  const tools=createNativeStickerTools(context,{host:async()=>({client}) as any,files:async(_ctx:any,_guard:any,run:any)=>run({create:async(path:string,bytes:Buffer)=>writes.push({path,bytes})})});
  return {tools,context,client,writes,run:(input:unknown)=>tools[0].execute("call",input)};
}
test("sticker tool requires host owner proof and returns native visual image content",async()=>{
  assert.equal(fixture({senderIsOwner:false}).tools.length,0);
  assert.equal(fixture({assertInvocationCurrent:undefined}).tools.length,0);
  const f=fixture(); const result:any=await f.run({action:"preview",preparationId:"prep"});
  assert.equal(result.content[1].type,"image");assert.equal(result.content[1].mimeType,"image/png");
  assert.ok(result.content[1].data);assert.equal(result.details.source,"sampled_frames");
});
test("original export publishes verified GIF bytes to execution workspace, not a gateway path",async()=>{
  const f=fixture();const result:any=await f.run({action:"download",preparationId:"prep",output:"stickers/chosen.gif"});
  assert.equal(result.details.path,"stickers/chosen.gif");assert.equal(result.details.contentType,"image/gif");
  assert.equal((f.writes[0] as any).bytes.toString(),"GIF89a synthetic");
  await assert.rejects(f.run({action:"download",preparationId:"prep",output:"wrong.png"}));
});
test("hash failures and stale ownership never deliver a preview or export",async()=>{
  const f=fixture();f.client.mediaContent=async()=>({bytes:Buffer.from("wrong"),contentType:"image/png"});
  await assert.rejects(f.run({action:"preview",preparationId:"prep"}),/integrity/);
  assert.equal(f.writes.length,0);
  f.context.senderIsOwner=false;await assert.rejects(f.run({action:"list"}));
});
test("Saved preview delivers visual bytes and original export preserves its format", async()=>{
  const f=fixture();
  const gif=Buffer.from("GIF89a synthetic"),png=Buffer.from("89504e470d0a1a0a","hex");
  const asset={id:"saved-original",accountId:"a",state:"available",contentType:"image/gif",sha256:createHash("sha256").update(gif).digest("hex"),byteLength:gif.length,
    preview:{contentType:"image/png",sha256:createHash("sha256").update(png).digest("hex"),byteLength:png.length}};
  f.client.customSticker=async()=>({id:"chosen",original:asset});
  f.client.mediaContent=async(_a:string,_id:string,options:any)=>({bytes:options.representation === "preview"?png:gif,contentType:options.representation === "preview"?"image/png":"image/gif"});
  const visual:any=await f.run({action:"preview",stickerId:"chosen"});
  assert.equal(visual.content[1].type,"image");assert.equal(visual.details.source,"sampled_frames");
  const exported:any=await f.run({action:"download",stickerId:"chosen",output:"stickers/chosen.gif"});
  assert.equal(exported.details.path,"stickers/chosen.gif");assert.equal(f.writes.length,1);
  await assert.rejects(f.run({action:"download",stickerId:"chosen",output:"chosen.png"}),/extension/);
});
test("missing Saved originals return acquisition jobs without claiming workspace staging", async()=>{
  const f=fixture();let calls=0;
  f.client.customSticker=async()=>({id:"chosen",original:{id:"pending",accountId:"a",state:"pending"}});
  f.client.downloadSticker=async(_a:string,request:any,key:string)=>{calls++;assert.deepEqual(request,{stickerId:"chosen"});assert.equal(key,"acquire");return {id:"job",state:"queued"};};
  await assert.rejects(f.run({action:"preview",stickerId:"chosen"}),/unavailable/);
  const result:any=await f.run({action:"download",stickerId:"chosen",output:"chosen.gif",idempotencyKey:"acquire"});
  assert.equal(result.details.staged,false);assert.equal(result.details.path,undefined);assert.equal(f.writes.length,0);assert.equal(calls,1);
});
test("custom mutations retain revision, relative placement and stable submission keys", async()=>{
  const f=fixture();const calls:unknown[]=[];
  f.client.addCustomSticker=async(...args:any[])=>{calls.push(args);return {id:"add",state:"queued"};};
  f.client.moveCustomSticker=async(...args:any[])=>{calls.push(args);return {id:"move",state:"queued"};};
  f.client.removeCustomSticker=async(...args:any[])=>{calls.push(args);return {id:"remove",state:"queued"};};
  await f.run({action:"add",preparationId:"prep",idempotencyKey:"save"});
  await f.run({action:"move",stickerId:"chosen",revision:"revision",afterId:null,idempotencyKey:"move"});
  await f.run({action:"remove",stickerId:"chosen",revision:"revision",idempotencyKey:"remove"});
  assert.deepEqual((calls[0] as any[]).slice(0,3),["a",{source:{kind:"prepared",preparationId:"prep"}},"save"]);
  assert.deepEqual((calls[1] as any[]).slice(0,5),["a","chosen",{position:{afterId:null}},"revision","move"]);
  await assert.rejects(f.run({action:"move",stickerId:"chosen",revision:"revision",idempotencyKey:"incomplete"}),/afterId/);
});
test("adding a received sticker finds the message's original when no asset is given", async()=>{
  const f=fixture();const added:any[]=[];const lookups:string[]=[];
  const media=(id:string,messageId:string,kind:string,variant:string)=>({id,accountId:"a",messageId,kind,variant,actualVariant:variant,state:"available"});
  f.client.mediaAssets=async(_account:string,messageId:string)=>{lookups.push(messageId);
    return {items:[media("thumb","msg_1","sticker","thumbnail"),media("asset_original","msg_1","sticker","original"),media("other","msg_2","sticker","original"),media("photo","msg_1","image","original")]};};
  f.client.addCustomSticker=async(...args:any[])=>{added.push(args);return {id:"add",state:"queued"};};
  await f.run({action:"add",messageId:"msg_1",idempotencyKey:"save-received"});
  assert.deepEqual(lookups,["msg_1"]);
  assert.deepEqual(added[0].slice(0,3),["a",{source:{kind:"message",messageId:"msg_1",assetId:"asset_original"}},"save-received"]);
  // A supplied asset is passed through without a lookup.
  await f.run({action:"add",messageId:"msg_1",assetId:"asset_given",idempotencyKey:"save-given"});
  assert.equal(lookups.length,1);assert.equal(added[1][1].source.assetId,"asset_given");
});
test("received sticker adds stop when the message has no single original sticker", async()=>{
  const f=fixture();let adds=0;
  f.client.addCustomSticker=async()=>{adds++;return {id:"add",state:"queued"};};
  const media=(id:string,variant:string)=>({id,accountId:"a",messageId:"msg_1",kind:"sticker",variant,actualVariant:null,state:"pending"});
  f.client.mediaAssets=async()=>({items:[media("thumb","thumbnail")]});
  await assert.rejects(f.run({action:"add",messageId:"msg_1",idempotencyKey:"none"}),/no received sticker/);
  f.client.mediaAssets=async()=>({items:[media("one","original"),media("two","original")]});
  await assert.rejects(f.run({action:"add",messageId:"msg_1",idempotencyKey:"two"}),/more than one/);
  await assert.rejects(f.run({action:"add",preparationId:"prep",messageId:"msg_1",idempotencyKey:"both"}),/preparation or a received sticker message/);
  assert.equal(adds,0);
});
