import test from "node:test";
import assert from "node:assert/strict";
import { createNativeContactTools, registerNativeContactTools } from "./native-contact-tool.ts";
function fixture(extra: Record<string, unknown> = {}, deps: Record<string, unknown> = {}) {
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
    ...deps,
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
// OpenClaw's Claude CLI tool bridge supplies the owner bit, session, requester and
// invocation authority, but no memory audience. The stored session decides.
const ownerChat = {
  sessionId: "s1",
  chatType: "direct",
  delivery: {
    kind: "external",
    context: { channel: "agent-wechat", to: "wechat:wxid_owner", accountId: "default" },
  },
};
function cliBridge(extra: Record<string, unknown> = {}, stored: any = ownerChat) {
  const store = { entry: stored, reads: 0 };
  const sessions = {
    resolveStorePath: () => "/private/store",
    getSessionEntry: () => store.entry,
    getSessionEntryAsync: async () => {
      store.reads += 1;
      return store.entry;
    },
  };
  const f = fixture({
    memoryAudience: undefined,
    assertMemoryAudienceCurrent: undefined,
    agentId: "main",
    sessionKey: "agent:main:agent-wechat:direct:wxid_owner",
    sessionId: "s1",
    requesterSenderId: "wxid_owner",
    ...extra,
  }, { sessions });
  return Object.assign(f, { store });
}
const withRoute = (context: Record<string, unknown>, extra: Record<string, unknown> = {}) => ({
  ...ownerChat,
  ...extra,
  delivery: { kind: "external", context },
});
test("without a host memory audience, only the owner's own stored direct chat gets the tools", async () => {
  assert.equal(cliBridge().tools.length, 2);
  assert.equal(
    cliBridge(
      { requesterSenderId: "+15550100", sessionKey: "agent:main:whatsapp:direct:+15550100" },
      withRoute({ channel: "whatsapp", to: "+15550100" })
    ).tools.length,
    2
  );
  for (const [label, stored] of [
    ["no stored session", null],
    ["group", { ...ownerChat, chatType: "group" }],
    ["cron (no external route)", { ...ownerChat, delivery: { kind: "none" } }],
    ["main (internal route)", { ...ownerChat, delivery: { kind: "internal" } }],
    ["spawned sub-agent", { ...ownerChat, spawnedBy: "agent:main:main" }],
    ["reset session", { ...ownerChat, sessionId: "s0" }],
    ["someone else's chat", withRoute({ channel: "agent-wechat", to: "wechat:wxid_alice" })],
    ["group route", withRoute({ channel: "agent-wechat", to: "channel:1@chatroom" })],
    ["thread", withRoute({ channel: "agent-wechat", to: "wechat:wxid_owner", threadId: "9" })],
  ] as const)
    assert.equal(cliBridge({}, stored).tools.length, 0, label);
  for (const extra of [
    { senderIsOwner: false },
    { senderIsOwner: undefined },
    { assertInvocationCurrent: undefined },
    { requesterSenderId: undefined },
    // The shared `main` DM scope, or a session keyed by another person.
    { sessionKey: "agent:main:main" },
    { sessionKey: "agent:main:agent-wechat:direct:wxid_alice" },
    { sessionKey: "agent:main:agent-wechat:group:wxid_owner" },
    { sessionId: undefined },
    { agentId: undefined },
    // An audience the host did resolve decides, even in the owner's direct chat.
    { memoryAudience: { kind: "conversation" } },
    { memoryAudience: { kind: "conversation" }, assertMemoryAudienceCurrent() {} },
    // A half-supplied audience is not the CLI bridge shape.
    { assertMemoryAudienceCurrent() {} },
  ])
    assert.equal(cliBridge(extra).tools.length, 0, JSON.stringify(extra));
  // Without a session reader the plugin cannot check, so it offers nothing.
  assert.equal(fixture({ memoryAudience: undefined, assertMemoryAudienceCurrent: undefined, agentId: "main", sessionKey: "k", sessionId: "s1", requesterSenderId: "wxid_owner" }).tools.length, 0);
  const f = cliBridge();
  await f.run({ action: "list" });
  assert.equal(f.calls.length, 1);
  assert.equal(f.store.reads, 2, "rereads before the effect and before release");
});
test("the stored-session check is rerun before the effect and before release", async () => {
  const reset = cliBridge();
  reset.store.entry = { ...ownerChat, sessionId: "s2" };
  await assert.rejects(reset.run({ action: "list" }), /owner/);
  assert.equal(reset.calls.length, 0);
  const rebound = cliBridge();
  rebound.after(() => {
    rebound.store.entry = withRoute({ channel: "agent-wechat", to: "wechat:wxid_alice" });
  });
  await assert.rejects(rebound.run({ action: "list" }), /owner/);
  assert.equal(rebound.calls.length, 1);
  const audience = cliBridge();
  audience.after(() => {
    audience.context.memoryAudience = { kind: "conversation" };
  });
  await assert.rejects(audience.run({ action: "list" }), /owner/);
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
