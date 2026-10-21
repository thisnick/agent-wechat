import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { NativeMessage, NativeWeChatClient, MediaAsset } from "@agent-wechat/shared";
import { prepareNativeAttachment, sendNativeAttachment } from "./native-media.ts";
import { resolveWeChatAccount } from "./types.ts";
const account=resolveWeChatAccount({channels:{"agent-wechat":{backend:"native",native:{mode:"external",accountId:"a",endpoint:"http://127.0.0.1:6174",token: "synthetic-native-token-".repeat(3),consumerStateDir:"/unused"}}}})!;
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

test("large model image has a separately bound preview while original bytes remain intact", async t => {
  const { readFile } = await import("node:fs/promises");
  const large = await readFile(new URL("../test/fixtures/native-model-large.png", import.meta.url));
  const originalHash = createHash("sha256").update(large).digest("hex");
  const directory = await mkdtemp(join(tmpdir(), "native-model-preview-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const cached = asset({ filename: "large.png", contentType: "image/png", byteLength: large.length, sha256: originalHash });
  const client = { mediaAssets: async () => ({ items: [cached] }), mediaContent: async () => ({ bytes: large, contentType: "image/png" }) } as unknown as NativeWeChatClient;
  const saved: Buffer[] = [];
  const result = await prepareNativeAttachment({ client, account, message, cfg: {}, core: { channel: { media: { saveMediaBuffer: async (buffer: Buffer, _mime: string, _subdir: string, _max: number, filename: string) => {
    const path = join(directory, filename); saved.push(buffer); await writeFile(path, buffer); return { path };
  } } } } });
  assert.equal(saved.length, 2); assert.deepEqual(saved[0], large); assert.equal(result.attachment?.quality, "full");
  const supplied = result.suppliedAssets![0]; assert.equal(supplied.sourceSha256, originalHash); assert.notEqual(supplied.sha256, originalHash);
  assert.equal(supplied.mime, "image/jpeg"); assert.equal(supplied.role, "model_image"); assert.notEqual(supplied.path, supplied.sourcePath);
  assert.equal(createHash("sha256").update(await readFile(supplied.path)).digest("hex"), supplied.sha256);
  assert.equal(result.mediaPath, supplied.path); assert.match(result.body, /prepared still preview/); assert.ok(result.body.includes(`[Image file: <${supplied.sourcePath}>]`));
});


test("foreign account or message asset metadata never reaches content retrieval", async () => {
  for (const metadata of [asset({ accountId: "other" }), asset({ messageId: "other" })]) {
    let reads = 0;
    const client = { mediaAssets: async () => ({ items: [metadata] }), mediaContent: async () => { reads++; throw Error("must not read"); } } as unknown as NativeWeChatClient;
    await assert.rejects(prepareNativeAttachment({ client, account, message, core: {} }), /identity/); assert.equal(reads, 0);
  }
});

const quoteMessage = { ...message, type: 49, appMessageType: 57, body: "Please explain this", quote: { serverId: "18446744073709551615", type: 3 } } as NativeMessage;
function quoteClient(original: NativeMessage | null, state = original ? "resolved" : "missing", extra: any = {}) {
  const calls: string[] = [];
  return { calls, client: {
    quotedMessage: async (a: string, id: string) => { calls.push("quote"); assert.equal(a,"a"); assert.equal(id,"m"); return { accountId:"a", messageId:"m", threadId:"u", state, message:original, ...extra }; },
    mediaAssets: async (_a: string, id: string) => { calls.push(`assets:${id}`); return { items:[asset({messageId:id, ...extra.asset})] }; },
    transferMedia: async (_a: string, input: any) => { calls.push(`transfer:${input.messageId}`); return { id:"job",state:"queued" }; },
    mediaContent: async () => { calls.push("bytes"); return { bytes, contentType:"image/gif" }; },
  } as unknown as NativeWeChatClient };
}
const quotedOriginal = { ...message, id:"original", serverId:"18446744073709551615", senderId:"source-author", direction:"incoming" } as NativeMessage;
const quoteCore = { channel:{media:{saveMediaBuffer:async()=>({path:"/managed/quoted.gif"})}} };
test("resolved text quotes retain the comment and stored original, with no media request", async () => {
  const f = quoteClient({ ...quotedOriginal, type:1, body:"The stored original text" });
  const prepared = await prepareNativeAttachment({ client:f.client, account, message:{...quoteMessage,quote:{...quoteMessage.quote!,type:1}}, core:{} });
  assert.match(prepared.body,/Please explain this/); assert.match(prepared.body,/The stored original text/);
  assert.match(prepared.body,/Quoted text from source-author/); assert.equal(prepared.quote?.messageId,"original");
  assert.deepEqual(f.calls,["quote"]);
});
test("quoted images use verified original assets and retain source message identity", async () => {
  const f = quoteClient(quotedOriginal);
  const prepared = await prepareNativeAttachment({client:f.client,account,message:quoteMessage,core:quoteCore});
  assert.equal(prepared.mediaPath,"/managed/quoted.gif"); assert.equal(prepared.suppliedAssets![0].sha256,hash);
  assert.equal(prepared.suppliedAssets![0].sourceMessageId,"original"); assert.match(prepared.body,/Please explain this/);
  assert.deepEqual(f.calls,["quote","assets:original","bytes"]);
  const { buildNativeContext } = await import("./native-history.ts");
  const { nativeDispatchManifest } = await import("./native-prepared-boundary.ts");
  const context = buildNativeContext([],quoteMessage,prepared);
  const manifest = nativeDispatchManifest({message:quoteMessage,inputBody:context.inputBody,assets:context.assets,sessionKey:"s",agentId:"agent",channelAccountId:"default"});
  assert.equal(manifest.assets[0].messageId,"m"); assert.equal(manifest.assets[0].sourceMessageId,"original");
});
test("quote references never cross account/chat, mismatch type/server ID, or recurse", async () => {
  for (const change of [{accountId:"other"},{threadId:"other"},{id:"m"},{serverId:"2"},{type:49}]) {
    const f = quoteClient({...quotedOriginal,...change});
    await assert.rejects(prepareNativeAttachment({client:f.client,account,message:quoteMessage,core:quoteCore}),/boundary/);
    assert.deepEqual(f.calls,["quote"]);
  }
  const f = quoteClient(quotedOriginal,"resolved",{threadId:"foreign"});
  await assert.rejects(prepareNativeAttachment({client:f.client,account,message:quoteMessage,core:quoteCore}),/identity/);
});
test("missing, invalid, unsupported and ambiguous quotes keep current text without file transfers", async () => {
  for (const state of ["missing","invalid","unsupported","ambiguous"]) {
    const f = quoteClient(null,state);
    const result = await prepareNativeAttachment({client:f.client,account,message:quoteMessage,core:{}});
    assert.match(result.body,/Please explain this/); assert.ok(result.body.includes(state)); assert.equal(result.pending,false);
    assert.deepEqual(f.calls,["quote"]);
  }
});
test("blocked quoted authors cannot supply text or image assets", async () => {
  for(const type of [1,3]) {
    const f=quoteClient({...quotedOriginal,type,body:"must not reach model"});
    const result=await prepareNativeAttachment({client:f.client,account,message:{...quoteMessage,quote:{...quoteMessage.quote!,type}},core:{},allowQuotedMessage:async()=>false});
    assert.match(result.body,/not permitted/); assert.ok(!result.body.includes("must not reach model")); assert.deepEqual(f.calls,["quote"]);
  }
});
test("pending and expired quoted images preserve the comment and explicit state", async () => {
  for (const state of ["pending","expired"]) {
    const f=quoteClient(quotedOriginal,"resolved",{asset:{state}});
    const result=await prepareNativeAttachment({client:f.client,account,message:quoteMessage,core:{},waitMs:0});
    assert.equal(result.pending,false); assert.match(result.body,/Please explain this/); assert.match(result.body,new RegExp(state));
    assert.equal(result.attachment?.status,state); assert.equal(result.mediaPath,undefined);
    assert.equal(f.calls.filter(call=>call.startsWith("transfer")).length,state==="expired"?0:1);
  }
});
test("embedded text and image remain usable when the original is absent locally", async () => {
  const text=quoteClient(null,"embedded",{senderId:"source-author",embeddedText:"Text carried inside the quote",embeddedImage:false});
  const result=await prepareNativeAttachment({client:text.client,account,message:{...quoteMessage,quote:{...quoteMessage.quote!,type:1}},core:{}});
  assert.match(result.body,/original not stored/); assert.match(result.body,/Text carried inside/); assert.deepEqual(text.calls,["quote"]);
  const image=quoteClient(null,"embedded",{senderId:"source-author",embeddedImage:true});
  const prepared=await prepareNativeAttachment({client:image.client,account,message:quoteMessage,core:quoteCore});
  assert.equal(prepared.mediaPath,"/managed/quoted.gif"); assert.equal(prepared.quote?.state,"embedded");
  assert.deepEqual(image.calls,["quote","assets:m","bytes"]);
  const blocked=await prepareNativeAttachment({client:image.client,account,message:quoteMessage,core:{},allowQuotedMessage:async()=>false});
  assert.match(blocked.body,/not permitted/);
});
test("forwarded bundles render stored text and unsupported cards never enter the file pipeline", async () => {
  for(const type of [19,5,33,2000]) {
    const result=await prepareNativeAttachment({client:{} as NativeWeChatClient,account,message:{...quoteMessage,appMessageType:type,body:"[Chat History] Alice: forwarded text"},core:{}});
    assert.equal(result.pending,false); assert.equal(result.attachment,undefined);
    if(type===19)assert.match(result.body,/Alice: forwarded text/);else assert.match(result.body,/Unsupported/);
  }
});

test("repeated quotes reuse a cached original even after CDN expiry without another transfer", async () => {
  let transfers=0,contents=0;
  const client={
    quotedMessage:async(_a:string,id:string)=>({accountId:"a",messageId:id,threadId:"u",state:"resolved",message:quotedOriginal}),
    mediaAssets:async(_a:string,id:string)=>{assert.equal(id,"original");return {items:[asset({messageId:"original",expiresAt:1})]};},
    mediaContent:async()=>{contents++;return {bytes,contentType:"image/gif"};},
    transferMedia:async()=>{transfers++;throw Error("cached original must not be downloaded again");},
  } as unknown as NativeWeChatClient;
  for(const id of ["quote-one","quote-two"]){
    const result=await prepareNativeAttachment({client,account,message:{...quoteMessage,id},core:quoteCore});
    assert.equal(result.attachment?.status,"ready");assert.equal(result.suppliedAssets![0].sourceMessageId,"original");
  }
  assert.equal(contents,2);assert.equal(transfers,0);
});
