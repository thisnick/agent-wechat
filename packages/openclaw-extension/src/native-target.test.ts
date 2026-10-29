import test from "node:test";
import assert from "node:assert/strict";
import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { NativeApiError, NativeWeChatClient } from "@agent-wechat/shared";
import { wechatPlugin } from "./channel.ts";
import { createWeChatMessagingAdapter, normalizeWeChatTarget } from "./native-target.ts";

// Exercise the resolver actually shipped by the pinned OpenClaw dependency,
// including its ID classification and directory-name fallback ordering.
const sdkDist = fileURLToPath(new URL("../", import.meta.resolve("openclaw/plugin-sdk/channel-outbound")));
const resolverFile = (await readdir(sdkDist)).find(name => /^target-resolver-.*\.mjs$/.test(name));
assert.ok(resolverFile);
const hostModule = await import(pathToFileURL(join(sdkDist, resolverFile)).href);
const resolveHostTarget = Object.values(hostModule).find(value => typeof value === "function" && value.name === "resolveChannelTarget") as (params: Record<string, unknown>) => Promise<any>;
assert.equal(typeof resolveHostTarget, "function");

const peer = "custom_username";
const nativeAccount = "native-account";
const contact = { id: peer, username: peer, displayName: "Contact name", nickname: "Contact name", remark: null, alias: "changeable_alias", relation: "unknown" };
const thread = { id: peer, accountId: nativeAccount, peerId: peer, kind: "direct", displayName: "Conversation name" };
type Reply = { status?: number; value?: unknown; failure?: Error };
function fixture(options: { contact?: Reply | (() => Reply); thread?: Reply } = {}) {
  const calls: string[] = [];
  const cfg = {};
  const client = new NativeWeChatClient({ baseUrl: "http://127.0.0.1:6174", token: "synthetic-target-token", fetch: async (url, init) => {
    assert.equal(new Headers(init?.headers).get("authorization"), "Bearer synthetic-target-token");
    assert.equal(init?.method ?? "GET", "GET", "Resolution cannot send or mutate a native resource");
    const path = new URL(String(url)).pathname;
    const match = /^\/v1\/accounts\/([^/]+)\/(contacts|threads)\/([^/]+)$/.exec(path);
    assert.ok(match, path); assert.equal(decodeURIComponent(match[1]), nativeAccount);
    calls.push(`${match[2]}:${decodeURIComponent(match[3])}`);
    const result = match[2] === "contacts" ? (typeof options.contact === "function" ? options.contact() : options.contact ?? { value: contact }) : options.thread ?? { value: thread };
    if (result.failure) throw result.failure;
    return Response.json(result.status ? { error: { code: "synthetic_target_error" } } : result.value, { status: result.status ?? 200 });
  } });
  const messaging = createWeChatMessagingAdapter(async (receivedCfg, channelAccount) => {
    assert.equal(receivedCfg, cfg); assert.equal(channelAccount, "channel-account");
    return { client, accountId: nativeAccount };
  });
  const plugin = { ...wechatPlugin, messaging, directory: {
    listPeers: async () => assert.fail("A native ID must not enter fuzzy contact-name lookup"),
    listGroups: async () => assert.fail("A native DM ID must not enter group-name lookup"),
  } };
  return { calls, resolve: (input: string, preferredKind?: string) => resolveHostTarget({ cfg, channel: "agent-wechat", accountId: "channel-account", input, preferredKind, plugin }) };
}

test("explicit non-wxid native peer IDs reach identity validation instead of group-name lookup", () => {
  const resolver = wechatPlugin.messaging!.targetResolver!;
  assert.equal(resolver.looksLikeId!("wechat:custom_username"), true);
  assert.equal(resolver.looksLikeId!("agent-wechat:custom_username"), true);
  assert.equal(typeof resolver.resolveTarget, "function");
});

test("normalization trims channel prefixes without changing native identity", () => {
  assert.equal(normalizeWeChatTarget("  WeChat:custom_username  "), peer);
  assert.equal(normalizeWeChatTarget(" agent-wechat:custom_username "), peer);
  assert.equal(normalizeWeChatTarget(" custom_username "), peer);
  assert.equal(normalizeWeChatTarget("wechat:"), undefined);
});

test("the real OpenClaw resolver accepts canonical non-wxid contacts through exact account-scoped reads", async () => {
  for (const input of [peer, `wechat:${peer}`, `agent-wechat:${peer}`, ` WeChat:${peer} `]) {
    const f = fixture(); const result = await f.resolve(input);
    assert.equal(result.ok, true); assert.equal(result.target.to, peer); assert.equal(result.target.kind, "user");
    assert.equal(result.target.display, contact.displayName);
    assert.deepEqual(f.calls, [`contacts:${peer}`]);
  }
});

test("wxid and group destinations retain their native ID forms without catalog reads", async () => {
  for (const [input, to, kind] of [["wechat:wxid_peer", "wxid_peer", "user"], ["agent-wechat:room@chatroom", "room@chatroom", "group"]]) {
    const f = fixture(); const result = await f.resolve(input);
    assert.equal(result.ok, true); assert.equal(result.target.to, to); assert.equal(result.target.kind, kind);
    assert.deepEqual(f.calls, []);
  }
});

test("only an exact contact 404 permits fallback to an account-bound direct thread", async () => {
  const f = fixture({ contact: { status: 404 } }); const result = await f.resolve(`wechat:${peer}`);
  assert.equal(result.ok, true); assert.equal(result.target.to, peer); assert.equal(result.target.kind, "user");
  assert.equal(result.target.display, thread.displayName);
  assert.deepEqual(f.calls, [`contacts:${peer}`, `threads:${peer}`]);
});

test("authentication, server and transport errors never fall back to threads or guessed destinations", async () => {
  for (const status of [401, 403, 409, 410, 429, 500]) {
    const f = fixture({ contact: { status } });
    await assert.rejects(f.resolve(peer), error => error instanceof NativeApiError && error.status === status);
    assert.deepEqual(f.calls, [`contacts:${peer}`]);
  }
  const failure = new Error("synthetic network error"); const f = fixture({ contact: { failure } });
  await assert.rejects(f.resolve(peer), error => error === failure);
  assert.deepEqual(f.calls, [`contacts:${peer}`]);
});

test("contact aliases, deletion markers, changed usernames and identity mismatches cannot redirect sends", async () => {
  for (const value of [null, [], {}, { ...contact, id: "other_peer" }, { ...contact, username: "renamed_peer" },
    { ...contact, deleted: true }, { ...contact, accountId: "other-account" }]) {
    const f = fixture({ contact: { value } });
    await assert.rejects(f.resolve(peer), /contact identity mismatch/);
    assert.deepEqual(f.calls, [`contacts:${peer}`]);
  }
  for (const input of ["changeable_alias", "Contact name"]) {
    const f = fixture(); await assert.rejects(f.resolve(input), /contact identity mismatch/);
    assert.deepEqual(f.calls, [`contacts:${input}`]);
  }
});

test("thread fallback rejects foreign accounts, different peers, group/system kinds and tombstones", async () => {
  for (const value of [null, [], {}, { ...thread, id: "other_peer" }, { ...thread, peerId: "other_peer" },
    { ...thread, accountId: "other-account" }, { ...thread, kind: "group" }, { ...thread, kind: "system" }, { ...thread, deleted: true }]) {
    const f = fixture({ contact: { status: 404 }, thread: { value } });
    await assert.rejects(f.resolve(peer), /conversation identity mismatch/);
    assert.deepEqual(f.calls, [`contacts:${peer}`, `threads:${peer}`]);
  }
});

test("unknown native IDs remain rejected when neither exact resource exists", async () => {
  const f = fixture({ contact: { status: 404 }, thread: { status: 404 } });
  await assert.rejects(f.resolve(peer), error => error instanceof NativeApiError && error.status === 404);
  assert.deepEqual(f.calls, [`contacts:${peer}`, `threads:${peer}`]);
});

test("changing a mutable alias does not change or cache the canonical destination", async () => {
  let alias = "old_alias";
  const f = fixture({ contact: () => ({ value: { ...contact, alias } }) });
  assert.equal((await f.resolve(peer)).target.to, peer);
  alias = "new_alias";
  assert.equal((await f.resolve(peer)).target.to, peer);
  await assert.rejects(f.resolve("old_alias"), /contact identity mismatch/);
  await assert.rejects(f.resolve("new_alias"), /contact identity mismatch/);
  assert.deepEqual(f.calls, [`contacts:${peer}`, `contacts:${peer}`, "contacts:old_alias", "contacts:new_alias"]);
});

test("a group display name matching the requested native ID cannot override identity validation", async () => {
  // The fixture's directory methods fail if called. Older bindings instead
  // selected listGroups and could resolve this name to a different group ID.
  const f = fixture(); assert.equal((await f.resolve(peer)).target.to, peer);
  const unknown = fixture({ contact: { status: 404 }, thread: { status: 404 } });
  await assert.rejects(unknown.resolve(peer), error => error instanceof NativeApiError && error.status === 404);
});

test("invalid IDs and incompatible target kinds fail before any native lookup", async () => {
  for (const input of ["wechat:", "agent-wechat:", "wechat:.", "wechat:..", "wechat:\u0000peer", "wechat:" + "x".repeat(4097)]) {
    const f = fixture(); await assert.rejects(f.resolve(input), /Invalid WeChat target ID/); assert.deepEqual(f.calls, []);
  }
  for (const [input, kind] of [[peer, "group"], ["wxid_peer", "group"], ["room@chatroom", "user"]]) {
    const f = fixture(); await assert.rejects(f.resolve(input, kind), /target kind/); assert.deepEqual(f.calls, []);
  }
});
