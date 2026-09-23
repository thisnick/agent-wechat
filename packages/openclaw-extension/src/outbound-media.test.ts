import test from "node:test";
import assert from "node:assert/strict";
import { sendWeChatMedia } from "./outbound-media.ts";

const cfg = { channels: { "agent-wechat": { serverUrl: "https://wechat.test", token: "test" } } };

test("voice intent submits a voice job and waits for verification", async () => {
  const original = globalThis.fetch;
  const calls: string[] = [];
  globalThis.fetch = async (input, init) => {
    const url = String(input);
    calls.push(url);
    if (url === "https://media.test/voice.mp3") {
      return new Response(new Uint8Array([1, 2, 3]), { headers: { "content-type": "audio/mpeg" } });
    }
    if (url === "https://wechat.test/api/messages/voice" && init?.method === "POST") {
      assert.equal((init.body as FormData).get("chatId"), "filehelper");
      assert.equal((init.headers as Record<string, string>)["Idempotency-Key"], "reply-391");
      return Response.json({ jobId: "job-1", status: "queued", chunks: [] }, { status: 202 });
    }
    if (url === "https://wechat.test/api/messages/voice/job-1") {
      return Response.json({ jobId: "job-1", status: "completed", chunks: [{ status: "verified", messageId: 26 }] });
    }
    throw new Error(`Unexpected request: ${url}`);
  };
  try {
    const id = await sendWeChatMedia(cfg, "filehelper", "", "https://media.test/voice.mp3", true, "reply-391");
    assert.equal(id, "agent-wechat:voice:job-1");
    assert.deepEqual(calls, ["https://media.test/voice.mp3", "https://wechat.test/api/messages/voice", "https://wechat.test/api/messages/voice/job-1"]);
  } finally {
    globalThis.fetch = original;
  }
});

test("audio without voice intent remains a file attachment", async () => {
  const original = globalThis.fetch;
  let body: Record<string, unknown> | undefined;
  globalThis.fetch = async (input, init) => {
    const url = String(input);
    if (url === "https://media.test/voice.mp3") {
      return new Response(new Uint8Array([1, 2, 3]), { headers: { "content-type": "audio/mpeg" } });
    }
    assert.equal(url, "https://wechat.test/api/messages/send");
    body = JSON.parse(String(init?.body));
    return Response.json({ success: true });
  };
  try {
    await sendWeChatMedia(cfg, "filehelper", "", "https://media.test/voice.mp3", false);
    assert.ok(body?.file);
    assert.equal(body?.voice, undefined);
  } finally {
    globalThis.fetch = original;
  }
});
