import test from "node:test";
import assert from "node:assert/strict";
import { IDBFactory } from "fake-indexeddb";
import { NativeApiError } from "@agent-wechat/shared";
import { BrowserCache } from "../src/browser-cache.ts";
import { HubSession } from "../src/session.ts";

const page = (items, extra = {}) => ({
  items,
  nextCursor: null,
  coverage: {},
  sourceGeneration: "1",
  asOfEventCursor: "20",
  ...extra,
});
const message = (id, createdAt = "1", extra = {}) => ({
  id,
  serverId: id,
  accountId: "a",
  threadId: "peer",
  senderId: "peer",
  direction: "incoming",
  type: 1,
  body: id,
  createdAt,
  version: "1",
  media: [],
  ...extra,
});
const thread = (id = "peer", accountId = "a") => ({
  id,
  accountId,
  peerId: id,
  kind: "direct",
  unreadCount: 0,
  readStateVersion: "1",
  history: {},
});
function cache(t) {
  const values = new Map();
  const result = new BrowserCache("https://hub.test", "hub", {
    factory: new IDBFactory(),
    broadcast: false,
    storage: {
      getItem: (key) => values.get(key) ?? null,
      setItem: (key, value) => values.set(key, value),
      removeItem: (key) => values.delete(key),
    },
  });
  t.after(() => result.close());
  return result;
}
async function seed(cache, cursor = "10") {
  await cache.writeAccount("a", {
    cursor,
    threads: [thread()],
    contacts: [],
    histories: [
      [
        "peer",
        {
          messages: [
            message("original"),
            message("quote", "2", {
              quote: {
                serverId: "original",
                state: "available",
                content: "private old excerpt",
                sender: "peer",
              },
            }),
          ],
          nextMessages: "old-page",
          sourceGeneration: "1",
          group: null,
        },
      ],
    ],
  });
  cache.rememberAccount("a");
  cache.saveView("a", { thread: "peer" });
}

test("sign-out clears fetched cache but preserves drafts and send recovery", async (t) => {
  const disk = cache(t);
  await seed(disk);
  disk.writeState("a", "peer", { text: "unfinished reply" }, "drafts");
  disk.writeState("a", "send:pending", { key: "pending" });
  const hub = session(t, disk, {
    events: async () => page([], { asOfEventCursor: "10" }),
  });
  await hub.activate("a");
  await hub.consume(
    {
      cursor: "21",
      kind: "account.updated",
      data: { authentication: "logged_out" },
    },
    "a",
  );
  await disk.flush();
  assert.equal(disk.lastAccount(), undefined);
  assert.equal(await disk.readAccount("a"), undefined);
  assert.deepEqual(disk.readState("a", "peer", "drafts"), {
    text: "unfinished reply",
  });
  assert.deepEqual(disk.readState("a", "send:pending"), { key: "pending" });
});

test("a missed logout during cache catch-up prevents history restoration", async (t) => {
  const disk = cache(t);
  await seed(disk);
  let historyReads = 0,
    streams = 0;
  const hub = session(t, disk, {
    events: async () =>
      page([
        {
          cursor: "20",
          kind: "account.updated",
          data: { authentication: "logged_out" },
        },
      ]),
    messages: async () => {
      historyReads++;
      return page([]);
    },
    async *watch() {
      streams++;
    },
  });
  await hub.activate("a");
  assert.equal(hub.snapshot().signInRequired, "a");
  assert.deepEqual(hub.snapshot().messages, []);
  assert.equal(historyReads, 0);
  assert.equal(streams, 0);
  await disk.flush();
  assert.equal(await disk.readAccount("a"), undefined);
});
function session(t, cache, overrides = {}) {
  const hub = new HubSession(
    {
      accounts: async () =>
        page([
          { id: "a", authenticated: true },
          { id: "b", authenticated: true },
        ]),
      threads: async (account) =>
        page([thread("other", account), thread("peer", account)]),
      thread: async (account, id) => thread(id, account),
      messages: async () =>
        page([message("latest", "100")], { nextCursor: "fresh-page" }),
      events: async () => ({
        items: [{ cursor: "20", kind: "push.hint", data: {} }],
        nextCursor: "20",
      }),
      contacts: async () => page([]),
      capabilities: async () => ({}),
      async *watch(_account, _after, signal) {
        await new Promise((resolve) =>
          signal.aborted
            ? resolve()
            : signal.addEventListener("abort", resolve, { once: true }),
        );
      },
      ...overrides,
    },
    undefined,
    cache,
  );
  t.after(() => hub.close());
  return hub;
}

test("reload restores last chat, catches up a missed recall, and redacts older quoted excerpts", async (t) => {
  const persistent = cache(t);
  await seed(persistent);
  persistent.saveBlob(
    "media",
    "a",
    "preview",
    { blob: new Blob(["old photo"]), checkedAt: 1 },
    "original",
  );
  const hub = session(t, persistent, {
    events: async () => ({
      items: [
        {
          cursor: "20",
          kind: "message.recalled",
          data: { messageId: "original" },
        },
      ],
    }),
    message: async () =>
      message("original", "1", { body: null, recall: { actor: "self" } }),
  });
  hub.onMediaInvalidated((account, id) =>
    persistent.invalidateMessage(account, id),
  );
  const seen = [];
  hub.subscribe(() => seen.push(...hub.snapshot().messages));
  await hub.activate("a");
  assert.equal(hub.snapshot().selected, "peer");
  assert.equal(
    hub.snapshot().messages.find((item) => item.id === "original").body,
    null,
  );
  assert.equal(
    hub.snapshot().messages.find((item) => item.id === "quote").quote.state,
    "recalled",
  );
  assert.equal(
    seen.some(
      (item) =>
        item.body === "original" ||
        item.quote?.content === "private old excerpt",
    ),
    false,
  );
  await persistent.flush();
  assert.equal(await persistent.blob("media", "a", "preview"), undefined);
  assert.equal((await persistent.readAccount("a")).cursor, "20");
});

for (const reason of ["expired events", "rewound journal", "changed source"])
  test(`${reason} discards older persisted history before rendering`, async (t) => {
    const persistent = cache(t);
    await seed(persistent, reason === "rewound journal" ? "30" : "10");
    const hub = session(t, persistent, {
      ...(reason === "expired events"
        ? {
            events: async () => {
              throw new NativeApiError(410, "event_cursor_expired");
            },
          }
        : {}),
      messages: async () =>
        page([message("latest", "100")], {
          nextCursor: "fresh-page",
          sourceGeneration: reason === "changed source" ? "2" : "1",
        }),
    });
    await hub.activate("a");
    assert.deepEqual(
      hub.snapshot().messages.map((item) => item.id),
      ["latest"],
    );
    await persistent.flush();
    assert.equal((await persistent.readAccount("a")).cursor, "20");
  });

test("an unauthenticated account cannot read its persistent cache", async (t) => {
  const persistent = cache(t);
  await seed(persistent);
  let reads = 0;
  const read = persistent.readAccount.bind(persistent);
  persistent.readAccount = async (account) => {
    reads++;
    return read(account);
  };
  const hub = session(t, persistent, {
    accounts: async () => page([{ id: "a", authenticated: false }]),
  });
  await assert.rejects(hub.activate("a"), { code: "auth_required" });
  assert.equal(reads, 0);
  assert.equal(persistent.lastAccount(), undefined);
  assert.equal(await read("a"), undefined);
});

test("a failed first page cannot make unvalidated persisted bodies visible on retry", async (t) => {
  const persistent = cache(t);
  await seed(persistent);
  const hub = session(t, persistent, {
    messages: async () => {
      throw Error("offline");
    },
  });
  await hub.activate("a");
  assert.deepEqual(hub.snapshot().messages, []);
  let finish;
  hub.client.messages = () =>
    new Promise((resolve) => {
      finish = resolve;
    });
  const retry = hub.select("peer");
  for (let i = 0; i < 20 && !finish; i++)
    await new Promise((resolve) => setTimeout(resolve, 1));
  assert.deepEqual(hub.snapshot().messages, []);
  finish(page([message("latest", "100")], { sourceGeneration: "2" }));
  await retry;
  assert.deepEqual(
    hub.snapshot().messages.map((item) => item.id),
    ["latest"],
  );
});

test("late account validation cannot switch away from the more recent activation", async (t) => {
  const persistent = cache(t);
  let finish;
  const late = new Promise((resolve) => {
    finish = resolve;
  });
  let calls = 0;
  const hub = session(t, persistent, {
    accounts: async () =>
      ++calls === 1 ? late : page([{ id: "b", authenticated: true }]),
  });
  const first = hub.activate("a");
  await hub.activate("b");
  finish(page([{ id: "a", authenticated: true }]));
  await first;
  assert.equal(hub.snapshot().account, "b");
  assert.equal(persistent.lastAccount(), "b");
});
