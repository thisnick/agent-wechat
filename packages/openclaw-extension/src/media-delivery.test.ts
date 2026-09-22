import test from "node:test";
import assert from "node:assert/strict";
import {
  applyCatchupAttachmentPolicy,
  buildMediaSegments,
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
  assert.match(body, /File unavailable: report\.pdf \(pending\)/);
  assert.doesNotMatch(body, /Local file/);
});
