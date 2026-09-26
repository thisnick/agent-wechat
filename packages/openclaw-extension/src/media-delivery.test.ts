import test from "node:test";
import assert from "node:assert/strict";
import {
  applyCatchupAttachmentPolicy,
  attachmentFallbackFilename,
  attachmentSourceBody,
  buildMediaSegments,
  mediaRequestQuality,
  renderAttachmentBody,
  type MessageWithAttachment,
  type WeChatAttachment,
} from "./media-delivery.ts";

function message(rawBody: string, attachment?: WeChatAttachment): MessageWithAttachment {
  return {
    rawBody,
    attachment,
    mediaPath: attachment?.status === "ready" && attachment.kind !== "file" ? attachment.path : undefined,
    mediaMime: attachment?.mime,
    hasMedia: !!attachment,
  };
}

const ready = (kind: WeChatAttachment["kind"], filename: string, path: string): WeChatAttachment => ({
  kind,
  filename,
  path,
  mime: kind === "image" ? "image/png" : "application/octet-stream",
  status: "ready",
});

test("catch-up retains all file paths and selects only the latest image", () => {
  const result = applyCatchupAttachmentPolicy([
    message(renderAttachmentBody("", ready("file", "brief.pdf", "/managed/brief.pdf")), ready("file", "brief.pdf", "/managed/brief.pdf")),
    message("<media:image>", ready("image", "a.png", "/managed/a.png")),
    message(renderAttachmentBody("", ready("file", "sheet.xlsx", "/managed/sheet.xlsx")), ready("file", "sheet.xlsx", "/managed/sheet.xlsx")),
    message("<media:image>", ready("image", "b.png", "/managed/b.png")),
    message("@bot summarize"),
  ]);

  assert.match(result[0].rawBody, /\/managed\/brief\.pdf/);
  assert.match(result[2].rawBody, /\/managed\/sheet\.xlsx/);
  assert.match(result[1].rawBody, /Image file: \/managed\/a\.png/);
  assert.equal(result[1].mediaPath, undefined);
  assert.equal(result[3].mediaPath, "/managed/b.png");
  assert.equal(result.filter((item) => item.mediaPath).length, 1);
  assert.deepEqual(buildMediaSegments(result).map((segment) => segment.length), [5]);
});

test("files do not consume the model media slot", () => {
  const file = ready("file", "data.zip", "/managed/data.zip");
  const firstImage = ready("image", "one.png", "/managed/one.png");
  const secondImage = ready("image", "two.png", "/managed/two.png");
  const result = buildMediaSegments([
    message("one", firstImage),
    message(renderAttachmentBody("file", file), file),
    message("two", secondImage),
  ]);
  assert.deepEqual(result.map((segment) => segment.length), [2, 1]);
  assert.match(result[0][1].rawBody, /Local file/);
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
