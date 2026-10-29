import test from "node:test";
import assert from "node:assert/strict";
import { wechatPlugin } from "./channel.ts";

test("explicit non-wxid native peer IDs reach identity validation instead of group-name lookup", () => {
  const resolver = wechatPlugin.messaging!.targetResolver!;
  assert.equal(resolver.looksLikeId!("wechat:custom_username"), true);
  assert.equal(resolver.looksLikeId!("agent-wechat:custom_username"), true);
  assert.equal(typeof resolver.resolveTarget, "function");
});
