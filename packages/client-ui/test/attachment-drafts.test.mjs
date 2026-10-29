import test from "node:test";
import assert from "node:assert/strict";
import {
  AttachmentDrafts,
  sendAttachmentBatch,
} from "../src/attachment-drafts.ts";
import { MediaOutbox } from "../src/media-outbox.ts";

const supported = ["image", "video", "file"];
const file = (name, type = "") =>
  new File([`synthetic ${name}`], name, { type });

test("picking and pasting append unsent files only to their account and conversation", () => {
  const drafts = new AttachmentDrafts();
  assert.equal(
    drafts.claim("a", "mei"),
    null,
    "text-only sends do not lock an attachment draft",
  );
  const photo = file("photo.png"),
    movie = file("clip.mov"),
    note = file("note.txt");
  drafts.add("a", "mei", [photo], supported);
  drafts.add("a", "mei", [movie, note], supported);
  drafts.add("a", "dan", [file("other.pdf")], supported);
  drafts.add("b", "mei", [file("private.jpg")], supported);
  assert.deepEqual(
    drafts.get("a", "mei").files.map((item) => item.file),
    [photo, movie, note],
  );
  assert.deepEqual(
    drafts.get("a", "mei").files.map((item) => item.kind),
    supported,
  );
  assert.equal(drafts.get("a", "dan").files.length, 1);
  assert.equal(drafts.get("b", "mei").files.length, 1);
  const removed = drafts.get("a", "mei").files[1].id;
  drafts.remove("a", "mei", removed);
  assert.deepEqual(
    drafts.get("a", "mei").files.map((item) => item.file),
    [photo, note],
  );
  assert.equal(drafts.get("b", "mei").files.length, 1);
});

test("cumulative limits and invalid additions preserve the existing draft", () => {
  const drafts = new AttachmentDrafts();
  drafts.add(
    "a",
    "mei",
    Array.from({ length: 9 }, (_, index) => file(`${index}.txt`)),
    supported,
  );
  const saved = drafts.get("a", "mei");
  assert.throws(
    () => drafts.add("a", "mei", [file("10.txt"), file("11.txt")], supported),
    /up to 10/,
  );
  assert.throws(
    () =>
      drafts.add(
        "a",
        "mei",
        [file("okay.txt"), new File([], "empty.txt")],
        supported,
      ),
    /nonempty/,
  );
  assert.equal(drafts.get("a", "mei"), saved);
  drafts.add("a", "mei", [file("10.txt")], supported);
  assert.equal(drafts.get("a", "mei").files.length, 10);
});

test("one send claims the draft, a rejected text admission preserves it, and sign-out cannot resurrect files", () => {
  const drafts = new AttachmentDrafts();
  drafts.add("a", "mei", [file("clip.mov")], supported);
  drafts.quality("a", "mei", "original");
  const first = drafts.claim("a", "mei");
  assert.equal(
    drafts.claim("a", "mei"),
    null,
    "repeated Send cannot claim the same files",
  );
  assert.throws(
    () => drafts.add("a", "mei", [file("later.txt")], supported),
    /Wait/,
  );
  drafts.remove("a", "mei", first.files[0].id);
  assert.equal(drafts.get("a", "mei").files.length, 1);
  assert.equal(drafts.finish(first, false), true);
  assert.equal(drafts.get("a", "mei").videoQuality, "original");
  const second = drafts.claim("a", "mei");
  drafts.clear("a");
  assert.equal(drafts.finish(second, false), false);
  assert.equal(drafts.get("a", "mei").files.length, 0);
});

test("Send creates individual outbox operations with distinct keys and captured destination/quality", async (t) => {
  const drafts = new AttachmentDrafts();
  const files = [file("photo.png"), file("clip.mov"), file("note.txt")];
  const uploads = [],
    submissions = [];
  const outbox = new MediaOutbox(
    {
      upload: async (account, bytes, name) => {
        uploads.push({ account, name, bytes });
        return { id: `source-${uploads.length}` };
      },
      sendMedia: async (account, request, key) => {
        submissions.push({ account, request, key });
        return {
          id: `job-${submissions.length}`,
          accountId: account,
          kind: "send",
          state: "accepted",
          phase: "completed",
          result: {},
        };
      },
    },
    "a",
    { read: () => [], write() {} },
  );
  t.after(() => outbox.close());
  drafts.add("a", "mei", files, supported);
  drafts.quality("a", "mei", "original");
  assert.equal(uploads.length, 0, "staging must not upload or send anything");
  const batch = drafts.claim("a", "mei");
  assert.equal(drafts.finish(batch, true), true);
  drafts.add("b", "dan", [file("new-draft.txt")], supported);
  await sendAttachmentBatch(outbox, batch);
  assert.deepEqual(
    uploads.map((item) => item.name),
    files.map((item) => item.name),
  );
  assert.deepEqual(
    submissions.map((item) => [
      item.account,
      item.request.threadId,
      item.request.kind,
    ]),
    [
      ["a", "mei", "image"],
      ["a", "mei", "video"],
      ["a", "mei", "file"],
    ],
  );
  assert.equal(new Set(submissions.map((item) => item.key)).size, 3);
  assert.equal(submissions[1].request.videoQuality, "original");
  assert.equal(submissions[0].request.videoQuality, undefined);
  assert.equal(drafts.get("a", "mei").files.length, 0);
  assert.equal(drafts.get("b", "dan").files.length, 1);
});
