import test from "node:test";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { AttachmentTooLargeError, saveManagedAttachment, saveManagedAttachmentBytes, saveVerifiedManagedAttachmentStream } from "./attachment-store.ts";

test("managed attachment passes an explicit bound and filename to OpenClaw storage", async () => {
  const content = Buffer.from("synthetic attachment");
  const calls: unknown[][] = [];
  const result = await saveManagedAttachment({
    data: content.toString("base64"),
    mime: "text/plain",
    filename: "report.txt",
    maxBytes: 1024,
    async saveMediaBuffer(...args) {
      calls.push(args);
      return { path: "/openclaw/media/inbound/report.txt", size: content.length };
    },
  });
  assert.equal(result.path, "/openclaw/media/inbound/report.txt");
  assert.equal(result.size, content.length);
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0]?.slice(1), ["text/plain", "inbound", 1024, "report.txt"]);
  assert.deepEqual(calls[0]?.[0], content);
});

test("managed attachment rejects encoded data above the configured limit", async () => {
  let saveCalled = false;
  await assert.rejects(
    saveManagedAttachment({
      data: Buffer.alloc(11).toString("base64"),
      mime: "application/octet-stream",
      filename: "large.bin",
      maxBytes: 10,
      async saveMediaBuffer() {
        saveCalled = true;
        return { path: "/unexpected" };
      },
    }),
    AttachmentTooLargeError,
  );
  assert.equal(saveCalled, false);
});

test("binary managed storage keeps the selected view without a base64 representation", async () => {
  const bytes = new Uint8Array([99, 1, 2, 3, 99]).subarray(1, 4);
  const result = await saveManagedAttachmentBytes({ bytes, mime: "application/octet-stream", filename: "view.bin", maxBytes: 3,
    saveMediaBuffer: async buffer => { assert.deepEqual(buffer, Buffer.from([1, 2, 3])); assert.equal(buffer.buffer, bytes.buffer); return { path: "/managed/view.bin" }; } });
  assert.equal(result.size, 3); assert.equal(result.sha256, createHash("sha256").update(bytes).digest("hex"));
});

test("stream storage validates length and digest before publishing and closes rejected sources", async () => {
  const content = new Uint8Array([1, 2, 3, 4]), sha256 = createHash("sha256").update(content).digest("hex");
  for (const mode of ["valid", "digest", "short", "overflow", "cancel"] as const) {
    let published = 0, closed = false, guards = 0;
    const run = saveVerifiedManagedAttachmentStream({ stream: (async function* () { try { yield content.subarray(0, 2); yield content.subarray(2); } finally { closed = true; } })(),
      mime: "application/octet-stream", filename: "stream.bin", maxBytes: 512 * 1024 * 1024, expectedBytes: mode === "short" ? 5 : mode === "overflow" ? 3 : 4,
      expectedSha256: mode === "digest" ? "0".repeat(64) : sha256, assertCurrent: () => { if (mode === "cancel" && ++guards > 1) throw Error("source recalled"); },
      saveMediaStream: async (stream, _mime, _subdir, max, _filename, _hint, options) => {
        assert.equal(max, 512 * 1024 * 1024); assert.equal(options.durable, true);
        for await (const _chunk of stream) { /* consume into the simulated private temporary file */ }
        options.assertCommitAllowed(); published++; return { path: "/managed/stream.bin", size: 4 };
      } });
    if (mode === "valid") assert.equal((await run).sha256, sha256); else await assert.rejects(run, /integrity|exceeds|recalled/);
    assert.equal(published, mode === "valid" ? 1 : 0); assert.equal(closed, true);
  }
});
