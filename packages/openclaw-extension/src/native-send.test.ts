import test from "node:test";
import assert from "node:assert/strict";
import type { NativeWeChatClient } from "@agent-wechat/shared";
import { submitNativeText } from "./native-send.ts";
test("native send deadline never resubmits or includes message content in error", async () => {
  let submissions = 0;
  const client = {
    sendText: async () => { submissions++; return { id: "send_fixture", state: "queued" }; },
    send: async () => ({ id: "send_fixture", state: "queued" }),
  } as unknown as NativeWeChatClient;
  await assert.rejects(submitNativeText(client, "a", "u", "private fixture text", "key", { timeoutMs: 5, initialDelayMs: 1 }), error => error instanceof Error && /queued/.test(error.message) && !error.message.includes("private fixture text"));
  assert.equal(submissions, 1);
});
