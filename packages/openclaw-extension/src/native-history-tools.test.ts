import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createNativeHistoryTools, registerNativeHistoryTools } from "./native-history-tools.ts";

function fixture(context: Record<string, unknown> = {}) {
  let cfg: any = { channels: { "agent-wechat": { dmPolicy: "allowlist", allowFrom: ["u1"], groupPolicy: "allowlist", groupAllowFrom: ["u1"],
    native: { mode: "external", endpoint: "http://127.0.0.1:6174", accountId: "a", token: "synthetic-native-token-".repeat(3), consumerStateDir: "/private/fixture" } } } };
  const calls: any[] = [];
  let messages: any[] = [{ id: "stored-before-buffer", accountId: "a", threadId: "room@chatroom", senderId: "u1", direction: "incoming", type: 1, body: "older searchable text", serverId: "9007199254740999" }];
  let guards = 0;
  const client: any = {
    threads: async (...args: any[]) => { calls.push(["threads", ...args]); return { items: [{ id: "room@chatroom", displayName: "Fixture" }], nextCursor: "older-threads" }; },
    thread: async (...args: any[]) => { calls.push(["thread", ...args]); return { id: args[1] }; },
    messages: async (...args: any[]) => { calls.push(["messages", ...args]); return { items: messages, nextCursor: "opaque-continuation", sourceGeneration: "g", coverage: { history: "partial" } }; },
    message: async (...args: any[]) => { calls.push(["message", ...args]); return messages[0]; },
  };
  const toolContext: any = { getRuntimeConfig: () => cfg, messageChannel: "agent-wechat", nativeChannelId: "room@chatroom", requesterSenderId: "u1", assertInvocationCurrent: () => { guards++; }, ...context };
  const tools = createNativeHistoryTools(toolContext, { host: async () => ({ client }) as any, core: { channel: { pairing: { readAllowFromStore: async () => [] } } } });
  const run = async (name: string, input: any, signal?: AbortSignal) => tools.find(tool => tool.name === name)!.execute("tool-call", input, signal);
  return { tools, calls, client, run, toolContext, guards: () => guards, config: () => cfg, setConfig: (value: any) => { cfg = value; }, setMessages: (value: any[]) => { messages = value; } };
}
test("history/search tools query the selected profile, preserving query cursors and string IDs", async () => {
  const value = fixture();
  const result: any = await value.run("wechat_search", { query: "searchable", limit: 7, cursor: "previous-cursor" });
  assert.deepEqual(value.calls, [["messages", "a", "room@chatroom", { limit: 7, cursor: "previous-cursor", query: "searchable" }]]);
  assert.equal(result.details.items[0].id, "stored-before-buffer"); assert.equal(result.details.items[0].serverId, "9007199254740999");
  assert.equal(result.details.nextCursor, "opaque-continuation"); assert.equal(result.details.coverage.history, "partial"); assert.ok(value.guards() >= 3);
});
test("owners can discover threads and search another conversation outside group turns", async () => {
  const value = fixture({ messageChannel: "webchat", nativeChannelId: undefined, senderIsOwner: true });
  await value.run("wechat_chats", { query: "Fixture", limit: 1 });
  await value.run("wechat_history", { threadId: "room@chatroom" });
  assert.deepEqual(value.calls[0], ["threads", "a", { limit: 1, cursor: undefined, query: "Fixture" }]);
  assert.equal(value.calls[1][2], "room@chatroom");
  await assert.rejects(value.run("wechat_history", {}), /Specify a threadId/);
});
test("group turns including the owner cannot list or read other conversations", async () => {
  for (const senderIsOwner of [false, true]) {
    const value = fixture({ senderIsOwner });
    await assert.rejects(value.run("wechat_search", { threadId: "private-peer", query: "secret" }), /current conversation/);
    assert.equal(value.calls.length, 0);
    const result: any = await value.run("wechat_chats", {}); assert.equal(result.details.items[0].id, "room@chatroom");
    assert.equal(value.calls[0][0], "thread");
  }
});
test("missing trusted conversation/owner context hides history tools", () => {
  for (const context of [{ messageChannel: "webchat", nativeChannelId: undefined }, { nativeChannelId: undefined, senderIsOwner: true }, { requesterSenderId: undefined }]) assert.equal(fixture(context).tools.length, 0);
});
test("trusted delivery context scopes released hosts that omit NativeChannelId", async () => {
  const value = fixture({ nativeChannelId: undefined, deliveryContext: { channel: "agent-wechat", to: "wechat:room@chatroom" } });
  await value.run("wechat_history", {}); assert.equal(value.calls[0][2], "room@chatroom");
  await assert.rejects(value.run("wechat_history", { threadId: "private-peer" }), /current conversation/);
  assert.equal(fixture({ deliveryContext: { channel: "agent-wechat", to: "wechat:other@chatroom" } }).tools.length, 0);
});
test("current sender policy is rechecked for the requester and historical authors", async () => {
  const value = fixture();
  value.setMessages([{ id: "blocked", accountId: "a", threadId: "room@chatroom", senderId: "blocked", type: 1, direction: "incoming", body: "must not leak" }]);
  const result: any = await value.run("wechat_history", {}); assert.deepEqual(result.details.items, []); assert.equal(result.details.nextCursor, "opaque-continuation");
  value.config().channels["agent-wechat"].groupAllowFrom = ["different"];
  const before = value.calls.length; await assert.rejects(value.run("wechat_history", {}), /no longer allowed/); assert.equal(value.calls.length, before);
});
test("account replacement, aborted calls and stale invocation fail before returning data", async () => {
  const value = fixture(); const abort = new AbortController(); abort.abort();
  await assert.rejects(value.run("wechat_history", {}, abort.signal)); assert.equal(value.calls.length, 0);
  value.toolContext.assertInvocationCurrent = () => { throw Error("stale turn"); };
  await assert.rejects(value.run("wechat_history", {}), /stale turn/);
  value.toolContext.assertInvocationCurrent = () => {};
  value.config().channels["agent-wechat"].native.accountId = "different";
  await assert.rejects(value.run("wechat_history", {}), /configuration changed/);
});
test("message lookup verifies thread/account and never exposes raw attachment XML", async () => {
  const value = fixture();
  value.setMessages([{ id: "file", accountId: "a", threadId: "room@chatroom", senderId: "u1", type: 49, body: "<private signed CDN XML>", direction: "incoming" }]);
  const result: any = await value.run("wechat_message", { messageId: "file" }); assert.match(result.details.body, /file message/); assert.ok(!JSON.stringify(result).includes("XML"));
  value.setMessages([{ id: "foreign", accountId: "a", threadId: "elsewhere", senderId: "u1", type: 1, body: "hidden", direction: "incoming" }]);
  await assert.rejects(value.run("wechat_message", { messageId: "foreign" }), /outside the permitted/);
  assert.ok(value.calls.every(call => ["messages", "message", "threads", "thread"].includes(call[0])));
});
test("tool bounds and required search query are checked before requests", async () => {
  const value = fixture();
  for (const args of [{}, { query: " " }, { query: "x", limit: 51 }, { query: "x", cursor: "x".repeat(8193) }, { query: "x".repeat(257) }, { query: "中".repeat(100) }]) await assert.rejects(value.run("wechat_search", args));
  assert.equal(value.calls.length, 0);
});
test("history tool registration uses contextual host authority and advertises all names", () => {
  let registration: any[] = []; registerNativeHistoryTools({ registerTool: (...args: any[]) => { registration = args; } } as any);
  assert.equal(registration[0].contextVersion, 2); assert.deepEqual(registration[1].names, ["wechat_chats", "wechat_history", "wechat_search", "wechat_message"]);
  const manifest = JSON.parse(readFileSync(new URL("../openclaw.plugin.json", import.meta.url), "utf8"));
  for (const name of registration[1].names) assert.ok(manifest.contracts.tools.includes(name), `${name} must be declared for Gateway tool discovery`);
});
