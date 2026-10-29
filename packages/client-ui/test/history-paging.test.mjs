import test from "node:test";
import assert from "node:assert/strict";
import { HistoryPaging, HISTORY_SCROLL_EDGE } from "../src/history-paging.ts";

test("wheel bursts and button clicks share one in-flight history page", async () => {
  const paging = new HistoryPaging();
  let release,
    calls = 0;
  const page = {
    hasMore: true,
    loading: false,
    load: () => {
      calls++;
      return new Promise((resolve) => (release = resolve));
    },
  };
  const first = paging.request(page, true);
  await Promise.all([paging.request(page, true), paging.request(page, false)]);
  assert.equal(calls, 1);
  release();
  await first;
  await paging.request(page, true);
  assert.equal(
    calls,
    1,
    "remaining at the same edge cannot automatically drain more history"
  );
});

test("leaving the edge permits another page and loading/exhaustion never fetch", async () => {
  const paging = new HistoryPaging();
  let calls = 0;
  const page = {
    hasMore: true,
    loading: false,
    load: async () => {
      calls++;
    },
  };
  await paging.request({ ...page, loading: true }, true);
  await paging.request({ ...page, hasMore: false }, true);
  assert.equal(calls, 0);
  await paging.request(page, true);
  paging.observe(HISTORY_SCROLL_EDGE);
  await paging.request(page, true);
  assert.equal(calls, 1);
  paging.observe(HISTORY_SCROLL_EDGE + 1);
  await paging.request(page, true);
  assert.equal(calls, 2);
});

test("a failed page waits for a manual retry or a new visit instead of retrying each wheel event", async () => {
  const paging = new HistoryPaging();
  let calls = 0;
  const page = {
    hasMore: true,
    loading: false,
    load: async () => {
      calls++;
      if (calls === 1) throw Error("offline");
    },
  };
  await assert.rejects(paging.request(page, true), /offline/);
  await paging.request(page, true);
  assert.equal(calls, 1);
  await paging.request(page, false);
  assert.equal(calls, 2);
  await paging.request(page, true);
  assert.equal(calls, 2);
  paging.observe(HISTORY_SCROLL_EDGE + 1);
  await paging.request(page, true);
  assert.equal(calls, 3);
});
