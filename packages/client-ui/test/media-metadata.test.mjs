import test from "node:test";
import assert from "node:assert/strict";
import { MediaMetadata } from "../src/media-metadata.ts";

test("visible metadata requests are deduplicated, batched and account scoped", async (t) => {
  const calls = [];
  const queue = new MediaMetadata({
    mediaAssetsBatch: async (account, ids) => {
      calls.push([account, ids]);
      return {
        items: ids.map((messageId) => ({
          messageId,
          assets: [{ accountId: account, messageId }],
          error: null,
        })),
      };
    },
  });
  t.after(() => queue.close());
  const signal = new AbortController().signal;
  const results = await Promise.all([
    queue.lookup("a", "one", signal),
    queue.lookup("a", "two", signal),
    queue.lookup("a", "one", signal),
    queue.lookup("b", "one", signal),
  ]);
  assert.deepEqual(calls, [
    ["a", ["one", "two"]],
    ["b", ["one"]],
  ]);
  assert.equal(results[3].items[0].accountId, "b");
});

test("one unavailable attachment does not fail the other batch members", async (t) => {
  const queue = new MediaMetadata({
    mediaAssetsBatch: async () => ({
      items: [
        { messageId: "one", assets: [], error: "message_recalled" },
        { messageId: "two", assets: [], error: null },
      ],
    }),
  });
  t.after(() => queue.close());
  const signal = new AbortController().signal;
  const [first, second] = await Promise.allSettled([
    queue.lookup("a", "one", signal),
    queue.lookup("a", "two", signal),
  ]);
  assert.equal(first.reason.code, "message_recalled");
  assert.deepEqual(second.value, { items: [] });
});

test("cancelled queued metadata is never requested", async (t) => {
  const queue = new MediaMetadata({
    mediaAssetsBatch: async () => assert.fail("cancelled work reached the hub"),
  });
  t.after(() => queue.close());
  const controller = new AbortController();
  const pending = queue.lookup("a", "one", controller.signal);
  controller.abort();
  await assert.rejects(pending, { name: "AbortError" });
});
