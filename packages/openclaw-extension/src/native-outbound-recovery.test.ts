import test from "node:test";
import assert from "node:assert/strict";
import { NativeApiError } from "@agent-wechat/shared";
import { createWeChatDurableFinal } from "./native-outbound-recovery.ts";
import { nativeOutboundKey } from "./native-send-key.ts";
import { nativeSourceMessageSid } from "./native-prepared-boundary.ts";

function fixture() {
  const cfg: any = { channels: { "agent-wechat": { enabled: true, backend: "native", native: { mode: "external", accountId: "account",
    endpoint: "http://127.0.0.1:6174", token: "synthetic-token-".repeat(3), consumerStateDir: "/unused/synthetic" } } } };
  const calls: unknown[][] = [], mutations: string[] = [];
  let intent: any = { id: "send_original", accountId: "account", destination: "g@chatroom", state: "accepted", localMessageId: "message_original", serverId: "9007199254740999" };
  let message: any = { id: "message_original", accountId: "account", threadId: "g@chatroom", direction: "outgoing", type: 1, body: "@Member\u2005please check", serverId: "9007199254740999",
    mentions: { state: "known", userIds: ["wxid_member"], all: false, self: false, source: "msgsource.atuserlist" } };
  const client = { sendByKey: async (...args: unknown[]) => { calls.push(["inspect", ...args]); if (!intent) throw new NativeApiError(404, "not_found"); return intent; },
    message: async (...args: unknown[]) => { calls.push(["message", ...args]); return args[1] === "msg_source"
      ? { id: "msg_source", accountId: "account", threadId: "g@chatroom", serverId: "18446744073709551615" } : message; },
    sendText: async () => { mutations.push("sendText"); assert.fail("Recovery must never submit"); },
    sendMedia: async () => { mutations.push("sendMedia"); assert.fail("Recovery must never transfer"); },
    group: async () => { mutations.push("group"); assert.fail("Recovery must not resolve current group names"); },
    contact: async () => { mutations.push("contact"); assert.fail("Recovery must not resolve current contact names"); } };
  const hook = createWeChatDurableFinal((async () => ({ client })) as any).reconcileUnknownSend!;
  const ctx: any = { cfg, queueId: "retained-queue", channel: "agent-wechat", to: "g@chatroom", enqueuedAt: 1, retryCount: 0,
    payloads: [{ text: "please check", channelData: { "agent-wechat": { mentions: { userIds: ["wxid_member"] } } } }] };
  return { ctx, calls, mutations, hook, setIntent: (value: any) => { intent = value; }, getIntent: () => intent, setMessage: (value: any) => { message = value; }, getMessage: () => message };
}

test("queued mention recovery checks the original key and returns only a verified full receipt", async () => {
  const value = fixture();
  const result = await value.hook(value.ctx);
  assert.equal(result?.status, "sent");
  if (result?.status !== "sent") assert.fail("Expected sent");
  assert.equal(result.messageId, "message_original"); assert.deepEqual(result.receipt.platformMessageIds, ["message_original"]);
  assert.deepEqual(value.calls, [["inspect", "account", nativeOutboundKey("retained-queue")],
    ["message", "account", "message_original"], ["message", "account", "message_original"]]);
});


test("mention recovery verifies the full original text and exact direct recipients without new name lookups", async () => {
  const text = "Check the entire comment @typed\u2005exactly";
  for (const quoted of [false, true]) for (const [mentions, prefix] of [
    [{ userIds: ["wxid_member"] }, "@Historical name\u2005"],
    [{ userIds: ["wxid_member", "wxid_second"] }, "@Historical name\u2005@Earlier 😀 label\u2005"],
    [{ all: true }, "@all\u2005"],
  ] as const) {
    const value = fixture();
    value.ctx.payloads[0] = { text, ...(quoted && { replyToId: "msg_source" }), channelData: { "agent-wechat": { mentions } } };
    const directMentions = { state: "known", userIds: "userIds" in mentions ? [...mentions.userIds] : [], all: "all" in mentions,
      self: false, source: "msgsource.atuserlist" };
    const stored = { ...value.getMessage(), type: quoted ? 49 : 1, appMessageType: quoted ? 57 : undefined,
      body: prefix + text, mentions: directMentions, ...(quoted && { quote: { serverId: "18446744073709551615" } }) };
    value.setMessage(stored);
    assert.equal((await value.hook(value.ctx))?.status, "sent", "saved labels need no current member lookup");
    for (const body of [prefix + "Check the", prefix + "Different comment", prefix + text + " extra", text,
      "@No separator" + text, "@\u2005" + text, "@ \u2005" + text, "@bad\nlabel\u2005" + text,
      "@\ud800\u2005" + text, "@" + "x".repeat(1025) + "\u2005" + text,
      prefix + "@Extra\u2005" + text, " " + prefix + text]) {
      value.setMessage({ ...stored, body });
      assert.equal((await value.hook(value.ctx))?.status, "unresolved", "partial, changed and malformed text cannot confirm delivery");
    }
    for (const actual of [undefined, { ...directMentions, state: "unknown" }, { ...directMentions, all: null },
      { ...directMentions, all: !directMentions.all }, { ...directMentions, userIds: ["wxid_wrong"] },
      { ...directMentions, userIds: [...directMentions.userIds, "wxid_extra"] },
      ...(!directMentions.all ? [{ ...directMentions, userIds: [] }] : []),
      ...(directMentions.userIds.length > 1 ? [{ ...directMentions, userIds: [...directMentions.userIds].reverse() }] : [])]) {
      value.setMessage({ ...stored, mentions: actual, ...(quoted && { quote: { ...stored.quote, mentions: directMentions } }) });
      assert.equal((await value.hook(value.ctx))?.status, "unresolved", "quoted metadata cannot replace canonical direct recipients");
    }
    for (const change of [{ type: 3 }, { quote: quoted ? undefined : { serverId: "1" } }, { recall: { replacementText: "Recalled" } }]) {
      value.setMessage({ ...stored, ...change });
      assert.equal((await value.hook(value.ctx))?.status, "unresolved");
    }
    // Authored text may itself begin with a mention-looking literal. Strip
    // only the generated prefix, leaving every authored character to compare.
    value.ctx.payloads[0].text = "@Typed literal\u2005" + text;
    value.setMessage({ ...stored, body: prefix + value.ctx.payloads[0].text });
    assert.equal((await value.hook(value.ctx))?.status, "sent");
    assert.deepEqual(value.mutations, []);
  }
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
  for (const change of [{ mediaUrl: "/fixture/image.png" }, { audioAsVoice: true }, { text: "" },
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
  for (const location of ["payload", "context", "effective", "qualified-current"]) for (const mentions of [undefined, { userIds: ["wxid_member"] }, { all: true }]) {
    const value = fixture();
    value.ctx.payloads[0] = { text: "Agreed", ...(location === "payload" && { replyToId: "msg_source" }),
      ...(mentions && { channelData: { "agent-wechat": { mentions } } }) };
    if (location === "context") value.ctx.replyToId = "msg_source";
    if (location === "effective") { value.ctx.replyToId = "older"; value.ctx.effectiveReplyToId = "msg_source"; }
    if (location === "qualified-current") value.ctx.replyToId = nativeSourceMessageSid({ id: "msg_source", accountId: "account", threadId: "g@chatroom", serverId: "18446744073709551615" } as any);
    value.setMessage({ ...value.getMessage(), type: 49, appMessageType: 57, body: mentions ? "@Member\u2005Agreed" : "Agreed", quote: { serverId: "18446744073709551615" },
      mentions: { state: "known", userIds: mentions?.userIds ?? [], all: mentions?.all === true, self: false, source: "msgsource.atuserlist" } });
    const hook = createWeChatDurableFinal((async () => ({ client: {
      sendByKey: async (...args: unknown[]) => { value.calls.push(["inspect", ...args]); return value.getIntent(); },
      message: async (_account: string, id: string) => id === "msg_source"
        ? { id, accountId: "account", threadId: "g@chatroom", serverId: "18446744073709551615", recall: { replacementText: "Recalled later" } }
        : value.getMessage(),
      sendText: async () => assert.fail("Recovery must never submit"),
    } })) as any).reconcileUnknownSend!;
    assert.equal((await hook(value.ctx))?.status, "sent");
    assert.deepEqual(value.calls, [["inspect", "account", nativeOutboundKey("retained-queue")]]);
    for (const change of [{ body: "Only part" }, { body: mentions ? "@Member\u2005Different" : "Different" }, { type: 1 }, { appMessageType: 5 }, { quote: undefined },
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
