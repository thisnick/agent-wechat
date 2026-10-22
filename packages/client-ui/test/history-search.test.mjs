import test from "node:test";
import assert from "node:assert/strict";
import { HistorySearch } from "../src/history-search.ts";

const page = (items, nextCursor = null) => ({ items, nextCursor });
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
};

test("history search uses the conversation and opaque continuation with the same query", async () => {
  const calls = [];
  const search = new HistorySearch(
    {
      async messages(account, thread, options) {
        calls.push({ account, thread, ...options });
        return options.cursor
          ? page([{ id: "older", body: "needle in older history" }])
          : page(
              [{ id: "newer", body: "needle in recent history" }],
              "opaque-page",
            );
      },
    },
    "personal",
    "mei",
  );
  await search.search(" needle ");
  await search.more();
  assert.deepEqual(calls, [
    { account: "personal", thread: "mei", query: "needle", limit: 50 },
    {
      account: "personal",
      thread: "mei",
      query: "needle",
      limit: 50,
      cursor: "opaque-page",
    },
  ]);
  assert.deepEqual(
    search.snapshot().items.map((item) => item.id),
    ["newer", "older"],
  );
});

test("late search pages cannot replace a newer query or a closed conversation", async () => {
  const late = deferred();
  const search = new HistorySearch(
    {
      messages: async (_a, _t, options) =>
        options.query === "old" ? late.promise : page([{ id: "current" }]),
    },
    "a",
    "peer",
  );
  const older = search.search("old");
  await search.search("new");
  late.resolve(page([{ id: "stale" }]));
  await older;
  assert.equal(search.snapshot().query, "new");
  assert.deepEqual(search.snapshot().items, [{ id: "current" }]);

  const afterClose = deferred();
  const closed = new HistorySearch(
    { messages: () => afterClose.promise },
    "b",
    "peer",
  );
  const request = closed.search("private to b");
  let updates = 0;
  closed.subscribe(() => updates++);
  closed.close();
  afterClose.resolve(page([{ id: "wrong-account" }]));
  await request;
  assert.equal(updates, 0);
  assert.deepEqual(closed.snapshot().items, []);
});

test("clearing a query invalidates pending results without fetching all history", async () => {
  const late = deferred();
  let calls = 0;
  const search = new HistorySearch(
    {
      messages: () => {
        calls++;
        return late.promise;
      },
    },
    "a",
    "peer",
  );
  const request = search.search("needle");
  await search.search(" ");
  late.resolve(page([{ id: "stale" }]));
  await request;
  assert.equal(calls, 1);
  assert.deepEqual(search.snapshot(), {
    query: "",
    items: [],
    nextCursor: null,
    loading: false,
    error: null,
  });
});
