import test from "node:test";
import assert from "node:assert/strict";
import { NativeApiError } from "@agent-wechat/shared";
import { createNativeShareTools, registerNativeShareTools } from "./native-share-tool.ts";

const args = { title: "A shared article", url: "https://example.invalid/article?a=1&b=2", description: "An explicit description", thumbnailUrl: "https://example.invalid/preview.png", idempotencyKey: "logical-share-1" };
function fixture(context: Record<string, unknown> = {}) {
  const cfg: any = { channels: { "agent-wechat": { dmPolicy: "allowlist", allowFrom: ["u1"], groupPolicy: "allowlist", groupAllowFrom: ["u1"],
    native: { mode: "external", endpoint: "http://127.0.0.1:6174", accountId: "a", token: "synthetic-native-token-".repeat(3), consumerStateDir: "/unused" } } } };
  const calls: any[] = []; let state = "queued", change: any = {}, failure: unknown, hostEffect = () => {};
  const core = { channel: { pairing: { readAllowFromStore: async () => [] } } };
  const client: any = { sendShare: async (...values: any[]) => {
    calls.push(values); if (failure) throw failure;
    return { id: "send-share", accountId: "a", destination: values[1].threadId, state, serverId: state === "accepted" ? "18446744073709551615" : null,
      localMessageId: state === "accepted" ? "stored-share" : null, ...change };
  }, message: async (_account: string, id: string) => ({ id, accountId: "a", threadId: calls.at(-1)[1].threadId, direction: "outgoing", type: 49, serverId: "18446744073709551615" }) };
  const ctx: any = { getRuntimeConfig: () => cfg, messageChannel: "agent-wechat", nativeChannelId: "room@chatroom", requesterSenderId: "u1", ...context };
  const tools = createNativeShareTools(ctx, { core, host: async () => { hostEffect(); return { client } as any; } });
  const run = async (input: any = args, signal?: AbortSignal) => { assert.equal(tools.length, 1); return tools[0].execute("tool-call", input, signal); };
  return { cfg, ctx, calls, tools, run, state: (value: string) => { state = value; }, change: (value: any) => { change = value; }, failure: (value: unknown) => { failure = value; }, hostEffect: (effect: () => void) => { hostEffect = effect; } };
}
test("share tool sends only to the trusted current conversation with the caller's stable key", async () => {
  for (const nativeChannelId of ["room@chatroom", "u1"]) {
    const f = fixture({ nativeChannelId });
    const queued: any = await f.run(); assert.equal(queued.details.state, "queued"); assert.equal(queued.details.id, "send-share");
    f.state("accepted"); const accepted: any = await f.run();
    assert.equal(accepted.details.serverId, "18446744073709551615"); assert.match(accepted.details.note, /rendering is not confirmed/);
    assert.equal(accepted.details.messageId, "stored-share"); assert.equal(accepted.details.receiptState, "verified");
    assert.equal(f.calls.length, 2); assert.deepEqual(f.calls[0], f.calls[1]);
    assert.equal(f.calls[0][0], "a"); assert.equal(f.calls[0][2], args.idempotencyKey);
    assert.deepEqual(f.calls[0][1], { kind: "share", threadId: nativeChannelId, title: args.title, url: args.url, description: args.description, thumbnailUrl: args.thumbnailUrl });
  }
});
test("share tool is hidden without matching channel/account/current-thread facts", () => {
  assert.deepEqual(createNativeShareTools({} as any), []);
  for (const value of [{ messageChannel: "webchat" }, { nativeChannelId: undefined }, { requesterSenderId: undefined }, { agentAccountId: "foreign" },
    { deliveryContext: { channel: "agent-wechat", to: "foreign" } }, { deliveryContext: { channel: "agent-wechat", to: "room@chatroom", accountId: "foreign" } }]) {
    assert.equal(fixture({ senderIsOwner: true, ...value }).tools.length, 0);
  }
  assert.equal(fixture({ nativeChannelId: undefined, deliveryContext: { channel: "agent-wechat", to: "wechat:group:room@chatroom" } }).tools.length, 1);
});
test("share arguments cannot change destination or submit malformed/over-bound metadata", async () => {
  const f = fixture();
  for (const value of [{ ...args, threadId: "elsewhere" }, { ...args, accountId: "other" }, { ...args, kind: "text" }, { ...args, idempotencyKey: "" },
    { ...args, url: "javascript:alert(1)" }, { ...args, thumbnailUrl: "https://user:secret@example.invalid/p.png" }, { ...args, title: "🙂".repeat(5000) }, { ...args, description: "hidden\0value" }]) {
    await assert.rejects(f.run(value));
  }
  assert.equal(f.calls.length, 0);
});
test("share sender admission, invocation freshness and account changes are checked before submission", async () => {
  for (const kind of ["policy", "account", "endpoint", "cancel", "invocation"]) {
    const f = fixture(), abort = new AbortController();
    if (kind === "policy") f.hostEffect(() => { f.cfg.channels["agent-wechat"].groupAllowFrom = ["other"]; });
    if (kind === "account") f.hostEffect(() => { f.cfg.channels["agent-wechat"].native.accountId = "other"; });
    if (kind === "endpoint") f.hostEffect(() => { f.cfg.channels["agent-wechat"].native.endpoint = "http://127.0.0.1:6175"; });
    if (kind === "cancel") abort.abort();
    if (kind === "invocation") f.ctx.assertInvocationCurrent = () => { throw Error("stale invocation"); };
    await assert.rejects(f.run(args, abort.signal)); assert.equal(f.calls.length, 0);
  }
});
test("uncertain shares return the original key without retry or claiming success", async () => {
  for (const error of [new NativeApiError(503, "transport_unavailable", true), Error("synthetic lost response")]) {
    const f = fixture(); f.failure(error);
    const result: any = await f.run(); assert.equal(result.isError, true); assert.equal(result.details.state, "submission_unknown");
    assert.equal(result.details.idempotencyKey, args.idempotencyKey); assert.match(result.details.note, /Do not resend/); assert.equal(f.calls.length, 1);
  }
  for (const state of ["failed", "needs_review", "cancelled"]) {
    const f = fixture(); f.state(state); const result: any = await f.run();
    assert.equal(result.isError, true); assert.equal(result.details.state, state); assert.equal(f.calls.length, 1);
  }
  const rejected = fixture(); rejected.failure(new NativeApiError(409, "idempotency_conflict"));
  await assert.rejects(rejected.run(), /idempotency_conflict/); assert.equal(rejected.calls.length, 1);
});
test("foreign share receipts fail without another submission and registration uses context v2", async () => {
  for (const change of [{ accountId: "other" }, { destination: "other" }, { id: "" }]) {
    const f = fixture(); f.change(change); await assert.rejects(f.run(), /receipt crosses/); assert.equal(f.calls.length, 1);
  }
  const registrations: any[] = []; registerNativeShareTools({ registerTool: (...values: any[]) => registrations.push(values) } as any);
  assert.equal(registrations[0][0].contextVersion, 2); assert.deepEqual(registrations[0][1].names, ["wechat_send_share"]);
});
