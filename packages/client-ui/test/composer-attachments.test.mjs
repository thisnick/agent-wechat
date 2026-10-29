import test from "node:test";
import assert from "node:assert/strict";
import {
  attachmentKind,
  reviewAttachments,
  transferredFiles,
  bindComposerAttachments,
  hasTransferredFiles,
  COMPOSER_MAX_SOURCE_BYTES,
} from "../src/composer-attachments.ts";

test("attachment classification uses supported MIME types, then extensions when MIME is absent", () => {
  for (const [name, type, expected] of [
    ["photo", "image/jpeg", "image"],
    ["photo", "image/png", "image"],
    ["photo", "IMAGE/GIF", "image"],
    ["photo", "image/webp", "image"],
    ["photo.JPG", "", "image"],
    ["photo.JPEG", "application/octet-stream", "image"],
    ["photo.PNG", "", "image"],
    ["clip", "video/mp4", "video"],
    ["clip.MP4", "", "video"],
    ["clip.mp4", "application/octet-stream", "video"],
    ["report.pdf", "application/pdf", "file"],
    ["voice.mp3", "audio/mpeg", "file"],
    ["photo.heic", "image/heic", "file"],
    ["image.svg", "image/svg+xml", "file"],
    ["clip.webm", "video/webm", "file"],
    ["report.png", "application/pdf", "file"],
    ["unknown", "", "file"],
  ])
    assert.equal(attachmentKind({ name, type }), expected, `${name} (${type})`);
});

test("mixed batches retain every file with its detected type and enforce size/capability limits before staging", () => {
  const files = [
    new File(["image"], "a.png"),
    new File(["video"], "b.mp4"),
    new File(["document"], "c.pdf"),
  ];
  const result = reviewAttachments(files, ["image", "video", "file"]);
  assert.deepEqual(
    result.map((item) => item.kind),
    ["image", "video", "file"]
  );
  assert.deepEqual(
    result.map((item) => item.file),
    files
  );
  assert.throws(() => reviewAttachments([], ["file"]), /Choose a file/);
  assert.throws(
    () => reviewAttachments(Array(11).fill(files[0]), ["image"]),
    /up to 10/
  );
  assert.throws(
    () => reviewAttachments([new File([], "empty.txt")], ["file"]),
    /nonempty/
  );
  assert.throws(
    () =>
      reviewAttachments(
        [
          {
            name: "large.pdf",
            type: "application/pdf",
            size: COMPOSER_MAX_SOURCE_BYTES + 1,
          },
        ],
        ["file"]
      ),
    /smaller than 25 MB/
  );
  assert.throws(
    () => reviewAttachments(files, ["file"]),
    /doesn't support image/
  );
  assert.equal(
    reviewAttachments(
      [
        {
          name: "limit.pdf",
          type: "application/pdf",
          size: COMPOSER_MAX_SOURCE_BYTES,
        },
      ],
      ["file"]
    ).length,
    1
  );
});

const data = (files = [], types = ["Files"]) => ({
  files,
  types,
  items: files.map((file) => ({ kind: "file", getAsFile: () => file })),
  dropEffect: "none",
});
function binding(t) {
  const previous = globalThis.window;
  const window = (globalThis.window = new EventTarget());
  const root = new EventTarget(),
    received = [],
    hover = [];
  const remove = bindComposerAttachments(
    root,
    (files) => received.push(files),
    (value) => hover.push(value)
  );
  t.after(() => {
    remove();
    globalThis.window = previous;
  });
  return {
    root,
    window,
    received,
    hover,
    remove,
    fire(name, transfer) {
      const event = new Event(name, { bubbles: true, cancelable: true });
      Object.defineProperty(
        event,
        name === "paste" ? "clipboardData" : "dataTransfer",
        { value: transfer }
      );
      root.dispatchEvent(event);
      return event;
    },
  };
}

test("clipboard item fallback includes pasted screenshots without duplicating the FileList", () => {
  const file = new File(["screenshot"], "image.png", { type: "image/png" });
  const transfer = data([file]);
  assert.equal(hasTransferredFiles(transfer), true);
  assert.deepEqual(transferredFiles(transfer), [file]);
  transfer.files = [];
  transfer.items.push({
    kind: "string",
    getAsFile: () => {
      throw new Error("Text must not be interpreted as a file");
    },
  });
  transfer.items.push({ kind: "file", getAsFile: () => null });
  assert.deepEqual(transferredFiles(transfer), [file]);
  assert.equal(hasTransferredFiles(null), false);
});

test("paste captures file payloads while leaving ordinary text and HTML handling to the editor", (t) => {
  const handler = binding(t),
    file = new File(["png"], "image.png", { type: "image/png" });
  assert.equal(handler.fire("paste", data([file])).defaultPrevented, true);
  assert.deepEqual(handler.received, [[file]]);
  const text = data([], ["text/plain", "text/html"]);
  assert.equal(handler.fire("paste", text).defaultPrevented, false);
  assert.equal(handler.received.length, 1);
});

test("protected drag data shows a drop target; nested drag leaves, dropping and blur reset it", (t) => {
  const handler = binding(t),
    protectedData = data();
  assert.equal(handler.fire("dragenter", protectedData).defaultPrevented, true);
  handler.fire("dragenter", protectedData);
  handler.fire("dragleave", protectedData);
  assert.equal(handler.hover.at(-1), true);
  handler.fire("dragover", protectedData);
  assert.equal(protectedData.dropEffect, "copy");
  const files = [new File(["pdf"], "one.pdf"), new File(["png"], "two.png")];
  assert.equal(handler.fire("drop", data(files)).defaultPrevented, true);
  assert.deepEqual(handler.received, [files]);
  assert.equal(handler.hover.at(-1), false);
  handler.fire("dragenter", protectedData);
  handler.window.dispatchEvent(new Event("blur"));
  assert.equal(handler.hover.at(-1), false);
  assert.equal(
    handler.fire("drop", data([], ["text/plain"])).defaultPrevented,
    false
  );
});

test("unmounting a composer removes its file handlers before another chat can receive files", (t) => {
  const handler = binding(t),
    file = new File(["pdf"], "document.pdf");
  handler.remove();
  assert.equal(handler.fire("paste", data([file])).defaultPrevented, false);
  assert.equal(handler.fire("drop", data([file])).defaultPrevented, false);
  assert.deepEqual(handler.received, []);
});
