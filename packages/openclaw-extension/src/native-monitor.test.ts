import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { NativeApiError, type NativeMessage, type NativeWeChatClient } from "@agent-wechat/shared";
import { NativeConsumerState } from "./native-consumer-state.ts";
import { acceptNativeEvent, dispatchNativeMessage, nativeWasMentioned, projectNativeRead, startNativeReadPump, synchronizeNativeEvents } from "./native-monitor.ts";
import { prepareNativeAttachment } from "./native-media.ts";
import { resolveWeChatAccount, type WeChatNativeConfig } from "./types.ts";
function fixture(t: any, options: { group?: boolean; mention?: boolean; origin?: string; readPolicy?: WeChatNativeConfig["readPolicy"] } = {}) {
  const directory = mkdtempSync(join(tmpdir(), "native-model-harness-")); t.after(() => rmSync(directory, { recursive: true, force: true }));
  const cfg: any = { channels: { "agent-wechat": { backend: "native", dmPolicy: "open", allowFrom: ["*"], groupPolicy: "open", groupAllowFrom: ["*"],
    native: { mode: "external", endpoint: "http://127.0.0.1:6174", accountId: "a", token: "synthetic-native-token-".repeat(3), consumerStateDir: directory, readPolicy: options.readPolicy } } } };
  const account = resolveWeChatAccount(cfg)!;
  const state = new NativeConsumerState(directory, "s", "a");
  const message: NativeMessage = { id: "m1", accountId: "a", threadId: options.group ? "r@chatroom" : "u", senderId: "u", serverId: "9007199254740999", type: 1, direction: "incoming", body: "fixture", createdAt: "2026-10-20T12:00:00Z", version: "1", media: [], mentions: { state: options.mention ? "known" : "unknown", userIds: [], self: options.mention ?? null, all: null, source: null } };
  state.accept("1", message, options.origin ?? "live");
  const runs: any[] = [];
  const core: any = { channel: {
    commands: { shouldHandleTextCommands: () => false, isControlCommandMessage: () => false, shouldComputeCommandAuthorized: () => false },
    pairing: { readAllowFromStore: async () => [] },
    inbound: { ingress: { resolveStable: async (params: any) => ({ ingress: { admission: params.policy.activation?.requireMention && !params.mentionFacts?.wasMentioned ? "skip" : "dispatch", reasonCode: "fixture" }, activationAccess: {}, commandAccess: {} }) } },
    routing: { resolveAgentRoute: () => ({ agentId: "main", sessionKey: "test", accountId: "default" }) },
    session: { resolveStorePath: () => "/unused", recordInboundSession: async () => {} },
    reply: { resolveEnvelopeFormatOptions: () => ({}), formatAgentEnvelope: ({ body }: any) => body, finalizeInboundContext: (ctx: any) => ctx,
      dispatchReplyWithBufferedBlockDispatcher: async (params: any) => { runs.push(params); params.replyOptions.onPreparedAgentRunStart?.("run_1"); } },
  } };
  const client = { sendText: async () => ({ id: "send_1", state: "accepted" }),
    message: async (_account: string, id: string) => state.get(id)!.message } as unknown as NativeWeChatClient;
  return { cfg, account, state, message, core, client, runs, createPipeline: (() => ({ onModelSelected: () => {} })) as any };
}
test("structured self mentions use exact facts, unknown and display text do not activate", t => {
  const value = fixture(t); value.message.body = "@Self Name fixture"; assert.equal(nativeWasMentioned(value.message), false);
  value.message.mentions = { state: "known", userIds: ["different"], self: false, all: null, source: "msgsource.atuserlist" }; assert.equal(nativeWasMentioned(value.message), false);
});
test("group mention gate runs before model dispatch and leaves ignored messages unread", async t => {
  const value = fixture(t, { group: true }); assert.equal(await dispatchNativeMessage(value), "ignored");
  assert.equal(value.runs.length, 0); assert.equal(value.state.get("m1")?.readPending, undefined);
});
test("prepared model harness records exact input custody and read projection separately", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" }); assert.equal(await dispatchNativeMessage(value), "completed");
  assert.equal(value.runs.length, 1); assert.equal(value.state.get("m1")?.runId, "run_1"); assert.equal(value.state.get("m1")?.readPending, true);
  assert.match(value.runs[0].ctx.MessageSid, /^wechat-native:a:m1:[a-f0-9]{64}$/);
});
test("embedded quoted links reach prepared model input without original resolution", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" });
  const link = "[Link] Article title\nArticle description\nhttps://example.com/article?a=1&b=2";
  Object.assign(value.message, { type: 49, appMessageType: 57, body: "Explain this article",
    quote: { serverId: "42", type: 49, appMessageType: 5, state: "embedded", senderId: "u", embeddedText: link } });
  assert.equal(await dispatchNativeMessage(value), "completed");
  assert.ok(value.runs[0].ctx.BodyForAgent.includes(link)); assert.ok(value.runs[0].ctx.BodyForAgent.includes("Explain this article"));
  assert.equal(value.state.get("m1")?.readPending, true);
});
test("unsupported embedded media quotes dispatch their comment instead of requiring review", async t => {
  for (const type of [34, 43, 49]) {
    const value = fixture(t);
    Object.assign(value.message, { type: 49, appMessageType: 57, body: "Explain this attachment",
      quote: { serverId: "42", type, appMessageType: type === 49 ? 6 : undefined, state: "unsupported", senderId: "u" } });
    assert.equal(await dispatchNativeMessage(value), "completed");
    assert.match(value.runs[0].ctx.BodyForAgent, /Explain this attachment/);
    assert.match(value.runs[0].ctx.BodyForAgent, /attachment unavailable/);
  }
});
test("inline quoted recall prevents stale embedded text from reaching the model", async t => {
  const value = fixture(t);
  Object.assign(value.message, { type: 49, appMessageType: 57, body: "Current comment",
    quote: { serverId: "42", type: 1, state: "embedded", senderId: "u", embeddedText: "Stale excerpt" } });
  let reads = 0;
  value.client.message = async () => ++reads === 1 ? value.message : { ...value.message, quote: { serverId: "42", type: 1, state: "recalled" } };
  await dispatchNativeMessage(value);
  assert.ok(reads >= 2); assert.equal(value.runs.length, 0); assert.equal(value.state.get("m1")?.dispatchManifest, undefined);
});
test("catchup/import messages are not new automatic model turns or read projections", async t => {
  for (const origin of ["catchup", "import", "unknown"]) { const value = fixture(t, { origin }); assert.equal(await dispatchNativeMessage(value), "ignored"); assert.equal(value.runs.length, 0); }
});
test("self-sends from another device or this hub never start a model or mark a chat read", async t => {
  for (const origin of ["live", "local_send"]) {
    for (const threadId of ["u", "filehelper"]) {
      const value = fixture(t, { origin });
      value.message.direction = "outgoing"; value.message.senderId = "sim_self"; value.message.threadId = threadId;
      assert.equal(await dispatchNativeMessage(value), "ignored");
      assert.equal(value.runs.length, 0); assert.equal(value.state.get("m1")?.readPending, undefined);
    }
  }
});
test("released SDK missing prepared hook leaves read unchanged and records uncertainty", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" }); value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async () => {};
  assert.equal(await dispatchNativeMessage(value), "completed"); assert.equal(value.state.get("m1")?.readPending, undefined); assert.equal(value.state.get("m1")?.handoffUnconfirmed, true);
});
test("default inbound read is durable after normal handling without any model callback", async t => {
  const value = fixture(t);
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
    assert.equal(params.replyOptions.onPreparedAgentRunStart, undefined);
    assert.equal(value.state.get("m1")?.readPending, undefined);
    value.runs.push(params);
  };
  assert.equal(await dispatchNativeMessage(value), "completed");
  const reopened = new NativeConsumerState(dirname(value.state.file), "s", "a");
  assert.equal(reopened.get("m1")?.readPending, true);
  assert.equal(reopened.get("m1")?.runId, undefined);
  assert.equal(reopened.get("m1")?.handoffUnconfirmed, false);
  let requests = 0;
  value.client.threads = async () => ({ items: [{ id: "u", readStateVersion: "1" } as any], nextCursor: null, sourceGeneration: "g", coverage: {} });
  value.client.markRead = async () => { requests++; return { phoneNotification: { state: "accepted" } }; };
  await projectNativeRead(value.client, "a", reopened);
  await projectNativeRead(value.client, "a", reopened);
  assert.equal(reopened.get("m1")?.readPending, false);
  assert.equal(reopened.get("m1")?.phoneReadState, "accepted");
  assert.equal(await dispatchNativeMessage({ ...value, state: reopened }), "needs_review");
  assert.equal(requests, 1); assert.equal(value.runs.length, 1);
});
test("inbound commands queue a thread read without starting a model", async t => {
  const value = fixture(t, { readPolicy: "on_inbound" }); value.message.body = "/status";
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
    assert.equal(params.ctx.CommandBody, "/status");
    assert.equal(params.replyOptions.onPreparedAgentRunStart, undefined);
  };
  assert.equal(await dispatchNativeMessage(value), "completed");
  assert.equal(value.state.get("m1")?.readPending, true);
  assert.equal(value.state.get("m1")?.runId, undefined);
});
test("never policy does not queue an inbound read", async t => {
  const value = fixture(t, { readPolicy: "never" });
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async () => {};
  assert.equal(await dispatchNativeMessage(value), "completed");
  assert.equal(value.state.get("m1")?.readPending, undefined);
  assert.equal(value.state.get("m1")?.handoffUnconfirmed, false);
});
test("failed or interrupted inbound handling stays unread without automatic replay", async t => {
  for (const throws of [false, true]) {
    const value = fixture(t);
    value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
      value.runs.push(params);
      if (throws) throw Error("fixture handler failure");
      params.dispatcherOptions.onError();
    };
    assert.equal(await dispatchNativeMessage(value), "needs_review");
    assert.equal(value.state.get("m1")?.readPending, undefined);
    assert.equal(await dispatchNativeMessage(value), "needs_review");
    assert.equal(value.runs.length, 1);
  }
});
test("a command rejected by access checks does not gain an inbound read from its denial reply", async t => {
  const value = fixture(t);
  value.core.channel.inbound.ingress.resolveStable = async () => ({ ingress: { admission: "skip", reasonCode: "control_command_unauthorized" }, activationAccess: {}, commandAccess: { authorized: false } });
  assert.equal(await dispatchNativeMessage(value), "completed");
  assert.equal(value.runs.length, 1);
  assert.equal(value.state.get("m1")?.readPending, undefined);
});
test("failure after prepared handoff retains read receipt and prevents model replay", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" }); value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => { params.replyOptions.onPreparedAgentRunStart("run_1"); throw new Error("provider failed after handoff"); };
  assert.equal(await dispatchNativeMessage(value), "needs_review"); assert.equal(value.state.get("m1")?.readPending, true);
  assert.equal(await dispatchNativeMessage(value), "needs_review");
});
test("queued native reply waits for accepted without submitting a second send", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" }); let submissions = 0; let inspections = 0;
  value.client.sendText = async () => { submissions++; return { id: "send_1", state: "queued" } as any; };
  value.client.send = async (_account, sendId, signal) => {
    assert.equal(sendId, "send_1"); assert.equal(signal?.aborted, false);
    return { id: "send_1", state: ++inspections === 1 ? "submitting" : "accepted" } as any;
  };
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
    params.replyOptions.onPreparedAgentRunStart("run_1"); await params.dispatcherOptions.deliver({ text: "reply fixture" });
  };
  assert.equal(await dispatchNativeMessage(value), "completed");
  assert.equal(submissions, 1); assert.equal(inspections, 2); assert.equal(value.state.get("m1")?.state, "completed");
});
test("queued native reply ending needs_review retains uncertainty without resend", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" }); let submissions = 0; let inspections = 0;
  value.client.sendText = async () => { submissions++; return { id: "send_1", state: "queued" } as any; };
  value.client.send = async () => { inspections++; return { id: "send_1", state: "needs_review" } as any; };
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
    params.replyOptions.onPreparedAgentRunStart("run_1"); await params.dispatcherOptions.deliver({ text: "reply fixture" });
  };
  assert.equal(await dispatchNativeMessage(value), "needs_review");
  assert.equal(await dispatchNativeMessage(value), "needs_review");
  assert.equal(submissions, 1); assert.equal(inspections, 1); assert.equal(value.state.get("m1")?.readPending, true);
});
test("whole-thread read CAS refreshes version without sending message IDs", async t => {
  const value = fixture(t); value.state.accept("2", { ...value.message, id: "gap" }, "live"); await dispatchNativeMessage(value);
  const requests: any[] = []; let attempt = 0;
  value.client.threads = async () => ({ items: [{ id: "u", readStateVersion: String(attempt) } as any], nextCursor: null, sourceGeneration: "g", coverage: {} });
  value.client.markRead = async (_account, threadId, expectedVersion) => { requests.push({ threadId, expectedVersion }); if (attempt++ === 0) throw new NativeApiError(409, "conflict"); return {}; };
  await projectNativeRead(value.client, "a", value.state);
  assert.deepEqual(requests, [{ threadId: "u", expectedVersion: "0" }, { threadId: "u", expectedVersion: "1" }]);
  assert.equal(value.state.get("gap")?.readPending, undefined); assert.equal(value.state.get("m1")?.readPending, false);
});
test("one thread read settles prepared receipts and retains remote uncertainty without retry", async t => {
  const value = fixture(t); await dispatchNativeMessage(value);
  value.state.accept("2", { ...value.message, id: "m2" }, "live");
  value.state.update("m2", { state: "handed_off", runId: "second-run", readPending: true });
  let requests = 0;
  value.client.threads = async () => ({ items: [{ id: "u", readStateVersion: "v" } as any], nextCursor: null, sourceGeneration: "g", coverage: {} });
  value.client.markRead = async (_account, threadId, expectedVersion) => {
    requests++; assert.equal(threadId, "u"); assert.equal(expectedVersion, "v");
    return { phoneReceiptSent: false, phoneNotification: { state: "needs_review", retryable: false } };
  };
  await projectNativeRead(value.client, "a", value.state);
  await projectNativeRead(value.client, "a", value.state);
  assert.equal(requests, 1);
  for (const id of ["m1", "m2"]) {
    assert.equal(value.state.get(id)?.readPending, false);
    assert.equal(value.state.get(id)?.phoneReadState, "needs_review");
  }
});
test("rejected group media never reaches retrieval, transcription or the model", async t => {
  const value=fixture(t,{group:true});value.message.type=34;value.message.body=null;let preparations=0;
  assert.equal(await dispatchNativeMessage({...value,prepareAttachment:async()=>{preparations++;throw Error('must not prepare');}}),'ignored');assert.equal(preparations,0);assert.equal(value.runs.length,0);assert.equal(value.state.get('m1')?.readPending,undefined);
});
test("pending native media keeps dispatch custody without model or read receipt",async t=>{
  const value=fixture(t);value.message.type=3;value.message.body='<private transport XML>';
  assert.equal(await dispatchNativeMessage({...value,prepareAttachment:async()=>({body:'image pending',pending:true})}),'pending');assert.equal(value.runs.length,0);assert.equal(value.state.get('m1')?.state,'received');assert.equal(value.state.get('m1')?.readPending,undefined);
});
test("incoming download status failures recover after ten minutes and media.ready without a second transfer", async t => {
  let clock = 0; t.mock.method(Date, "now", () => clock);
  for (const failureStage of ["initial", "later"]) {
    clock = 0;
    const value = fixture(t); value.message.type = 49; value.message.appMessageType = 6; value.message.body = null;
    const source = Buffer.from("%PDF-synthetic-small-file"), sha256 = createHash("sha256").update(source).digest("hex");
    let transfers = 0, polls = 0, attached = false, ready = false, failStatus = true;
    Object.assign(value.client, {
      mediaAssets: async () => ({ items: [{ id: "file-asset", accountId: "a", messageId: value.message.id, kind: "file", variant: "original", actualVariant: ready ? "original" : null,
        state: ready ? "available" : "pending", byteLength: source.length, sha256, filename: "synthetic.pdf", jobId: attached ? "download-job" : null }] }),
      transferMedia: async () => { transfers++; attached = true; return { id: "download-job", accountId: "a", assetId: "file-asset", kind: "download", state: "running" }; },
      job: async (_account: string, id: string) => { polls++; assert.equal(id, "download-job");
        if (failStatus) { failStatus = false; throw new TypeError("synthetic network interruption"); }
        return { id, accountId: "a", assetId: "file-asset", kind: "download", state: ready ? "available" : "running" }; },
      mediaContent: async () => ({ bytes: source, contentType: "application/pdf" }),
    });
    value.core.channel.media = { saveMediaBuffer: async () => ({ path: "/managed/synthetic.pdf" }) };
    const params = { ...value, prepareAttachment: (input: Parameters<typeof prepareNativeAttachment>[0]) => prepareNativeAttachment({ ...input,
      ...(failureStage === "later" && transfers === 0 ? { waitMs: 0 } : {}) }) };
    assert.equal(await dispatchNativeMessage(params), "pending");
    if (failureStage === "later") assert.equal(await dispatchNativeMessage(params), "pending");
    assert.equal(polls, 1); assert.equal(transfers, 1); assert.equal(value.runs.length, 0);
    assert.equal(value.state.get(value.message.id)?.state, "received"); assert.equal(value.state.get(value.message.id)?.readPending, undefined);
    clock = 10 * 60000;
    assert.equal(await dispatchNativeMessage(params), "pending");
    assert.equal(polls, 2); assert.equal(transfers, 1); assert.equal(value.state.get(value.message.id)?.state, "received");
    ready = true;
    await acceptNativeEvent(value.client, "a", value.state, { cursor: "2", kind: "media.ready", data: { accountId: "a", assetId: "file-asset", jobId: "download-job" }, createdAt: "2026-10-22T12:10:00Z" });
    assert.equal(await dispatchNativeMessage(params), "completed");
    assert.equal(transfers, 1); assert.equal(value.runs.length, 1); assert.equal(value.state.get(value.message.id)?.state, "completed");
    assert.deepEqual(value.runs[0].ctx.Attachments.map((attachment: any) => attachment.path), ["/managed/synthetic.pdf"]);
  }
});
test("verified native media path is included in the correlated prepared model input",async t=>{
  const value=fixture(t,{readPolicy:"on_model_dispatch"});value.message.type=3;value.message.body='<private transport XML>';
  assert.equal(await dispatchNativeMessage({...value,prepareAttachment:async()=>({body:'<media:image>',pending:false,mediaPath:'/managed/fixture.png',mediaMime:'image/png',attachment:{kind:'image',status:'ready',filename:'fixture.png',path:'/managed/fixture.png',mime:'image/png'},suppliedAssets:[{id:"asset",sha256:"0".repeat(64),byteLength:10,mime:"image/png",path:"/managed/fixture.png",role:"model_image",kind:"image",runtimeMedia:true,contextRole:"trigger"}]} )}),'completed');
  assert.equal(value.runs[0].ctx.MediaPath,'/managed/fixture.png');assert.equal(value.runs[0].ctx.BodyForAgent,'<media:image>');assert.equal(value.state.get('m1')?.readPending,true);assert.equal(value.state.get('m1')?.runId,'run_1');
});
test("512 MiB file metadata reaches correlated dispatch and one excess byte stops before model handoff", async t => {
  for (const excess of [0, 1]) {
    const value = fixture(t, { readPolicy: "on_model_dispatch" }); value.message.type = 49; value.message.appMessageType = 6; value.message.body = null;
    const byteLength = 512 * 1024 * 1024 + excess;
    const result = await dispatchNativeMessage({ ...value, prepareAttachment: async () => ({ body: "[Local file: </managed/large.pdf>]", pending: false,
      attachment: { kind: "file", status: "ready", filename: "large.pdf", path: "/managed/large.pdf", mime: "application/pdf" },
      suppliedAssets: [{ id: "large-file-asset", sha256: "0".repeat(64), byteLength, mime: "application/pdf", path: "/managed/large.pdf",
        role: "file_reference", kind: "file", runtimeMedia: false, contextRole: "trigger" }] }) });
    assert.equal(result, excess ? "needs_review" : "completed"); assert.equal(value.runs.length, excess ? 0 : 1);
    assert.equal(value.state.get(value.message.id)?.readPending, excess ? undefined : true);
    if (!excess) {
      const asset = value.state.get(value.message.id)!.dispatchManifest!.assets[0];
      assert.equal(asset.byteLength, byteLength); assert.equal(asset.role, "file_reference"); assert.equal(asset.runtimeMedia, false);
      assert.equal(value.state.get(value.message.id)?.runId, "run_1");
    }
  }
});
test("media preparation failure cannot record a prepared input or mark read",async t=>{
  const value=fixture(t);value.message.type=3;
  assert.equal(await dispatchNativeMessage({...value,prepareAttachment:async()=>{throw Error('integrity failure');}}),'needs_review');assert.equal(value.runs.length,0);assert.equal(value.state.get('m1')?.readPending,undefined);
});


test("late media receipt retries its whole-thread read during quiet SSE without another model turn", async t => {
  const value = fixture(t); value.state.update("m1", { state: "dispatching" });
  let requests = 0, failures = 0;
  value.client.threads = async () => ({ items: [{ id: "u", readStateVersion: "1" } as any], nextCursor: null, sourceGeneration: "g", coverage: {} });
  value.client.markRead = async (_account, threadId, expectedVersion) => { assert.equal(threadId, "u"); assert.equal(expectedVersion, "1"); if (++requests === 1) throw Error("lost response"); return {}; };
  const stop = startNativeReadPump(() => projectNativeRead(value.client, "a", value.state), () => { failures++; }, 5); t.after(stop);
  await new Promise(resolve => setTimeout(resolve, 15)); assert.equal(requests, 0);
  value.state.preparedCallback(["m1"], "on_model_dispatch")("late-media-run");
  for (let attempt = 0; attempt < 50 && value.state.get("m1")?.readPending; attempt++) await new Promise(resolve => setTimeout(resolve, 5));
  assert.equal(requests, 2); assert.equal(failures, 1); assert.equal(value.state.get("m1")?.readPending, false); assert.equal(value.runs.length, 0);
  stop(); const before = requests; await new Promise(resolve => setTimeout(resolve, 15)); assert.equal(requests, before);
});

test("group attachments buffered before a mention survive restart and retain original mention facts", async t => {
  const value = fixture(t, { group: true });
  value.core.mediaUnderstanding = { transcribeAudioFile: async () => ({ text: "Historical voice transcript" }) };
  const prepared: string[] = [];
  const prepareAttachment: any = async ({ message }: any) => {
    prepared.push(message.id);
    if (message.type === 1) return { body: message.body, pending: false };
    const kind = message.type === 3 ? "image" : message.type === 34 ? "audio" : message.type === 43 ? "video" : "file";
    const path = `/managed/${message.id}.original`, preview = kind === "image" ? `/managed/${message.id}.preview.jpg` : path;
    return { body: `[${kind} file: ${path}]`, pending: false, attachment: { kind, status: "ready", filename: message.id, path, mime: kind === "image" ? "image/jpeg" : "application/octet-stream" },
      mediaPath: ["image", "video"].includes(kind) ? preview : undefined, mediaMime: "image/jpeg",
      suppliedAssets: [{ id: message.id, sha256: "0".repeat(64), byteLength: 10, mime: "image/jpeg", path: preview,
        sourcePath: path, sourceSha256: "1".repeat(64), sourceByteLength: 20, sourceMime: "image/png", role: kind === "image" ? "model_image" : "file_reference", kind, runtimeMedia: ["image", "video"].includes(kind), contextRole: "trigger" }] };
  };
  for (const [index, type] of [3, 3, 49, 43, 34].entries()) {
    const message = index === 0 ? value.message : { ...value.message, id: `history-${index}` };
    message.type = type; message.body = "<private transport XML>";
    if (index) value.state.accept(String(index + 1), message, "live");
    assert.equal(await dispatchNativeMessage({ ...value, message, prepareAttachment }), "ignored");
  }
  assert.deepEqual(prepared, []); assert.equal(value.runs.length, 0);
  const state = new NativeConsumerState(dirname(value.state.file), "s", "a");
  value.client.message = async (_account, id) => state.get(id)!.message;
  const trigger = { ...value.message, id: "trigger", type: 1, body: "What did we share?", mentions: { ...value.message.mentions, state: "known" as const, self: true } };
  state.accept("6", trigger, "live");
  assert.equal(await dispatchNativeMessage({ ...value, state, message: trigger, prepareAttachment }), "completed");
  const ctx = value.runs[0].ctx; assert.equal(ctx.WasMentioned, true); assert.equal(ctx.Attachments.length, 5); assert.equal(ctx.InboundHistory.length, 5);
  for (const attachment of ctx.Attachments) assert.ok(ctx.BodyForAgent.includes(attachment.path));
  assert.match(ctx.BodyForAgent, /Historical voice transcript/); assert.ok(!ctx.BodyForAgent.includes("private transport XML"));
  assert.equal(ctx.MediaPath, "/managed/history-1.preview.jpg");
  const manifest = state.get("trigger")!.dispatchManifest!;
  assert.deepEqual(manifest.newInputs.map(input => input.id), ["trigger"]); assert.equal(manifest.historyInputs?.length, 5);
  assert.equal(manifest.assets.filter(asset => asset.runtimeMedia).length, 1); assert.equal(manifest.assets.filter(asset => asset.role === "model_image").length, 1);
  assert.equal(manifest.assets.find(asset => asset.runtimeMedia)?.contextRole, "history");
  assert.equal(state.get("m1")?.message.mentions?.self, null); assert.equal(state.get("m1")?.readPending, undefined);
  assert.equal(state.get("trigger")?.readPending, true); assert.deepEqual(state.groupHistory(trigger.threadId, trigger.id, 50), []);
});
test("historical sender revocation excludes media before retrieval and commands do not consume context", async t => {
  const value = fixture(t, { group: true }); value.message.type = 3;
  await dispatchNativeMessage(value); assert.equal(value.state.get("m1")?.groupContext, true);
  const command = { ...value.message, id: "command", type: 1, body: "/status", mentions: { ...value.message.mentions, state: "known" as const, self: true } };
  value.state.accept("2", command, "live");
  value.core.channel.commands.shouldHandleTextCommands = () => true; value.core.channel.commands.isControlCommandMessage = () => true;
  await dispatchNativeMessage({ ...value, message: command }); assert.equal(value.state.get("m1")?.groupContext, true);
  value.core.channel.commands.isControlCommandMessage = () => false;
  value.core.channel.inbound.ingress.resolveStable = async (params: any) => ({ ingress: { admission: params.subject.stableId === "revoked" ? "drop" : "dispatch" }, commandAccess: {}, activationAccess: {} });
  value.message.senderId = "revoked";
  const trigger = { ...command, id: "trigger", senderId: "allowed", body: "mention" }; value.state.accept("3", trigger, "live");
  const prepared: string[] = [];
  assert.equal(await dispatchNativeMessage({ ...value, message: trigger, prepareAttachment: async ({ message }) => { prepared.push(message.id); return { body: "current", pending: false }; } }), "completed");
  assert.deepEqual(prepared, ["trigger"]); assert.equal(value.runs[1].ctx.InboundHistory, undefined);
});
test("a pending historical attachment stays explicit without blocking the current mention", async t => {
  const value = fixture(t, { group: true }); value.message.type = 3; await dispatchNativeMessage(value);
  const trigger = { ...value.message, id: "trigger", type: 1, body: "mention", mentions: { ...value.message.mentions, state: "known" as const, self: true } }; value.state.accept("2", trigger, "live");
  assert.equal(await dispatchNativeMessage({ ...value, message: trigger, prepareAttachment: async ({ message }) => ({ body: message.type === 3 ? "[image pending]" : "current", pending: message.type === 3 }) }), "completed");
  assert.match(value.runs[0].ctx.BodyForAgent, /image pending/); assert.deepEqual(value.runs[0].ctx.Attachments, []); assert.equal(value.runs[0].ctx.MediaPath, undefined);
});

function deferred<T>() { let resolve!: (value: T) => void, reject!: (reason?: unknown) => void; const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; }); return { promise, resolve, reject }; }
const recallEvent = (message: NativeMessage, cursor = "2") => ({ cursor, kind: "message.recalled", createdAt: "2026-10-21T12:00:00Z",
  data: { threadId: message.threadId, serverId: message.serverId!, observedAt: "2026-10-21T12:00:00Z", origin: "live" } });
test("incoming recalls bypass self and group mention gates and invalidate custody before ACK", async t => {
  for (const own of [true, false]) {
    const value = fixture(t, { group: true }); value.message.direction = own ? "outgoing" : "incoming";
    await acceptNativeEvent(value.client, "a", value.state, recallEvent(value.message));
    assert.equal(value.state.cursor, "2"); assert.equal(value.state.get(value.message.id)?.message.body, "[Message recalled]");
    assert.equal(await dispatchNativeMessage(value), "ignored"); assert.equal(value.runs.length, 0);
  }
  const value = fixture(t); await assert.rejects(acceptNativeEvent(value.client, "other-account", value.state, recallEvent(value.message)), /account boundary/);
  assert.equal(value.state.cursor, "1");
});
test("a recall during attachment preparation cannot deliver a late file or start a model", async t => {
  const value = fixture(t); value.message.type = 3;
  const started = deferred<void>(), pending = deferred<any>();
  const work = dispatchNativeMessage({ ...value, prepareAttachment: async () => { started.resolve(); return pending.promise; } });
  await started.promise;
  await acceptNativeEvent(value.client, "a", value.state, recallEvent(value.message));
  pending.resolve({ body: "[Local file: /private/late.png]", mediaPath: "/private/late.png", pending: false });
  assert.equal(await work, "ignored"); assert.equal(value.runs.length, 0); assert.equal(value.state.get(value.message.id)?.dispatchManifest, undefined);
});
test("a recall during voice transcription discards the late transcript", async t => {
  const value = fixture(t); value.message.type = 34;
  const started = deferred<void>(), pending = deferred<any>();
  value.core.mediaUnderstanding = { transcribeAudioFile: async () => { started.resolve(); return pending.promise; } };
  const work = dispatchNativeMessage({ ...value, prepareAttachment: async () => ({ body: "voice", pending: false,
    attachment: { kind: "audio", status: "ready", filename: "voice.wav", path: "/private/late-voice.wav" } }) });
  await started.promise; await acceptNativeEvent(value.client, "a", value.state, recallEvent(value.message));
  pending.resolve({ text: "late private transcript" });
  assert.equal(await work, "ignored"); assert.equal(value.runs.length, 0); assert.equal(value.state.get(value.message.id)?.message.body, "[Message recalled]");
});
test("a historical recall during media work lets the triggering mention retry with sanitized context", async t => {
  const value = fixture(t, { group: true }); value.message.type = 3;
  assert.equal(await dispatchNativeMessage(value), "ignored");
  const trigger: NativeMessage = { ...value.message, id: "mention", serverId: "9007199254741000", type: 1, body: "current mention",
    mentions: { ...value.message.mentions, state: "known", self: true } };
  value.state.accept("2", trigger, "live");
  const started = deferred<void>(), pending = deferred<any>();
  const prepareAttachment: any = async ({ message }: any) => message.id === trigger.id ? { body: message.body, pending: false }
    : (started.resolve(), pending.promise);
  const work = dispatchNativeMessage({ ...value, message: trigger, prepareAttachment });
  await started.promise; await acceptNativeEvent(value.client, "a", value.state, recallEvent(value.message, "3"));
  pending.resolve({ body: "[Local file: /private/recalled-history.png]", pending: false });
  assert.equal(await work, "pending"); assert.equal(value.runs.length, 0);
  assert.equal(await dispatchNativeMessage({ ...value, message: trigger, prepareAttachment }), "completed");
  assert.equal(value.runs.length, 1); assert.equal(value.runs[0].ctx.InboundHistory, undefined);
  assert.ok(!value.runs[0].ctx.BodyForAgent.includes("recalled-history"));
});
test("recall intake can invalidate an in-flight model turn and suppress its later reply", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" });
  const started = deferred<any>(), finished = deferred<void>(); let sends = 0;
  value.client.sendText = async () => { sends++; return { id: "must-not-send", state: "accepted" } as any; };
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
    value.runs.push(params); params.replyOptions.onPreparedAgentRunStart("run-before-recall"); started.resolve(params); await finished.promise;
  };
  const work = dispatchNativeMessage(value); const turn = await started.promise;
  await acceptNativeEvent(value.client, "a", value.state, recallEvent(value.message));
  await turn.dispatcherOptions.deliver({ text: "late automatic reply" });
  assert.throws(() => turn.replyOptions.onPreparedAgentRunStart("late-run"), /invalidated/);
  finished.resolve(); assert.equal(await work, "ignored"); assert.equal(sends, 0);
  assert.equal(value.state.get(value.message.id)?.readPending, false);
  assert.equal(value.state.get(value.message.id)?.dispatchManifest, undefined);
});
test("cancellation during a late reply's point-read cannot submit after the recheck returns", async t => {
  const value = fixture(t); const abort = new AbortController(); let sends = 0;
  value.client.sendText = async () => { sends++; return { id: "must-not-send", state: "accepted" } as any; };
  value.client.message = async (_account, id) => { if (value.runs.length) abort.abort(); return value.state.get(id)!.message; };
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
    value.runs.push(params); await params.dispatcherOptions.deliver({ text: "late reply" });
  };
  assert.equal(await dispatchNativeMessage({ ...value, signal: abort.signal }), "needs_review"); assert.equal(sends, 0);
  assert.equal(value.state.get(value.message.id)?.readPending, undefined);
});
test("cancellation during voice transcription cannot start a late model turn", async t => {
  const value = fixture(t); const abort = new AbortController(); value.message.type = 34;
  value.core.mediaUnderstanding = { transcribeAudioFile: async () => { abort.abort(); return { text: "Late transcript" }; } };
  const prepareAttachment: any = async () => ({ pending: false, body: "Voice", attachment: { kind: "audio", status: "ready", path: "/synthetic/voice.wav", mime: "audio/wav" } });
  assert.equal(await dispatchNativeMessage({ ...value, signal: abort.signal, prepareAttachment }), "needs_review");
  assert.equal(value.runs.length, 0); assert.equal(value.state.get(value.message.id)?.readPending, undefined);
});
test("reconnect drains durable recall events before dispatching previously received originals", async t => {
  const value = fixture(t); const acknowledgements: string[] = [];
  value.client.events = async () => ({ items: [recallEvent(value.message)], nextCursor: "2" });
  value.client.ack = async (_account, _consumer, cursor) => { acknowledgements.push(cursor); };
  await synchronizeNativeEvents(value.client, "a", value.state);
  assert.deepEqual(acknowledgements, ["2"]); assert.equal(await dispatchNativeMessage(value), "ignored"); assert.equal(value.runs.length, 0);
});
test("a delayed recalled attachment cannot discard a new exact message reusing its source ID", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" }); value.message.type = 3;
  const started = deferred<void>(), lateMedia = deferred<any>();
  const prepareAttachment: any = async () => { started.resolve(); return lateMedia.promise; };
  const workA = dispatchNativeMessage({ ...value, prepareAttachment }); await started.promise;
  await acceptNativeEvent(value.client, "a", value.state, recallEvent(value.message, "2"));
  const b: NativeMessage = { ...value.message, serverId: "9007199254741000", type: 1, body: "New exact message" };
  value.client.message = async () => b;
  await acceptNativeEvent(value.client, "a", value.state, { cursor: "3", kind: "message.committed", data: { messageId: b.id, threadId: b.threadId, origin: "live" }, createdAt: "2026-10-21T12:00:00Z" });
  lateMedia.resolve({ body: "[Local file: /private/old-source.png]", pending: false }); assert.equal(await workA, "ignored");
  assert.equal(value.state.get(b.id)?.state, "received"); assert.equal(value.state.get(b.id)?.message.body, b.body);
  assert.equal(await dispatchNativeMessage({ ...value, message: b }), "completed");
  value.state.accept("4", b, "live"); assert.equal(await dispatchNativeMessage({ ...value, message: b }), "needs_review");
  assert.equal(value.runs.length, 1); assert.equal(value.runs[0].ctx.BodyForAgent, b.body);
  assert.ok(!JSON.stringify(value.runs).includes("old-source"));
});
test("a delayed old media failure cannot overwrite a replacement already dispatched once", async t => {
  const value = fixture(t); value.message.type = 3;
  const started = deferred<void>(), lateMedia = deferred<any>();
  const workA = dispatchNativeMessage({ ...value, prepareAttachment: async () => { started.resolve(); return lateMedia.promise; } }); await started.promise;
  const b: NativeMessage = { ...value.message, serverId: "9007199254741000", type: 1, body: "Replacement after media failure" };
  value.state.accept("2", b, "live"); value.client.message = async () => b;
  assert.equal(await dispatchNativeMessage({ ...value, message: b }), "completed"); const revisionB = value.state.get(b.id)?.revision;
  lateMedia.reject(Error("Old download failed")); assert.equal(await workA, "needs_review");
  assert.equal(value.state.get(b.id)?.state, "completed"); assert.equal(value.state.get(b.id)?.revision, revisionB);
  assert.equal(value.state.get(b.id)?.readPending, true); assert.equal(value.runs.length, 1);
});
test("late old model completion or failure cannot assign its receipt or state to a new occupant", async t => {
  for (const fails of [false, true]) {
    const value = fixture(t, { readPolicy: "on_model_dispatch" });
    const started = deferred<any>(), finishA = deferred<void>();
    value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
      value.runs.push(params);
      if (params.ctx.BodyForAgent === value.message.body) { started.resolve(params); await finishA.promise; if (fails) throw Error("Old model failed"); }
      else params.replyOptions.onPreparedAgentRunStart("replacement-run");
    };
    const workA = dispatchNativeMessage(value); const oldTurn = await started.promise;
    if (!fails) await acceptNativeEvent(value.client, "a", value.state, recallEvent(value.message, "2"));
    const b: NativeMessage = { ...value.message, serverId: "9007199254741000", body: "New model occupant" };
    value.state.accept(fails ? "2" : "3", b, "live"); value.client.message = async () => b;
    assert.equal(await dispatchNativeMessage({ ...value, message: b }), "completed"); const revisionB = value.state.get(b.id)?.revision;
    assert.throws(() => oldTurn.replyOptions.onPreparedAgentRunStart("old-run"), /source identity changed/);
    await assert.rejects(oldTurn.dispatcherOptions.deliver({ text: "Old reply must not send" }));
    finishA.resolve(); assert.equal(await workA, fails ? "needs_review" : "ignored");
    assert.equal(value.state.get(b.id)?.state, "completed"); assert.equal(value.state.get(b.id)?.revision, revisionB);
    assert.equal(value.state.get(b.id)?.runId, "replacement-run"); assert.equal(value.state.get(b.id)?.readPending, true);
    assert.equal(await dispatchNativeMessage({ ...value, message: b }), "needs_review"); assert.equal(value.runs.length, 2);
  }
});
test("an old read completion cannot clear a replacement's new read receipt", async t => {
  const value = fixture(t); value.state.update(value.message.id, { state: "completed", readPending: true });
  const started = deferred<void>(), threads = deferred<any>(); let reads = 0;
  value.client.threads = async () => { started.resolve(); return threads.promise; };
  value.client.markRead = async () => { reads++; return { phoneNotification: { state: "accepted" } }; };
  const reading = projectNativeRead(value.client, "a", value.state); await started.promise;
  const b = { ...value.message, serverId: "9007199254741000", body: "New read owner" };
  value.state.accept("2", b, "live"); value.state.update(b.id, { state: "completed", readPending: true });
  threads.resolve({ items: [{ id: b.threadId, readStateVersion: "1" }], nextCursor: null, coverage: {}, sourceGeneration: "fixture" }); await reading;
  assert.equal(value.state.get(b.id)?.readPending, true);
  await projectNativeRead(value.client, "a", value.state); assert.equal(value.state.get(b.id)?.readPending, false); assert.equal(reads, 2);
});
test("a fresh exact occupant has a distinct host turn and reply key while duplicate custody stays stable", async t => {
  const value = fixture(t); const keys: string[] = [];
  value.client.sendText = async (_account, _thread, _text, key) => { keys.push(key); return { id: "reply", state: "accepted" } as any; };
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => { value.runs.push(params); await params.dispatcherOptions.deliver({ text: "Synthetic reply" }); };
  assert.equal(await dispatchNativeMessage(value), "completed");
  await acceptNativeEvent(value.client, "a", value.state, recallEvent(value.message, "2"));
  const b = { ...value.message, serverId: "9007199254741000", body: "Fresh reply owner" };
  value.state.accept("3", b, "live"); value.client.message = async () => b;
  assert.equal(await dispatchNativeMessage({ ...value, message: b }), "completed");
  assert.notEqual(value.runs[0].ctx.MessageSid, value.runs[1].ctx.MessageSid); assert.equal(new Set(keys).size, 2);
  const manifestB = value.state.get(b.id)?.dispatchManifest;
  value.state.accept("4", { ...b }, "live"); assert.equal(await dispatchNativeMessage({ ...value, message: b }), "needs_review");
  assert.equal(value.state.get(b.id)?.dispatchManifest, manifestB); assert.equal(keys.length, 2);
});
