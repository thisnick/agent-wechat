import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { NativeApiError } from "@agent-wechat/shared";
import { createNativeHistoryTools, registerNativeHistoryTools } from "./native-history-tools.ts";

const toolNames = ["wechat_history", "wechat_search", "wechat_message", "wechat_media"];
function fixture(context: Record<string, unknown> = {}, directory?: string) {
  let cfg: any = { channels: { "agent-wechat": { dmPolicy: "allowlist", allowFrom: ["u1"], groupPolicy: "allowlist", groupAllowFrom: ["u1"],
    native: { mode: "external", endpoint: "http://127.0.0.1:6174", accountId: "a", token: "synthetic-native-token-".repeat(3), consumerStateDir: "/private/fixture" } } } };
  const calls: any[] = [];
  let messages: any[] = [{ id: "stored-before-buffer", accountId: "a", threadId: "room@chatroom", senderId: "u1", direction: "incoming", type: 1, body: "older searchable text", serverId: "9007199254740999" }];
  let assets: any[] = [], content: Buffer = Buffer.from("Synthetic attachment"), contentType = "text/plain", guards = 0;
  const client: any = {
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
  const tools = createNativeHistoryTools(toolContext, { host: async () => ({ client }) as any, core });
  const run = async (name: string, input: any, signal?: AbortSignal) => { const tool = tools.find(tool => tool.name === name); assert.ok(tool); return tool.execute("tool-call", input, signal); };
  return { tools, calls, client, run, toolContext, guards: () => guards, config: () => cfg, setConfig: (value: any) => { cfg = value; }, setMessages: (value: any[]) => { messages = value; },
    setAssets: (value: any[]) => { assets = value; }, setContent: (bytes: Buffer, mime: string) => { content = bytes; contentType = mime; } };
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
test("tool bounds and unsupported arguments are checked before API requests", async () => {
  const value = fixture();
  for (const args of [{}, { query: " " }, { query: "x", limit: 51 }, { query: "x", cursor: "x".repeat(8193) }, { query: "x".repeat(257) }, { query: "中".repeat(100) }, { query: "x", download: true }]) await assert.rejects(value.run("wechat_search", args));
  for (const args of [{}, { messageId: " " }, { messageId: "m", download: "yes" }, { messageId: "m", quality: "full" }, { messageId: "m", assetId: "foreign-asset" }, { messageId: "m", path: "/private/file" }, { messageId: "m", jobId: "foreign-job" }]) await assert.rejects(value.run("wechat_media", args));
  assert.equal(value.calls.length, 0);
});
test("registration and manifest expose only current-thread lookup/retrieval without target selectors", () => {
  let registration: any[] = []; registerNativeHistoryTools({ registerTool: (...args: any[]) => { registration = args; } } as any);
  assert.equal(registration[0].contextVersion, 2); assert.deepEqual(registration[1].names, toolNames);
  const manifest = JSON.parse(readFileSync(new URL("../openclaw.plugin.json", import.meta.url), "utf8"));
  assert.deepEqual(manifest.contracts.tools, toolNames);
  for (const tool of fixture().tools) {
    const schema = tool.parameters as any; assert.equal(schema.additionalProperties, false);
    for (const key of ["threadId", "accountId", "assetId", "path", "jobId"]) assert.equal(schema.properties[key], undefined);
    assert.equal((tool as any).catalogMode, tool.name === "wechat_media" ? "direct-only" : undefined);
  }
});

async function mediaFixture(t: any, type = 3, context: Record<string, unknown> = {}) {
  const directory = await mkdtemp(join(tmpdir(), "wechat-media-tool-")); t.after(() => rm(directory, { recursive: true, force: true }));
  const value = fixture(context, directory);
  const bytes = type === 3 ? await readFile(new URL("../test/fixtures/native-model.png", import.meta.url)) : Buffer.from("Synthetic non-image attachment");
  const mime = type === 3 ? "image/png" : type === 43 ? "video/mp4" : type === 34 ? "audio/wav" : "text/plain";
  const asset = { id: "asset", accountId: "a", messageId: "media-message", kind: type === 3 ? "image" : type === 43 ? "video" : type === 34 ? "voice" : "file",
    variant: type === 3 ? "original" : "normal", actualVariant: type === 3 ? "original" : "normal", state: "available", contentType: mime, byteLength: bytes.length,
    sha256: createHash("sha256").update(bytes).digest("hex"), filename: type === 3 ? "original.png" : "attachment.bin", expiresAt: 1, retryable: false };
  value.setMessages([{ id: "media-message", accountId: "a", threadId: "room@chatroom", senderId: "u1", type, body: "<private signed CDN XML>", direction: "incoming" }]);
  value.setAssets([asset]); value.setContent(bytes, mime);
  return { ...value, asset, bytes };
}
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
  assert.ok(value.calls.every(call => ["message", "mediaAssets"].includes(call[0])));
});
test("expired/unsupported media never requests a transfer; terminal failures remain explicit", async t => {
  for (const state of ["expired", "unsupported"]) {
    const value = await mediaFixture(t); value.setAssets([{ ...value.asset, state }]);
    const result: any = await value.run("wechat_media", { messageId: "media-message" });
    assert.equal(result.details.status, state); assert.equal(result.details.retryable, false); assert.equal(result.details.attachment?.path, undefined);
    assert.ok(value.calls.every(call => ["message", "mediaAssets"].includes(call[0])));
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
  assert.deepEqual(value.calls.map(call => call[0]), ["message", "mediaAssets", "transferMedia"]);
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
  assert.deepEqual(value.calls.map(call => call[0]), ["message", "mediaAssets"]);
});
test("unsupported historical media and transient catalog errors return bounded statuses without raw details", async t => {
  for (const [code, retryable, status] of [["unsupported_profile", false, "unsupported"], ["unavailable", true, "pending"]] as const) {
    const value = await mediaFixture(t); value.client.mediaAssets = async () => { throw new NativeApiError(retryable ? 503 : 409, code, retryable); };
    const result: any = await value.run("wechat_media", { messageId: "media-message", download: false });
    assert.equal(result.details.status, status); assert.equal(result.details.retryable, retryable); assert.equal(result.details.reason, code);
    assert.equal(result.details.attachment, undefined); assert.equal(value.calls.some(call => ["transferMedia", "mediaContent", "saveMedia"].includes(call[0])), false);
  }
});
