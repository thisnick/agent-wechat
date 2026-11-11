import test from "node:test";
import assert from "node:assert/strict";
import {
  attachmentFallbackFilename,
  attachmentSourceBody,
  mediaRequestQuality,
  renderAttachmentBody,
  type WeChatAttachment,
} from "./media-delivery.ts";

const ready = (kind: WeChatAttachment["kind"], filename: string, path: string): WeChatAttachment => ({
  kind,
  filename,
  path,
  mime: kind === "image" ? "image/png" : "application/octet-stream",
  status: "ready",
});

test("ready attachments are named in the text without a local path", () => {
  // The file itself reaches OpenClaw as a media fact; a gateway path in the text
  // would point a sandboxed agent at a file it cannot open.
  assert.equal(renderAttachmentBody("", ready("file", "招标文件.doc", "/managed/doc---1.doc")), "[File: 招标文件.doc]");
  assert.equal(renderAttachmentBody("brief.pdf", ready("file", "brief.pdf", "/managed/brief.pdf")), "[File: brief.pdf]");
  assert.equal(renderAttachmentBody("see attached", ready("file", "brief.pdf", "/managed/brief.pdf")), "see attached\n[File: brief.pdf]");
  assert.equal(renderAttachmentBody("", ready("image", "a.png", "/managed/a.png")), "<media:image>");
  assert.equal(renderAttachmentBody("", ready("audio", "v.mp3", "/managed/v.mp3")), "<media:audio>");
  assert.equal(renderAttachmentBody("", ready("video", "c.mp4", "/managed/c.mp4")), "<media:video>");
  for (const kind of ["file", "image", "audio", "video"] as const) {
    assert.doesNotMatch(renderAttachmentBody("", ready(kind, "x", "/managed/x")), /\/managed|Local file/);
  }
});

test("unavailable attachments expose status without a fake path", () => {
  const body = renderAttachmentBody("report.pdf", {
    kind: "file",
    filename: "report.pdf",
    status: "pending",
  });
  assert.match(body, /File pending: report\.pdf/);
  assert.doesNotMatch(body, /Local file/);
});

test("expired files are visible to the model without suggesting more polling", () => {
  const body = renderAttachmentBody("report.pdf", { kind: "file", filename: "report.pdf", status: "expired" });
  assert.match(body, /File expired: report\.pdf/);
  assert.match(body, /Ask the sender to resend/);
  assert.doesNotMatch(body, /pending|Local file/);
});

test("pending videos never expose raw message XML", () => {
  const raw = '<msg><videomsg cdnvideourl="private" /></msg>';
  const source = attachmentSourceBody(raw, "video");
  const filename = attachmentFallbackFilename("video", 60, raw);
  const body = renderAttachmentBody(source, { kind: "video", filename, status: "pending" });
  assert.equal(source, "");
  assert.equal(filename, "message-60.mp4");
  assert.equal(body, "[Video pending: message-60.mp4]");
  assert.doesNotMatch(body, /videomsg|cdnvideourl|private/);
});

test("images request best available quality while other media stays strict", () => {
  assert.equal(mediaRequestQuality(3), "best");
  assert.equal(mediaRequestQuality(34), "full");
  assert.equal(mediaRequestQuality(43), "full");
  assert.equal(mediaRequestQuality(49), "full");
});

test("a delivered thumbnail is labeled as a preview for the model", () => {
  const preview: WeChatAttachment = {
    ...ready("image", "photo.jpg", "/managed/photo.jpg"),
    quality: "thumbnail",
  };
  assert.equal(renderAttachmentBody("", preview), "<media:image>\n[Image preview only: thumbnail quality]");
  assert.equal(renderAttachmentBody("", { ...preview, quality: "standard" }), "<media:image>");
});
