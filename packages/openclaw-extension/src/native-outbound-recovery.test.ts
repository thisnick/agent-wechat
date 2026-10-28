import test from "node:test";
import assert from "node:assert/strict";
import { NativeApiError } from "@agent-wechat/shared";
import { createWeChatDurableFinal } from "./native-outbound-recovery.ts";
import { nativeOutboundKey } from "./native-send-key.ts";
import { nativeSourceMessageSid } from "./native-prepared-boundary.ts";

function fixture() {
  const cfg: any = { channels: { "agent-wechat": { enabled: true, backend: "native", native: { mode: "external", accountId: "account",
    endpoint: "http://127.0.0.1:6174", token: "synthetic-token-".repeat(3), consumerStateDir: "/unused/synthetic" } } } };
  const calls: unknown[][] = [];
  let intent: any = { id: "send_original", accountId: "account", destination: "g@chatroom", state: "accepted", localMessageId: "message_original", serverId: "9007199254740999" };
  let message: any = { id: "message_original", accountId: "account", threadId: "g@chatroom", direction: "outgoing", type: 1, body: "@Member\u2005please check", serverId: "9007199254740999" };
  const client = { sendByKey: async (...args: unknown[]) => { calls.push(["inspect", ...args]); if (!intent) throw new NativeApiError(404, "not_found"); return intent; },
    message: async (...args: unknown[]) => { calls.push(["message", ...args]); return message; },
    sendText: async () => assert.fail("Recovery must never submit"), sendMedia: async () => assert.fail("Recovery must never transfer") };
  const hook = createWeChatDurableFinal((async () => ({ client })) as any).reconcileUnknownSend!;
  const ctx: any = { cfg, queueId: "retained-queue", channel: "agent-wechat", to: "g@chatroom", enqueuedAt: 1, retryCount: 0,
    payloads: [{ text: "please check", channelData: { "agent-wechat": { mentions: { userIds: ["wxid_member"] } } } }] };
  return { ctx, calls, hook, setIntent: (value: any) => { intent = value; }, getIntent: () => intent, setMessage: (value: any) => { message = value; }, getMessage: () => message };
}

test("queued mention recovery checks the original key and returns only a verified full receipt", async () => {
  const value = fixture();
  const result = await value.hook(value.ctx);
  assert.equal(result?.status, "sent");
  if (result?.status !== "sent") assert.fail("Expected sent");
  assert.equal(result.messageId, "message_original"); assert.deepEqual(result.receipt.platformMessageIds, ["message_original"]);
  assert.deepEqual(value.calls, [["inspect", "account", nativeOutboundKey("retained-queue")], ["message", "account", "message_original"]]);
});

test("unavailable, pending and foreign receipts never authorize an automatic resend", async () => {
  for (const change of [undefined, { state: "queued" }, { state: "needs_review" }, { accountId: "other" }, { destination: "other@chatroom" },
    { localMessageId: undefined }, { serverId: undefined }]) {
    const value = fixture(); value.setIntent(change === undefined ? undefined : { ...value.getIntent(), ...change });
    const result = await value.hook(value.ctx);
    assert.equal(result?.status, "unresolved"); if (result?.status === "unresolved") assert.equal(result.retryable, false);
    assert.equal(value.calls.length, 1);
  }
  for (const change of [{ id: "different" }, { direction: "incoming" }, { serverId: "1" }]) {
    const value = fixture(); value.setMessage({ ...value.getMessage(), ...change });
    assert.equal((await value.hook(value.ctx))?.status, "unresolved");
  }
});

test("recovery rejects unsupported combinations before consulting the hub", async () => {
  for (const change of [{ mediaUrl: "/fixture/image.png" }, { audioAsVoice: true }, { replyToId: "quoted" }, { text: "" },
    { channelData: { "agent-wechat": { mentions: { userIds: ["duplicate", "duplicate"] } } } }]) {
    const value = fixture(); value.ctx.payloads[0] = { ...value.ctx.payloads[0], ...change };
    assert.equal((await value.hook(value.ctx))?.status, "unresolved"); assert.deepEqual(value.calls, []);
  }
});

test("a first text chunk cannot confirm an entire queued message", async () => {
  const value = fixture(); value.ctx.payloads[0] = { text: "the complete original message" };
  value.setMessage({ ...value.getMessage(), body: "the complete" });
  assert.equal((await value.hook(value.ctx))?.status, "unresolved");
  value.setMessage({ ...value.getMessage(), body: "the complete original message" });
  assert.equal((await value.hook(value.ctx))?.status, "sent");
});

test("queued quoted replies recover by their original key and exact source identity", async () => {
  for (const location of ["payload", "context", "effective", "qualified-current"]) {
    const value = fixture();
    value.ctx.payloads[0] = { text: "Agreed", ...(location === "payload" && { replyToId: "msg_source" }) };
    if (location === "context") value.ctx.replyToId = "msg_source";
    if (location === "effective") { value.ctx.replyToId = "older"; value.ctx.effectiveReplyToId = "msg_source"; }
    if (location === "qualified-current") value.ctx.replyToId = nativeSourceMessageSid({ id: "msg_source", accountId: "account", threadId: "g@chatroom", serverId: "18446744073709551615" } as any);
    value.setMessage({ ...value.getMessage(), type: 49, appMessageType: 57, body: "Agreed", quote: { serverId: "18446744073709551615" } });
    const hook = createWeChatDurableFinal((async () => ({ client: {
      sendByKey: async (...args: unknown[]) => { value.calls.push(["inspect", ...args]); return value.getIntent(); },
      message: async (_account: string, id: string) => id === "msg_source"
        ? { id, accountId: "account", threadId: "g@chatroom", serverId: "18446744073709551615", recall: { replacementText: "Recalled later" } }
        : value.getMessage(),
      sendText: async () => assert.fail("Recovery must never submit"),
    } })) as any).reconcileUnknownSend!;
    assert.equal((await hook(value.ctx))?.status, "sent");
    assert.deepEqual(value.calls, [["inspect", "account", nativeOutboundKey("retained-queue")]]);
    for (const change of [{ body: "Only part" }, { type: 1 }, { appMessageType: 5 }, { quote: undefined },
      { quote: { serverId: "1" } }, { accountId: "other" }, { threadId: "other@chatroom" }]) {
      const prior = value.getMessage(); value.setMessage({ ...prior, ...change });
      assert.equal((await hook(value.ctx))?.status, "unresolved"); value.setMessage(prior);
    }
  }
});

test("missing or foreign originals cannot confirm queued quoted replies", async () => {
  for (const source of [undefined, { id: "other" }, { accountId: "other" }, { threadId: "other@chatroom" }, { serverId: "0" }]) {
    const value = fixture(); value.ctx.payloads[0] = { text: "Agreed", replyToId: "msg_source" };
    value.setMessage({ ...value.getMessage(), type: 49, appMessageType: 57, body: "Agreed", quote: { serverId: "18446744073709551615" } });
    const hook = createWeChatDurableFinal((async () => ({ client: {
      sendByKey: async () => value.getIntent(),
      message: async (_account: string, id: string) => {
        if (id !== "msg_source") return value.getMessage();
        if (!source) throw Error("Original unavailable");
        return { id, accountId: "account", threadId: "g@chatroom", serverId: "18446744073709551615", ...source };
      }, sendText: async () => assert.fail("Recovery must never submit"),
    } })) as any).reconcileUnknownSend!;
    const result = await hook(value.ctx);
    assert.equal(result?.status, "unresolved"); if (result?.status === "unresolved") assert.equal(result.retryable, false);
  }
});
