import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, writeFile, truncate, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { NATIVE_MAX_SOURCE_BYTES, NativeApiError, NativeWeChatClient, type NativeMessage, type MediaAsset, type MediaJob } from "@agent-wechat/shared";
import { NativeMediaJobInspectionError, prepareNativeAttachment, sendNativeAttachment, waitNativeMediaJob, waitNativeMediaSendReceipts } from "./native-media.ts";
import { resolveWeChatAccount } from "./types.ts";
const account=resolveWeChatAccount({channels:{"agent-wechat":{backend:"native",native:{mode:"external",accountId:"a",endpoint:"http://127.0.0.1:6174",token: "synthetic-native-token-".repeat(3),consumerStateDir:"/unused"}}}})!;
const message={id:"m",accountId:"a",threadId:"u",type:3} as NativeMessage;
const bytes=new Uint8Array([71,73,70,56,57,97,1,2,3]);
const hash=createHash("sha256").update(bytes).digest("hex");
function asset(overrides:Partial<MediaAsset>={}):MediaAsset{return {id:"asset",accountId:"a",messageId:"m",kind:"image",variant:"original",actualVariant:"original",state:"available",contentType:"image/gif",byteLength:bytes.length,sha256:hash,filename:"animated.gif",expiresAt:null,reason:null,retryable:false,jobId:null,contentUrl:null,animationFrames:4,officialWriteback:"pending",...overrides};}
function sendJob(overrides: Partial<MediaJob> = {}): MediaJob { return { id: "job", accountId: "a", kind: "send", state: "accepted", phase: "completed", attempts: 1,
  cancelRequested: false, retryable: false, reason: null, assetId: null, sourceId: "upload", threadId: "filehelper", serverMessageId: "18446744073709551614",
  completedBytes: bytes.length, totalBytes: bytes.length, result: {}, createdAt: "2026-10-21T12:00:00Z", updatedAt: "2026-10-21T12:00:01Z", ...overrides }; }
test("native original media preserves exact bytes, managed path and explicit quality",async()=>{
  let saved=0;const client={mediaAssets:async()=>({items:[asset()]}),mediaContent:async()=>({bytes,contentType:"image/gif"})}as unknown as NativeWeChatClient;
  const result=await prepareNativeAttachment({client,account,message,core:{channel:{media:{saveMediaBuffer:async(buffer:Buffer,mime:string,_subdir:string,max:number,name:string)=>{saved++;assert.deepEqual(new Uint8Array(buffer),bytes);assert.equal(mime,"image/gif");assert.equal(name,"animated.gif");assert.equal(max,50*1024*1024);return{path:"/managed/animated.gif"};}}}}});
  assert.equal(saved,1);assert.equal(result.mediaPath,"/managed/animated.gif");assert.equal(result.attachment?.quality,"full");assert.equal(result.pending,false);
});
test("PDF attachments preserve exact source bytes as file references without image preparation", async t => {
  const { readFile } = await import("node:fs/promises");
  // An account-independent blank PDF with a complete cross-reference table.
  const objects = [
    "<< /Type /Catalog /Pages 2 0 R >>",
    "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] /Contents 4 0 R >>",
    "<< /Length 0 >>\nstream\n\nendstream",
  ];
  let pdf = "%PDF-1.4\n";
  const offsets = objects.map((object, index) => {
    const offset = Buffer.byteLength(pdf);
    pdf += `${index + 1} 0 obj\n${object}\nendobj\n`;
    return offset;
  });
  const xref = Buffer.byteLength(pdf);
  pdf += `xref\n0 5\n0000000000 65535 f \n${offsets.map(offset => `${String(offset).padStart(10, "0")} 00000 n \n`).join("")}trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`;
  const original = Buffer.from(pdf), originalHash = createHash("sha256").update(original).digest("hex");
  const directory = await mkdtemp(join(tmpdir(), "native-pdf-reference-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const metadata = asset({ kind: "file", filename: "synthetic.pdf", contentType: "application/octet-stream", byteLength: original.length,
    sha256: originalHash, animationFrames: null, expiresAt: 1 });
  const calls: string[] = [];
  const client = { mediaAssets: async () => ({ items: [metadata] }), mediaContent: async (_account: string, _id: string, options: any) => {
    calls.push("original"); assert.equal(options.representation, undefined);
    return { bytes: original, contentType: "application/octet-stream" };
  }, transferMedia: async () => { throw Error("Cached PDF must not request a transfer"); } } as unknown as NativeWeChatClient;
  const result = await prepareNativeAttachment({ client, account, message: { ...message, type: 49, appMessageType: 6 },
    cfg: { get agents() { throw Error("PDF must not enter image preparation"); } },
    core: { channel: { media: { saveMediaBuffer: async (buffer: Buffer, mime: string, subdir: string, _max: number, filename: string) => {
      calls.push("save"); assert.deepEqual(buffer, original); assert.equal(mime, "application/octet-stream"); assert.equal(subdir, "inbound");
      const path = join(directory, filename); await writeFile(path, buffer); return { path };
    } } } } });
  assert.deepEqual(calls, ["original", "save"]);
  assert.equal(result.pending, false); assert.equal(result.attachment?.status, "ready"); assert.equal(result.attachment.kind, "file");
  assert.deepEqual(await readFile(result.attachment.path!), original);
  assert.equal(result.mediaPath, undefined); assert.equal(result.mediaMime, "application/octet-stream");
  assert.match(result.body, /\[Local file: </); assert.ok(result.body.includes(`<${result.attachment.path}>`));
  assert.equal(result.suppliedAssets?.length, 1);
  const supplied = result.suppliedAssets![0];
  assert.equal(supplied.role, "file_reference"); assert.equal(supplied.kind, "file"); assert.equal(supplied.runtimeMedia, false);
  assert.equal(supplied.path, result.attachment.path); assert.equal(supplied.sourcePath, supplied.path);
  assert.equal(supplied.sha256, originalHash); assert.equal(supplied.sourceSha256, originalHash); assert.equal(supplied.byteLength, original.length);
  assert.equal(createHash("sha256").update(await readFile(supplied.path)).digest("hex"), originalHash);
});
test("a 512 MiB file streams into managed storage and non-file retrieval keeps its existing bound", async () => {
  const chunk = new Uint8Array(64 * 1024), total = 512 * 1024 * 1024, digest = createHash("sha256");
  for (let offset = 0; offset < total; offset += chunk.byteLength) digest.update(chunk);
  const sha256 = digest.digest("hex"), largeAccount = { ...account, mediaMaxMb: 512 };
  let streams = 0, cancelled = 0, published = 0;
  const metadata = asset({ kind: "file", byteLength: total, sha256, filename: "large.pdf", contentType: "application/pdf" });
  const client = { mediaAssets: async () => ({ items: [metadata] }), mediaContent: async () => assert.fail("Large files must not request an in-memory response"),
    mediaContentStream: async (_account: string, _asset: string, options: { maxBytes: number }) => {
      streams++; assert.equal(options.maxBytes, total);
      return { contentType: "application/pdf", stream: (async function* () { for (let offset = 0; offset < total; offset += chunk.byteLength) yield chunk; })(), cancel: async () => { cancelled++; } };
    } } as unknown as NativeWeChatClient;
  const result = await prepareNativeAttachment({ client, account: largeAccount, message: { ...message, type: 49, appMessageType: 6 }, core: {},
    saveMediaStream: async (stream, _mime, _subdir, max, filename, _hint, options) => {
      assert.equal(max, total); assert.equal(filename, "large.pdf"); let received = 0;
      for await (const value of stream) received += (value as Uint8Array).byteLength;
      options?.assertCommitAllowed?.(); published++; return { id: "managed", path: "/managed/large.pdf", size: received };
    } });
  assert.equal(published, 1); assert.equal(streams, 1); assert.equal(cancelled, 1); assert.equal(result.attachment?.path, "/managed/large.pdf");
  assert.equal(result.suppliedAssets?.[0].byteLength, total); assert.equal(result.suppliedAssets?.[0].sha256, sha256);
  assert.equal(result.suppliedAssets?.[0].role, "file_reference"); assert.equal(result.mediaPath, undefined);
  const image = await prepareNativeAttachment({ client, account: largeAccount, message, core: {} });
  assert.equal(image.attachment?.status, "too_large"); assert.equal(streams, 1);
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
  const kinds:string[]=[];let submissions=0;const client={upload:async()=>({id:"upload"}),sendMedia:async(_account:string,input:any,key:string)=>{submissions++;kinds.push(input.kind);assert.equal(key,"stable-key");return sendJob({result:{messageIds:["msg"],chunks:[{state:"accepted",serverMessageId:"18446744073709551614"}]}});}}as unknown as NativeWeChatClient;
  await sendNativeAttachment(client,account,"filehelper",path,false,"stable-key");await sendNativeAttachment(client,account,"filehelper",path,true,"stable-key");assert.deepEqual(kinds,["file","voice"]);assert.equal(submissions,2);
});
test("video quality reaches the native request, 50 MiB admission is separate, and document mode preserves file selection", async t => {
  const directory = await mkdtemp(join(tmpdir(), "native-video-options-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, "source.mp4"); await writeFile(path, new Uint8Array(1)); await truncate(path, 50 * 1024 * 1024);
  const selected: any[] = []; let uploads = 0;
  const client = { upload: async (_a: string, source: Blob) => { uploads++; assert.ok(source instanceof Blob); assert.equal(source.size, 50 * 1024 * 1024); return { id: "source" }; },
    sendMedia: async (_a: string, input: any) => { selected.push(input); return sendJob({ result: { messageIds: ["stored"] } }); },
    message: async () => ({ id: "stored", accountId: "a", threadId: "filehelper", direction: "outgoing", serverId: "18446744073709551614" }),
  } as unknown as NativeWeChatClient;
  for (const videoQuality of [undefined, "normal", "original"] as const) await sendNativeAttachment(client, account, "filehelper", path, false, `video-${videoQuality}`, undefined, { videoQuality });
  assert.deepEqual(selected.map(request => [request.kind, request.videoQuality]), [["video", "normal"], ["video", "normal"], ["video", "original"]]);
  await sendNativeAttachment(client, account, "filehelper", path, false, "document", undefined, { forceDocument: true });
  assert.equal(selected[3].kind, "file"); assert.equal(selected[3].videoQuality, undefined);
  await truncate(path, 50 * 1024 * 1024 + 1);
  await assert.rejects(sendNativeAttachment(client, { ...account, mediaMaxMb: 512 }, "filehelper", path, false, "too-large"), /bounded regular file/);
  assert.equal(uploads, 4);
});
test("document/quality conflicts and quality on a non-video fail before staging", async t => {
  const directory = await mkdtemp(join(tmpdir(), "native-invalid-video-options-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, "image.png"); await writeFile(path, bytes); let uploads = 0;
  const client = { upload: async () => { uploads++; assert.fail("Invalid options must not stage a source"); } } as unknown as NativeWeChatClient;
  await assert.rejects(sendNativeAttachment(client, account, "filehelper", path, false, "invalid", undefined, { videoQuality: "original" }), /requires a video attachment/);
  await assert.rejects(sendNativeAttachment(client, account, "filehelper", path, false, "invalid", undefined, { videoQuality: "original", forceDocument: true }), /cannot also request/);
  await assert.rejects(sendNativeAttachment(client, account, "filehelper", path, true, "invalid", undefined, { forceDocument: true }), /cannot also request/);
  assert.equal(uploads, 0);
});
test("only selected file sends accept the exact 512 MiB file limit without reading it into memory", async t => {
  const directory = await mkdtemp(join(tmpdir(), "native-file-size-")); t.after(() => rm(directory, { recursive: true, force: true }));
  let uploads = 0, sends = 0;
  const largeAccount = { ...account, mediaMaxMb: 512 };
  const client = { upload: async (_a: string, content: Blob) => { uploads++; assert.ok(content instanceof Blob); assert.equal(content.size, 512 * 1024 * 1024); return { id: "upload" }; },
    sendMedia: async (_a: string, input: any, key: string) => { sends++; assert.equal(input.kind, "file"); assert.equal(key, "large-stable"); return sendJob({ result: { messageIds: ["stored-file"] } }); },
    message: async () => ({ id: "stored-file", accountId: "a", threadId: "filehelper", direction: "outgoing", serverId: "18446744073709551614" }),
  } as unknown as NativeWeChatClient;
  const file = join(directory, "synthetic.bin"); await writeFile(file, new Uint8Array(1)); await truncate(file, 512 * 1024 * 1024);
  assert.equal(NATIVE_MAX_SOURCE_BYTES, 512 * 1024 * 1024);
  const result = await sendNativeAttachment(client, largeAccount, "filehelper", file, false, "large-stable");
  assert.equal(result.messageId, "stored-file"); assert.equal(uploads, 1); assert.equal(sends, 1);
  await assert.rejects(sendNativeAttachment(client, { ...account, mediaMaxMb: 25 }, "filehelper", file), /bounded regular file/);
  await truncate(file, 512 * 1024 * 1024 + 1);
  await assert.rejects(sendNativeAttachment(client, { ...largeAccount, mediaMaxMb: 1024 }, "filehelper", file), /bounded regular file/);
  for (const [extension, voice] of [["jpg", false], ["mp4", false], ["wav", true]] as const) {
    const path = join(directory, `synthetic.${extension}`); await writeFile(path, new Uint8Array(1)); await truncate(path, extension === "mp4" ? 50 * 1024 * 1024 + 1 : 25 * 1024 * 1024 - 16);
    await assert.rejects(sendNativeAttachment(client, largeAccount, "filehelper", path, voice), /bounded regular file/);
  }
  assert.equal(uploads, 1); assert.equal(sends, 1);
});
test("remote image metadata selects the ordinary bound before staging bytes", async t => {
  let cancelled = 0, uploads = 0;
  t.mock.method(globalThis, "fetch", async () => new Response(new ReadableStream({ cancel() { cancelled++; } }), {
    headers: { "content-type": "image/jpeg", "content-length": String(25 * 1024 * 1024 - 16) },
  }));
  const client = { upload: async () => { uploads++; throw Error("must not stage"); } } as unknown as NativeWeChatClient;
  await assert.rejects(sendNativeAttachment(client, account, "filehelper", "https://example.invalid/file.bin"), /configured size/);
  assert.equal(cancelled, 1); assert.equal(uploads, 0);
});
test("remote ordinary files spool exact bytes for one upload and release the private source", async t => {
  t.mock.method(globalThis, "fetch", async () => new Response(bytes, { headers: { "content-type": "application/pdf", "content-length": String(bytes.length) } }));
  let uploaded: Blob | undefined, uploads = 0, sends = 0;
  const client = { upload: async (_account: string, blob: Blob) => { uploads++; uploaded = blob; assert.deepEqual(new Uint8Array(await blob.arrayBuffer()), bytes); return { id: "upload" }; },
    sendMedia: async () => { sends++; return sendJob({ result: { messageIds: ["stored-file"] } }); },
    message: async () => ({ id: "stored-file", accountId: "a", threadId: "filehelper", direction: "outgoing", serverId: "18446744073709551614" }) } as unknown as NativeWeChatClient;
  assert.equal((await sendNativeAttachment(client, account, "filehelper", "https://example.invalid/file.pdf", false, "remote-key")).messageId, "stored-file");
  assert.equal(uploads, 1); assert.equal(sends, 1); await assert.rejects(uploaded!.arrayBuffer());
});
test("large file polling allows a 36-minute transfer and a one-hour deadline never resubmits", async t => {
  const directory = await mkdtemp(join(tmpdir(), "native-large-file-wait-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, "synthetic.bin"); await writeFile(path, new Uint8Array(1)); await truncate(path, 26 * 1024 * 1024);
  let clock = 0; t.mock.method(Date, "now", () => clock);
  for (const completes of [true, false]) {
    clock = 0; let uploads = 0, sends = 0, polls = 0;
    const initial = sendJob({ state: "running", phase: "uploading" });
    const client = { upload: async () => { uploads++; return { id: "upload" }; },
      sendMedia: async (_a: string, _input: unknown, key: string) => { sends++; assert.equal(key, "same-large-key"); return initial; },
      job: async (a: string, id: string) => { polls++; clock += completes ? 12 * 60000 : 20 * 60000; assert.equal(a, "a"); assert.equal(id, initial.id);
        return completes && clock >= 30 * 60000 ? sendJob({ result: { messageIds: ["stored-file"] } }) : initial; },
      message: async () => ({ id: "stored-file", accountId: "a", threadId: "filehelper", direction: "outgoing", serverId: "18446744073709551614" }),
      retryJob: async () => assert.fail("Polling cannot retry a send"),
    } as unknown as NativeWeChatClient;
    if (completes) assert.equal((await sendNativeAttachment(client, account, "filehelper", path, false, "same-large-key")).messageId, "stored-file");
    else await assert.rejects(sendNativeAttachment(client, account, "filehelper", path, false, "same-large-key"), /job job requires inspection.*do not automatically resend/);
    assert.equal(uploads, 1); assert.equal(sends, 1); assert.equal(polls, 3);
  }
});
test("a failed status poll retains the submitted media job without another POST or private error content", async t => {
  const directory = await mkdtemp(join(tmpdir(), "native-send-poll-error-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, "synthetic.pdf"); await writeFile(path, bytes);
  for (const failure of [new DOMException("synthetic-private-response", "TimeoutError"), new Error("synthetic-private-response")]) {
    let uploads = 0, sends = 0, polls = 0;
    const client = new NativeWeChatClient({ baseUrl: "http://127.0.0.1:6174", token: "synthetic-api-token", fetch: async (input, init) => {
      const url = new URL(String(input));
      if (url.pathname === "/v1/accounts/a/uploads" && init?.method === "POST") {
        uploads++; return Response.json({ id: "upload" }, { status: 201 });
      }
      if (url.pathname === "/v1/accounts/a/sends" && init?.method === "POST") {
        sends++; assert.equal(new Headers(init.headers).get("Idempotency-Key"), "stable-poll-key");
        return Response.json(sendJob({ id: "known-submitted-job", state: "running", phase: "readback", serverMessageId: null }), { status: 202 });
      }
      if (url.pathname === "/v1/accounts/a/jobs/known-submitted-job" && !init?.method) { polls++; throw failure; }
      assert.fail(`Unexpected request ${init?.method ?? "GET"} ${url.pathname}`);
    } });
    await assert.rejects(sendNativeAttachment(client, account, "filehelper", path, false, "stable-poll-key"), (error: unknown) => {
      assert.ok(error instanceof NativeMediaJobInspectionError);
      assert.equal(error.jobId, "known-submitted-job"); assert.equal(error.accountId, "a"); assert.equal(error.threadId, "filehelper");
      assert.equal(error.idempotencyKey, "stable-poll-key"); assert.equal(error.state, "running"); assert.equal(error.reason, "status_unavailable");
      assert.equal(error.retryable, false); assert.match(error.message, /known-submitted-job.*do not automatically resend/);
      assert.equal(error.cause, undefined);
      for (const sensitive of ["synthetic-private-response", "synthetic-api-token"]) {
        assert.ok(!String(error).includes(sensitive)); assert.ok(!JSON.stringify(error).includes(sensitive));
      }
      return true;
    });
    assert.equal(uploads, 1); assert.equal(sends, 1); assert.equal(polls, 1);
  }
});
test("caller cancellation still leaves media polling with the original abort reason", async () => {
  const controller = new AbortController(), reason = new Error("caller stopped waiting"); controller.abort(reason);
  const client = { job: async () => assert.fail("Cancelled callers must not poll or resubmit") } as unknown as NativeWeChatClient;
  await assert.rejects(waitNativeMediaJob(client, "a", sendJob({ state: "running" }), 300000, controller.signal), error => error === reason);
});
test("outbound media returns verified stored receipts and a recall during upload prevents submission", async t => {
  const directory = await mkdtemp(join(tmpdir(), "native-send-receipt-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, "fixture.wav"); await writeFile(path, bytes);
  let sent = 0, invalidated = false;
  const client = { upload: async () => { invalidated = true; return { id: "upload" }; },
    sendMedia: async () => { sent++; return sendJob({ result: { messageIds: ["msg_voice"] } }); },
    message: async () => ({ id: "msg_voice", accountId: "a", threadId: "filehelper", direction: "outgoing", serverId: "18446744073709551614" }),
  } as unknown as NativeWeChatClient;
  await assert.rejects(sendNativeAttachment(client, account, "filehelper", path, false, "stable", async () => {
    if (invalidated) throw Error("input recalled during upload");
  }), /input recalled/);
  assert.equal(sent, 0);
  const result = await sendNativeAttachment(client, account, "filehelper", path, false, "another-target");
  assert.equal(sent, 1); assert.equal(result.messageId, "msg_voice"); assert.equal(result.meta.receiptState, "verified");
  assert.equal(result.meta.serverId, "18446744073709551614"); assert.equal(result.meta.jobId, "job");
});
test("accepted media and split voice wait for complete stored receipts using only the same job reads", async t => {
  const directory = await mkdtemp(join(tmpdir(), "native-delayed-send-receipt-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, "fixture.wav"); await writeFile(path, bytes);
  for (const voice of [false, true]) {
    let uploads = 0, sends = 0, polls = 0, reads = 0;
    const chunks = voice ? [{ state: "accepted", serverMessageId: "9007199254740999" }, { state: "accepted", serverMessageId: "18446744073709551614" }] : undefined;
    const initial = sendJob({ result: { nativeReceipt: { status: "server_accepted" }, uploadReadbackVerified: true, verifiedSha256: hash, ...(chunks ? { chunks } : {}) } });
    const client = { upload: async () => { uploads++; return { id: "upload" }; }, sendMedia: async () => { sends++; return initial; },
      job: async (a: string, id: string, signal: AbortSignal) => {
        assert.equal(a, "a"); assert.equal(id, initial.id); assert.equal(signal.aborted, false); polls++;
        return { ...initial, result: { ...initial.result as object, messageIds: polls === 1 ? voice ? ["msg_1"] : undefined : voice ? ["msg_1", "msg_2"] : ["msg_1"], profileProjection: polls === 1 ? "pending" : "complete" } };
      }, message: async (a: string, id: string) => { reads++; assert.equal(polls, 2); assert.equal(a, "a");
        return { id, accountId: a, threadId: "filehelper", direction: "outgoing", serverId: voice && id === "msg_1" ? "9007199254740999" : "18446744073709551614" }; },
      retryJob: async () => assert.fail("Receipt waiting cannot retry a send"), transferMedia: async () => assert.fail("Receipt waiting cannot transfer media"),
    } as unknown as NativeWeChatClient;
    const result = await sendNativeAttachment(client, account, "filehelper", path, voice, "stable-delayed-receipt");
    assert.equal(result.messageId, "msg_1"); assert.equal(result.meta.receiptState, "verified"); assert.equal(result.meta.jobId, initial.id);
    assert.equal((result.meta.messageIds as unknown[]).length, voice ? 2 : 1);
    assert.equal(uploads, 1); assert.equal(sends, 1); assert.equal(polls, 2); assert.equal(reads, voice ? 2 : 1);
  }
});
test("missing or foreign receipt job reads remain explicit uncertainty without another send", async t => {
  const directory = await mkdtemp(join(tmpdir(), "native-missing-send-receipt-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, "fixture.wav"); await writeFile(path, bytes);
  for (const mismatch of ["unavailable", "id", "accountId", "threadId", "kind", "sourceId", "serverMessageId"]) {
    let sends = 0, polls = 0;
    const initial = sendJob(), client = { upload: async () => ({ id: "upload" }), sendMedia: async () => { sends++; return initial; },
      job: async () => { polls++; if (mismatch === "unavailable") throw Error("Job read unavailable");
        return { ...initial, [mismatch]: "another-target", result: { messageIds: ["foreign_message"], profileProjection: "complete" } }; },
      message: async () => assert.fail("Unbound job receipts cannot be verified as this send"),
    } as unknown as NativeWeChatClient;
    const result = await sendNativeAttachment(client, account, "filehelper", path, false, "stable-missing-receipt");
    assert.equal(result.messageId, ""); assert.equal(result.meta.receiptState, "needs_review"); assert.equal(result.meta.retryable, false);
    assert.equal(result.meta.jobId, initial.id); assert.deepEqual(result.meta.messageIds, []); assert.match(String(result.meta.note), /do not resend/);
    assert.equal(sends, 1); assert.equal(polls, 1);
  }
  const initial = sendJob(); let polls = 0;
  assert.equal(await waitNativeMediaSendReceipts({ job: async () => { polls++; return initial; } } as unknown as NativeWeChatClient, "a", "filehelper", initial, false, 0), initial);
  assert.equal(polls, 0, "A bounded receipt wait must preserve the incomplete job at its deadline");
});
test("failed partial and uncertain media jobs stop without polling or resubmission", async t => {
  const directory = await mkdtemp(join(tmpdir(), "native-terminal-send-receipt-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const path = join(directory, "fixture.wav"); await writeFile(path, bytes);
  for (const state of ["failed", "partial", "needs_review"]) {
    let sends = 0, polls = 0;
    const client = { upload: async () => ({ id: "upload" }), sendMedia: async () => { sends++; return sendJob({ state, result: { messageIds: ["partial"] } }); },
      job: async () => { polls++; assert.fail("A terminal send cannot be polled for more receipts"); },
    } as unknown as NativeWeChatClient;
    await assert.rejects(sendNativeAttachment(client, account, "filehelper", path, true, "stable-terminal-receipt"), /requires inspection/);
    assert.equal(sends, 1); assert.equal(polls, 0);
  }
  let polls = 0;
  const client = { upload: async () => ({ id: "upload" }), sendMedia: async () => sendJob({ result: { messageIds: ["partial"],
    chunks: [{ state: "accepted", serverMessageId: "9007199254740999" }, { state: "needs_review" }] } }),
    job: async () => { polls++; assert.fail("Partial chunks cannot become another send attempt"); }, message: async () => assert.fail("Partial voice cannot be a full verified delivery"),
  } as unknown as NativeWeChatClient;
  const result = await sendNativeAttachment(client, account, "filehelper", path, true, "stable-partial-chunks");
  assert.equal(result.meta.receiptState, "needs_review"); assert.equal(result.messageId, ""); assert.equal(polls, 0);
});
test("recalled media and a recalled download refresh remain terminal without another transfer", async () => {
  for (const queued of [false, true]) {
    let catalogs = 0, polls = 0, transfers = 0;
    const client = { mediaAssets: async () => ({ items: [asset({ state: queued && catalogs++ === 0 ? "pending" : "recalled", jobId: "old-job" })] }),
      job: async () => { polls++; return { id: "old-job", accountId: "a", assetId: "asset", kind: "download", state: "queued" }; },
      transferMedia: async () => { transfers++; throw Error("must not transfer"); },
    } as unknown as NativeWeChatClient;
    const result = await prepareNativeAttachment({ client, account, message, core: {} });
    assert.equal(result.reason, "message_recalled"); assert.equal(result.pending, false); assert.equal(result.retryable, false);
    assert.equal(result.attachment, undefined); assert.equal(result.mediaPath, undefined); assert.equal(transfers, 0); assert.equal(polls, queued ? 1 : 0);
  }
});
test("temporary incoming status GET failures retain the existing download job without another transfer", async () => {
  for (const failure of [new TypeError("synthetic fetch failure"), new DOMException("synthetic timeout", "TimeoutError"), new NativeApiError(503, "unavailable", true)]) {
    let polls = 0, transfers = 0;
    const client = { mediaAssets: async () => ({ items: [asset({ state: "pending", jobId: "existing-download" })] }),
      job: async () => { polls++; throw failure; }, transferMedia: async () => { transfers++; assert.fail("A known download cannot be resubmitted"); },
    } as unknown as NativeWeChatClient;
    const result = await prepareNativeAttachment({ client, account, message, core: {} });
    assert.equal(result.pending, true); assert.equal(result.retryable, true); assert.equal(result.jobId, "existing-download");
    assert.equal(result.attachment, undefined); assert.equal(polls, 1); assert.equal(transfers, 0);
    assert.ok(!JSON.stringify(result).includes("synthetic"));
  }
});
test("incoming status recovery preserves permanent errors, identity checks and caller scope invalidation", async () => {
  const permanent = new NativeApiError(403, "auth_required", false), unexpected = Error("unexpected status decoder failure");
  for (const failure of [permanent, unexpected]) {
    const client = { mediaAssets: async () => ({ items: [asset({ state: "pending", jobId: "existing-download" })] }), job: async () => { throw failure; } } as unknown as NativeWeChatClient;
    await assert.rejects(prepareNativeAttachment({ client, account, message, core: {} }), error => error === failure);
  }
  for (const code of ["media_source_changed", "media_identity_unavailable"]) {
    const client = { mediaAssets: async () => ({ items: [asset({ state: "pending", jobId: "existing-download" })] }),
      job: async () => { throw new NativeApiError(409, code, true); },
    } as unknown as NativeWeChatClient;
    const result = await prepareNativeAttachment({ client, account, message, core: {} });
    assert.equal(result.pending, false); assert.equal(result.retryable, false); assert.equal(result.reason, code);
  }
  for (const mismatch of [{ id: "other-job" }, { accountId: "other" }, { kind: "send" }, { assetId: "other-asset" }]) {
    const client = { mediaAssets: async () => ({ items: [asset({ state: "pending", jobId: "existing-download" })] }),
      job: async () => ({ id: "existing-download", accountId: "a", assetId: "asset", kind: "download", state: "running", ...mismatch }),
    } as unknown as NativeWeChatClient;
    await assert.rejects(prepareNativeAttachment({ client, account, message, core: {} }), /identity does not match/);
  }
  for (const invalidation of ["abort", "policy"]) {
    const controller = new AbortController(), reason = Error("Caller scope ended"); let invalidated = false;
    const client = { mediaAssets: async () => ({ items: [asset({ state: "pending", jobId: "existing-download" })] }),
      job: async () => { invalidated = true; if (invalidation === "abort") controller.abort(reason); throw new TypeError("synthetic fetch failure"); },
    } as unknown as NativeWeChatClient;
    await assert.rejects(prepareNativeAttachment({ client, account, message, core: {}, signal: controller.signal,
      assertCurrent: () => { if (invalidated && invalidation === "policy") throw reason; } }), error => error === reason);
  }
});
test("unbound or replaced media identities are terminal before saving and never refresh after transfer rejection", async () => {
  for (const code of ["media_identity_unavailable", "media_source_changed"]) for (const stage of ["catalog", "content", "transfer"]) {
    let catalogs = 0, downloads = 0, reads = 0;
    const client = {
      mediaAssets: async () => { catalogs++; if (stage === "catalog") throw new NativeApiError(409, code, false); return { items: [asset({ state: stage === "transfer" ? "pending" : "available" })] }; },
      mediaContent: async () => { reads++; throw new NativeApiError(409, code, false); },
      transferMedia: async () => { downloads++; throw new NativeApiError(409, code, false); },
    } as unknown as NativeWeChatClient;
    const result = await prepareNativeAttachment({ client, account, message, core: { channel: { media: { saveMediaBuffer: async () => assert.fail("Unverified media must not be saved") } } } });
    assert.equal(result.attachment?.status, "unavailable"); assert.equal(result.attachment?.path, undefined);
    assert.equal(result.pending, false); assert.equal(result.retryable, false); assert.equal(result.reason, code); assert.equal(result.mediaPath, undefined);
    assert.equal(catalogs, 1); assert.equal(downloads, stage === "transfer" ? 1 : 0); assert.equal(reads, stage === "content" ? 1 : 0);
  }
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

test("WXGF images and stickers keep original bytes while supplying a source-bound still preview", async t => {
  const { readFile } = await import("node:fs/promises");
  const original = Buffer.from("wxgf synthetic source bytes");
  const originalHash = createHash("sha256").update(original).digest("hex");
  const preview = await readFile(new URL("../test/fixtures/native-model.png", import.meta.url));
  const previewHash = createHash("sha256").update(preview).digest("hex");
  const directory = await mkdtemp(join(tmpdir(), "native-wxgf-preview-")); t.after(() => rm(directory, { recursive: true, force: true }));
  for (const type of [3, 47]) {
    const metadata = asset({ kind: type === 47 ? "sticker" : "image", filename: "original.wxgf", contentType: "image/x-wxgf", byteLength: original.length, sha256: originalHash,
      animationFrames: null, preview: { sourceSha256: originalHash, sha256: previewHash, byteLength: preview.length, contentType: "image/png", filename: "preview.png", contentUrl: "/unused" } });
    const representations: unknown[] = [];
    const client = { mediaAssets: async () => ({ items: [metadata] }), mediaContent: async (_a: string, _id: string, options: any) => {
      representations.push(options.representation ?? "original");
      return options.representation === "preview" ? { bytes: preview, contentType: "image/png", sourceSha256: originalHash } : { bytes: original, contentType: "image/x-wxgf" };
    } } as unknown as NativeWeChatClient;
    const result = await prepareNativeAttachment({ client, account, message: { ...message, type }, cfg: {}, core: { channel: { media: { saveMediaBuffer: async (buffer: Buffer, _mime: string, _dir: string, _max: number, name: string) => {
      const path = join(directory, `${type}-${name}`); await writeFile(path, buffer); return { path };
    } } } } });
    assert.deepEqual(representations, ["original", "preview"]);
    assert.deepEqual(await readFile(result.attachment!.path!), original);
    const supplied = result.suppliedAssets![0]; assert.equal(supplied.sourceSha256, originalHash); assert.equal(supplied.sha256, previewHash);
    assert.equal(supplied.sourceMime, "image/x-wxgf"); assert.equal(supplied.mime, "image/png");
    assert.notEqual(supplied.path, supplied.sourcePath); assert.equal(result.mediaPath, supplied.path); assert.match(result.body, /WXGF still preview/);
  }
});

test("WXGF preview metadata and bytes cannot cross the original source boundary", async () => {
  const original = Buffer.from("wxgf synthetic source bytes"), originalHash = createHash("sha256").update(original).digest("hex");
  const preview = new Uint8Array([1,2,3]), previewHash = createHash("sha256").update(preview).digest("hex");
  for (const bad of ["metadata-source", "metadata-size", "content-source", "content-hash", "content-mime"]) {
    let previewReads=0;
    const metadata = asset({contentType:"image/x-wxgf",byteLength:original.length,sha256:originalHash,preview:{sourceSha256:bad==="metadata-source"?"f".repeat(64):originalHash,
      sha256:previewHash,byteLength:bad==="metadata-size"?17*1024*1024:preview.length,contentType:"image/png",filename:"preview.png",contentUrl:"/unused"}});
    const client = { mediaAssets:async()=>({items:[metadata]}),mediaContent:async(_a:string,_id:string,options:any)=>{
      if(options.representation!=="preview") return {bytes:original,contentType:"image/x-wxgf"};
      previewReads++; return {bytes:bad==="content-hash"?new Uint8Array([0,0,0]):preview,contentType:bad==="content-mime"?"text/plain":"image/png",sourceSha256:bad==="content-source"?"f".repeat(64):originalHash};
    }} as unknown as NativeWeChatClient;
    await assert.rejects(prepareNativeAttachment({client,account,message,cfg:{},core:{channel:{media:{saveMediaBuffer:async()=>({path:"/managed/original.wxgf"})}}}}),/WXGF preview/);
    assert.equal(previewReads,bad.startsWith("metadata-")?0:1);
  }
});


test("foreign account or message asset metadata never reaches content retrieval", async () => {
  for (const metadata of [asset({ accountId: "other" }), asset({ messageId: "other" })]) {
    let reads = 0;
    const client = { mediaAssets: async () => ({ items: [metadata] }), mediaContent: async () => { reads++; throw Error("must not read"); } } as unknown as NativeWeChatClient;
    await assert.rejects(prepareNativeAttachment({ client, account, message, core: {} }), /identity/); assert.equal(reads, 0);
  }
});

const quoteMessage = { ...message, type: 49, appMessageType: 57, body: "Please explain this",
  quote: { serverId: "18446744073709551615", type: 3, state: "embedded", senderId: "source-author", embeddedImage: true } } as NativeMessage;
const quoteCore = { channel: { media: { saveMediaBuffer: async () => ({ path: "/managed/quoted.gif" }) } } };
function embeddedClient(extra: Partial<MediaAsset> = {}) {
  const calls: string[] = [];
  const client = {
    message: async () => assert.fail("Quote rendering must not fetch another message"),
    mediaAssets: async (_account: string, id: string) => { calls.push(`assets:${id}`); return { items: [asset({ messageId: id, ...extra })] }; },
    mediaContent: async () => { calls.push("bytes"); return { bytes, contentType: "image/gif" }; },
    transferMedia: async (_account: string, input: any) => { calls.push(`transfer:${input.messageId}`); return { id: "job", state: "queued" }; },
  } as unknown as NativeWeChatClient;
  return { client, calls };
}
test("embedded text and links reach the model with no quote or original lookup", async () => {
  for (const type of [1, 49]) {
    const f = embeddedClient(); const text = "https://example.com/article?a=1&b=2";
    const input = { ...quoteMessage, quote: { ...quoteMessage.quote!, type, appMessageType: type === 49 ? 5 : undefined, embeddedImage: false, embeddedText: text } };
    const result = await prepareNativeAttachment({ client: f.client, account, message: input, core: {} });
    assert.ok(result.body.includes(text)); assert.match(result.body, /Please explain this/); assert.match(result.body, /embedded in reply/);
    assert.equal(result.quote?.state, "embedded"); assert.deepEqual(f.calls, []);
    const blocked = await prepareNativeAttachment({ client: f.client, account, message: input, core: {}, allowQuotedMessage: async () => false });
    assert.match(blocked.body, /not permitted/); assert.ok(!blocked.body.includes(text)); assert.deepEqual(f.calls, []);
  }
});
test("embedded quoted images retrieve verified bytes under the parent ID", async () => {
  const f = embeddedClient();
  const result = await prepareNativeAttachment({ client: f.client, account, message: quoteMessage, core: quoteCore });
  assert.equal(result.mediaPath, "/managed/quoted.gif"); assert.equal(result.suppliedAssets![0].sha256, hash);
  assert.equal(result.suppliedAssets![0].sourceMessageId, undefined); assert.deepEqual(f.calls, ["assets:m", "bytes"]);
  const { buildNativeContext } = await import("./native-history.ts");
  const { nativeDispatchManifest } = await import("./native-prepared-boundary.ts");
  const context = buildNativeContext([], quoteMessage, result);
  const manifest = nativeDispatchManifest({ message: quoteMessage, inputBody: context.inputBody, assets: context.assets, sessionKey: "s", agentId: "agent", channelAccountId: "default" });
  assert.equal(manifest.assets[0].messageId, "m"); assert.equal(manifest.assets[0].sourceMessageId, undefined);
});
test("quoted voice, video and files preserve the comment with an unavailable attachment status", async () => {
  for (const type of [34, 43, 49]) {
    const f = embeddedClient();
    const result = await prepareNativeAttachment({ client: f.client, account, message: { ...quoteMessage,
      quote: { ...quoteMessage.quote!, type, appMessageType: type === 49 ? 6 : undefined, state: "unsupported", embeddedImage: false,
        embeddedText: type === 49 ? "report.pdf" : undefined } }, core: {} });
    assert.match(result.body, /Please explain this/); assert.match(result.body, /attachment unavailable/);
    if (type === 49) assert.match(result.body, /report.pdf/);
    assert.equal(result.pending, false); assert.equal(result.quote?.state, "unsupported"); assert.deepEqual(f.calls, []);
  }
});
test("missing, legacy, malformed and recalled embedded quotes never fetch an original", async () => {
  for (const state of [undefined, "missing", "invalid", "resolved", "recalled"]) {
    const f = embeddedClient();
    const result = await prepareNativeAttachment({ client: f.client, account, message: { ...quoteMessage,
      quote: { ...quoteMessage.quote!, state, embeddedImage: false, embeddedText: "must not escape" } }, core: {} });
    assert.match(result.body, /Please explain this/); assert.ok(!result.body.includes("must not escape")); assert.equal(result.pending, false);
    assert.deepEqual(f.calls, []);
  }
  const f = embeddedClient();
  const absent = await prepareNativeAttachment({ client: f.client, account, message: { ...quoteMessage, quote: undefined }, core: {} });
  assert.match(absent.body, /unavailable/); assert.deepEqual(f.calls, []);
});
test("blocked quoted images never reach attachment retrieval", async () => {
  const f = embeddedClient();
  const result = await prepareNativeAttachment({ client: f.client, account, message: quoteMessage, core: {}, allowQuotedMessage: async () => false });
  assert.match(result.body, /not permitted/); assert.deepEqual(f.calls, []);
});
test("pending and expired embedded images preserve the current comment", async () => {
  for (const state of ["pending", "expired"]) {
    const f = embeddedClient({ state });
    const result = await prepareNativeAttachment({ client: f.client, account, message: quoteMessage, core: {}, waitMs: 0 });
    assert.equal(result.pending, false); assert.match(result.body, /Please explain this/); assert.match(result.body, new RegExp(state));
    assert.equal(result.attachment?.status, state); assert.equal(result.mediaPath, undefined);
    assert.equal(f.calls.filter(call => call.startsWith("transfer")).length, state === "expired" ? 0 : 1);
  }
});
test("cached quote-owned image bytes remain usable after CDN expiry", async () => {
  const f = embeddedClient({ expiresAt: 1 });
  for (let attempt = 0; attempt < 2; attempt++) {
    const result = await prepareNativeAttachment({ client: f.client, account, message: quoteMessage, core: quoteCore });
    assert.equal(result.attachment?.status, "ready"); assert.equal(result.suppliedAssets![0].sourceMessageId, undefined);
  }
  assert.deepEqual(f.calls, ["assets:m", "bytes", "assets:m", "bytes"]);
});
test("forwarded bundles render stored text and unsupported cards never enter the file pipeline", async () => {
  for (const type of [19, 3, 4, 33, 2000]) {
    const result = await prepareNativeAttachment({ client: {} as NativeWeChatClient, account,
      message: { ...quoteMessage, appMessageType: type, body: "[Chat History] Alice: forwarded text" }, core: {} });
    assert.equal(result.pending, false); assert.equal(result.attachment, undefined);
    if (type === 19) assert.match(result.body, /Alice: forwarded text/); else assert.match(result.body, /Unsupported/);
  }
});

const linkMessage = { ...message, type: 49, appMessageType: 5, body: "[Link] Shared title\nShared description\nhttps://example.invalid/article",
  link: { title: "Shared title", description: "Shared description", url: "https://example.invalid/article", sourceName: "Publisher" } } as NativeMessage;
test("shared link previews supply verified thumbnails under the stored parent ID", async t => {
  const directory = await mkdtemp(join(tmpdir(), "native-link-preview-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const png = await readFile(new URL("../test/fixtures/native-model.png", import.meta.url));
  const sha256 = createHash("sha256").update(png).digest("hex");
  for (const subtype of [3, 4, 5]) for (const cached of [false, true]) {
    const calls: string[] = []; let ready = cached;
    const metadata = () => asset({ variant: "thumbnail", actualVariant: ready ? "thumbnail" : null, state: ready ? "available" : "pending",
      contentType: "image/png", filename: "preview.png", byteLength: png.length, sha256, expiresAt: cached ? 1 : null, officialWriteback: "unsupported" });
    const client = { mediaAssets: async (a: string, id: string) => { assert.equal(a, "a"); assert.equal(id, "m"); calls.push("assets"); return { items: [metadata()] }; },
      transferMedia: async (a: string, request: any) => { assert.equal(a, "a"); assert.deepEqual(request, { messageId: "m", assetId: null, quality: "thumbnail" }); calls.push("transfer"); ready = true; return { id: "job", state: "available" }; },
      mediaContent: async () => { calls.push("bytes"); return { bytes: png, contentType: "image/png" }; },
    } as unknown as NativeWeChatClient;
    const result = await prepareNativeAttachment({ client, account, message: { ...linkMessage, appMessageType: subtype }, cfg: {}, core: { channel: { media: {
      saveMediaBuffer: async (buffer: Buffer) => { const path = join(directory, `${subtype}-${cached}.png`); await writeFile(path, buffer); return { path }; },
    } } } });
    assert.equal(result.pending, false); assert.equal(result.attachment?.status, "ready"); assert.equal(result.attachment.kind, "image");
    assert.equal(result.variant, "thumbnail"); assert.match(result.body, /Shared title\nShared description\nhttps:\/\/example.invalid\/article/);
    assert.equal(result.suppliedAssets![0].role, "model_image"); assert.equal(result.suppliedAssets![0].sha256, sha256);
    assert.deepEqual(await readFile(result.mediaPath!), png); assert.equal(calls.includes("transfer"), !cached);
    const { buildNativeContext } = await import("./native-history.ts");
    const { nativeDispatchManifest } = await import("./native-prepared-boundary.ts");
    const context = buildNativeContext([], linkMessage, result);
    const manifest = nativeDispatchManifest({ message: linkMessage, inputBody: context.inputBody, assets: context.assets, sessionKey: "s", agentId: "agent", channelAccountId: "default" });
    assert.equal(manifest.assets[0].messageId, "m"); assert.equal(manifest.assets[0].sourceMessageId, undefined);
    const changed = nativeDispatchManifest({ message: { ...linkMessage, link: { ...linkMessage.link!, title: "Changed" } }, inputBody: context.inputBody, sessionKey: "s", agentId: "agent", channelAccountId: "default" });
    assert.notEqual(manifest.newInputs[0].contentSha256, changed.newInputs[0].contentSha256);
  }
});
test("optional absent, pending, expired and failed link previews preserve card text", async () => {
  for (const state of ["absent", "pending", "expired", "unsupported", "failed", "api-error"]) {
    let transfers = 0;
    const client = { mediaAssets: async () => {
      if (state === "api-error") throw new NativeApiError(503, "transport_unavailable", true);
      return { items: state === "absent" ? [] : [asset({ variant: "thumbnail", actualVariant: null, state: state === "failed" ? "pending" : state })] };
    }, transferMedia: async () => { transfers++; return { id: "job", state: state === "failed" ? "failed" : "queued", retryable: false, reason: "media_integrity_failed" }; },
    } as unknown as NativeWeChatClient;
    const result = await prepareNativeAttachment({ client, account, message: linkMessage, core: {}, waitMs: 0, download: state !== "absent" });
    assert.equal(result.pending, false); assert.equal(result.mediaPath, undefined); assert.equal(result.suppliedAssets, undefined);
    assert.match(result.body, /Shared title\nShared description\nhttps:\/\/example.invalid\/article/);
    assert.equal(result.attachment?.status, state === "pending" ? "pending" : ["expired", "unsupported"].includes(state) ? state : "unavailable");
    assert.equal(transfers, ["pending", "failed"].includes(state) ? 1 : 0);
  }
  const unsupported = await prepareNativeAttachment({ client: {} as NativeWeChatClient, account, message: linkMessage, core: {}, quality: "original" });
  assert.equal(unsupported.attachment?.status, "unsupported"); assert.match(unsupported.body, /Shared title/);
});
test("link previews cannot hide identity, integrity, cancellation or access failures", async () => {
  for (const change of [{ accountId: "foreign" }, { messageId: "foreign" }, { sha256: "0".repeat(64) }]) {
    const client = { mediaAssets: async () => ({ items: [asset({ variant: "thumbnail", actualVariant: "thumbnail", ...change })] }),
      mediaContent: async () => ({ bytes, contentType: "image/gif" }),
    } as unknown as NativeWeChatClient;
    await assert.rejects(prepareNativeAttachment({ client, account, message: linkMessage, core: {} }), /identity|integrity/);
  }
  let guards = 0, transfers = 0;
  const client = { mediaAssets: async () => ({ items: [] }), transferMedia: async () => { transfers++; throw Error("must not transfer"); } } as unknown as NativeWeChatClient;
  await assert.rejects(prepareNativeAttachment({ client, account, message: linkMessage, core: {}, beforeMediaAccess: async () => { if (++guards > 1) throw Error("access revoked"); } }), /access revoked/);
  assert.equal(transfers, 0);
  const controller = new AbortController(); controller.abort();
  await assert.rejects(prepareNativeAttachment({ client, account, message: linkMessage, core: {}, signal: controller.signal }));
});
test("recalled or replaced link sources never restore their original metadata through preview fallback", async () => {
  for (const code of ["message_recalled", "media_identity_unavailable", "media_source_changed"]) {
    const client = { mediaAssets: async () => { throw new NativeApiError(410, code); } } as unknown as NativeWeChatClient;
    if (code === "message_recalled") {
      await assert.rejects(prepareNativeAttachment({ client, account, message: linkMessage, core: {} }), /message_recalled/);
    } else {
      const result = await prepareNativeAttachment({ client, account, message: linkMessage, core: {} });
      assert.equal(result.reason, code); assert.ok(!result.body.includes("Shared title")); assert.ok(!result.body.includes("example.invalid/article"));
    }
  }
  const client = { mediaAssets: async () => ({ items: [asset({ state: "recalled", variant: "thumbnail", actualVariant: null })] }) } as unknown as NativeWeChatClient;
  const result = await prepareNativeAttachment({ client, account, message: linkMessage, core: {} });
  assert.equal(result.body, "[Message recalled]"); assert.equal(result.reason, "message_recalled");
});
