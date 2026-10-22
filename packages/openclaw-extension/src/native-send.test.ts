import test from "node:test";
import assert from "node:assert/strict";
import type { NativeWeChatClient } from "@agent-wechat/shared";
import { nativeOutboundDelivery, submitNativeText } from "./native-send.ts";
test("native send deadline never resubmits or includes message content in error", async () => {
  let submissions = 0;
  const client = {
    sendText: async () => { submissions++; return { id: "send_fixture", state: "queued" }; },
    send: async () => ({ id: "send_fixture", state: "queued" }),
  } as unknown as NativeWeChatClient;
  await assert.rejects(submitNativeText(client, "a", "u", "private fixture text", "key", { timeoutMs: 5, initialDelayMs: 1 }), error => error instanceof Error && /queued/.test(error.message) && !error.message.includes("private fixture text"));
  assert.equal(submissions, 1);
});

test("verified send receipts return actual stored IDs and exact server IDs for each sent note", async () => {
  const calls: unknown[][] = [];
  const ids = ["msg_first", "msg_second"];
  const serverIds = ["18446744073709551614", "18446744073709551615"];
  const client = { message: async (accountId: string, id: string) => {
    calls.push([accountId, id]); return { id, accountId, threadId: "peer", direction: "outgoing", serverId: serverIds[ids.indexOf(id)] };
  } } as unknown as NativeWeChatClient;
  const result = await nativeOutboundDelivery(client, "account", "peer", ids, { jobId: "job_voice", state: "completed", serverIds });
  assert.equal(result.messageId, "msg_first"); assert.equal(result.meta.serverId, serverIds[0]);
  assert.equal(result.meta.receiptState, "verified"); assert.equal(result.meta.outcome, "sent");
  assert.deepEqual(result.meta.messageIds, ids.map((messageId, index) => ({ messageId, serverId: serverIds[index], accountId: "account", threadId: "peer" })));
  assert.deepEqual(calls, [["account", "msg_first"], ["account", "msg_second"]]);
});

test("missing, foreign or reused send receipts retain the submission without inventing a message ID", async () => {
  const sent = { id: "msg_stored", accountId: "account", threadId: "peer", direction: "outgoing", serverId: "9007199254740999" };
  let reads = 0;
  let message = sent;
  const client = { message: async () => { reads++; return message; } } as unknown as NativeWeChatClient;
  for (const ids of [undefined, [], ["msg_stored", "msg_stored"], [4], ["x".repeat(4097)]]) {
    const result = await nativeOutboundDelivery(client, "account", "peer", ids, { sendId: "send_pending_projection", state: "accepted" });
    assert.equal(result.messageId, ""); assert.equal(result.meta.receiptState, "needs_review");
    assert.equal(result.meta.outcome, "unknown"); assert.equal(result.meta.retryable, false);
    assert.equal(result.meta.sendId, "send_pending_projection");
  }
  assert.equal(reads, 0);
  for (const changed of [{ id: "msg_other" }, { accountId: "other" }, { threadId: "other" }, { direction: "incoming" },
    { serverId: "0" }, { serverId: "18446744073709551616" }, { serverId: "9007199254740998" }]) {
    message = { ...sent, ...changed };
    const result = await nativeOutboundDelivery(client, "account", "peer", ["msg_stored"], { sendId: "send_accepted", state: "accepted", serverIds: [sent.serverId] });
    assert.equal(result.messageId, ""); assert.equal(result.meta.receiptState, "needs_review");
    assert.deepEqual(result.meta.messageIds, []); assert.match(String(result.meta.note), /do not resend/);
  }
});

test("partial voice receipt verification preserves confirmed notes and never repeats submission", async () => {
  const client = { message: async (_account: string, id: string) => {
    if (id === "msg_missing") throw Error("projection pending");
    return { id, accountId: "account", threadId: "peer", direction: "outgoing", serverId: "9007199254740999" };
  } } as unknown as NativeWeChatClient;
  const result = await nativeOutboundDelivery(client, "account", "peer", ["msg_confirmed", "msg_missing"], { jobId: "job_partially_visible", state: "completed" });
  assert.equal(result.messageId, ""); assert.equal(result.meta.receiptState, "needs_review");
  assert.deepEqual(result.meta.messageIds, [{ messageId: "msg_confirmed", serverId: "9007199254740999", accountId: "account", threadId: "peer" }]);
  assert.equal(result.meta.jobId, "job_partially_visible");
});
