import test from "node:test";
import assert from "node:assert/strict";
import type { NativeWeChatClient } from "@agent-wechat/shared";
import { nativeOutboundDelivery, submitNativeText, NativeTextSendInspectionError } from "./native-send.ts";
import { NativeApiError } from "@agent-wechat/shared";
import { readFileSync } from "node:fs";

const legacyIntent = Object.freeze(JSON.parse(readFileSync(new URL("../test/fixtures/native-send-legacy.json", import.meta.url), "utf8")));
const currentIntent = Object.freeze(JSON.parse(readFileSync(new URL("../test/fixtures/native-send-current.json", import.meta.url), "utf8")));

test("immutable legacy and current acknowledgements retain authenticated request scope", async () => {
  assert.deepEqual(legacyIntent, { id: "send_fixture", state: "accepted" });
  assert.match(currentIntent.clientId, /^\d+$/);
  for (const fixture of [legacyIntent, currentIntent]) {
    let submissions = 0;
    const client = { sendText: async () => { submissions++; return fixture; } } as unknown as NativeWeChatClient;
    const intent = await submitNativeText(client, "a", "g@chatroom", "fixture text", "stable-key");
    assert.deepEqual(intent, { ...fixture, accountId: "a", destination: "g@chatroom" });
    assert.equal(submissions, 1); assert.equal(Object.isFrozen(fixture), true);
  }
  assert.equal(Object.hasOwn(legacyIntent, "accountId"), false); assert.equal(Object.hasOwn(legacyIntent, "destination"), false);
});

test("legacy and current queued acknowledgements poll the same exact send without resubmitting", async () => {
  for (const fixture of [legacyIntent, currentIntent]) {
    let submissions = 0; const polls: unknown[][] = [];
    const client = { sendText: async () => { submissions++; return { ...fixture, state: "queued" }; },
      send: async (...args: unknown[]) => { polls.push(args); return { ...fixture, state: polls.length === 1 ? "submitting" : "accepted" }; } } as unknown as NativeWeChatClient;
    const intent = await submitNativeText(client, "a", "g@chatroom", "fixture text", "stable-key", { timeoutMs: 1000, initialDelayMs: 1 });
    assert.equal(intent.id, fixture.id); assert.equal(intent.accountId, "a"); assert.equal(intent.destination, "g@chatroom");
    assert.equal(intent.state, "accepted"); assert.equal(submissions, 1);
    assert.deepEqual(polls.map(args => args.slice(0, 2)), [["a", fixture.id], ["a", fixture.id]]);
  }
});

test("legacy and current lost-response recovery use only the original key", async () => {
  for (const fixture of [legacyIntent, currentIntent]) {
    const calls: unknown[][] = [];
    const client = { sendText: async (...args: unknown[]) => { calls.push(["submit", ...args]); throw Error("synthetic lost response"); },
      sendByKey: async (...args: unknown[]) => { calls.push(["recover", ...args]); return fixture; } } as unknown as NativeWeChatClient;
    const intent = await submitNativeText(client, "a", "g@chatroom", "fixture text", "stable-key");
    assert.equal(intent.id, fixture.id); assert.equal(intent.accountId, "a"); assert.equal(intent.destination, "g@chatroom");
    assert.deepEqual(calls.map(args => args[0]), ["submit", "recover"]);
    assert.deepEqual(calls[1], ["recover", "a", "stable-key"]);
  }
});

test("supplied mismatched or null identities fail at submission, polling and recovery", async () => {
  for (const fixture of [legacyIntent, currentIntent]) for (const change of [
    { accountId: "other" }, { accountId: null }, { destination: "other@chatroom" }, { destination: null }, { kind: "send", threadId: "g@chatroom" },
  ]) for (const stage of ["submit", "poll", "recover"]) {
    let submissions = 0, polls = 0, recoveries = 0;
    const wrong = { ...fixture, ...change };
    const client = {
      sendText: async () => { submissions++; if (stage === "recover") throw Error("lost response"); return stage === "submit" ? wrong : { ...fixture, state: "queued" }; },
      send: async () => { polls++; return wrong; },
      sendByKey: async () => { recoveries++; return wrong; },
    } as unknown as NativeWeChatClient;
    await assert.rejects(submitNativeText(client, "a", "g@chatroom", "fixture text", "retained-key", { initialDelayMs: 1 }), error => {
      assert.ok(error instanceof NativeTextSendInspectionError); assert.equal(error.state, "receipt_identity_mismatch");
      assert.equal(error.idempotencyKey, "retained-key"); assert.equal(error.retryable, false); return true;
    });
    assert.equal(submissions, 1); assert.equal(polls, stage === "poll" ? 1 : 0); assert.equal(recoveries, stage === "recover" ? 1 : 0);
  }
});

test("identity-free polling cannot switch the send ID", async () => {
  const client = { sendText: async () => ({ ...legacyIntent, state: "queued" }), send: async () => ({ id: "send_other", state: "accepted" }) } as unknown as NativeWeChatClient;
  await assert.rejects(submitNativeText(client, "a", "g@chatroom", "fixture text", "retained-key", { initialDelayMs: 1 }), error => {
    assert.ok(error instanceof NativeTextSendInspectionError); assert.equal(error.sendId, legacyIntent.id); return true;
  });
});

test("legacy acknowledgement is acceptance only and canonical readback is required for verified delivery", async () => {
  const stored = { id: "msg_fixture", accountId: "a", threadId: "g@chatroom", direction: "outgoing", serverId: "9007199254740999" };
  let canonical: any = stored, reads = 0;
  const client = { sendText: async () => legacyIntent, message: async () => { reads++; return canonical; } } as unknown as NativeWeChatClient;
  const accepted = await submitNativeText(client, "a", "g@chatroom", "fixture text", "retained-key");
  const receipt = await nativeOutboundDelivery(client, "a", "g@chatroom", [], { sendId: accepted.id, state: accepted.state });
  assert.equal(receipt.meta.outcome, "unknown"); assert.equal(receipt.meta.receiptState, "needs_review"); assert.equal(receipt.messageId, ""); assert.equal(reads, 0);
  for (const fixture of [legacyIntent, currentIntent]) {
    for (const change of [{ accountId: "other" }, { threadId: "other@chatroom" }, { direction: "incoming" }, { serverId: "1" }]) {
      canonical = { ...stored, ...change };
      const result = await nativeOutboundDelivery(client, "a", "g@chatroom", [stored.id], { sendId: fixture.id, state: fixture.state, serverIds: [stored.serverId] });
      assert.equal(result.meta.outcome, "unknown"); assert.equal(result.meta.receiptState, "needs_review"); assert.equal(result.messageId, "");
    }
    canonical = stored;
    const verified = await nativeOutboundDelivery(client, "a", "g@chatroom", [stored.id], { sendId: fixture.id, state: fixture.state, serverIds: [stored.serverId] });
    assert.equal(verified.meta.outcome, "sent"); assert.equal(verified.meta.receiptState, "verified"); assert.equal(verified.messageId, stored.id);
  }
});
test("native send deadline never resubmits or includes message content in error", async () => {
  let submissions = 0;
  const client = {
    sendText: async () => { submissions++; return { id: "send_fixture", state: "queued" }; },
    send: async () => ({ id: "send_fixture", state: "queued" }),
  } as unknown as NativeWeChatClient;
  await assert.rejects(submitNativeText(client, "a", "u", "private fixture text", "key", { timeoutMs: 5, initialDelayMs: 1 }), error => error instanceof Error && /queued/.test(error.message) && !error.message.includes("private fixture text"));
  assert.equal(submissions, 1);
});

test("text mentions survive a lost response and recover only through their original key", async () => {
  const calls: unknown[][] = [];
  const receipt = { id: "send_mentions", accountId: "a", destination: "g@chatroom", state: "accepted" };
  const client = {
    sendText: async (...args: unknown[]) => { calls.push(["submit", ...args]); throw Error("connection lost after submission"); },
    sendByKey: async (...args: unknown[]) => { calls.push(["recover", ...args]); return receipt; },
  } as unknown as NativeWeChatClient;
  assert.deepEqual(await submitNativeText(client, "a", "g@chatroom", "check this", "stable-key", { mentions: { userIds: ["wxid_a", "wxid_b"] } }), receipt);
  assert.deepEqual(calls, [["submit", "a", "g@chatroom", "check this", "stable-key", { mentions: { userIds: ["wxid_a", "wxid_b"] } }], ["recover", "a", "stable-key"]]);
});

test("unknown mention sends retain identity and never resubmit or accept foreign receipts", async () => {
  for (const recovered of [undefined, { id: "send_foreign", accountId: "other", destination: "g@chatroom", state: "accepted" },
    { id: "media_job", accountId: "a", threadId: "g@chatroom", state: "accepted" },
    { id: "send_review", accountId: "a", destination: "g@chatroom", state: "needs_review" }]) {
    let submissions = 0, recoveries = 0;
    const client = { sendText: async () => { submissions++; throw Error("private source detail"); }, sendByKey: async () => { recoveries++; if (!recovered) throw Error("unavailable"); return recovered; } } as unknown as NativeWeChatClient;
    await assert.rejects(submitNativeText(client, "a", "g@chatroom", "private message", "retained-key", { mentions: { all: true } }), error => {
      assert.ok(error instanceof NativeTextSendInspectionError); assert.equal(error.retryable, false);
      assert.equal(error.idempotencyKey, "retained-key"); assert.equal(error.accountId, "a"); assert.equal(error.threadId, "g@chatroom");
      assert.ok(!error.message.includes("private")); return true;
    });
    assert.equal(submissions, 1); assert.equal(recoveries, 1);
  }
});

test("rejected mention sends preserve the daemon error without lookup or resubmission", async () => {
  for (const rejected of [new NativeApiError(400, "unsupported_mentions"), new NativeApiError(403, "mention_all_not_allowed"), new NativeApiError(409, "group_membership_changed")]) {
    let submissions = 0, recoveries = 0;
    const client = { sendText: async () => { submissions++; throw rejected; }, sendByKey: async () => { recoveries++; } } as unknown as NativeWeChatClient;
    await assert.rejects(submitNativeText(client, "a", "g@chatroom", "hello", "key", { mentions: { all: true } }), error => error === rejected);
    assert.equal(submissions, 1); assert.equal(recoveries, 0);
  }
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
