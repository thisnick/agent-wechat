import test from "node:test";
import assert from "node:assert/strict";
import { DEFAULT_MEDIA_MAX_MB, resolveWeChatAccount } from "./types.ts";

function config(mediaMaxMb?: number): Record<string, unknown> {
  return { channels: { "agent-wechat": { serverUrl: "http://localhost:6174", mediaMaxMb } } };
}

test("media size uses a bounded explicit account setting", () => {
  assert.equal(resolveWeChatAccount(config(75))?.mediaMaxMb, 75);
});

test("media size defaults when omitted or invalid", () => {
  assert.equal(resolveWeChatAccount(config())?.mediaMaxMb, DEFAULT_MEDIA_MAX_MB);
  assert.equal(resolveWeChatAccount(config(0))?.mediaMaxMb, DEFAULT_MEDIA_MAX_MB);
  assert.equal(resolveWeChatAccount(config(1.5))?.mediaMaxMb, DEFAULT_MEDIA_MAX_MB);
  assert.equal(resolveWeChatAccount(config(1025))?.mediaMaxMb, DEFAULT_MEDIA_MAX_MB);
});

test("native plugin defaults resolve without a legacy serverUrl and channel remains closed by default", () => {
  const native = { mode: "external", accountId: "native-a", endpoint: "https://daemon.example.net", tokenFile: "/private/token", consumerStateDir: "/private/consumer" };
  const account = resolveWeChatAccount({ plugins: { entries: { "agent-wechat": { config: { backend: "native", native } } } } });
  assert.equal(account?.backend, "native"); assert.equal(account?.native?.accountId, "native-a");
  assert.equal(account?.serverUrl, "https://daemon.example.net"); assert.equal(account?.dmPolicy, "disabled");
});
test("legacy remains the default and native does not inherit a legacy endpoint", () => {
  assert.equal(resolveWeChatAccount(config())?.backend, "legacy");
  assert.equal(resolveWeChatAccount({ channels: { "agent-wechat": { backend: "native", serverUrl: "http://legacy.invalid" } } }), null);
});
