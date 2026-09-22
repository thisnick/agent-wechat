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
