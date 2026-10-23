import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createNativeGroupTools, registerNativeGroupTools } from "./native-group-tool.ts";
function fixture(extra: Record<string, unknown> = {}) {
  const cfg: any = { channels: { "agent-wechat": { native: { mode: "external", endpoint: "http://127.0.0.1:6174", accountId: "a", token: "fixture-secret", consumerStateDir: "/private/fixture" } } } };
  const calls: unknown[] = [];
  const context: any = { senderIsOwner: true, getRuntimeConfig: () => cfg, assertInvocationCurrent: () => {}, ...extra };
  const tools = createNativeGroupTools(context, { host: async () => ({ client: {
    group: async (...args: unknown[]) => { calls.push(["group", ...args]); return { id: "room@chatroom", roomId: "room@chatroom" }; },
    groupMembers: async (...args: unknown[]) => { calls.push(["list", ...args]); return { items: [], nextCursor: "opaque", coverage: { complete: false } }; },
    groupMember: async (...args: unknown[]) => { calls.push(["get", ...args]); return { groupId: "room@chatroom", username: "peer", state: "unconfirmed", member: null }; },
  } }) as any });
  const run = (input: unknown = { groupId: "room@chatroom" }, signal?: AbortSignal) => tools[0].execute("call", input, signal);
  return { tools, calls, context, cfg, run };
}
test("group info is visible only to a host-proved owner with a live invocation guard", () => {
  for (const senderIsOwner of [false, undefined, "true", 1]) assert.equal(fixture({ senderIsOwner }).tools.length, 0);
  assert.equal(fixture({ assertInvocationCurrent: undefined }).tools.length, 0);
  assert.equal(fixture({ agentAccountId: "other" }).tools.length, 0);
  assert.equal(fixture().tools.length, 1);
  const registered: any[] = [];
  registerNativeGroupTools({ registerTool: (...args: any[]) => registered.push(args) } as any);
  assert.equal(registered[0][0].contextVersion, 2);
  assert.deepEqual(registered[0][1].names, ["wechat_group_info"]);
  const manifest = JSON.parse(readFileSync(new URL("../openclaw.plugin.json", import.meta.url), "utf8"));
  assert.ok(manifest.contracts.tools.includes("wechat_group_info"));
  assert.equal(manifest.contracts.tools.some((n: string) => /group_(?:create|rename|leave|invite|remove)/.test(n)), false);
});
test("owner group info preserves pagination and unconfirmed single-member absence", async () => {
  const value = fixture();
  const listed: any = await value.run({ groupId: "room@chatroom", limit: 7, cursor: "old" });
  assert.equal(listed.details.members.nextCursor, "opaque");
  assert.deepEqual(value.calls[1], ["list", "a", "room@chatroom", { limit: 7, cursor: "old" }, undefined]);
  const single: any = await value.run({ groupId: "room@chatroom", username: "peer" });
  assert.equal(single.details.members.state, "unconfirmed");
  for (const input of [{ groupId: "peer" }, { groupId: "room@chatroom", accountId: "other" }, { groupId: "room@chatroom", username: "peer", limit: 1 }, { groupId: "room@chatroom", limit: 101 }]) await assert.rejects(value.run(input));
});
test("owner revocation, account changes, cancellation and stale calls fail before release", async () => {
  const value = fixture();
  const controller = new AbortController(); controller.abort();
  await assert.rejects(value.run(undefined, controller.signal)); assert.equal(value.calls.length, 0);
  value.context.senderIsOwner = false; await assert.rejects(value.run(), /owner/);
  value.context.senderIsOwner = true; value.context.assertInvocationCurrent = () => { throw Error("stale"); };
  await assert.rejects(value.run(), /stale/);
  value.context.assertInvocationCurrent = () => {}; value.cfg.channels["agent-wechat"].native.accountId = "other";
  await assert.rejects(value.run(), /configuration changed/);
  assert.equal(value.calls.length, 0);
});
