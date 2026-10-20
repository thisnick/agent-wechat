import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { NativeApiError, type NativeMessage, type NativeWeChatClient } from "@agent-wechat/shared";
import { NativeConsumerState } from "./native-consumer-state.ts";
import { dispatchNativeMessage, nativeWasMentioned, projectNativeRead } from "./native-monitor.ts";
import { resolveWeChatAccount } from "./types.ts";
function fixture(t: any, options: { group?: boolean; mention?: boolean; origin?: string } = {}) {
  const directory = mkdtempSync(join(tmpdir(), "native-model-harness-")); t.after(() => rmSync(directory, { recursive: true, force: true }));
  const cfg: any = { channels: { "agent-wechat": { backend: "native", dmPolicy: "open", allowFrom: ["*"], groupPolicy: "open", groupAllowFrom: ["*"],
    native: { mode: "external", endpoint: "http://127.0.0.1:6174", accountId: "a", tokenFile: "/unused", consumerStateDir: directory } } } };
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
      dispatchReplyWithBufferedBlockDispatcher: async (params: any) => { runs.push(params); params.replyOptions.onPreparedAgentRunStart("run_1"); } },
  } };
  const client = { sendText: async () => ({ id: "send_1", state: "accepted" }) } as unknown as NativeWeChatClient;
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
  const value = fixture(t); assert.equal(await dispatchNativeMessage(value), "completed");
  assert.equal(value.runs.length, 1); assert.equal(value.state.get("m1")?.runId, "run_1"); assert.equal(value.state.get("m1")?.readPending, true);
  assert.equal(value.runs[0].ctx.MessageSid, "wechat-native:a:m1");
});
test("catchup/import messages are not new automatic model turns or read projections", async t => {
  for (const origin of ["catchup", "import", "unknown"]) { const value = fixture(t, { origin }); assert.equal(await dispatchNativeMessage(value), "ignored"); assert.equal(value.runs.length, 0); }
});
test("released SDK missing prepared hook leaves read unchanged and records uncertainty", async t => {
  const value = fixture(t); value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async () => {};
  assert.equal(await dispatchNativeMessage(value), "completed"); assert.equal(value.state.get("m1")?.readPending, undefined); assert.equal(value.state.get("m1")?.handoffUnconfirmed, true);
});
test("failure after prepared handoff retains read receipt and prevents model replay", async t => {
  const value = fixture(t); value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => { params.replyOptions.onPreparedAgentRunStart("run_1"); throw new Error("provider failed after handoff"); };
  assert.equal(await dispatchNativeMessage(value), "needs_review"); assert.equal(value.state.get("m1")?.readPending, true);
  assert.equal(await dispatchNativeMessage(value), "needs_review");
});
test("queued native reply waits for accepted without submitting a second send", async t => {
  const value = fixture(t); let submissions = 0; let inspections = 0;
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
  const value = fixture(t); let submissions = 0; let inspections = 0;
  value.client.sendText = async () => { submissions++; return { id: "send_1", state: "queued" } as any; };
  value.client.send = async () => { inspections++; return { id: "send_1", state: "needs_review" } as any; };
  value.core.channel.reply.dispatchReplyWithBufferedBlockDispatcher = async (params: any) => {
    params.replyOptions.onPreparedAgentRunStart("run_1"); await params.dispatcherOptions.deliver({ text: "reply fixture" });
  };
  assert.equal(await dispatchNativeMessage(value), "needs_review");
  assert.equal(await dispatchNativeMessage(value), "needs_review");
  assert.equal(submissions, 1); assert.equal(inspections, 1); assert.equal(value.state.get("m1")?.readPending, true);
});
test("read CAS retries same sparse IDs without marking concurrent arrival or gap", async t => {
  const value = fixture(t); value.state.accept("2", { ...value.message, id: "gap" }, "live"); await dispatchNativeMessage(value);
  const requests: any[] = []; let attempt = 0;
  value.client.threads = async () => ({ items: [{ id: "u", readStateVersion: String(attempt) } as any], nextCursor: null, sourceGeneration: "g", coverage: {} });
  value.client.markRead = async (_account, _thread, ids, expectedVersion) => { requests.push({ ids, expectedVersion }); if (attempt++ === 0) throw new NativeApiError(409, "conflict"); return {}; };
  await projectNativeRead(value.client, "a", value.state);
  assert.deepEqual(requests, [{ ids: ["m1"], expectedVersion: "0" }, { ids: ["m1"], expectedVersion: "1" }]);
  assert.equal(value.state.get("gap")?.readPending, undefined); assert.equal(value.state.get("m1")?.readPending, false);
});
