import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { NativeApiError } from "@agent-wechat/shared";
import { createNativeRecallTools, nativeRecallKey, registerNativeRecallTools } from "./native-recall-tools.ts";

const serverId = "18446744073709551614";
function fixture(overrides: Record<string, unknown> = {}, waitMs = 0) {
  const cfg: any = { channels: { "agent-wechat": { dmPolicy: "allowlist", allowFrom: ["requester"], groupPolicy: "allowlist", groupAllowFrom: ["requester"],
    native: { mode: "external", endpoint: "http://127.0.0.1:6174", accountId: "account", token: "synthetic-token-".repeat(4), consumerStateDir: "/private/fixture" } } } };
  const calls: any[] = [];
  let message: any = { id: "msg_exact", accountId: "account", threadId: "room@chatroom", direction: "outgoing", serverId };
  let operation: any = { id: "op_recall", accountId: "account", kind: "message_recall", state: "succeeded",
    result: { messageId: message.id, threadId: message.threadId, serverId, status: "recalled", remoteOutcome: "accepted", retryable: false } };
  const client: any = {
    message: async (...args: any[]) => { calls.push(["message", ...args]); return message; },
    recallMessage: async (...args: any[]) => { calls.push(["recall", ...args.slice(0, 4)]); return operation; },
    operation: async (...args: any[]) => { calls.push(["operation", ...args.slice(0, 2)]); return operation; },
  };
  const context: any = { getRuntimeConfig: () => cfg, messageChannel: "agent-wechat", nativeChannelId: "room@chatroom", requesterSenderId: "requester",
    assertInvocationCurrent: () => {}, ...overrides };
  const tools = createNativeRecallTools(context, { core: { channel: { pairing: { readAllowFromStore: async () => [] } } }, host: async () => ({ client }) as any, waitMs });
  const run = (input: any = { messageId: "msg_exact", serverId }, signal?: AbortSignal) => { assert.equal(tools.length, 1); return tools[0].execute("arbitrary-tool-call", input, signal); };
  return { cfg, calls, client, context, tools, run, setMessage: (value: any) => { message = value; }, message: () => message,
    setOperation: (value: any) => { operation = value; }, operation: () => operation };
}

test("recall is registered through the real version 2 current-conversation tool factory", () => {
  const registrations: any[] = [];
  registerNativeRecallTools({ registerTool: (...args: any[]) => registrations.push(args) } as any);
  assert.equal(registrations[0][0].contextVersion, 2);
  assert.deepEqual(registrations[0][1], { names: ["wechat_recall"] });
  const manifest = JSON.parse(readFileSync(new URL("../openclaw.plugin.json", import.meta.url), "utf8"));
  assert.ok(manifest.contracts.tools.includes("wechat_recall"));
  assert.match(readFileSync(new URL("../index.ts", import.meta.url), "utf8"), /registerNativeRecallTools\(api\)/);
});
test("verified exact references produce one scoped recall and a stable target key independent of tool-call IDs", async () => {
  const value = fixture();
  const first: any = await value.run(); const second: any = await value.run();
  assert.equal(first.details.status, "recalled"); assert.equal(first.details.serverId, serverId);
  const submits = value.calls.filter(call => call[0] === "recall");
  assert.equal(submits.length, 2); assert.deepEqual(submits[0], submits[1]);
  assert.equal(submits[0][4], nativeRecallKey("account", "room@chatroom", "msg_exact", serverId));
  assert.notEqual(submits[0][4], nativeRecallKey("account", "room@chatroom", "msg_exact", "18446744073709551613"));
  assert.equal(first.details.idempotencyKey, second.details.idempotencyKey);
});
test("normalized recalled placeholders can inspect the retained exact outcome after own attribution is removed", async () => {
  const value = fixture(); const first: any = await value.run();
  value.setMessage({ ...value.message(), type: 10000, body: "[Message recalled]", direction: null, senderId: "system",
    recall: { observedAt: "2026-10-21T12:00:00Z" } });
  value.setOperation({ ...value.operation(), result: { ...value.operation().result, status: "already_recalled" } });
  const replay: any = await value.run(); assert.equal(replay.details.status, "already_recalled");
  assert.equal(replay.details.action, "inspect_recalled"); assert.equal(replay.details.idempotencyKey, first.details.idempotencyKey);
  value.setMessage({ ...value.message(), serverId: "7" }); await assert.rejects(value.run(), /identity does not match/);
});
test("missing or contradictory trusted route context hides recall, including for owners", () => {
  for (const senderIsOwner of [true, false]) for (const context of [
    { messageChannel: "webchat" }, { messageChannel: undefined }, { nativeChannelId: undefined }, { requesterSenderId: undefined },
    { agentAccountId: "other" }, { deliveryContext: { channel: "agent-wechat", accountId: "other", to: "room@chatroom" } },
    { deliveryContext: { channel: "agent-wechat", to: "other@chatroom" } },
  ]) assert.equal(fixture({ senderIsOwner, ...context }).tools.length, 0);
  assert.equal(fixture({ nativeChannelId: undefined, deliveryContext: { channel: "agent-wechat", to: "wechat:group:room@chatroom" } }).tools.length, 1);
});
test("owners cannot widen the current account/thread or recall another sender's message", async () => {
  for (const senderIsOwner of [true, false]) {
    const value = fixture({ senderIsOwner });
    for (const extra of [{ accountId: "other" }, { threadId: "other" }, { direction: "incoming" }, { id: "other" }]) {
      value.setMessage({ ...value.message(), id: "msg_exact", accountId: "account", threadId: "room@chatroom", direction: "outgoing", ...extra });
      await assert.rejects(value.run(), /outside this conversation/);
    }
    assert.equal(value.calls.some(call => call[0] === "recall"), false);
  }
});
test("reused local IDs, missing exact IDs and caller-selected scopes fail before mutation", async () => {
  const value = fixture();
  for (const input of [{ messageId: "msg_exact" }, { messageId: "msg_exact", serverId: 9007199254740992 },
    { messageId: "msg_exact", serverId: "01" }, { messageId: "msg_exact", serverId: "18446744073709551616" },
    { messageId: "msg_exact", serverId, threadId: "other" }, { messageId: "msg_exact", serverId, accountId: "other" }]) await assert.rejects(value.run(input));
  assert.equal(value.calls.length, 0);
  value.setMessage({ ...value.message(), serverId: "18446744073709551613" });
  await assert.rejects(value.run(), /may have been reused/);
  assert.equal(value.calls.some(call => call[0] === "recall"), false);
});
test("requester policy, account changes, cancellation and stale invocations cannot be bypassed", async () => {
  for (const senderIsOwner of [true, false]) {
    const value = fixture({ senderIsOwner }); value.cfg.channels["agent-wechat"].groupAllowFrom = ["other"];
    await assert.rejects(value.run(), /no longer allowed/); assert.equal(value.calls.length, 0);
  }
  const value = fixture(); const abort = new AbortController(); abort.abort();
  await assert.rejects(value.run(undefined, abort.signal)); assert.equal(value.calls.length, 0);
  value.context.assertInvocationCurrent = () => { throw Error("stale turn"); }; await assert.rejects(value.run(), /stale turn/);
  value.context.assertInvocationCurrent = () => {}; value.cfg.channels["agent-wechat"].native.accountId = "changed";
  await assert.rejects(value.run(), /configuration changed/);
});
test("permission revoked during exact message lookup prevents the recall POST", async () => {
  const value = fixture(); value.client.message = async () => { value.cfg.channels["agent-wechat"].groupAllowFrom = ["other"]; return value.message(); };
  await assert.rejects(value.run(), /no longer allowed/);
  assert.equal(value.calls.some(call => call[0] === "recall"), false);
});
test("replacing the daemon or credential provider cannot reuse an admitted recall context", async () => {
  for (const change of ['endpoint', 'provider']) {
    const value = fixture();
    value.client.message = async () => {
      if (change === 'endpoint') value.cfg.channels["agent-wechat"].native.endpoint = "http://127.0.0.1:6175";
      else value.cfg.secrets = { providers: { fixture: { source: 'env', allowlist: ['SYNTHETIC_TOKEN'] } } };
      return value.message();
    };
    await assert.rejects(value.run(), /configuration changed/);
    assert.equal(value.calls.some(call => call[0] === "recall"), false);
  }
});
test("uncertain submission and pending/rejected outcomes never trigger automatic repeat POSTs", async () => {
  const value = fixture(); let submissions = 0;
  value.client.recallMessage = async () => { submissions++; throw new NativeApiError(503, "response_unavailable", true); };
  const unknown: any = await value.run(); assert.equal(unknown.details.status, "needs_review"); assert.equal(unknown.details.retryable, false); assert.equal(submissions, 1);
  assert.equal(value.calls.some(call => call[0] === "operation"), false);
  for (const [state, status] of [["queued", "pending"], ["needs_review", "needs_review"], ["failed", "not_eligible"], ["succeeded", "already_recalled"]]) {
    const next = fixture(); next.setOperation({ ...next.operation(), state, result: { ...next.operation().result, status: state === "queued" ? undefined : status } });
    const result: any = await next.run(); assert.equal(result.details.status, status);
    assert.equal(next.calls.filter(call => call[0] === "recall").length, 1);
  }
});
test("a success without an exact target receipt cannot be reported as confirmed", async () => {
  const value = fixture(); value.setOperation({ ...value.operation(), result: { status: "recalled" } });
  assert.equal((await value.run() as any).details.status, "needs_review");
  value.setOperation({ ...value.operation(), result: { ...fixture().operation().result, serverId: "3" } });
  await assert.rejects(value.run(), /target identity/);
});
test("contradictory terminal states cannot be reported as recalled or as a confirmed rejection", async () => {
  for (const [state, receipt] of [
    ["failed", { status: "recalled" }], ["needs_review", { status: "already_recalled" }],
    ["succeeded", { status: "not_self" }], ["failed", { status: "not_eligible", messageId: undefined }],
  ]) {
    const value = fixture(); value.setOperation({ ...value.operation(), state, result: { ...value.operation().result, ...(receipt as object) } });
    assert.equal((await value.run() as any).details.status, "needs_review");
    assert.equal(value.calls.filter(call => call[0] === "recall").length, 1);
  }
});
test("aborting after submission preserves the stable uncertain receipt without cancellation", async () => {
  const value = fixture(); const abort = new AbortController(); let submitted = 0;
  value.client.recallMessage = async () => { submitted++; abort.abort(); return value.operation(); };
  value.client.cancelOperation = async () => { assert.fail("A submitted recall must remain daemon-owned"); };
  const result: any = await value.run(undefined, abort.signal);
  assert.equal(result.details.status, "needs_review"); assert.equal(result.details.operationId, "op_recall");
  assert.equal(result.details.idempotencyKey, nativeRecallKey("account", "room@chatroom", "msg_exact", serverId));
  assert.equal(submitted, 1);
});
test("operation polling cannot adopt another operation even for the same exact message", async () => {
  const value = fixture({}, 1000);
  value.client.recallMessage = async () => { value.calls.push(["recall"]); return { ...value.operation(), state: "queued" }; };
  value.client.operation = async () => { value.calls.push(["operation"]); return { ...value.operation(), id: "op_other" }; };
  const result: any = await value.run();
  assert.equal(result.details.status, "needs_review"); assert.equal(value.calls.filter(call => call[0] === "recall").length, 1);
  assert.equal(result.details.operationId, "op_recall");
  assert.equal(value.calls.filter(call => call[0] === "operation").length, 1);
});
