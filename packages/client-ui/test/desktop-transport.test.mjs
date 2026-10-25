import test from "node:test";
import assert from "node:assert/strict";
globalThis.window = globalThis;
const { mockIPC, clearMocks } = await import("@tauri-apps/api/mocks");
const { desktopFetch } = await import("../src/desktop.ts");
const session = {
  id: "fixture",
  endpoint: "http://127.0.0.1:6174",
  local: true,
};
const packet = (value) => ({
  type: "data",
  bytes: Buffer.from(value).toString("base64"),
});
test("native transport streams bytes arriving before response headers and omits webview credentials", async (t) => {
  t.after(clearMocks);
  let acknowledged = 0;
  mockIPC((command, args) => {
    if (command === "http_ack") {
      acknowledged++;
      return;
    }
    assert.equal(command, "http_request");
    assert.equal(args.request.connectionId, "fixture");
    assert.equal(args.request.path, "/v1/status");
    assert.equal(args.request.headers.authorization, undefined);
    args.channel.onmessage(packet("first"));
    args.channel.onmessage(packet(" second"));
    args.channel.onmessage({ type: "end" });
    return { status: 200, headers: { "content-type": "text/plain" } };
  });
  const response = await desktopFetch(session)(
    session.endpoint + "/v1/status",
    { headers: { Authorization: "Bearer webview-placeholder" } },
  );
  assert.equal(await response.text(), "first second");
  assert.equal(acknowledged, 2);
});
test("aborting a stream cancels native work and rejects a pending reader", async (t) => {
  t.after(clearMocks);
  let cancelled = 0;
  mockIPC((command) => {
    if (command === "http_request")
      return { status: 200, headers: { "content-type": "text/event-stream" } };
    if (command === "http_cancel") cancelled++;
  });
  const abort = new AbortController();
  const response = await desktopFetch(session)(
    session.endpoint + "/v1/accounts/fixture/events/stream",
    { signal: abort.signal },
  );
  const pending = response.body.getReader().read();
  abort.abort();
  await assert.rejects(pending, { name: "AbortError" });
  assert.equal(cancelled, 1);
});
test("multipart uploads cross IPC in small chunks and retain file bytes", async (t) => {
  t.after(clearMocks);
  const chunks = [];
  mockIPC((command, args) => {
    if (command === "http_body_chunk") {
      assert.ok(args.bytes.length <= 65536);
      chunks.push(Buffer.from(args.bytes));
    }
    if (command === "http_request") {
      assert.equal(args.request.body, true);
      assert.match(
        args.request.headers["content-type"],
        /^multipart\/form-data; boundary=/,
      );
      args.channel.onmessage({ type: "end" });
      return { status: 204, headers: {} };
    }
  });
  const form = new FormData();
  form.set("file", new Blob([Buffer.alloc(150000, 97)]), "fixture.bin");
  const response = await desktopFetch(session)(
    session.endpoint + "/v1/accounts/fixture/uploads",
    { method: "POST", body: form },
  );
  assert.equal(response.status, 204);
  const bytes = Buffer.concat(chunks);
  assert.ok(bytes.includes(Buffer.alloc(150000, 97)));
  assert.match(bytes.toString(), /filename="fixture.bin"/);
});
test("a saved connection cannot send native requests to another origin", async (t) => {
  t.after(clearMocks);
  mockIPC(() => {
    assert.fail("No native command should run");
  });
  await assert.rejects(
    desktopFetch(session)("https://other.example/v1/status"),
    /Unexpected hub origin/,
  );
});
