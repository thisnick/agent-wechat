import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { NativeApiError, type NativeMessage, type NativeWeChatClient } from "@agent-wechat/shared";
import { NativeConsumerState } from "./native-consumer-state.ts";
import { acceptNativeEvent, dispatchNativeMessage, NATIVE_CATCH_UP_LIMIT, nativeMayAnswer, nativeWasMentioned, projectNativeRead, readNativeGroupHistory, resumeFromCatchUpWindow, startNativeReadPump, synchronizeNativeEvents } from "./native-monitor.ts";
import { prepareNativeAttachment } from "./native-media.ts";
import { resolveWeChatAccount, type WeChatNativeConfig } from "./types.ts";
function fixture(t: any, options: { group?: boolean; mention?: boolean; origin?: string; readPolicy?: WeChatNativeConfig["readPolicy"] } = {}) {
  const directory = mkdtempSync(join(tmpdir(), "native-model-harness-")); t.after(() => rmSync(directory, { recursive: true, force: true }));
  const cfg: any = { channels: { "agent-wechat": { backend: "native", dmPolicy: "open", allowFrom: ["*"], groupPolicy: "open", groupAllowFrom: ["*"],
    native: { mode: "external", endpoint: "http://127.0.0.1:6174", accountId: "a", token: "synthetic-native-token-".repeat(3), consumerStateDir: directory, readPolicy: options.readPolicy } } } };
  const account = resolveWeChatAccount(cfg)!;
  const state = new NativeConsumerState(directory, "s", "a");
  const message: NativeMessage = { id: "m1", accountId: "a", threadId: options.group ? "r@chatroom" : "u", senderId: "u", serverId: "9007199254740999", type: 1, direction: "incoming", body: "fixture", createdAt: "2026-10-20T12:00:00Z", version: "1", media: [], mentions: { state: options.mention ? "known" : "unknown", userIds: [], self: options.mention ?? null, all: null, source: null } };
  // The daemon's saved messages, newest last. Queued work stores only identities.
  const store = new Map<string, NativeMessage>([[message.id, message]]);
  state.accept("1", message, options.origin ?? "live");
  const add = (cursor: string, next: NativeMessage, origin = "live") => { store.set(next.id, next); state.accept(cursor, next, origin); return next; };
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
    message: async (_account: string, id: string) => store.get(id)!,
    messages: async (_account: string, threadId: string) => ({ items: [...store.values()].filter(item => item.threadId === threadId).reverse(),
      nextCursor: null, coverage: {}, sourceGeneration: "g" }) } as unknown as NativeWeChatClient;
  return { cfg, account, state, message, core, client, runs, store, add, createPipeline: (() => ({ onModelSelected: () => {} })) as any };
}
test("structured self mentions use exact facts, unknown and display text do not activate", t => {
  const value = fixture(t); value.message.body = "@Self Name fixture"; assert.equal(nativeWasMentioned(value.message), false);
  value.message.mentions = { state: "known", userIds: ["different"], self: false, all: null, source: "msgsource.atuserlist" }; assert.equal(nativeWasMentioned(value.message), false);
  value.message.mentions.all = true; assert.equal(nativeWasMentioned(value.message), true);
  value.message.mentions.state = "unknown"; assert.equal(nativeWasMentioned(value.message), false);
});
test("group mention gate runs before model dispatch and leaves ignored messages unread", async t => {
  const value = fixture(t, { group: true }); assert.equal(await dispatchNativeMessage(value), "ignored");
  assert.equal(value.runs.length, 0); assert.equal(value.state.readPending(value.message.threadId), false);
});
test("a known all-members mention satisfies the group gate without a direct self mention", async t => {
  const value = fixture(t, { group: true, mention: false });
  value.message.mentions.state = "known";
  value.message.mentions.source = "msgsource.atuserlist";
  value.message.mentions.all = true;
  value.state.accept("2", value.message, "live");
  assert.equal(await dispatchNativeMessage(value), "completed");
  assert.equal(value.runs.length, 1);
  assert.equal(value.runs[0].ctx.WasMentioned, true);
});
test("prepared model harness records exact input custody and read projection separately", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" }); assert.equal(await dispatchNativeMessage(value), "completed");
  assert.equal(value.runs.length, 1); assert.equal(value.state.get("m1"), undefined); assert.equal(value.state.readPending(value.message.threadId), true);
  assert.match(value.runs[0].ctx.MessageSid, /^wechat-native:a:m1:[a-f0-9]{64}$/);
});
test("application text shares reach model input without media access", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" });
  const text = "https://example.invalid/shared?a=1&b=2";
  Object.assign(value.message, { type: 49, appMessageType: 1, body: text });
  await dispatchNativeMessage(value);
  assert.equal(value.runs.length, 1);
  assert.ok(value.runs[0].ctx.BodyForAgent.includes(text));
  assert.equal(value.state.readPending(value.message.threadId), true);
});
test("upload progress notices do not start model work before a final attachment", async t => {
  const value = fixture(t);
  Object.assign(value.message, { type: 49, appMessageType: 74, body: "[File upload notice]\nreport.pdf" });
  assert.equal(await dispatchNativeMessage(value), "ignored");
  assert.equal(value.runs.length, 0);
  assert.equal(value.state.readPending(value.message.threadId), false);
});

test("location context respects mention gates, read policy and source rechecks", async t => {
  const location = { latitude: 12.5, longitude: -45.25, name: "Example park", address: null, scale: null, mapType: null, poiId: null, coordinateSystem: "unknown" as const };
  const body = "[Location]\nExample park\nLatitude: 12.5\nLongitude: -45.25\nCoordinate system: unknown";
  for (const group of [false, true]) {
    const value = fixture(t, { group, readPolicy: "on_model_dispatch" });
    Object.assign(value.message, { type: 48, location, body });
    assert.equal(await dispatchNativeMessage(value), group ? "ignored" : "completed");
    assert.equal(value.runs.length, group ? 0 : 1);
    if (!group) {
      assert.ok(value.runs[0].ctx.BodyForAgent.includes(body));
      assert.equal(value.state.readPending(value.message.threadId), true);
    }
  }
  const stale = fixture(t);
  Object.assign(stale.message, { type: 48, location, body });
  stale.client.message = async () => ({ ...stale.message, location: { ...location, latitude: 13 } });
  assert.equal(await dispatchNativeMessage(stale), "needs_review");
  assert.equal(stale.runs.length, 0);
  const malformed = fixture(t);
  Object.assign(malformed.message, { type: 48, body });
  assert.equal(await dispatchNativeMessage(malformed), "ignored");
});

test("location-sharing subtype 17 supplies a passive notice to the model", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" });
  const body = "[Location sharing notice]\nSharing location";
  Object.assign(value.message, { type: 49, appMessageType: 17, body });
  assert.equal(await dispatchNativeMessage(value), "completed");
  assert.ok(value.runs[0].ctx.BodyForAgent.includes(body));
  assert.equal(value.state.readPending(value.message.threadId), true);
});
test("embedded quoted links reach prepared model input without original resolution", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" });
  const link = "[Link] Article title\nArticle description\nhttps://example.com/article?a=1&b=2";
  Object.assign(value.message, { type: 49, appMessageType: 57, body: "Explain this article",
    quote: { serverId: "42", type: 49, appMessageType: 5, state: "embedded", senderId: "u", embeddedText: link } });
  assert.equal(await dispatchNativeMessage(value), "completed");
  assert.ok(value.runs[0].ctx.BodyForAgent.includes(link)); assert.ok(value.runs[0].ctx.BodyForAgent.includes("Explain this article"));
  assert.equal(value.state.readPending(value.message.threadId), true);
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
      const own = value.add("2", { ...value.message, id: "own", serverId: "2", direction: "outgoing", senderId: "sim_self", threadId }, origin);
      assert.equal(await dispatchNativeMessage({ ...value, message: own }), "ignored");
      assert.equal(value.runs.length, 0); assert.equal(value.state.readPending(threadId), false);
    }
  }
});
test("released SDK missing prepared hook leaves read unchanged and records uncertainty", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" }); value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async () => {};
  assert.equal(await dispatchNativeMessage(value), "completed"); assert.equal(value.state.readPending(value.message.threadId), false); assert.equal(value.state.get("m1")?.handoffUnconfirmed, true);
});
test("default inbound read is durable after normal handling without any model callback", async t => {
  const value = fixture(t);
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
    assert.equal(params.replyOptions.onPreparedAgentRunStart, undefined);
    assert.equal(value.state.readPending(value.message.threadId), false);
    value.runs.push(params);
  };
  assert.equal(await dispatchNativeMessage(value), "completed");
  const reopened = new NativeConsumerState(dirname(value.state.file), "s", "a");
  assert.equal(reopened.readPending("u"), true);
  assert.equal(reopened.get("m1"), undefined);
  let requests = 0;
  value.client.threads = async () => ({ items: [{ id: "u", readStateVersion: "1" } as any], nextCursor: null, sourceGeneration: "g", coverage: {} });
  value.client.markRead = async () => { requests++; return { phoneNotification: { state: "accepted" } }; };
  await projectNativeRead(value.client, "a", reopened);
  await projectNativeRead(value.client, "a", reopened);
  assert.equal(reopened.readPending("u"), false);
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
  assert.equal(value.state.readPending(value.message.threadId), true);
  assert.equal(value.state.get("m1"), undefined);
});
test("never policy does not queue an inbound read", async t => {
  const value = fixture(t, { readPolicy: "never" });
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async () => {};
  assert.equal(await dispatchNativeMessage(value), "completed");
  assert.equal(value.state.readPending(value.message.threadId), false);
  assert.equal(value.state.get("m1"), undefined);
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
    assert.equal(value.state.readPending(value.message.threadId), false);
    assert.equal(await dispatchNativeMessage(value), "needs_review");
    assert.equal(value.runs.length, 1);
  }
});
test("a command rejected by access checks does not gain an inbound read from its denial reply", async t => {
  const value = fixture(t);
  value.core.channel.inbound.ingress.resolveStable = async () => ({ ingress: { admission: "skip", reasonCode: "control_command_unauthorized" }, activationAccess: {}, commandAccess: { authorized: false } });
  assert.equal(await dispatchNativeMessage(value), "completed");
  assert.equal(value.runs.length, 1);
  assert.equal(value.state.readPending(value.message.threadId), false);
});
test("failure after prepared handoff retains read receipt and prevents model replay", async t => {
  const value = fixture(t, { readPolicy: "on_model_dispatch" }); value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => { params.replyOptions.onPreparedAgentRunStart("run_1"); throw new Error("provider failed after handoff"); };
  assert.equal(await dispatchNativeMessage(value), "needs_review"); assert.equal(value.state.readPending(value.message.threadId), true);
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
  assert.equal(submissions, 1); assert.equal(inspections, 2); assert.equal(value.state.get("m1"), undefined);
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
  assert.equal(submissions, 1); assert.equal(inspections, 1); assert.equal(value.state.readPending(value.message.threadId), true);
});
test("buffered group replies retain structured mention IDs instead of dropping them", async t => {
  const value = fixture(t, { group: true, mention: true }), calls: unknown[][] = [];
  value.client.sendText = (async (...args: unknown[]) => {
    calls.push(args); return { id: "send_mentions", accountId: "a", destination: value.message.threadId, state: "accepted" };
  }) as any;
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
    await params.dispatcherOptions.deliver({ text: "please check", channelData: { "agent-wechat": { mentions: { userIds: ["wxid_member"] } } } });
  };
  assert.equal(await dispatchNativeMessage(value), "completed"); assert.equal(calls.length, 1);
  assert.deepEqual(calls[0][4], { mentions: { userIds: ["wxid_member"] } }); assert.match(calls[0][3] as string, /^[a-f0-9]{64}$/);
});
test("buffered quoted group replies retain both the source identity and mention recipients", async t => {
  const value = fixture(t, { group: true, mention: true }), calls: unknown[][] = [];
  value.client.sendText = (async (...args: unknown[]) => {
    calls.push(args); return { id: "send_quoted_mentions", accountId: "a", destination: value.message.threadId, state: "accepted" };
  }) as any;
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
    await params.dispatcherOptions.deliver({ text: "please check", replyToId: value.message.id,
      channelData: { "agent-wechat": { mentions: { userIds: ["wxid_member"] } } } });
  };
  assert.equal(await dispatchNativeMessage(value), "completed"); assert.equal(calls.length, 1);
  assert.deepEqual(calls[0][4], { quote: { messageId: value.message.id, serverId: value.message.serverId }, mentions: { userIds: ["wxid_member"] } });
});
test("buffered reply mention conflicts fail before any message or attachment send", async t => {
  for (const payload of [{ text: "hi", mediaUrl: "/fixture/a.png" }, { text: "hi", audioAsVoice: true }]) {
    const value = fixture(t, { group: true, mention: true }); let sends = 0;
    value.client.sendText = async () => { sends++; throw Error("must not send"); };
    value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
      await params.dispatcherOptions.deliver({ ...payload, channelData: { "agent-wechat": { mentions: { all: true } } } });
    };
    assert.equal(await dispatchNativeMessage(value), "needs_review"); assert.equal(sends, 0);
  }
});
test("whole-thread read CAS refreshes version without sending message IDs", async t => {
  const value = fixture(t); value.add("2", { ...value.message, id: "gap", serverId: "2" }); await dispatchNativeMessage(value);
  const requests: any[] = []; let attempt = 0;
  value.client.threads = async () => ({ items: [{ id: "u", readStateVersion: String(attempt) } as any], nextCursor: null, sourceGeneration: "g", coverage: {} });
  value.client.markRead = async (_account, threadId, expectedVersion) => { requests.push({ threadId, expectedVersion }); if (attempt++ === 0) throw new NativeApiError(409, "conflict"); return {}; };
  await projectNativeRead(value.client, "a", value.state);
  assert.deepEqual(requests, [{ threadId: "u", expectedVersion: "0" }, { threadId: "u", expectedVersion: "1" }]);
  assert.equal(value.state.get("gap")?.state, "received"); assert.equal(value.state.readPending(value.message.threadId), false);
});
test("one thread read settles every request in the thread and retains remote uncertainty without retry", async t => {
  const value = fixture(t); await dispatchNativeMessage(value);
  const second = value.add("2", { ...value.message, id: "m2", serverId: "2" });
  assert.equal(await dispatchNativeMessage({ ...value, message: second }), "completed");
  let requests = 0;
  value.client.threads = async () => ({ items: [{ id: "u", readStateVersion: "v" } as any], nextCursor: null, sourceGeneration: "g", coverage: {} });
  value.client.markRead = async (_account, threadId, expectedVersion) => {
    requests++; assert.equal(threadId, "u"); assert.equal(expectedVersion, "v");
    return { phoneReceiptSent: false, phoneNotification: { state: "needs_review", retryable: false } };
  };
  await projectNativeRead(value.client, "a", value.state);
  await projectNativeRead(value.client, "a", value.state);
  assert.equal(requests, 1); assert.equal(value.state.readPending("u"), false);
});
test("rejected group media never reaches retrieval, transcription or the model", async t => {
  const value=fixture(t,{group:true});value.message.type=34;value.message.body=null;let preparations=0;
  assert.equal(await dispatchNativeMessage({...value,prepareAttachment:async()=>{preparations++;throw Error('must not prepare');}}),'ignored');assert.equal(preparations,0);assert.equal(value.runs.length,0);assert.equal(value.state.readPending(value.message.threadId), false);
});
test("pending native media keeps dispatch custody without model or read receipt",async t=>{
  const value=fixture(t);value.message.type=3;value.message.body='<private transport XML>';
  assert.equal(await dispatchNativeMessage({...value,prepareAttachment:async()=>({body:'image pending',pending:true})}),'pending');assert.equal(value.runs.length,0);assert.equal(value.state.get('m1')?.state,'received');assert.equal(value.state.readPending(value.message.threadId), false);
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
      mediaContentStream: async () => ({ contentType: "application/pdf", stream: (async function* () { yield new Uint8Array(source); })(), cancel: async () => {} }),
    });
    const saveMediaStream: any = async (stream: AsyncIterable<Uint8Array>) => { let size = 0; for await (const chunk of stream) size += chunk.byteLength; return { id: "pdf", path: "/managed/synthetic.pdf", size }; };
    const params = { ...value, prepareAttachment: (input: Parameters<typeof prepareNativeAttachment>[0]) => prepareNativeAttachment({ ...input, saveMediaStream,
      ...(failureStage === "later" && transfers === 0 ? { waitMs: 0 } : {}) }) };
    assert.equal(await dispatchNativeMessage(params), "pending");
    if (failureStage === "later") assert.equal(await dispatchNativeMessage(params), "pending");
    assert.equal(polls, 1); assert.equal(transfers, 1); assert.equal(value.runs.length, 0);
    assert.equal(value.state.get(value.message.id)?.state, "received"); assert.equal(value.state.readPending(value.message.threadId), false);
    clock = 10 * 60000;
    assert.equal(await dispatchNativeMessage(params), "pending");
    assert.equal(polls, 2); assert.equal(transfers, 1); assert.equal(value.state.get(value.message.id)?.state, "received");
    ready = true;
    await acceptNativeEvent(value.client, "a", value.state, { cursor: "2", kind: "media.ready", data: { accountId: "a", assetId: "file-asset", jobId: "download-job" }, createdAt: "2026-10-22T12:10:00Z" });
    assert.equal(await dispatchNativeMessage(params), "completed");
    assert.equal(transfers, 1); assert.equal(value.runs.length, 1); assert.equal(value.state.get(value.message.id), undefined);
    assert.deepEqual(value.runs[0].ctx.Attachments.map((attachment: any) => attachment.path), ["/managed/synthetic.pdf"]);
  }
});
test("verified native media path is included in the correlated prepared model input",async t=>{
  const value=fixture(t,{readPolicy:"on_model_dispatch"});value.message.type=3;value.message.body='<private transport XML>';
  assert.equal(await dispatchNativeMessage({...value,prepareAttachment:async()=>({body:'<media:image>',pending:false,mediaPath:'/managed/fixture.png',mediaMime:'image/png',attachment:{kind:'image',status:'ready',filename:'fixture.png',path:'/managed/fixture.png',mime:'image/png'},suppliedAssets:[{id:"asset",sha256:"0".repeat(64),byteLength:10,mime:"image/png",path:"/managed/fixture.png",role:"model_image",kind:"image",runtimeMedia:true,contextRole:"trigger"}]} )}),'completed');
  assert.equal(value.runs[0].ctx.MediaPath,undefined);assert.equal(value.runs[0].ctx.BodyForAgent,'<media:image>');
  assert.deepEqual(value.runs[0].ctx.media.map((fact:any)=>[fact.path,fact.url,fact.contentType,fact.kind,fact.fileName,fact.messageId,fact.hydrationSuppressed]),[['/managed/fixture.png','/managed/fixture.png','image/png','image','fixture.png','m1',undefined]]);assert.equal(value.state.readPending(value.message.threadId), true);assert.equal(value.state.get('m1'),undefined);
});
test("a received document reaches OpenClaw as a media fact and is named, not pathed, in the text", async t => {
  const value = fixture(t); value.message.type = 49; value.message.appMessageType = 6; value.message.body = "招标文件.doc";
  const source = Buffer.from("synthetic word document"), sha256 = createHash("sha256").update(source).digest("hex");
  Object.assign(value.client, {
    mediaAssets: async () => ({ items: [{ id: "doc-asset", accountId: "a", messageId: "m1", kind: "file", variant: "original", actualVariant: "original",
      state: "available", byteLength: source.length, sha256, filename: "招标文件.doc", jobId: null }] }),
    mediaContentStream: async () => ({ contentType: "application/msword", stream: (async function* () { yield new Uint8Array(source); })(), cancel: async () => {} }),
  });
  const saveMediaStream: any = async (stream: AsyncIterable<Uint8Array>) => { let size = 0; for await (const chunk of stream) size += chunk.byteLength; return { id: "doc", path: "/managed/招标文件---1.doc", size }; };
  assert.equal(await dispatchNativeMessage({ ...value, prepareAttachment: input => prepareNativeAttachment({ ...input, saveMediaStream }) }), "completed");
  const ctx = value.runs[0].ctx;
  assert.equal(ctx.BodyForAgent, "[File: 招标文件.doc]"); assert.equal(ctx.MediaPath, undefined);
  assert.deepEqual(ctx.media.map((fact: any) => [fact.path, fact.url, fact.contentType, fact.kind, fact.fileName, fact.messageId, fact.hydrationSuppressed]),
    [["/managed/招标文件---1.doc", "/managed/招标文件---1.doc", "application/msword", "document", "招标文件.doc", "m1", undefined]]);
});
test("a voice note is marked transcribed only when its transcript is in the text", async t => {
  for (const transcript of ["synthetic transcript", ""]) {
    const value = fixture(t); value.message.type = 34; value.message.body = null;
    value.core.mediaUnderstanding = { transcribeAudioFile: async () => ({ text: transcript }) };
    const prepareAttachment: any = async () => ({ body: "<media:audio>", pending: false,
      attachment: { kind: "audio", status: "ready", filename: "voice.mp3", path: "/managed/voice.mp3", mime: "audio/mpeg" },
      suppliedAssets: [{ id: "voice", sha256: "0".repeat(64), byteLength: 10, mime: "audio/mpeg", path: "/managed/voice.mp3",
        role: "file_reference", kind: "audio", runtimeMedia: false, contextRole: "trigger" }] });
    assert.equal(await dispatchNativeMessage({ ...value, prepareAttachment }), "completed");
    const ctx = value.runs[0].ctx;
    assert.equal(ctx.BodyForAgent, `<media:audio>\n${transcript ? `[Voice transcript: ${transcript}]` : "[Voice transcription unavailable]"}`);
    // Without a transcript OpenClaw keeps the voice note listed and may transcribe it itself.
    assert.deepEqual(ctx.media.map((fact: any) => [fact.kind, fact.transcribed, fact.path]), [["audio", !!transcript, "/managed/voice.mp3"]]);
  }
});
test("512 MiB file metadata reaches correlated dispatch and one excess byte stops before model handoff", async t => {
  for (const excess of [0, 1]) {
    const value = fixture(t, { readPolicy: "on_model_dispatch" }); value.message.type = 49; value.message.appMessageType = 6; value.message.body = null;
    const byteLength = 512 * 1024 * 1024 + excess;
    // The turn record is removed when the turn finishes; look at it during the run.
    let custody: any; const run = value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher;
    value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => { await run(params); custody = structuredClone(value.state.get(value.message.id)); };
    const result = await dispatchNativeMessage({ ...value, prepareAttachment: async () => ({ body: "[File: large.pdf]", pending: false,
      attachment: { kind: "file", status: "ready", filename: "large.pdf", path: "/managed/large.pdf", mime: "application/pdf" },
      suppliedAssets: [{ id: "large-file-asset", sha256: "0".repeat(64), byteLength, mime: "application/pdf", path: "/managed/large.pdf",
        role: "file_reference", kind: "file", runtimeMedia: false, contextRole: "trigger" }] }) });
    assert.equal(result, excess ? "needs_review" : "completed"); assert.equal(value.runs.length, excess ? 0 : 1);
    assert.equal(value.state.readPending(value.message.threadId), !excess);
    if (!excess) {
      const asset = custody.dispatchManifest.assets[0];
      assert.equal(asset.byteLength, byteLength); assert.equal(asset.role, "file_reference"); assert.equal(asset.runtimeMedia, false);
      assert.equal(custody.runId, "run_1");
    }
  }
});
test("media preparation failure cannot record a prepared input or mark read",async t=>{
  const value=fixture(t);value.message.type=3;
  assert.equal(await dispatchNativeMessage({...value,prepareAttachment:async()=>{throw Error('integrity failure');}}),'needs_review');assert.equal(value.runs.length,0);assert.equal(value.state.readPending(value.message.threadId), false);
});


test("late media receipt retries its whole-thread read during quiet SSE without another model turn", async t => {
  const value = fixture(t); value.state.update("m1", { state: "dispatching" });
  let requests = 0, failures = 0;
  value.client.threads = async () => ({ items: [{ id: "u", readStateVersion: "1" } as any], nextCursor: null, sourceGeneration: "g", coverage: {} });
  value.client.markRead = async (_account, threadId, expectedVersion) => { assert.equal(threadId, "u"); assert.equal(expectedVersion, "1"); if (++requests === 1) throw Error("lost response"); return {}; };
  const stop = startNativeReadPump(() => projectNativeRead(value.client, "a", value.state), () => { failures++; }, 5); t.after(stop);
  await new Promise(resolve => setTimeout(resolve, 15)); assert.equal(requests, 0);
  value.state.preparedCallback(["m1"], "on_model_dispatch")("late-media-run");
  for (let attempt = 0; attempt < 50 && value.state.readPending("u"); attempt++) await new Promise(resolve => setTimeout(resolve, 5));
  assert.equal(requests, 2); assert.equal(failures, 1); assert.equal(value.state.readPending(value.message.threadId), false); assert.equal(value.runs.length, 0);
  stop(); const before = requests; await new Promise(resolve => setTimeout(resolve, 15)); assert.equal(requests, before);
});

test("unmentioned group attachments become context for the next mention, read from the profile after a restart", async t => {
  const value = fixture(t, { group: true });
  value.core.mediaUnderstanding = { transcribeAudioFile: async () => ({ text: "Historical voice transcript" }) };
  const prepared: string[] = [];
  const prepareAttachment: any = async ({ message }: any) => {
    prepared.push(message.id);
    if (message.type === 1) return { body: message.body, pending: false };
    const kind = message.type === 3 ? "image" : message.type === 34 ? "audio" : message.type === 43 ? "video" : "file";
    const path = `/managed/${message.id}.original`, preview = kind === "image" ? `/managed/${message.id}.preview.jpg` : path;
    const mime = kind === "file" ? "application/pdf" : kind === "video" ? "video/mp4" : "audio/mpeg";
    return { body: `<media:${kind}>`, pending: false, attachment: { kind, status: "ready", filename: message.id, path, mime: kind === "image" ? "image/jpeg" : "application/octet-stream" },
      mediaPath: ["image", "video"].includes(kind) ? preview : undefined, mediaMime: "image/jpeg",
      suppliedAssets: [{ id: message.id, sha256: "0".repeat(64), byteLength: 10, mime: kind === "image" ? "image/jpeg" : mime, path: preview,
        sourcePath: path, sourceSha256: "1".repeat(64), sourceByteLength: 20, sourceMime: kind === "image" ? "image/png" : mime, role: kind === "image" ? "model_image" : "file_reference", kind, runtimeMedia: ["image", "video"].includes(kind), contextRole: "trigger" }] };
  };
  for (const [index, type] of [3, 3, 49, 43, 34].entries()) {
    const message = index === 0 ? value.message : { ...value.message, id: `history-${index}`, serverId: String(index + 1) };
    message.type = type; message.body = "<private transport XML>";
    if (index) value.add(String(index + 1), message);
    assert.equal(await dispatchNativeMessage({ ...value, message, prepareAttachment }), "ignored");
  }
  assert.deepEqual(prepared, []); assert.equal(value.runs.length, 0); assert.equal(value.state.items().length, 0);
  const state = new NativeConsumerState(dirname(value.state.file), "s", "a");
  const trigger = { ...value.message, id: "trigger", serverId: "9", type: 1, body: "What did we share?", mentions: { ...value.message.mentions, state: "known" as const, self: true } };
  value.store.set(trigger.id, trigger); state.accept("6", trigger, "live");
  let custody: any; const run = value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher;
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => { await run(params); custody = structuredClone(state.get("trigger")); };
  assert.equal(await dispatchNativeMessage({ ...value, state, message: trigger, prepareAttachment }), "completed");
  const ctx = value.runs[0].ctx; assert.equal(ctx.WasMentioned, true); assert.equal(ctx.Attachments.length, 5); assert.equal(ctx.InboundHistory.length, 5);
  // Every attachment is a media fact (staged for sandboxed agents); none is a path in the text.
  assert.ok(!ctx.BodyForAgent.includes("/managed/")); assert.equal(ctx.MediaPath, undefined);
  assert.match(ctx.BodyForAgent, /Historical voice transcript/); assert.ok(!ctx.BodyForAgent.includes("private transport XML"));
  assert.deepEqual(ctx.media.map((fact: any) => [fact.messageId, fact.path, fact.kind, fact.hydrationSuppressed ?? false, fact.transcribed]), [
    ["m1", "/managed/m1.original", "image", true, false],
    // The selected image is the one native image input; its original follows as a file.
    ["history-1", "/managed/history-1.preview.jpg", "image", false, false],
    ["history-1", "/managed/history-1.original", "image", true, false],
    ["history-2", "/managed/history-2.original", "document", false, false],
    ["history-3", "/managed/history-3.original", "video", false, false],
    ["history-4", "/managed/history-4.original", "audio", false, true],
  ]);
  const manifest = custody.dispatchManifest as NonNullable<ReturnType<NativeConsumerState["get"]>>["dispatchManifest"] & {};
  assert.deepEqual(manifest.newInputs.map(input => input.id), ["trigger"]); assert.equal(manifest.historyInputs?.length, 5);
  assert.equal(manifest.assets.filter(asset => asset.runtimeMedia).length, 1); assert.equal(manifest.assets.filter(asset => asset.role === "model_image").length, 1);
  assert.equal(manifest.assets.find(asset => asset.runtimeMedia)?.contextRole, "history");
  assert.equal(value.store.get("m1")?.mentions?.self, null); assert.equal(state.readPending(trigger.threadId), true);
  // The next mention starts after this one.
  assert.equal(state.groupBoundary(trigger.threadId)?.messageId, "trigger");
});
test("historical sender revocation excludes media before retrieval and commands neither get nor use up context", async t => {
  const value = fixture(t, { group: true }); value.message.type = 3;
  assert.equal(await dispatchNativeMessage(value), "ignored"); assert.equal(value.state.get("m1"), undefined);
  const command = value.add("2", { ...value.message, id: "command", serverId: "2", type: 1, body: "/status", mentions: { ...value.message.mentions, state: "known" as const, self: true } });
  value.core.channel.commands.shouldHandleTextCommands = () => true; value.core.channel.commands.isControlCommandMessage = (body: string) => body.trim() === "/status";
  assert.equal(await dispatchNativeMessage({ ...value, message: command, prepareAttachment: async ({ message }) => ({ body: message.body ?? "", pending: false }) }), "completed");
  assert.equal(value.runs[0].ctx.InboundHistory, undefined); assert.equal(value.state.groupBoundary(command.threadId), undefined);
  value.core.channel.inbound.ingress.resolveStable = async (params: any) => ({ ingress: { admission: params.subject.stableId === "revoked" ? "drop" : "dispatch" }, commandAccess: {}, activationAccess: {} });
  value.message.senderId = "revoked";
  const trigger = value.add("3", { ...command, id: "trigger", serverId: "3", senderId: "allowed", body: "mention" });
  const prepared: string[] = [];
  assert.equal(await dispatchNativeMessage({ ...value, message: trigger, prepareAttachment: async ({ message }) => { prepared.push(message.id); return { body: "current", pending: false }; } }), "completed");
  assert.deepEqual(prepared, ["trigger"]); assert.equal(value.runs[1].ctx.InboundHistory, undefined);
});
test("a pending historical attachment stays explicit without blocking the current mention", async t => {
  const value = fixture(t, { group: true }); value.message.type = 3; await dispatchNativeMessage(value);
  const trigger = value.add("2", { ...value.message, id: "trigger", serverId: "2", type: 1, body: "mention", mentions: { ...value.message.mentions, state: "known" as const, self: true } });
  assert.equal(await dispatchNativeMessage({ ...value, message: trigger, prepareAttachment: async ({ message }) => ({ body: message.type === 3 ? "[image pending]" : "current", pending: message.type === 3 }) }), "completed");
  assert.match(value.runs[0].ctx.BodyForAgent, /image pending/); assert.deepEqual(value.runs[0].ctx.Attachments, []); assert.equal(value.runs[0].ctx.media, undefined);
});
test("group context stops at the previous turn and leaves out outgoing, queued, command and recalled messages", async t => {
  const value = fixture(t, { group: true, mention: true });
  value.core.channel.commands.shouldHandleTextCommands = () => true; value.core.channel.commands.isControlCommandMessage = (body: string) => body.trim() === "/status";
  assert.equal(await dispatchNativeMessage(value), "completed");
  assert.equal(value.state.groupBoundary("r@chatroom")?.messageId, "m1");
  const text = (id: string, serverId: string, extra: Partial<NativeMessage> = {}) => ({ ...value.message, id, serverId, body: `body ${id}`, mentions: { state: "known" as const, userIds: [], self: false, all: false, source: null }, ...extra }) as NativeMessage;
  value.store.set("kept", text("kept", "10"));
  value.store.set("own", text("own", "11", { direction: "outgoing", senderId: "self" }));
  value.store.set("command", text("command", "12", { body: "/status" }));
  value.store.set("recalled", text("recalled", "13"));
  value.store.set("quoting", text("quoting", "14", { type: 49, appMessageType: 57, quote: { serverId: "13", type: 1, state: "embedded", embeddedText: "recalled words" } as any }));
  value.add("2", text("waiting", "15"));
  await acceptNativeEvent(value.client, "a", value.state, { cursor: "3", kind: "message.recalled", createdAt: new Date().toISOString(),
    data: { threadId: "r@chatroom", serverId: "13", observedAt: new Date().toISOString(), origin: "live" } });
  const trigger = value.add("4", text("trigger", "16", { mentions: { state: "known", userIds: [], self: true, all: false, source: null } }));
  const history = await readNativeGroupHistory({ client: value.client, state: value.state, message: trigger, limit: 50, isCommand: message => message.body === "/status" });
  assert.deepEqual(history.map(message => message.id), ["kept", "quoting"]);
  assert.equal(history[1].quote, undefined); assert.equal(history[1].body, "body quoting");
  assert.deepEqual((await readNativeGroupHistory({ client: value.client, state: value.state, message: trigger, limit: 1, isCommand: () => false })).map(message => message.id), ["quoting"]);
  // A mention older than the boundary already had its context offered to a later turn.
  value.state.beginDispatch("trigger", { messageIds: ["trigger"], assets: [] } as any, { boundary: { createdAt: trigger.createdAt } });
  assert.deepEqual(await readNativeGroupHistory({ client: value.client, state: value.state, message: value.store.get("waiting")!, limit: 50, isCommand: () => false }), []);
});
test("group context without a boundary is bounded by the history limit and by the boundary time", async t => {
  const value = fixture(t, { group: true }); value.state.finish("m1");
  const at = (id: string, second: number) => ({ ...value.message, id, serverId: String(100 + second), createdAt: `2026-10-20T12:00:${String(second).padStart(2, "0")}Z` }) as NativeMessage;
  for (const second of [1, 2, 3]) value.store.set(`h${second}`, at(`h${second}`, second));
  const trigger = value.add("2", at("trigger", 9));
  assert.deepEqual((await readNativeGroupHistory({ client: value.client, state: value.state, message: trigger, limit: 50, isCommand: () => false })).map(m => m.id), ["m1", "h1", "h2", "h3"]);
  assert.deepEqual((await readNativeGroupHistory({ client: value.client, state: value.state, message: trigger, limit: 2, isCommand: () => false })).map(m => m.id), ["h2", "h3"]);
  // A boundary whose message is gone from the profile still stops at its time.
  value.state.accept("3", at("gone", 2), "live");
  value.state.beginDispatch("gone", { messageIds: ["gone"], assets: [] } as any, { boundary: { createdAt: "2026-10-20T12:00:02Z" } });
  const later = value.add("4", at("later", 10));
  assert.deepEqual((await readNativeGroupHistory({ client: value.client, state: value.state, message: later, limit: 50, isCommand: () => false })).map(m => m.id), ["h2", "h3"]);
});

function deferred<T>() { let resolve!: (value: T) => void, reject!: (reason?: unknown) => void; const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; }); return { promise, resolve, reject }; }
const recallEvent = (message: NativeMessage, cursor = "2") => ({ cursor, kind: "message.recalled", createdAt: "2026-10-21T12:00:00Z",
  data: { threadId: message.threadId, serverId: message.serverId!, observedAt: "2026-10-21T12:00:00Z", origin: "live" } });
test("incoming recalls bypass self and group mention gates and invalidate custody before ACK", async t => {
  for (const own of [true, false]) {
    const value = fixture(t, { group: true }); value.message.direction = own ? "outgoing" : "incoming";
    await acceptNativeEvent(value.client, "a", value.state, recallEvent(value.message));
    assert.equal(value.state.cursor, "2"); assert.equal(value.state.get(value.message.id), undefined);
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
  pending.resolve({ body: "<media:image>", mediaPath: "/private/late.png", pending: false });
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
  assert.equal(await work, "ignored"); assert.equal(value.runs.length, 0); assert.equal(value.state.get(value.message.id), undefined);
});
test("a historical recall during media work lets the triggering mention retry with sanitized context", async t => {
  const value = fixture(t, { group: true }); value.message.type = 3;
  assert.equal(await dispatchNativeMessage(value), "ignored");
  const trigger = value.add("2", { ...value.message, id: "mention", serverId: "9007199254741000", type: 1, body: "current mention",
    mentions: { ...value.message.mentions, state: "known", self: true } });
  const started = deferred<void>(), pending = deferred<any>();
  const prepareAttachment: any = async ({ message }: any) => message.id === trigger.id ? { body: message.body, pending: false }
    : (started.resolve(), pending.promise);
  const work = dispatchNativeMessage({ ...value, message: trigger, prepareAttachment });
  await started.promise; await acceptNativeEvent(value.client, "a", value.state, recallEvent(value.message, "3"));
  pending.resolve({ body: "<media:image>", pending: false });
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
  assert.equal(value.state.readPending(value.message.threadId), false);
  assert.equal(value.state.get(value.message.id)?.dispatchManifest, undefined);
});
test("cancellation during a late reply's point-read cannot submit after the recheck returns", async t => {
  const value = fixture(t); const abort = new AbortController(); let sends = 0;
  value.client.sendText = async () => { sends++; return { id: "must-not-send", state: "accepted" } as any; };
  value.client.message = async (_account, id) => { if (value.runs.length) abort.abort(); return value.store.get(id)!; };
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
    value.runs.push(params); await params.dispatcherOptions.deliver({ text: "late reply" });
  };
  assert.equal(await dispatchNativeMessage({ ...value, signal: abort.signal }), "needs_review"); assert.equal(sends, 0);
  assert.equal(value.state.readPending(value.message.threadId), false);
});
test("cancellation during voice transcription cannot start a late model turn", async t => {
  const value = fixture(t); const abort = new AbortController(); value.message.type = 34;
  value.core.mediaUnderstanding = { transcribeAudioFile: async () => { abort.abort(); return { text: "Late transcript" }; } };
  const prepareAttachment: any = async () => ({ pending: false, body: "Voice", attachment: { kind: "audio", status: "ready", path: "/synthetic/voice.wav", mime: "audio/wav" } });
  assert.equal(await dispatchNativeMessage({ ...value, signal: abort.signal, prepareAttachment }), "needs_review");
  assert.equal(value.runs.length, 0); assert.equal(value.state.readPending(value.message.threadId), false);
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
  const b: NativeMessage = { ...value.message, serverId: "9007199254741000", type: 1, body: "New exact message", createdAt: new Date().toISOString() };
  value.client.message = async () => b;
  const committed = { cursor: "3", kind: "message.committed", data: { messageId: b.id, threadId: b.threadId, origin: "live" }, createdAt: new Date().toISOString() };
  await acceptNativeEvent(value.client, "a", value.state, committed);
  lateMedia.resolve({ body: "<media:image>", pending: false }); assert.equal(await workA, "ignored");
  assert.equal(value.state.get(b.id)?.state, "received"); assert.equal(value.state.get(b.id)?.serverId, b.serverId);
  assert.equal(await dispatchNativeMessage({ ...value, message: b }), "completed");
  // A repeated event is behind the cursor, so the finished message is not queued again.
  await acceptNativeEvent(value.client, "a", value.state, committed); assert.equal(await dispatchNativeMessage({ ...value, message: b }), "needs_review");
  assert.equal(value.runs.length, 1); assert.equal(value.runs[0].ctx.BodyForAgent, b.body);
  assert.ok(!JSON.stringify(value.runs).includes("old-source"));
});
test("a delayed old media failure cannot overwrite a replacement already dispatched once", async t => {
  const value = fixture(t); value.message.type = 3;
  const started = deferred<void>(), lateMedia = deferred<any>();
  const workA = dispatchNativeMessage({ ...value, prepareAttachment: async () => { started.resolve(); return lateMedia.promise; } }); await started.promise;
  const b: NativeMessage = { ...value.message, serverId: "9007199254741000", type: 1, body: "Replacement after media failure" };
  value.state.accept("2", b, "live"); value.client.message = async () => b;
  assert.equal(await dispatchNativeMessage({ ...value, message: b }), "completed");
  value.add("3", { ...b, serverId: "9007199254741001", body: "Third occupant" }); const revisionC = value.state.get(b.id)?.revision;
  lateMedia.reject(Error("Old download failed")); assert.equal(await workA, "needs_review");
  assert.equal(value.state.get(b.id)?.state, "received"); assert.equal(value.state.get(b.id)?.revision, revisionC);
  assert.equal(value.state.readPending(b.threadId), true); assert.equal(value.runs.length, 1);
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
    assert.equal(await dispatchNativeMessage({ ...value, message: b }), "completed");
    assert.throws(() => oldTurn.replyOptions.onPreparedAgentRunStart("old-run"), /no longer in custody/);
    await assert.rejects(oldTurn.dispatcherOptions.deliver({ text: "Old reply must not send" }));
    finishA.resolve(); assert.equal(await workA, fails ? "needs_review" : "ignored");
    assert.equal(value.state.get(b.id), undefined); assert.equal(value.state.readPending(b.threadId), true);
    assert.equal(await dispatchNativeMessage({ ...value, message: b }), "needs_review"); assert.equal(value.runs.length, 2);
  }
});
test("a read started before a newer request in the same thread does not clear it", async t => {
  const value = fixture(t); assert.equal(await dispatchNativeMessage(value), "completed"); assert.equal(value.state.readPending("u"), true);
  const started = deferred<void>(), threads = deferred<any>(); let reads = 0;
  value.client.threads = async () => { started.resolve(); return threads.promise; };
  value.client.markRead = async () => { reads++; return { phoneNotification: { state: "accepted" } }; };
  const reading = projectNativeRead(value.client, "a", value.state); await started.promise;
  const b = value.add("2", { ...value.message, serverId: "9007199254741000", body: "New read owner" });
  assert.equal(await dispatchNativeMessage({ ...value, message: b }), "completed");
  threads.resolve({ items: [{ id: b.threadId, readStateVersion: "1" }], nextCursor: null, coverage: {}, sourceGeneration: "fixture" }); await reading;
  assert.equal(value.state.readPending("u"), true);
  await projectNativeRead(value.client, "a", value.state); assert.equal(value.state.readPending("u"), false); assert.equal(reads, 2);
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
  // The same event again is behind the cursor and queues nothing.
  assert.equal(value.state.accept("3", { ...b }, "live"), false); assert.equal(await dispatchNativeMessage({ ...value, message: b }), "needs_review");
  assert.equal(keys.length, 2);
});

const commitEvent = (cursor: string, messageId: string, createdAt: number, data: Record<string, unknown> = {}) => ({ cursor, kind: "message.committed",
  createdAt: new Date(createdAt).toISOString(), data: { messageId, threadId: "u", origin: "live", direction: "incoming", ...data } });
/** A daemon event log that keeps only events from `first` on. */
function journal(events: Array<ReturnType<typeof commitEvent>>, first = 1n) {
  return {
    events: async (_account: string, after: string, limit: number) => {
      if (BigInt(after) < first - 1n) throw new NativeApiError(410, "event_cursor_expired");
      const items = events.filter(event => BigInt(event.cursor) > BigInt(after)).slice(0, limit);
      return { items, nextCursor: items.at(-1)?.cursor ?? after };
    },
    threads: async () => ({ items: [], nextCursor: null, coverage: {}, sourceGeneration: "g", asOfEventCursor: events.at(-1)?.cursor ?? "0" }),
    ack: async () => {},
  };
}
test("intake queues only live incoming messages that arrived in the last 24 hours and reads no others", async t => {
  const value = fixture(t); const now = Date.parse("2026-11-18T12:00:00Z"), reads: string[] = [];
  const sent = (hours: number) => ({ createdAt: new Date(now - hours * 3600 * 1000).toISOString() });
  for (const [id, extra] of [["own", { direction: "outgoing" }], ["imported", {}], ["old", {}], ["recent", sent(23)], ["legacy", { direction: "outgoing" }], ["delayed", sent(30)]] as const)
    value.store.set(id, { ...value.message, id, serverId: String(value.store.size + 10), ...extra } as NativeMessage);
  value.client.message = async (_account, id) => { reads.push(id); return value.store.get(id)!; };
  for (const event of [commitEvent("2", "own", now, { direction: "outgoing" }), commitEvent("3", "imported", now, { origin: "catchup" }),
    commitEvent("4", "old", now - 25 * 3600 * 1000), commitEvent("5", "recent", now - 23 * 3600 * 1000),
    { ...commitEvent("6", "legacy", now), data: { messageId: "legacy", threadId: "u", origin: "live" } },
    { cursor: "7", kind: "thread.updated", createdAt: new Date(now).toISOString(), data: { threadId: "u" } },
    // Reached the daemon just now, but was sent more than 24 hours ago.
    commitEvent("8", "delayed", now)])
    await acceptNativeEvent(value.client, "a", value.state, event as any, { now });
  assert.deepEqual(reads, ["recent", "legacy", "delayed"]); assert.deepEqual(value.state.items().map(item => item.id), ["m1", "recent"]);
  assert.equal(value.state.cursor, "8"); assert.equal(value.state.skipped.old, 2);
});
test("catching up answers at most the newest 100 missed messages and does not queue chatter it cannot answer", async t => {
  const value = fixture(t); value.state.finish("m1"); const now = Date.now(); const events: Array<ReturnType<typeof commitEvent>> = [];
  const createdAt = new Date(now - 60000).toISOString();
  for (let i = 0; i < 130; i++) { value.store.set(`dm${i}`, { ...value.message, id: `dm${i}`, serverId: String(1000 + i), createdAt }); events.push(commitEvent(String(2 + i), `dm${i}`, now - 60000)); }
  for (let i = 0; i < 20; i++) {
    value.store.set(`chat${i}`, { ...value.message, id: `chat${i}`, threadId: "r@chatroom", serverId: String(5000 + i), createdAt });
    events.push(commitEvent(String(200 + i), `chat${i}`, now - 60000, { threadId: "r@chatroom" }));
  }
  const acknowledged: string[] = [];
  Object.assign(value.client, journal(events), { ack: async (_account: string, _consumer: string, cursor: string) => { acknowledged.push(cursor); } });
  await synchronizeNativeEvents(value.client, "a", value.state, { now, answerable: message => !message.threadId.endsWith("@chatroom") });
  const queued = value.state.items().map(item => item.id);
  assert.equal(queued.length, NATIVE_CATCH_UP_LIMIT); assert.equal(queued[0], "dm30"); assert.equal(queued.at(-1), "dm129");
  assert.equal(value.state.skipped.limit, 30); assert.equal(value.state.cursor, "219"); assert.deepEqual(acknowledged, ["219"]);
  assert.equal(new NativeConsumerState(dirname(value.state.file), "s", "a").items().length, NATIVE_CATCH_UP_LIMIT);
});
function journalFixture(t: any, oldCount: number, recentCount: number, first = 1n) {
  const directory = mkdtempSync(join(tmpdir(), "native-journal-")); t.after(() => rmSync(directory, { recursive: true, force: true }));
  const now = Date.parse("2026-11-18T12:00:00Z"), store = new Map<string, NativeMessage>(), events: Array<ReturnType<typeof commitEvent>> = [];
  for (let i = 0; i < oldCount + recentCount; i++) {
    const cursor = String(Number(first) + i), id = `e${cursor}`;
    store.set(id, { id, accountId: "a", threadId: "u", senderId: "u", serverId: cursor, type: 1, direction: "incoming", body: id, createdAt: null, version: "1", media: [], mentions: { state: "unknown", userIds: [], self: null, all: null, source: null } });
    events.push(commitEvent(cursor, id, i < oldCount ? now - 48 * 3600 * 1000 + i * 60000 : now - 3600 * 1000 + i * 60000));
  }
  const client = { ...journal(events, first), message: async (_account: string, id: string) => store.get(id)! } as unknown as NativeWeChatClient;
  return { state: new NativeConsumerState(directory, "s", "a"), client, now };
}
test("a new consumer starts at the first event of the last 24 hours instead of replaying the daemon's history", async t => {
  const { state, client, now } = journalFixture(t, 30, 10); const resumed: string[][] = [];
  await synchronizeNativeEvents(client, "a", state, { now, onResume: (from, to) => resumed.push([from, to]) });
  assert.deepEqual(resumed, [["0", "30"]]); assert.equal(state.cursor, "40");
  assert.deepEqual(state.items().map(item => item.id), Array.from({ length: 10 }, (_, i) => `e${31 + i}`));
});
test("an expired cursor resumes from the last 24 hours, or from the oldest kept event when that is later", async t => {
  const trimmed = journalFixture(t, 30, 10, 21n); trimmed.state.advance("5"); trimmed.state.commit(); const resumed: string[][] = [];
  await synchronizeNativeEvents(trimmed.client, "a", trimmed.state, { now: trimmed.now, onResume: (from, to) => resumed.push([from, to]) });
  assert.deepEqual(resumed, [["5", "50"]]); assert.equal(trimmed.state.items().length, 10);
  const recent = journalFixture(t, 0, 40, 21n); recent.state.advance("5"); recent.state.commit();
  await synchronizeNativeEvents(recent.client, "a", recent.state, { now: recent.now });
  assert.equal(recent.state.items().length, 40); assert.equal(recent.state.items()[0].id, "e21");
});
test("resuming never moves before the stored cursor and leaves a cursor already inside the window alone", async t => {
  const value = journalFixture(t, 30, 10);
  value.state.advance("12"); value.state.commit();
  assert.equal(await resumeFromCatchUpWindow(value.client, "a", value.state, { now: value.now }), "30");
  assert.equal(await resumeFromCatchUpWindow(value.client, "a", value.state, { now: value.now }), undefined);
  value.state.advance("35"); assert.equal(await resumeFromCatchUpWindow(value.client, "a", value.state, { now: value.now }), undefined); assert.equal(value.state.cursor, "35");
});
test("catch-up counts only what dispatch would answer: sender policy, group settings, mentions and commands", async t => {
  const value = fixture(t, { group: true }); const params = { account: value.account, cfg: value.cfg, core: value.core };
  value.core.channel.commands.shouldHandleTextCommands = () => true; value.core.channel.commands.isControlCommandMessage = (body: string) => body.trim() === "/status";
  // A resolver with OpenClaw's inputs: sender allowlists, blocked routes, the mention gate and command facts.
  value.core.channel.inbound.ingress.resolveStable = async (input: any) => {
    const list = input.conversation.kind === "direct" ? input.allowFrom : input.groupAllowFrom;
    const allowed = list.includes("*") || list.includes(input.subject.stableId);
    const gated = input.policy.activation?.requireMention && !input.mentionFacts?.wasMentioned && !input.command?.hasControlCommand;
    return { ingress: { admission: !allowed || input.route?.some((route: any) => !route.allowed) ? "drop" : gated ? "skip" : "dispatch", reasonCode: "fixture" }, commandAccess: {}, activationAccess: {} };
  };
  const mentioned = { state: "known" as const, userIds: [], self: true, all: null, source: "msgsource.atuserlist" };
  assert.equal(await nativeMayAnswer(value.message, params), false);
  assert.equal(await nativeMayAnswer({ ...value.message, threadId: "u" }, params), true);
  assert.equal(await nativeMayAnswer({ ...value.message, mentions: mentioned }, params), true);
  assert.equal(await nativeMayAnswer({ ...value.message, body: "/status" }, params), true);
  assert.equal(await nativeMayAnswer({ ...value.message, threadId: "u", direction: "outgoing" }, params), false);
  assert.equal(await nativeMayAnswer({ ...value.message, threadId: "u", type: 49, appMessageType: 74 }, params), false);
  const cfg = structuredClone(value.cfg); const section = cfg.channels["agent-wechat"];
  section.groups = { "r@chatroom": { requireMention: false } };
  assert.equal(await nativeMayAnswer(value.message, { ...params, cfg, account: resolveWeChatAccount(cfg)! }), true);
  section.groups = { "r@chatroom": { enabled: false, requireMention: false } };
  assert.equal(await nativeMayAnswer(value.message, { ...params, cfg, account: resolveWeChatAccount(cfg)! }), false);
  // With an allowlist, other senders' direct messages do not take the catch-up slots.
  Object.assign(section, { groups: {}, dmPolicy: "allowlist", allowFrom: ["wxid_boss"] });
  const allowlisted = { ...params, cfg, account: resolveWeChatAccount(cfg)! };
  assert.equal(await nativeMayAnswer({ ...value.message, threadId: "wxid_boss", senderId: "wxid_boss" }, allowlisted), true);
  assert.equal(await nativeMayAnswer({ ...value.message, threadId: "wxid_other", senderId: "wxid_other" }, allowlisted), false);
});
test("the catch-up limit holds at every acknowledgement and counts work already queued at startup", async t => {
  const value = fixture(t); const now = Date.now(), createdAt = new Date(now - 60000).toISOString();
  for (let i = 0; i < 59; i++) value.add(String(2 + i), { ...value.message, id: `stored${i}`, serverId: String(100 + i), createdAt });
  const events: Array<ReturnType<typeof commitEvent>> = [];
  for (let i = 0; i < 250; i++) { value.store.set(`new${i}`, { ...value.message, id: `new${i}`, serverId: String(1000 + i), createdAt }); events.push(commitEvent(String(100 + i), `new${i}`, now - 60000)); }
  const persisted: number[] = [];
  Object.assign(value.client, journal(events), { ack: async () => {
    // A crash here must not leave more than the limit queued.
    persisted.push(new NativeConsumerState(dirname(value.state.file), "s", "a").items().length);
    if (persisted.length === 1) throw Error("simulated stop after the first page");
  } });
  await assert.rejects(synchronizeNativeEvents(value.client, "a", value.state, { now, includeQueued: true }), /simulated stop/);
  assert.deepEqual(persisted, [NATIVE_CATCH_UP_LIMIT]);
  const reopened = new NativeConsumerState(dirname(value.state.file), "s", "a");
  assert.deepEqual(reopened.items().map(item => item.id), Array.from({ length: 100 }, (_, i) => `new${100 + i}`));
  await synchronizeNativeEvents(value.client, "a", reopened, { now });
  assert.equal(reopened.items().length, 150); assert.equal(reopened.items().at(-1)?.id, "new249");
});
test("at startup, queued work counts toward the limit only if the current policy would answer it", async t => {
  const value = fixture(t); value.state.finish("m1"); const createdAt = new Date().toISOString();
  // As an earlier release left it: unmentioned group messages queued before the direct messages.
  for (let i = 0; i < 60; i++) value.add(String(2 + i), { ...value.message, id: `chatter${i}`, threadId: "r@chatroom", serverId: String(100 + i), createdAt });
  for (let i = 0; i < 120; i++) value.add(String(100 + i), { ...value.message, id: `dm${i}`, serverId: String(1000 + i), createdAt });
  value.add("300", { ...value.message, id: "gone", serverId: "9999", createdAt }); value.store.delete("gone");
  const read = value.client.message;
  value.client.message = async (account, id) => { if (id === "gone") throw new NativeApiError(404, "message_not_found"); return read(account, id); };
  Object.assign(value.client, journal([]));
  await synchronizeNativeEvents(value.client, "a", value.state, { includeQueued: true, answerable: message => !message.threadId.endsWith("@chatroom") });
  assert.deepEqual(value.state.items().map(item => item.id), Array.from({ length: 100 }, (_, i) => `dm${20 + i}`));
  assert.equal(value.state.skipped.limit, 20);
});
test("a late-delivered mention sent before the group boundary keeps that boundary as the stopping point", t => {
  const value = fixture(t, { group: true });
  value.state.beginDispatch("m1", { messageIds: ["m1"], assets: [] } as any, { boundary: { createdAt: "2026-10-20T12:00:00Z" } });
  const late = value.add("2", { ...value.message, id: "late", serverId: "2", createdAt: "2026-10-20T11:00:00Z" });
  value.state.beginDispatch(late.id, { messageIds: [late.id], assets: [] } as any, { boundary: { createdAt: late.createdAt } });
  assert.deepEqual(value.state.groupBoundary("r@chatroom"), { cursor: "2", messageId: "m1", createdAt: "2026-10-20T12:00:00Z" });
});
