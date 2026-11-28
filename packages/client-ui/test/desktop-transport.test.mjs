import test from "node:test";
import assert from "node:assert/strict";
globalThis.window = globalThis;
const { mockIPC, clearMocks } = await import("@tauri-apps/api/mocks");
const { desktopFetch, saveDesktopFile } = await import("../src/desktop.ts");
const session = {
  id: "fixture",
  endpoint: "http://127.0.0.1:6174",
  local: true,
};
const chunkLimit = 256 * 1024;
const frame = (...bytes) => Uint8Array.of(...bytes).buffer;
const binaryPayload = () =>
  Buffer.from(
    Uint8Array.from(
      { length: 300 * 1024 + 17 },
      (_, index) => (index ^ (index >>> 9) ^ (index >>> 17)) & 0xff,
    ),
  );

function mockUploadIPC(onCommand) {
  let uploadId;
  const chunks = [];
  mockIPC((command, args) => {
    if (command === "http_body_begin") uploadId = args.id;
    if (command === "http_request") assert.equal(args.request.id, uploadId);
    if (command === "desktop_save_file" || command === "http_cancel") {
      assert.equal(args.id, uploadId);
    }
    return onCommand(command, args);
  });
  // mockIPC forwards the payload but drops InvokeOptions before calling its handler.
  const internals = window.__TAURI_INTERNALS__;
  const invoke = internals.invoke;
  internals.invoke = (command, payload, options) => {
    if (command === "http_body_chunk") {
      assert.ok(uploadId);
      assert.equal(new Headers(options?.headers).get("x-upload-id"), uploadId);
      assert.ok(payload instanceof Uint8Array || payload instanceof ArrayBuffer);
      assert.ok(payload.byteLength > 0 && payload.byteLength <= chunkLimit);
      if (payload instanceof Uint8Array) {
        assert.equal(payload.byteOffset, 0);
        assert.equal(payload.byteLength, payload.buffer.byteLength);
      }
      chunks.push(Buffer.from(new Uint8Array(payload)));
    }
    return invoke(command, payload, options);
  };
  return chunks;
}

test("native transport streams bytes arriving before response headers and omits webview credentials", async (t) => {
  t.after(clearMocks);
  let acknowledged = 0;
  const first = frame(0, 0, 128, 255);
  const second = frame(0, 2, 1, 0);
  mockIPC((command, args) => {
    if (command === "http_ack") {
      acknowledged++;
      return;
    }
    assert.equal(command, "http_request");
    assert.equal(args.request.connectionId, "fixture");
    assert.equal(args.request.path, "/v1/status");
    assert.equal(args.request.headers.authorization, undefined);
    args.channel.onmessage(first);
    args.channel.onmessage(second);
    args.channel.onmessage(frame(1));
    return { status: 200, headers: { "content-type": "application/octet-stream" } };
  });
  const response = await desktopFetch(session)(
    session.endpoint + "/v1/status",
    { headers: { Authorization: "Bearer webview-placeholder" } },
  );
  const reader = response.body.getReader();
  for (const [buffer, bytes] of [[first, [0, 128, 255]], [second, [2, 1, 0]]]) {
    const { value, done } = await reader.read();
    assert.equal(done, false);
    assert.equal(value.buffer, buffer);
    assert.deepEqual(value, Uint8Array.from(bytes));
  }
  assert.deepEqual(await reader.read(), { value: undefined, done: true });
  assert.equal(acknowledged, 2);
});

for (const [kind, packet] of [
  ["A binary error", frame(2)],
  ["An unknown tag", frame(99, 1, 2)],
  ["An empty binary", new ArrayBuffer(0)],
  ["A legacy JSON", { type: "data", bytes: "AP8=" }],
  ["A byte-array", [0, 1, 2]],
  ["A null", null],
]) {
  test(`${kind} frame rejects a pending reader and cancels native work`, async (t) => {
    t.after(clearMocks);
    let channel;
    let cancelled = 0;
    mockIPC((command, args) => {
      if (command === "http_cancel") {
        cancelled++;
        return;
      }
      assert.equal(command, "http_request");
      channel = args.channel;
      return { status: 200, headers: {} };
    });
    const response = await desktopFetch(session)(session.endpoint + "/v1/status");
    const pending = response.body.getReader().read();
    channel.onmessage(packet);
    assert.equal(cancelled, 1);
    await assert.rejects(pending, { message: "The hub connection was interrupted" });
  });
}

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

test("binary fetch uploads use raw 256 KiB chunks with the upload ID", async (t) => {
  t.after(clearMocks);
  const payload = binaryPayload();
  const chunks = mockUploadIPC((command, args) => {
    if (command === "http_request") {
      assert.equal(args.request.body, true);
      args.channel.onmessage(frame(1));
      return { status: 204, headers: {} };
    }
  });
  const response = await desktopFetch(session)(
    session.endpoint + "/v1/accounts/fixture/uploads",
    { method: "POST", body: payload },
  );
  assert.equal(response.status, 204);
  assert.deepEqual(chunks.map((chunk) => chunk.length), [chunkLimit, payload.length - chunkLimit]);
  assert.deepEqual(Buffer.concat(chunks), payload);
});

test("multipart uploads retain the boundary, filename and binary file bytes", async (t) => {
  t.after(clearMocks);
  const payload = binaryPayload();
  const chunks = mockUploadIPC((command, args) => {
    if (command === "http_request") {
      assert.equal(args.request.body, true);
      assert.match(
        args.request.headers["content-type"],
        /^multipart\/form-data; boundary=/,
      );
      args.channel.onmessage(frame(1));
      return { status: 204, headers: {} };
    }
  });
  const form = new FormData();
  form.set("file", new Blob([payload]), "fixture.bin");
  const response = await desktopFetch(session)(
    session.endpoint + "/v1/accounts/fixture/uploads",
    { method: "POST", body: form },
  );
  assert.equal(response.status, 204);
  const bytes = Buffer.concat(chunks);
  assert.ok(bytes.includes(payload));
  assert.match(bytes.toString(), /filename="fixture.bin"/);
});

test("saving a desktop file sends every byte as raw chunks before saving and cleanup", async (t) => {
  t.after(clearMocks);
  const payload = binaryPayload();
  const calls = [];
  const chunks = mockUploadIPC((command, args) => {
    if (command !== "http_body_chunk") calls.push(command);
    if (command === "desktop_save_file") {
      assert.equal(args.name, "fixture.bin");
      assert.deepEqual(Buffer.concat(chunks), payload);
    }
  });
  await saveDesktopFile(new Blob([payload]), "fixture.bin");
  assert.deepEqual(calls, ["http_body_begin", "desktop_save_file", "http_cancel"]);
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
