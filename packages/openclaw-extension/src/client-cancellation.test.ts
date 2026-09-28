import test from "node:test";
import assert from "node:assert/strict";
import { WeChatClient } from "@agent-wechat/shared";

for (const [name, request] of [
  ["GET", (client: WeChatClient) => client.getMedia("peer", 1)],
  ["POST", (client: WeChatClient) => client.openChat("peer")],
  ["DELETE", (client: WeChatClient) => client.deleteSession("test")],
  ["multipart POST", (client: WeChatClient) => client.createVoiceJob("peer", new Uint8Array(), "test")],
] as const) {
  test(`client cancellation reaches an in-flight ${name} request`, async t => {
    const controller = new AbortController();
    const client = new WeChatClient({ baseUrl: "http://unused.invalid", signal: controller.signal });
    t.mock.method(globalThis, "fetch", async (_url: unknown, init: RequestInit) => {
      assert.equal(init.signal, controller.signal);
      return new Promise((_resolve, reject) => {
        init.signal!.addEventListener("abort", () => reject(init.signal!.reason), { once: true });
      });
    });
    const pending = request(client);
    controller.abort();
    await assert.rejects(pending, { name: "AbortError" });
  });
}
