import test from "node:test";
import assert from "node:assert/strict";
import { IDBFactory } from "fake-indexeddb";
import { HubSession } from "../src/session.ts";
import { BrowserCache } from "../src/browser-cache.ts";
const page = (items, cursor = "20", nextCursor = null) => ({
  items,
  asOfEventCursor: cursor,
  nextCursor,
  sourceGeneration: "s",
  coverage: {},
});
const thread = (id, pinned = false) => ({
  id,
  accountId: "a",
  peerId: id,
  kind: id.endsWith("@chatroom") ? "group" : "direct",
  pinned,
  unreadCount: 0,
  readStateVersion: "v",
  lastMessage: null,
  history: {},
});
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
};
async function until(fn) {
  for (let i = 0; i < 200; i++) {
    if (fn()) return;
    await new Promise((done) => setTimeout(done, 10));
  }
  assert.ok(fn());
}
function setup(t, overrides = {}, cache) {
  const calls = { lists: [], points: [], marks: 0, catalog: 0 };
  const client = {
    accounts: async () =>
      page([
        { id: "a", authenticated: true },
        { id: "b", authenticated: true },
      ]),
    threads: async (a, options) => {
      calls.lists.push(options.cursor);
      return page([thread("active"), thread("old")]);
    },
    thread: async (_a, id) => {
      calls.points.push(id);
      return thread(id);
    },
    messages: async (a, id) =>
      page([
        {
          id: "m",
          accountId: a,
          threadId: id,
          createdAt: "1",
          body: "saved",
          type: 1,
          media: [],
        },
      ]),
    markRead: async () => {
      calls.marks++;
    },
    contacts: async () => {
      calls.catalog++;
      return page([]);
    },
    capabilities: async () => ({ threadPinningTargets: ["direct", "group"] }),
    async *watch(_a, _after, signal, options) {
      options.onActivity();
      await new Promise((done) =>
        signal.addEventListener("abort", done, { once: true }),
      );
    },
    ...overrides,
  };
  const hub = new HubSession(client, undefined, cache);
  t.after(() => hub.close());
  return { hub, calls, client };
}
test("phone and thread-only native pin events refresh authoritative order without opening the affected chat", async (t) => {
  let rows = [thread("active"), { ...thread("old"), unreadCount: 7 }];
  let lists = 0;
  const f = setup(t, {
    threads: async () => {
      lists++;
      return page(rows);
    },
  });
  await f.hub.activate("a");
  const messages = f.hub.snapshot().messages;
  const points = f.calls.points.length;
  rows = [{ ...rows[1], pinned: true }, rows[0]];
  await f.hub.consume(
    {
      kind: "thread.updated",
      data: { threadId: "old", source: "native_mutation" },
    },
    "a",
  );
  assert.deepEqual(
    f.hub.snapshot().threads.map((x) => [x.id, x.pinned]),
    [
      ["old", true],
      ["active", false],
    ],
  );
  assert.equal(f.hub.snapshot().selected, "active");
  assert.equal(f.hub.snapshot().messages, messages);
  assert.equal(f.hub.snapshot().threads[0].unreadCount, 7);
  assert.equal(f.calls.marks, 0);
  assert.equal(f.calls.points.length, points);
  rows = [rows[1], { ...rows[0], pinned: false }];
  await f.hub.consume(
    {
      kind: "thread.updated",
      data: { threadId: "old", origin: "contact_sync" },
    },
    "a",
  );
  assert.deepEqual(
    f.hub.snapshot().threads.map((x) => x.id),
    ["active", "old"],
  );
  assert.equal(lists, 3);
});
test("overlapping thread, group and contact changes share a fresh first-page inventory", async (t) => {
  const f = setup(t);
  await f.hub.activate("a");
  await until(() => f.calls.catalog === 1);
  await Promise.all([
    f.hub.consume({ kind: "thread.updated", data: { threadId: "old" } }, "a"),
    f.hub.consume(
      { kind: "group.updated", data: { groupId: "room@chatroom" } },
      "a",
    ),
    f.hub.consume(
      { kind: "contacts.updated", data: { contactId: "old" } },
      "a",
    ),
    f.hub.consume({ kind: "sync.completed", data: {} }, "a"),
  ]);
  assert.deepEqual(f.calls.lists, [undefined, undefined]);
  assert.equal(f.calls.catalog, 2);
});
test("new pin ordering discards old list responses and never uses an old continuation", async (t) => {
  const late = deferred();
  let count = 0,
    pinned = false;
  const cursors = [];
  const f = setup(t, {
    threads: async (_a, options) => {
      cursors.push(options.cursor);
      count++;
      if (count === 3) return late.promise;
      const rows = pinned
        ? [thread("old", true), thread("active")]
        : [thread("active"), thread("old")];
      if (options.cursor) return page([rows[1]]);
      return page([rows[0]], "20", pinned ? "pinned-page" : "initial-page");
    },
  });
  await f.hub.activate("a");
  const obsolete = f.hub.refresh("a", undefined, false);
  await until(() => count === 2 + 1);
  pinned = true;
  await f.hub.consume(
    { kind: "thread.updated", data: { threadId: "old" } },
    "a",
  );
  late.resolve(page([thread("active"), thread("old")]));
  await obsolete;
  assert.deepEqual(
    f.hub.snapshot().threads.map((x) => x.id),
    ["old", "active"],
  );
  assert.equal(cursors.at(-2), undefined);
  assert.equal(cursors.at(-1), "pinned-page");
});
test("a late point read cannot undo a newer pinned inventory", async (t) => {
  const late = deferred();
  let slow = false,
    pinned = false;
  const f = setup(t, {
    threads: async () =>
      page(
        pinned
          ? [thread("old", true), thread("active")]
          : [thread("active"), thread("old")],
      ),
    thread: async (_a, id) => (slow ? late.promise : thread(id)),
  });
  await f.hub.activate("a");
  slow = true;
  const opening = f.hub.select("old");
  await Promise.resolve();
  pinned = true;
  await f.hub.consume(
    { kind: "thread.updated", data: { threadId: "old" } },
    "a",
  );
  late.resolve(thread("old", false));
  await opening;
  assert.equal(f.hub.snapshot().threads[0].id, "old");
  assert.equal(f.hub.snapshot().threads[0].pinned, true);
});
test("an older inventory cannot overwrite a pin after a new inventory completes", async (t) => {
  const late = deferred();
  let slow = false,
    pinned = false;
  const f = setup(t, {
    threads: async () => {
      if (slow) {
        slow = false;
        return late.promise;
      }
      return page(
        pinned
          ? [thread("old", true), thread("active")]
          : [thread("active"), thread("old")],
      );
    },
  });
  await f.hub.activate("a");
  slow = true;
  const old = f.hub.refresh("a", undefined, false);
  await Promise.resolve();
  pinned = true;
  await f.hub.consume(
    { kind: "thread.updated", data: { threadId: "old" } },
    "a",
  );
  late.resolve(page([thread("active"), thread("old")]));
  await old;
  assert.equal(f.hub.snapshot().threads[0].pinned, true);
});
test("a refresh from a previous account cannot change the new account", async (t) => {
  const late = deferred();
  let slow = false;
  const f = setup(t, {
    threads: async (a) =>
      slow && a === "a" ? late.promise : page([{ ...thread(a), accountId: a }]),
  });
  await f.hub.activate("a");
  slow = true;
  const old = f.hub.refreshConversations("a");
  await new Promise((done) => setTimeout(done, 60));
  await f.hub.activate("b");
  late.resolve(page([thread("old", true)]));
  await old;
  assert.equal(f.hub.snapshot().account, "b");
  assert.deepEqual(
    f.hub.snapshot().threads.map((x) => x.id),
    ["b"],
  );
});
test("pin state and server order survive persistence and thread-only replay", async (t) => {
  const disk = new BrowserCache("https://hub.test", "hub", {
    factory: new IDBFactory(),
    broadcast: false,
  });
  t.after(() => disk.close());
  let rows = [thread("old", true), thread("active")];
  const f = setup(
    t,
    {
      threads: async () => page(rows),
      thread: async (_a, id) => rows.find((row) => row.id === id),
    },
    disk,
  );
  await f.hub.activate("a");
  await disk.flush();
  assert.deepEqual(
    (await disk.readAccount("a")).threads.map((x) => [x.id, x.pinned]),
    [
      ["old", true],
      ["active", false],
    ],
  );
  f.hub.close();
  rows = [thread("active"), thread("old", false)];
  const restored = setup(
    t,
    {
      threads: async () => page(rows, "21"),
      thread: async (_a, id) => rows.find((row) => row.id === id),
      events: async () =>
        page(
          [{ cursor: "21", kind: "thread.updated", data: { threadId: "old" } }],
          "21",
        ),
    },
    disk,
  );
  await restored.hub.activate("a");
  await disk.flush();
  assert.deepEqual(
    restored.hub.snapshot().threads.map((x) => x.id),
    ["active", "old"],
  );
  assert.equal((await disk.readAccount("a")).threads[1].pinned, false);
  assert.equal(
    restored.calls.points.length,
    0,
    "replay uses the fresh inventory rather than per-event point reads",
  );
});

test("reconnecting refreshes server order and resumes after thread-only events", async (t) => {
  const disconnect = deferred();
  let pinned = false,
    streams = 0;
  const f = setup(t, {
    threads: async () =>
      page(
        pinned
          ? [thread("old", true), thread("active")]
          : [thread("active"), thread("old")],
      ),
    async *watch(_account, after, signal, options) {
      streams++;
      options.onActivity();
      if (streams === 1) {
        pinned = true;
        yield {
          cursor: "21",
          kind: "thread.updated",
          data: { threadId: "old" },
        };
        await disconnect.promise;
        throw Error("disconnected");
      }
      assert.equal(after, "21");
      await new Promise((done) =>
        signal.addEventListener("abort", done, { once: true }),
      );
    },
  });
  await f.hub.activate("a");
  await until(() => f.hub.snapshot().threads[0].pinned === true);
  pinned = false;
  disconnect.resolve();
  await until(() => streams === 2 && f.hub.snapshot().connected);
  assert.deepEqual(
    f.hub.snapshot().threads.map((x) => x.id),
    ["active", "old"],
  );
  assert.equal(f.hub.snapshot().selected, "active");
});

test("a selected group's overlapping contact and group events do not duplicate group reads", async (t) => {
  let groups = 0;
  const f = setup(t, {
    threads: async () => page([thread("room@chatroom")]),
    group: async () => {
      groups++;
      return { id: "room@chatroom", accountId: "a", isInGroup: true };
    },
  });
  await f.hub.activate("a");
  await until(() => groups === 1);
  await Promise.all([
    f.hub.consume(
      { kind: "group.updated", data: { groupId: "room@chatroom" } },
      "a",
    ),
    f.hub.consume(
      { kind: "contacts.updated", data: { contactId: "someone" } },
      "a",
    ),
  ]);
  assert.equal(groups, 2, "one initial read and one coalesced refresh");
});
