import test from "node:test";
import assert from "node:assert/strict";
import { createNativeContactTools, registerNativeContactTools } from "./native-contact-tool.ts";
function fixture(extra: Record<string, unknown> = {}) {
  const cfg: any = {
    channels: {
      "agent-wechat": {
        native: {
          mode: "external",
          endpoint: "http://127.0.0.1:6174",
          accountId: "a",
          token: "fixture-secret",
          consumerStateDir: "/private/fixture",
        },
      },
    },
  };
  const calls: unknown[] = [];
  const context: any = {
    senderIsOwner: true,
    memoryAudience: { kind: "owner-private", agentId: "owner" },
    getRuntimeConfig: () => cfg,
    assertInvocationCurrent() {},
    assertMemoryAudienceCurrent() {},
    ...extra,
  };
  let after = () => {};
  const tools = createNativeContactTools(context, {
    host: async () =>
      ({
        client: {
          contactRequests: async (...args: unknown[]) => {
            calls.push(["requests", ...args]);
            after();
            return { items: [] };
          },
          sendContactRequest: async (...args: unknown[]) => {
            calls.push(["send", ...args]);
            return { id: "op", accountId: "a", kind: "contact_mutation" };
          },
          operation: async () => ({ id: "op", accountId: "other", kind: "group_mutation" }),
        },
      } as any),
  });
  return {
    tools,
    context,
    cfg,
    calls,
    after: (fn: () => void) => {
      after = fn;
    },
    run: (input: unknown) => tools[1].execute("test", input),
  };
}
test("account-wide contact tools require a live trusted private owner, and are optional", () => {
  for (const senderIsOwner of [undefined, false, 1, "true"])
    assert.equal(fixture({ senderIsOwner }).tools.length, 0);
  for (const memoryAudience of [undefined, { kind: "conversation" }])
    assert.equal(fixture({ memoryAudience }).tools.length, 0);
  assert.equal(fixture({ assertMemoryAudienceCurrent: undefined }).tools.length, 0);
  assert.equal(fixture({ assertInvocationCurrent: undefined }).tools.length, 0);
  assert.equal(fixture({ agentAccountId: "other" }).tools.length, 0);
  assert.equal(fixture().tools.length, 2);
  const registered: any[] = [];
  registerNativeContactTools({ registerTool: (...args: any[]) => registered.push(args) } as any);
  assert.equal(registered[0][0].contextVersion, 2);
  assert.equal(registered[0][1].optional, true);
});
test("contact tool rejects account spoofing, unrelated fields, malformed filters and uncertain-operation mismatches", async () => {
  const f = fixture();
  for (const input of [
    { action: "list", accountId: "other" },
    { action: "accept", id: "x", expectedVersion: "v" },
    { action: "list", direction: "both" },
    { action: "list", limit: 201 },
    { action: "send", recipientRef: "ref", message: "hi", idempotencyKey: "key", isOwner: true },
  ])
    await assert.rejects(f.run(input));
  assert.equal(f.calls.length, 0);
  await assert.rejects(f.run({ action: "status", operationId: "op" }), /identity/);
});
test("owner, audience and account changes are rechecked across awaits before result release", async () => {
  const f = fixture();
  f.after(() => {
    f.context.senderIsOwner = false;
  });
  await assert.rejects(f.run({ action: "list" }), /owner/);
  assert.equal(f.calls.length, 1);
  f.context.senderIsOwner = true;
  f.after(() => {
    f.cfg.channels["agent-wechat"].native.accountId = "other";
  });
  await assert.rejects(f.run({ action: "list" }), /configuration changed/);
  const g = fixture();
  g.context.assertInvocationCurrent = () => {
    throw Error("stale");
  };
  await assert.rejects(g.run({ action: "list" }), /stale/);
  assert.equal(g.calls.length, 0);
});
test("one send preserves the caller's original greeting, recipient reference and stable key", async () => {
  const f = fixture();
  await f.run({
    action: "send",
    recipientRef: "opaque",
    message: "hello",
    idempotencyKey: "same-key",
  });
  assert.deepEqual(f.calls[0], [
    "send",
    "a",
    { recipientRef: "opaque", message: "hello" },
    "same-key",
    undefined,
  ]);
});
