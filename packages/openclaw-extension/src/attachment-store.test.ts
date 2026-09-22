import test from "node:test";
import assert from "node:assert/strict";
import { AttachmentTooLargeError, saveManagedAttachment } from "./attachment-store.ts";

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
