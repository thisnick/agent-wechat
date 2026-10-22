import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { NativeApiError } from "@agent-wechat/shared";
import { createNativeHistoryTools, createNativeArchiveTools, registerNativeHistoryTools } from "./native-history-tools.ts";

const toolNames = ["wechat_history", "wechat_search", "wechat_message", "wechat_media"];
const archiveNames = ["wechat_archive_threads", "wechat_archive_search", "wechat_archive_history", "wechat_archive_message", "wechat_archive_media"];
function fixture(context: Record<string, unknown> = {}, directory?: string, archive = false) {
  let cfg: any = { channels: { "agent-wechat": { dmPolicy: "allowlist", allowFrom: ["u1"], groupPolicy: "allowlist", groupAllowFrom: ["u1"],
    native: { mode: "external", endpoint: "http://127.0.0.1:6174", accountId: "a", token: "synthetic-native-token-".repeat(3), consumerStateDir: "/private/fixture" } } } };
  const calls: any[] = [];
  let messages: any[] = [{ id: "stored-before-buffer", accountId: "a", threadId: "room@chatroom", senderId: "u1", direction: "incoming", type: 1, body: "older searchable text", serverId: "9007199254740999" }];
  let assets: any[] = [], content: Buffer = Buffer.from("Synthetic attachment"), contentType = "text/plain", guards = 0;
  let threads: any[] = [{ id: "private-peer", accountId: "a", peerId: "private-peer", kind: "direct", displayName: "Private peer", unreadCount: 3 }];
  const client: any = {
    threads: async (...args: any[]) => { calls.push(["threads", ...args]); return { items: threads, nextCursor: "thread-continuation", sourceGeneration: "g", coverage: { history: "partial" } }; },
    searchMessages: async (...args: any[]) => { calls.push(["searchMessages", ...args]); return { items: messages, nextCursor: "archive-continuation", sourceGeneration: "g", coverage: { history: "partial" } }; },
    messages: async (...args: any[]) => { calls.push(["messages", ...args]); return { items: messages, nextCursor: "opaque-continuation", sourceGeneration: "g", coverage: { history: "partial" } }; },
    message: async (...args: any[]) => { calls.push(["message", ...args]); return messages[0]; },
    mediaAssets: async (...args: any[]) => { calls.push(["mediaAssets", ...args]); return { items: assets }; },
    transferMedia: async (...args: any[]) => { calls.push(["transferMedia", ...args]); return { id: "download-job", accountId: "a", kind: "download", state: "queued", retryable: true }; },
    job: async (...args: any[]) => { calls.push(["job", args[0], args[1]]); return { id: "download-job", accountId: "a", kind: "download", state: "queued", retryable: true }; },
    mediaContent: async (...args: any[]) => { calls.push(["mediaContent", ...args]); return { bytes: content, contentType }; },
  };
  const core = { channel: { pairing: { readAllowFromStore: async () => [] }, media: { saveMediaBuffer: async (bytes: Buffer, mime: string, _subdir: string, _max: number, filename: string) => {
    assert.ok(directory, "Only media tests may save a file"); const path = join(directory, filename); calls.push(["saveMedia", mime]); await writeFile(path, bytes); return { path };
  } } } };
  const toolContext: any = { getRuntimeConfig: () => cfg, messageChannel: "agent-wechat", nativeChannelId: "room@chatroom", requesterSenderId: "u1", assertInvocationCurrent: () => { guards++; }, ...context };
  const tools = (archive ? createNativeArchiveTools : createNativeHistoryTools)(toolContext, { host: async () => ({ client }) as any, core });
  const run = async (name: string, input: any, signal?: AbortSignal) => { const tool = tools.find(tool => tool.name === name); assert.ok(tool); return tool.execute("tool-call", input, signal); };
  return { tools, calls, client, run, toolContext, guards: () => guards, config: () => cfg, setConfig: (value: any) => { cfg = value; }, setMessages: (value: any[]) => { messages = value; },
    setThreads: (value: any[]) => { threads = value; }, setAssets: (value: any[]) => { assets = value; }, setContent: (bytes: Buffer, mime: string) => { content = bytes; contentType = mime; } };
}
test("history/search query only the current profile/thread, preserving cursors and string IDs", async () => {
  const value = fixture();
  const result: any = await value.run("wechat_search", { query: "searchable", limit: 7, cursor: "previous-cursor" });
  assert.deepEqual(value.calls, [["messages", "a", "room@chatroom", { limit: 7, cursor: "previous-cursor", query: "searchable" }]]);
  assert.equal(result.details.items[0].id, "stored-before-buffer"); assert.equal(result.details.items[0].serverId, "9007199254740999");
  assert.equal(result.details.nextCursor, "opaque-continuation"); assert.equal(result.details.coverage.history, "partial"); assert.ok(value.guards() >= 3);
});
test("owners and non-owners in groups and DMs cannot choose another thread or account", async () => {
  for (const senderIsOwner of [false, true]) for (const conversation of ["room@chatroom", "u1"]) {
    const value = fixture({ senderIsOwner, nativeChannelId: conversation });
    value.setMessages([{ id: "current", accountId: "a", threadId: conversation, senderId: "u1", direction: "incoming", type: 1, body: "current conversation" }]);
    for (const name of toolNames) for (const selector of [{ threadId: "private-peer" }, { threadId: conversation }, { accountId: "other" }]) {
      const input = name === "wechat_search" ? { query: "secret" } : name === "wechat_message" || name === "wechat_media" ? { messageId: "private-message" } : {};
      await assert.rejects(value.run(name, { ...input, ...selector }), /current conversation/);
    }
    assert.equal(value.calls.length, 0);
    const result: any = await value.run("wechat_history", {}); assert.equal(result.details.items[0].threadId, conversation);
    assert.deepEqual(value.calls[0], ["messages", "a", conversation, { limit: 20, cursor: undefined }]);
    assert.equal(value.tools.some(tool => tool.name === "wechat_chats"), false);
  }
});
test("missing or conflicting trusted context hides every lookup tool, including for owners", () => {
  for (const senderIsOwner of [false, true]) for (const context of [
    { messageChannel: "webchat" }, { messageChannel: undefined }, { nativeChannelId: undefined },
    { nativeChannelId: "wechat:" }, { requesterSenderId: undefined }, { requesterSenderId: " " },
    { agentAccountId: "other" }, { deliveryContext: { channel: "agent-wechat", to: "room@chatroom", accountId: "other" } },
    { nativeChannelId: undefined, deliveryContext: { channel: "whatsapp", to: "room@chatroom" } },
    { deliveryContext: { channel: "agent-wechat", to: "wechat:other@chatroom" } },
  ]) assert.equal(fixture({ ...context, senderIsOwner }).tools.length, 0, JSON.stringify(context));
});
test("trusted delivery context scopes released hosts that omit NativeChannelId", async () => {
  const value = fixture({ nativeChannelId: undefined, deliveryContext: { channel: "agent-wechat", to: "wechat:group:room@chatroom" }, senderIsOwner: true });
  await value.run("wechat_history", {}); assert.equal(value.calls[0][2], "room@chatroom");
  assert.equal(fixture({ nativeChannelId: "agent-wechat:room@chatroom", deliveryContext: { channel: "agent-wechat", to: "wechat:group:room@chatroom" } }).tools.length, 4);
});
test("owner status never bypasses current requester or historical author policy", async () => {
  for (const senderIsOwner of [false, true]) for (const conversation of ["room@chatroom", "u1"]) {
    const value = fixture({ senderIsOwner, nativeChannelId: conversation });
    const blocked = { id: "blocked", accountId: "a", threadId: conversation, senderId: "blocked", type: 1, direction: "incoming", body: "must not leak" };
    value.setMessages([blocked, { ...blocked, id: "sent", direction: "outgoing" }]);
    const result: any = await value.run("wechat_history", {}); assert.deepEqual(result.details.items.map((message: any) => message.id), ["sent"]); assert.equal(result.details.nextCursor, "opaque-continuation");
    value.setMessages([blocked]);
    for (const name of ["wechat_message", "wechat_media"]) await assert.rejects(value.run(name, { messageId: "blocked" }), /outside the permitted/);
    assert.ok(value.calls.every(call => ["messages", "message"].includes(call[0])));
    value.config().channels["agent-wechat"][conversation.endsWith("@chatroom") ? "groupAllowFrom" : "allowFrom"] = ["different"];
    const before = value.calls.length; await assert.rejects(value.run("wechat_history", {}), /no longer allowed/); assert.equal(value.calls.length, before);
  }
});
test("account replacement, aborted calls and stale invocation fail before retrieving data", async () => {
  const value = fixture(); const abort = new AbortController(); abort.abort();
  await assert.rejects(value.run("wechat_media", { messageId: "stored-before-buffer" }, abort.signal)); assert.equal(value.calls.length, 0);
  value.toolContext.assertInvocationCurrent = () => { throw Error("stale turn"); };
  await assert.rejects(value.run("wechat_history", {}), /stale turn/);
  value.toolContext.assertInvocationCurrent = () => {};
  value.config().channels["agent-wechat"].native.accountId = "different";
  await assert.rejects(value.run("wechat_history", {}), /configuration changed/);
});
test("daemon replacement during a media lookup cannot return old data into a newly configured account scope", async t => {
  const value = await mediaFixture(t);
  value.client.message = async () => {
    value.config().channels["agent-wechat"].native.endpoint = "http://127.0.0.1:6175";
    return { id: "media-message", accountId: "a", threadId: "room@chatroom", senderId: "u1", direction: "incoming", type: 3 };
  };
  await assert.rejects(value.run("wechat_media", { messageId: "media-message" }), /configuration changed/);
  assert.equal(value.calls.some(call => ["mediaAssets", "mediaContent", "transferMedia", "saveMedia"].includes(call[0])), false);
});
test("foreign history/search responses and exact message IDs cannot cross account/thread boundaries", async () => {
  for (const senderIsOwner of [false, true]) for (const foreign of [{ threadId: "elsewhere" }, { accountId: "other" }]) {
    const value = fixture({ senderIsOwner });
    value.setMessages([{ id: "foreign", accountId: "a", threadId: "room@chatroom", senderId: "u1", type: 1, body: "hidden", direction: "outgoing", ...foreign }]);
    for (const name of ["wechat_history", "wechat_search"]) await assert.rejects(value.run(name, name === "wechat_search" ? { query: "hidden" } : {}), /conversation boundary/);
    for (const name of ["wechat_message", "wechat_media"]) await assert.rejects(value.run(name, { messageId: "foreign" }), /outside the permitted/);
    assert.ok(value.calls.every(call => ["messages", "message"].includes(call[0])));
  }
  const value = fixture(); await assert.rejects(value.run("wechat_message", { messageId: "wrong-id" }), /outside the permitted/);
});
test("search describes attachments and points to retrieval without returning transport XML or fetching media", async () => {
  const value = fixture();
  value.setMessages([{ id: "file", accountId: "a", threadId: "room@chatroom", senderId: "u1", type: 49, body: "<private signed CDN XML>", media: [{ contentUrl: "private-url" }], direction: "incoming" }]);
  const result: any = await value.run("wechat_search", { query: "file" });
  assert.deepEqual(result.details.items[0].attachment, { kind: "file", retrievalTool: "wechat_media", messageId: "file" });
  assert.match(result.details.items[0].body, /file message/); assert.ok(!JSON.stringify(result).includes("XML")); assert.ok(!JSON.stringify(result).includes("private-url"));
  assert.equal(value.calls.length, 1); assert.equal(value.calls[0][0], "messages");
});
test("link metadata and text survive history/search/exact lookup with preview retrieval hints", async () => {
  for (const archive of [false, true]) for (const subtype of [3, 4, 5]) {
    const f = fixture({}, undefined, archive), prefix = archive ? "wechat_archive_" : "wechat_";
    const link = { title: "Shared title", description: "Shared description", url: "https://example.invalid/article", sourceName: "Publisher" };
    f.setMessages([{ id: "link", accountId: "a", threadId: "room@chatroom", senderId: "u1", direction: "incoming", type: 49, appMessageType: subtype,
      body: "[Link] Shared title\nShared description\nhttps://example.invalid/article", link }]);
    for (const [name, input] of [["history", archive ? { threadId: "room@chatroom" } : {}], ["search", { query: "Shared title" }], ["message", { messageId: "link" }]] as const) {
      const result: any = await f.run(`${prefix}${name}`, input);
      const item = name === "message" ? result.details : result.details.items[0];
      assert.deepEqual(item.link, link); assert.match(item.body, /Shared title\nShared description\nhttps:/);
      assert.deepEqual(item.attachment, { kind: "link_preview", retrievalTool: `${prefix}media`, messageId: "link" });
    }
    assert.ok(f.calls.every(call => ["message", "messages", "searchMessages"].includes(call[0])));
  }
});
test("tool bounds and unsupported arguments are checked before API requests", async () => {
  const value = fixture();
  for (const args of [{}, { query: " " }, { query: "x", limit: 51 }, { query: "x", cursor: "x".repeat(8193) }, { query: "x".repeat(257) }, { query: "中".repeat(100) }, { query: "x", download: true }]) await assert.rejects(value.run("wechat_search", args));
  for (const args of [{}, { messageId: " " }, { messageId: "m", download: "yes" }, { messageId: "m", quality: "full" }, { messageId: "m", assetId: "foreign-asset" }, { messageId: "m", path: "/private/file" }, { messageId: "m", jobId: "foreign-job" }]) await assert.rejects(value.run("wechat_media", args));
  assert.equal(value.calls.length, 0);
});
test("registration separates default current-thread tools from optional account-wide tools", () => {
  const registrations: any[] = []; registerNativeHistoryTools({ registerTool: (...args: any[]) => { registrations.push(args); } } as any);
  assert.equal(registrations.length, 2);
  assert.equal(registrations[0][0].contextVersion, 2); assert.deepEqual(registrations[0][1], { names: toolNames });
  assert.equal(registrations[1][0].contextVersion, 2); assert.deepEqual(registrations[1][1], { names: archiveNames, optional: true });
  const manifest = JSON.parse(readFileSync(new URL("../openclaw.plugin.json", import.meta.url), "utf8"));
  assert.deepEqual(manifest.contracts.tools, [...toolNames, ...archiveNames, "wechat_recall", "wechat_send_share"]);
  for (const name of archiveNames) assert.equal(manifest.toolMetadata[name].optional, true);
  for (const tool of fixture().tools) {
    const schema = tool.parameters as any; assert.equal(schema.additionalProperties, false);
    for (const key of ["threadId", "accountId", "assetId", "path", "jobId"]) assert.equal(schema.properties[key], undefined);
    assert.equal((tool as any).catalogMode, tool.name === "wechat_media" ? "direct-only" : undefined);
  }
});

async function mediaFixture(t: any, type = 3, context: Record<string, unknown> = {}, archive = false) {
  const directory = await mkdtemp(join(tmpdir(), "wechat-media-tool-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const value = fixture(context, directory, archive);
  const bytes = type === 3 ? await readFile(new URL("../test/fixtures/native-model.png", import.meta.url)) : Buffer.from("Synthetic non-image attachment");
  const mime = type === 3 ? "image/png" : type === 43 ? "video/mp4" : type === 34 ? "audio/wav" : "text/plain";
  const asset = { id: "asset", accountId: "a", messageId: "media-message", kind: type === 3 ? "image" : type === 43 ? "video" : type === 34 ? "voice" : "file",
    variant: type === 3 ? "original" : "normal", actualVariant: type === 3 ? "original" : "normal", state: "available", contentType: mime, byteLength: bytes.length,
    sha256: createHash("sha256").update(bytes).digest("hex"), filename: type === 3 ? "original.png" : "attachment.bin", expiresAt: 1, retryable: false };
  value.setMessages([{ id: "media-message", accountId: "a", threadId: "room@chatroom", senderId: "u1", type, body: "<private signed CDN XML>", direction: "incoming" }]);
  value.setAssets([asset]); value.setContent(bytes, mime);
  return { ...value, asset, bytes };
}
test("archive tools rely on host policy without requiring an owner, agent allowlist or current WeChat route", async () => {
  for (const context of [
    { messageChannel: "webchat", nativeChannelId: undefined, requesterSenderId: undefined },
    { messageChannel: undefined, nativeChannelId: undefined, requesterSenderId: undefined },
    { agentId: "main", senderIsOwner: true }, { agentId: "researcher", senderIsOwner: false, requesterSenderId: "blocked" },
    { nativeChannelId: "current@chatroom", deliveryContext: { channel: "whatsapp", to: "other", accountId: "other" } },
  ]) {
    const value = fixture(context, undefined, true); assert.deepEqual(value.tools.map(tool => tool.name), archiveNames);
    value.config().channels["agent-wechat"].dmPolicy = "disabled";
    value.config().channels["agent-wechat"].groupPolicy = "disabled";
    value.setMessages(["private-peer", "another@chatroom"].map((threadId, i) => ({ id: `m${i}`, accountId: "a", threadId, senderId: "blocked", direction: "incoming", type: 1, body: "searchable" })));
    const result: any = await value.run("wechat_archive_search", { query: "searchable", limit: 2, cursor: "earlier" });
    assert.equal(result.details.items.length, 2); assert.equal(result.details.nextCursor, "archive-continuation");
    assert.deepEqual(value.calls, [["searchMessages", "a", { query: "searchable", limit: 2, cursor: "earlier" }]]);
  }
});
test("archive thread discovery and selected-thread history retain pagination without sender filtering", async () => {
  const value = fixture({}, undefined, true);
  value.setThreads([{ id: "gone", deleted: true, version: "v" }, { id: "private-peer", peerId: "private-peer", accountId: "a", displayName: "Private peer", kind: "direct", unreadCount: 3, extra: "must not be returned" }]);
  const threads: any = await value.run("wechat_archive_threads", { query: "Private", limit: 1, cursor: "previous" });
  assert.deepEqual(value.calls[0], ["threads", "a", { query: "Private", limit: 1, cursor: "previous" }]);
  assert.equal(threads.details.nextCursor, "thread-continuation"); assert.equal(threads.details.items.length, 1); assert.equal(threads.details.items[0].extra, undefined);
  value.setMessages([{ id: "private-file", accountId: "a", threadId: "private-peer", senderId: "blocked", direction: "incoming", type: 49, body: "<private signed transport XML>" }]);
  for (const name of ["wechat_archive_history", "wechat_archive_search"]) {
    const result: any = await value.run(name, { threadId: "wechat:private-peer", limit: 1, cursor: "older", ...(name.endsWith("search") ? { query: "file" } : {}) });
    assert.equal(result.details.items.length, 1); assert.equal(result.details.items[0].threadId, "private-peer");
    assert.equal(result.details.items[0].attachment.retrievalTool, "wechat_archive_media"); assert.ok(!JSON.stringify(result).includes("XML"));
  }
  const exact: any = await value.run("wechat_archive_message", { messageId: "private-file" });
  assert.equal(exact.details.attachment.retrievalTool, "wechat_archive_media");
  assert.ok(value.calls.every(call => ["threads", "messages", "message"].includes(call[0])));
});
test("archive arguments and account/message identities are still validated before returning data", async () => {
  const value = fixture({}, undefined, true);
  for (const [name, args] of [
    ["wechat_archive_search", {}], ["wechat_archive_search", { query: "中".repeat(100) }],
    ["wechat_archive_search", { query: "x", accountId: "other" }], ["wechat_archive_search", { query: "x", limit: 51 }],
    ["wechat_archive_history", {}], ["wechat_archive_history", { threadId: " " }],
    ["wechat_archive_threads", { cursor: "x".repeat(8193) }], ["wechat_archive_threads", { query: " " }],
    ["wechat_archive_media", { messageId: "m", assetId: "a" }], ["wechat_archive_media", { messageId: "m", path: "/etc/passwd" }],
  ] as const) await assert.rejects(value.run(name, args));
  assert.equal(value.calls.length, 0);
  value.setThreads([{ id: "t", accountId: "other" }]); await assert.rejects(value.run("wechat_archive_threads", {}), /account boundary/);
  value.setMessages([{ id: "m", accountId: "other", threadId: "t", direction: "outgoing", type: 1 }]);
  for (const name of ["wechat_archive_search", "wechat_archive_message", "wechat_archive_media"]) await assert.rejects(value.run(name, name.endsWith("search") ? { query: "x" } : { messageId: "m" }));
  value.setMessages([{ id: "m", accountId: "a", threadId: "t", direction: "outgoing", type: 1 }]);
  await assert.rejects(value.run("wechat_archive_history", { threadId: "different" }), /conversation boundary/);
  await assert.rejects(value.run("wechat_archive_message", { messageId: "different" }), /outside the permitted/);
});
test("archive tools fence cancelled/stale invocations and changed or disabled accounts", async () => {
  const value = fixture({}, undefined, true), abort = new AbortController(); abort.abort();
  await assert.rejects(value.run("wechat_archive_search", { query: "x" }, abort.signal));
  value.toolContext.assertInvocationCurrent = () => { throw Error("stale turn"); };
  await assert.rejects(value.run("wechat_archive_threads", {}), /stale turn/);
  value.toolContext.assertInvocationCurrent = () => {};
  value.config().channels["agent-wechat"].native.accountId = "changed";
  await assert.rejects(value.run("wechat_archive_search", { query: "x" }), /configuration changed/);
  assert.equal(value.calls.length, 0);
  value.config().channels["agent-wechat"].enabled = false;
  assert.equal(createNativeArchiveTools(value.toolContext).length, 0);
});
test("archive media returns verified attachments across threads regardless of author admission", async t => {
  for (const type of [3, 49, 34, 43]) {
    const value = await mediaFixture(t, type, { messageChannel: "webchat", nativeChannelId: undefined, requesterSenderId: undefined }, true);
    value.setMessages([{ id: "media-message", accountId: "a", threadId: "private-peer", senderId: "blocked", direction: "incoming", type, body: "<private signed transport XML>" }]);
    const result: any = await value.run("wechat_archive_media", { messageId: "media-message", download: false });
    assert.equal(result.details.status, "ready"); assert.equal(result.details.threadId, "private-peer");
    assert.deepEqual(await readFile(result.details.attachment.path), value.bytes);
    assert.equal(result.content.some((part: any) => part.type === "image"), type === 3);
    assert.equal((value.tools.find(tool => tool.name === "wechat_archive_media") as any).catalogMode, "direct-only");
    assert.equal(value.calls.some(call => call[0] === "transferMedia"), false);
  }
});
test("archive download keeps media identity checks and terminal transfer outcomes", async t => {
  const value = await mediaFixture(t, 3, {}, true);
  value.setMessages([{ id: "media-message", accountId: "a", threadId: "private-peer", senderId: "blocked", direction: "incoming", type: 3 }]);
  value.setAssets([{ ...value.asset, state: "pending" }]);
  value.client.transferMedia = async (...args: any[]) => { value.calls.push(["transferMedia", ...args]); value.setAssets([value.asset]); return { id: "job", state: "available", retryable: false }; };
  const ready: any = await value.run("wechat_archive_media", { messageId: "media-message", quality: "original" }); assert.equal(ready.details.status, "ready");
  assert.deepEqual(value.calls.find(call => call[0] === "transferMedia"), ["transferMedia", "a", { messageId: "media-message", assetId: null, quality: "original" }]);
  value.setAssets([{ ...value.asset, state: "pending", jobId: "job" }]);
  value.client.job = async () => ({ id: "job", accountId: "a", assetId: "asset", kind: "download", state: "failed", retryable: false, error: { code: "media_service_rejected" } });
  const before = value.calls.filter(call => call[0] === "transferMedia").length;
  const failed: any = await value.run("wechat_archive_media", { messageId: "media-message" });
  assert.equal(failed.details.retryable, false); assert.equal(value.calls.filter(call => call[0] === "transferMedia").length, before);
  value.setAssets([{ ...value.asset, accountId: "other" }]); await assert.rejects(value.run("wechat_archive_media", { messageId: "media-message" }), /identity|account|mismatch/);
});
test("cached image retrieval returns original path and verified vision bytes even after CDN expiry", async t => {
  const value = await mediaFixture(t);
  const result: any = await value.run("wechat_media", { messageId: "media-message" });
  assert.equal(result.details.status, "ready"); assert.equal(result.details.variant, "original"); assert.equal(result.details.attachment.quality, "full");
  assert.deepEqual(await readFile(result.details.attachment.path), value.bytes);
  const image = result.content.find((item: any) => item.type === "image"); assert.ok(image); assert.equal(image.mimeType, "image/png");
  assert.equal(createHash("sha256").update(Buffer.from(image.data, "base64")).digest("hex"), value.asset.sha256);
  assert.equal(value.calls.some(call => call[0] === "transferMedia"), false); assert.ok(!JSON.stringify(result).includes("XML"));
});
test("cached files, voice and videos return verified paths without automatically adding image bytes", async t => {
  for (const type of [49, 34, 43]) {
    const value = await mediaFixture(t, type); const result: any = await value.run("wechat_media", { messageId: "media-message" });
    assert.equal(result.details.status, "ready"); assert.deepEqual(await readFile(result.details.attachment.path), value.bytes);
    assert.equal(result.content.some((item: any) => item.type === "image"), false); assert.equal(value.calls.some(call => call[0] === "transferMedia"), false);
  }
});
test("missing media requests the authorized message's durable transfer and returns the downloaded bytes", async t => {
  const value = await mediaFixture(t); value.setAssets([{ ...value.asset, state: "pending" }]);
  value.client.transferMedia = async (...args: any[]) => { value.calls.push(["transferMedia", ...args]); value.setAssets([value.asset]); return { id: "download-job", state: "available", retryable: false }; };
  const result: any = await value.run("wechat_media", { messageId: "media-message", quality: "original" });
  assert.equal(result.details.status, "ready"); assert.equal(result.details.jobId, "download-job");
  assert.deepEqual(value.calls.find(call => call[0] === "transferMedia"), ["transferMedia", "a", { messageId: "media-message", assetId: null, quality: "original" }]);
  assert.deepEqual(await readFile(result.details.attachment.path), value.bytes);
});
test("queued media returns a pending job without claiming a file or image was retrieved", async t => {
  const value = await mediaFixture(t); value.setAssets([{ ...value.asset, state: "pending" }]);
  const result: any = await value.run("wechat_media", { messageId: "media-message" });
  assert.equal(result.details.status, "pending"); assert.equal(result.details.jobId, "download-job"); assert.equal(result.details.retryable, true);
  assert.equal(result.details.attachment?.path, undefined); assert.equal(result.content.some((item: any) => item.type === "image"), false);
  assert.equal(value.calls.filter(call => call[0] === "transferMedia").length, 1); assert.equal(value.calls.some(call => call[0] === "mediaContent"), false);
});
test("cache-only and strict quality never download or silently substitute a lower quality", async t => {
  const value = await mediaFixture(t); value.setAssets([{ ...value.asset, variant: "thumbnail", actualVariant: "thumbnail" }]);
  const result: any = await value.run("wechat_media", { messageId: "media-message", quality: "original", download: false });
  assert.equal(result.details.status, "unavailable"); assert.equal(result.details.retryable, true); assert.equal(result.details.attachment?.path, undefined);
  assert.ok(value.calls.every(call => ["message", "message", "mediaAssets"].includes(call[0])));
});
test("expired/unsupported media never requests a transfer; terminal failures remain explicit", async t => {
  for (const state of ["expired", "unsupported"]) {
    const value = await mediaFixture(t); value.setAssets([{ ...value.asset, state }]);
    const result: any = await value.run("wechat_media", { messageId: "media-message" });
    assert.equal(result.details.status, state); assert.equal(result.details.retryable, false); assert.equal(result.details.attachment?.path, undefined);
    assert.ok(value.calls.every(call => ["message", "message", "mediaAssets"].includes(call[0])));
  }
  const value = await mediaFixture(t); value.setAssets([{ ...value.asset, state: "pending" }]); value.client.transferMedia = async () => { throw new NativeApiError(400, "unsupported_media_variant", false); };
  const result: any = await value.run("wechat_media", { messageId: "media-message" }); assert.equal(result.details.status, "unavailable"); assert.equal(result.details.retryable, false); assert.equal(result.details.reason, "unsupported_media_variant");
});
test("foreign assets, oversized bytes and corrupt hashes never become local files", async t => {
  for (const override of [{ accountId: "other" }, { messageId: "other" }, { sha256: "0".repeat(64) }, { byteLength: 129 * 1024 * 1024 }, { byteLength: 129 * 1024 * 1024, state: "pending" }]) {
    const value = await mediaFixture(t); value.setAssets([{ ...value.asset, ...override }]);
    if (override.byteLength) { const result: any = await value.run("wechat_media", { messageId: "media-message" }); assert.equal(result.details.status, "too_large"); }
    else await assert.rejects(value.run("wechat_media", { messageId: "media-message" }), /identity|integrity/);
    assert.equal(value.calls.some(call => call[0] === "saveMedia"), false);
    if (override.byteLength) assert.equal(value.calls.some(call => call[0] === "transferMedia" || call[0] === "mediaContent"), false);
  }
});
test("revoked or aborted turns stop before attachment access, including owners", async t => {
  for (const action of ["abort", "policy", "account"]) {
    const value = await mediaFixture(t, 3, { senderIsOwner: true }); const abort = new AbortController(); const lookup = value.client.message;
    value.client.message = async (...args: any[]) => { const result = await lookup(...args);
      if (action === "abort") abort.abort();
      else if (action === "policy") value.config().channels["agent-wechat"].groupAllowFrom = ["other"];
      else value.config().channels["agent-wechat"].native.accountId = "other";
      return result;
    };
    await assert.rejects(value.run("wechat_media", { messageId: "media-message" }, abort.signal));
    assert.deepEqual(value.calls.map(call => call[0]), ["message"]);
  }
});
test("abort during transfer submission stops local work while leaving the durable download alone", async t => {
  const value = await mediaFixture(t); const abort = new AbortController(); value.setAssets([{ ...value.asset, state: "pending" }]);
  value.client.transferMedia = async (...args: any[]) => { value.calls.push(["transferMedia", ...args]); abort.abort(); return { id: "durable-job", state: "queued" }; };
  await assert.rejects(value.run("wechat_media", { messageId: "media-message" }, abort.signal));
  assert.deepEqual(value.calls.map(call => call[0]), ["message", "message", "mediaAssets", "message", "transferMedia"]);
});
test("an expired thumbnail does not prevent downloading an advertised original", async t => {
  const value = await mediaFixture(t); value.setAssets([{ ...value.asset, state: "pending" }, { ...value.asset, id: "thumb", variant: "thumbnail", actualVariant: "thumbnail", state: "expired" }]);
  value.client.transferMedia = async (...args: any[]) => { value.calls.push(["transferMedia", ...args]); value.setAssets([value.asset]); return { id: "original-job", state: "available" }; };
  const result: any = await value.run("wechat_media", { messageId: "media-message" }); assert.equal(result.details.status, "ready"); assert.equal(result.details.variant, "original");
});
test("sender policy revoked during asset lookup prevents download submission and local saving", async t => {
  const value = await mediaFixture(t, 3, { senderIsOwner: true }); value.setAssets([{ ...value.asset, state: "pending" }]);
  const assets = value.client.mediaAssets;
  value.client.mediaAssets = async (...args: any[]) => { const result = await assets(...args); value.config().channels["agent-wechat"].groupAllowFrom = ["other"]; return result; };
  await assert.rejects(value.run("wechat_media", { messageId: "media-message" }), /no longer allowed/);
  assert.deepEqual(value.calls.map(call => call[0]), ["message", "message", "mediaAssets"]);
});
test("unsupported historical media and transient catalog errors return bounded statuses without raw details", async t => {
  for (const [code, retryable, status] of [["unsupported_profile", false, "unsupported"], ["unavailable", true, "pending"]] as const) {
    const value = await mediaFixture(t); value.client.mediaAssets = async () => { throw new NativeApiError(retryable ? 503 : 409, code, retryable); };
    const result: any = await value.run("wechat_media", { messageId: "media-message", download: false });
    assert.equal(result.details.status, status); assert.equal(result.details.retryable, retryable); assert.equal(result.details.reason, code);
    assert.equal(result.details.attachment, undefined); assert.equal(value.calls.some(call => ["transferMedia", "mediaContent", "saveMedia"].includes(call[0])), false);
  }
});
test("existing download jobs are polled and terminal failures never resubmit, even in download mode", async t => {
  for (const download of [false, true]) for (const state of ["queued", "failed"]) {
    const value = await mediaFixture(t); value.setAssets([{ ...value.asset, state: "pending", jobId: "existing-job" }]);
    value.client.job = async (...args: any[]) => { value.calls.push(["job", args[0], args[1]]); return { id: "existing-job", accountId: "a", assetId: "asset", kind: "download", state, retryable: false, reason: state === "failed" ? "media_service_rejected" : null }; };
    const result: any = await value.run("wechat_media", { messageId: "media-message", download });
    assert.equal(result.details.jobId, "existing-job"); assert.equal(result.details.status, state === "queued" ? "pending" : "unavailable");
    assert.equal(result.details.retryable, state === "queued");
    if (state === "failed") assert.equal(result.details.reason, "media_service_rejected");
    assert.equal(value.calls.some(call => call[0] === "transferMedia" || call[0] === "mediaContent"), false);
  }
});

test("quote recall advertises original retrieval and enforces original sender admission", async () => {
  const value=fixture();
  const parent={id:"quote",accountId:"a",threadId:"room@chatroom",senderId:"u1",direction:"incoming",type:49,appMessageType:57,body:"Please explain",quote:{serverId:"42",type:1}};
  value.setMessages([parent]);
  value.client.quotedMessage=async()=>({accountId:"a",messageId:"quote",threadId:"room@chatroom",state:"resolved",message:{id:"original",accountId:"a",threadId:"room@chatroom",serverId:"42",senderId:"blocked",direction:"incoming",type:1,body:"blocked text"}});
  const history:any=await value.run("wechat_history",{});
  assert.equal(history.details.items[0].quote.serverId,"42"); assert.equal(history.details.items[0].attachment.retrievalTool,"wechat_media");
  const media:any=await value.run("wechat_media",{messageId:"quote"});
  assert.match(media.details.body,/not permitted/); assert.ok(!JSON.stringify(media).includes("blocked text"));
  assert.ok(value.calls.every(call=>["messages","message"].includes(call[0])));
  value.client.quotedMessage=async()=>({accountId:"a",messageId:"quote",threadId:"room@chatroom",state:"embedded",message:null,senderId:"blocked",embeddedText:"blocked embedded text"});
  const embedded:any=await value.run("wechat_media",{messageId:"quote"});
  assert.match(embedded.details.body,/not permitted/);assert.ok(!JSON.stringify(embedded).includes("blocked embedded text"));
});
test("archive quote recall keeps account/chat identity while allowing cross-author originals", async () => {
  const value=fixture({},undefined,true);
  value.setMessages([{id:"quote",accountId:"a",threadId:"private-peer",senderId:"blocked",direction:"incoming",type:49,appMessageType:57,body:"Comment",quote:{serverId:"42",type:1}}]);
  value.client.quotedMessage=async()=>({accountId:"a",messageId:"quote",threadId:"private-peer",state:"resolved",message:{id:"original",accountId:"a",threadId:"private-peer",serverId:"42",senderId:"other",direction:"incoming",type:1,body:"Quoted stored text"}});
  const result:any=await value.run("wechat_archive_media",{messageId:"quote"});
  assert.match(result.details.body,/Quoted stored text/); assert.equal(result.details.status,"resolved");
  value.client.quotedMessage=async()=>({accountId:"a",messageId:"quote",threadId:"foreign",state:"embedded",message:null,embeddedText:"foreign"});
  await assert.rejects(value.run("wechat_archive_media",{messageId:"quote"}),/identity/);
});

test("recalled history and cached attachments expose a terminal placeholder without paths or bytes", async t => {
  for (const archive of [false, true]) {
    const value = await mediaFixture(t, 3, {}, archive);
    value.setMessages([{ id: "media-message", accountId: "a", threadId: "room@chatroom", senderId: "u1", direction: "incoming", type: 3,
      body: "Original content must not escape", recall: { observedAt: "2026-10-21T12:00:00Z" } }]);
    const history: any = await value.run(archive ? "wechat_archive_history" : "wechat_history", archive ? { threadId: "room@chatroom" } : {});
    assert.equal(history.details.items[0].status, "recalled"); assert.equal(history.details.items[0].body, "[Message recalled]");
    assert.equal(history.details.items[0].attachment, undefined);
    const result: any = await value.run(archive ? "wechat_archive_media" : "wechat_media", { messageId: "media-message" });
    assert.equal(result.details.status, "recalled"); assert.equal(result.details.retryable, false);
    assert.equal(result.details.attachment, undefined); assert.equal(result.content.some((part: any) => part.type === "image"), false);
    assert.ok(!JSON.stringify(result).includes("Original content"));
    assert.equal(value.calls.some(call => ["mediaAssets", "transferMedia", "mediaContent", "saveMedia"].includes(call[0])), false);
  }
});
test("recall arriving during cached content retrieval prevents saving or returning the late image", async t => {
  const value = await mediaFixture(t);
  let contentStarted!: () => void, releaseContent!: () => void;
  const started = new Promise<void>(resolve => { contentStarted = resolve; });
  const released = new Promise<void>(resolve => { releaseContent = resolve; });
  value.client.mediaContent = async () => { value.calls.push(["mediaContent"]); contentStarted(); await released; return { bytes: value.bytes, contentType: "image/png" }; };
  const work = value.run("wechat_media", { messageId: "media-message" }); await started;
  value.setMessages([{ id: "media-message", accountId: "a", threadId: "room@chatroom", senderId: "u1", direction: "incoming", type: 10000,
    body: "[Message recalled]", recall: { observedAt: "2026-10-21T12:00:00Z" } }]);
  releaseContent(); const result: any = await work;
  assert.equal(result.details.status, "recalled"); assert.equal(result.details.retryable, false);
  assert.equal(result.details.attachment, undefined); assert.equal(result.content.length, 1);
  assert.equal(value.calls.some(call => call[0] === "saveMedia" || call[0] === "transferMedia"), false);
});
test("a recall after managed saving removes all attachment output while retained bytes follow host policy", async t => {
  const value = await mediaFixture(t); const lookup = value.client.message;
  value.client.message = async (...args: any[]) => {
    const message = await lookup(...args);
    return value.calls.some(call => call[0] === "saveMedia") ? { ...message, recall: { observedAt: "2026-10-21T12:00:00Z" } } : message;
  };
  const result: any = await value.run("wechat_media", { messageId: "media-message" });
  assert.equal(result.details.status, "recalled"); assert.equal(result.details.retryable, false);
  assert.equal(value.calls.filter(call => call[0] === "saveMedia").length, 1);
  assert.equal(result.content.some((part: any) => part.type === "image"), false);
  assert.equal(result.details.attachment, undefined); assert.equal(result.details.body, undefined);
  assert.ok(!JSON.stringify(result).includes("original.png"));
});
test("terminal recall responses and recalled media catalogs never request another transfer", async t => {
  for (const source of ["catalog", "response"]) {
    const value = await mediaFixture(t);
    if (source === "catalog") value.setAssets([{ ...value.asset, state: "recalled" }]);
    else value.client.mediaAssets = async () => { throw new NativeApiError(410, "message_recalled", false); };
    const result: any = await value.run("wechat_media", { messageId: "media-message" });
    assert.equal(result.details.status, "recalled"); assert.equal(result.details.retryable, false);
    assert.equal(result.details.reason, "message_recalled"); assert.equal(result.details.attachment, undefined);
    assert.equal(value.calls.some(call => ["transferMedia", "mediaContent", "saveMedia"].includes(call[0])), false);
  }
});
test("history media identity failures remain terminal unavailable without stale attachment paths", async t => {
  for (const code of ["media_identity_unavailable", "media_source_changed"]) for (const stage of ["catalog", "content", "transfer"]) {
    const value = await mediaFixture(t);
    if (stage === "catalog") value.client.mediaAssets = async () => { throw new NativeApiError(409, code, false); };
    if (stage === "content") value.client.mediaContent = async () => { throw new NativeApiError(409, code, false); };
    if (stage === "transfer") { value.setAssets([{ ...value.asset, state: "pending" }]); value.client.transferMedia = async () => { value.calls.push(["transferMedia"]); throw new NativeApiError(409, code, false); }; }
    const result: any = await value.run("wechat_media", { messageId: "media-message" });
    assert.equal(result.details.status, "unavailable"); assert.equal(result.details.retryable, false); assert.equal(result.details.reason, code);
    assert.equal(result.details.attachment?.path, undefined); assert.equal(result.content.some((part: any) => part.type === "image"), false);
    assert.equal(value.calls.some(call => call[0] === "saveMedia"), false);
    assert.equal(value.calls.filter(call => call[0] === "transferMedia").length, stage === "transfer" ? 1 : 0);
  }
});
