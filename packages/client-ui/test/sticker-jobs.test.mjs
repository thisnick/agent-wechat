import test from "node:test";
import assert from "node:assert/strict";
import { StickerJobError, waitStickerJob } from "../src/sticker-jobs.ts";
import { readableError } from "../src/session.ts";

test("a failed sticker job reports its result rather than a hub connection failure", async () => {
  let reads = 0;
  const client = { job: async () => { reads++; return { id: "job-one", state: "needs_review", reason: "sticker_upload_progress_invalid" }; } };
  await assert.rejects(waitStickerJob(client, "a", { id: "job-one", state: "queued", reason: null }), error => {
    assert.ok(error instanceof StickerJobError);
    assert.match(readableError(error), /job-one.*needs_review.*sticker_upload_progress_invalid/);
    assert.doesNotMatch(readableError(error), /Couldn't reach/);
    return true;
  });
  assert.equal(reads, 1);
});

test("real fetch failures remain connection errors and never expose raw diagnostics", async () => {
  const client = { job: async () => { throw Error("fetch failed: private diagnostic"); } };
  await assert.rejects(waitStickerJob(client, "a", { id: "job-one", state: "queued", reason: null }), error => {
    assert.match(readableError(error), /Couldn't reach/);
    assert.doesNotMatch(readableError(error), /private diagnostic/);
    return true;
  });
});

test("job errors retain safe result codes without displaying arbitrary response strings", () => {
  const error = new StickerJobError({ id: "https://private.example/key", state: "needs_review", reason: "https://private.example/secret" });
  assert.match(readableError(error), /needs_review/);
  assert.doesNotMatch(readableError(error), /https|private|secret/);
});
