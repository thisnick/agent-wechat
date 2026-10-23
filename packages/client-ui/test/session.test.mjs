import test from "node:test";
import assert from "node:assert/strict";
import { NativeApiError } from "@agent-wechat/shared";
import { HubSession } from "../src/session.ts";

const page = (items, cursor = "20", nextCursor = null) => ({
  items,
  nextCursor,
  asOfEventCursor: cursor,
  coverage: {},
  sourceGeneration: "1",
});
const message = (
  id,
  accountId = "a",
  threadId = "peer",
  createdAt = "10",
  body = id,
) => ({
  id,
  accountId,
  threadId,
  serverId: id,
  senderId: "peer",
  direction: "incoming",
  body,
  type: 1,
  createdAt,
  mentions: {
    state: "none",
    userIds: [],
    self: false,
    all: false,
    source: null,
  },
  version: "1",
  media: [],
});
const receipt = (state = "queued", extra = {}) => ({
  id: "send1",
  accountId: "a",
  destination: "peer",
  state,
  clientId: "12",
  serverId: null,
  errorCode: null,
  createdAt: "10",
  ...extra,
});
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
};
async function eventually(condition) {
  for (let i = 0; i < 100; i++) {
    if (condition()) return;
    await new Promise((done) => setTimeout(done, 10));
  }
  assert.ok(condition(), "condition did not become true");
}
function client(overrides = {}) {
  return {
    threads: async (account) =>
      page([
        {
          id: "peer",
          accountId: account,
          peerId: "peer",
          kind: "direct",
          displayName: "Peer",
          unreadCount: 0,
          readStateVersion: "1",
          history: {},
        },
      ]),
    contacts: async () => page([]),
    capabilities: async () => ({ mediaKinds: [] }),
    accounts: async () => ({
      items: [
        { id: "a", authenticated: true },
        { id: "b", authenticated: true },
      ],
    }),
    messages: async () => page([]),
    async *watch(_account, _after, signal) {
      await new Promise((done) => {
        if (signal.aborted) done();
        else signal.addEventListener("abort", done, { once: true });
      });
    },
    ...overrides,
  };
}
function session(t, overrides = {}) {
  const hub = new HubSession(client(overrides));
  t.after(() => hub.close());
  return hub;
}

test("a late read-state response cannot refresh the wrong account", async (t) => {
  const late = deferred(),
    seen = [];
  const hub = session(t, {
    markRead: async (account, thread, version) => {
      seen.push({ account, thread, version });
      return late.promise;
    },
  });
  await hub.activate("a");
  const reading = hub.markRead("peer");
  await hub.activate("b");
  late.resolve({ phoneReceiptSent: true });
  await reading;
  assert.deepEqual(seen, [{ account: "a", thread: "peer", version: "1" }]);
  assert.equal(hub.snapshot().account, "b");
});

test("media readiness reaches subscribers only for the active account", async (t) => {
  const hub = session(t),
    seen = [];
  await hub.activate("a");
  const unsubscribe = hub.onMediaReady((id) => seen.push(id));
  await hub.consume({ kind: "media.ready", data: { assetId: "asset-a" } }, "a");
  await hub.activate("b");
  await hub.consume({ kind: "media.ready", data: { assetId: "late-a" } }, "a");
  assert.deepEqual(seen, ["asset-a"]);
  unsubscribe();
});

test("history sorts decimal timestamps numerically and continues using the opaque cursor", async (t) => {
  const seen = [];
  const hub = session(t, {
    messages: async (_a, _t, options) => {
      seen.push(options.cursor);
      return options.cursor
        ? page([message("old", "a", "peer", "2")])
        : page(
            [
              message("new", "a", "peer", "100"),
              message("middle", "a", "peer", "9"),
            ],
            "20",
            "opaque",
          );
    },
  });
  await hub.activate("a");
  assert.deepEqual(
    hub.snapshot().messages.map((m) => m.id),
    ["middle", "new"],
  );
  await hub.older();
  assert.deepEqual(
    hub.snapshot().messages.map((m) => m.id),
    ["old", "middle", "new"],
  );
  assert.deepEqual(seen, [undefined, "opaque"]);
});

test("inventories follow every page and remove tombstones", async (t) => {
  const hub = session(t, {
    contacts: async (_a, options) =>
      options.cursor
        ? page([{ id: "gone", deleted: true, version: "2" }, { id: "second" }])
        : page([{ id: "first" }, { id: "gone" }], "20", "page2"),
  });
  await hub.activate("a");
  assert.deepEqual(
    hub.snapshot().contacts.map((c) => c.id),
    ["first", "second"],
  );
});

test("late account activation and account-list responses cannot overwrite another account", async (t) => {
  const late = deferred(),
    oldAccounts = deferred();
  let accountCalls = 0;
  const hub = session(t, {
    threads: async (a) =>
      a === "a" ? late.promise : page([{ id: "b-peer", accountId: "b" }], "90"),
    accounts: async () =>
      ++accountCalls === 1 ? oldAccounts.promise : { items: [{ id: "b" }] },
  });
  const first = hub.activate("a");
  await hub.activate("b");
  late.resolve(page([{ id: "a-peer" }], "30"));
  oldAccounts.resolve({ items: [{ id: "a" }] });
  await first;
  assert.equal(hub.snapshot().account, "b");
  assert.deepEqual(
    hub.snapshot().threads.map((x) => x.id),
    ["b-peer"],
  );
  assert.deepEqual(
    hub.snapshot().accounts.map((x) => x.id),
    ["b"],
  );
  assert.equal(hub.cursor, "90");
});

test("late thread responses cannot replace the selected conversation", async (t) => {
  const late = deferred();
  const hub = session(t, {
    messages: async (_a, thread) =>
      thread === "late"
        ? late.promise
        : page([message("current", "a", thread)]),
  });
  await hub.activate("a");
  const selecting = hub.select("late");
  await hub.select("current");
  late.resolve(page([message("stale", "a", "late")]));
  await selecting;
  assert.equal(hub.snapshot().selected, "current");
  assert.deepEqual(
    hub.snapshot().messages.map((m) => m.id),
    ["current"],
  );
});

test("committed events fetch content regardless of origin or sync event order", async (t) => {
  const bodies = new Map(
    [
      "incoming/live",
      "outgoing/live",
      "outgoing/local_send",
      "outgoing/catchup",
    ].map((kind, i) => [
      String(i),
      { ...message(String(i)), direction: kind.split("/")[0] },
    ]),
  );
  const fetched = [];
  const hub = session(t, {
    message: async (_a, id) => {
      fetched.push(id);
      return bodies.get(id);
    },
  });
  await hub.activate("a");
  for (const [id, m] of bodies)
    await hub.consume(
      {
        cursor: id,
        kind: "message.committed",
        data: { messageId: id, direction: m.direction, origin: "live" },
        createdAt: "1",
      },
      "a",
    );
  await hub.consume(
    {
      cursor: "9",
      kind: "projection.blocked",
      data: { messageId: "diagnostic" },
      createdAt: "1",
    },
    "a",
  );
  assert.equal(hub.snapshot().messages.length, 4);
  assert.deepEqual(fetched, ["0", "1", "2", "3"]);
});

test("events received during a history snapshot survive its late response", async (t) => {
  const late = deferred();
  let reads = 0;
  const hub = session(t, {
    messages: async () => (++reads === 1 ? page([]) : late.promise),
    message: async () => message("7", "a", "peer", "20", "fresh"),
  });
  await hub.activate("a");
  const selecting = hub.select("peer");
  await hub.consume(
    {
      cursor: "21",
      kind: "message.committed",
      data: { messageId: "7" },
      createdAt: "20",
    },
    "a",
  );
  late.resolve(page([message("7", "a", "peer", "20", "stale"), message("6")]));
  await selecting;
  assert.equal(hub.snapshot().messages.find((m) => m.id === "7").body, "fresh");
});

test("fast send events before the admission response reconcile without duplicate bubbles", async (t) => {
  const admission = deferred();
  let submissions = 0;
  const accepted = receipt("accepted", { serverId: "8", localMessageId: "8" });
  const hub = session(t, {
    sendText: async () => {
      submissions++;
      return admission.promise;
    },
    send: async () => accepted,
    message: async () => ({ ...message("8"), direction: "outgoing" }),
  });
  await hub.activate("a");
  const sending = hub.send("one send");
  await hub.consume(
    {
      cursor: "21",
      kind: "send.updated",
      data: { id: "send1" },
      createdAt: "1",
    },
    "a",
  );
  await hub.consume(
    {
      cursor: "22",
      kind: "message.committed",
      data: { messageId: "8" },
      createdAt: "1",
    },
    "a",
  );
  admission.resolve(receipt());
  await sending;
  assert.equal(submissions, 1);
  assert.equal(hub.snapshot().sends.length, 0);
  assert.deepEqual(
    hub.snapshot().messages.map((m) => m.id),
    ["8"],
  );
});

test("uncertain sends retain their key and check status without resubmitting", async (t) => {
  let submissions = 0,
    lookedUp;
  const hub = session(t, {
    sendText: async () => {
      submissions++;
      throw new Error("lost response");
    },
    sendByKey: async (_a, key) => {
      lookedUp = key;
      return receipt("needs_review");
    },
  });
  await hub.activate("a");
  await hub.send("uncertain");
  const key = hub.snapshot().sends[0].key;
  assert.equal(hub.snapshot().sends[0].uncertain, true);
  await hub.checkSend(key);
  assert.equal(lookedUp, key);
  assert.equal(submissions, 1);
  assert.equal(hub.snapshot().sends[0].intent.state, "needs_review");
});

test("switching accounts during a send retains its receipt only for the original account", async (t) => {
  const admission = deferred();
  const hub = session(t, {
    sendText: async () => admission.promise,
    sendByKey: async () => receipt("needs_review"),
  });
  await hub.activate("a");
  const sending = hub.send("for a");
  await hub.activate("b");
  admission.resolve(receipt("needs_review"));
  await sending;
  assert.equal(hub.snapshot().sends.length, 0);
  await hub.activate("a");
  await eventually(
    () => hub.snapshot().sends[0]?.intent?.state === "needs_review",
  );
  assert.equal(hub.snapshot().sends[0].text, "for a");
});

test("a quiet stream stays connecting until a real frame arrives", async (t) => {
  let activity;
  const hub = session(t, {
    async *watch(_a, _after, signal, options) {
      activity = options.onActivity;
      await new Promise((done) =>
        signal.addEventListener("abort", done, { once: true }),
      );
    },
  });
  await hub.activate("a");
  assert.equal(hub.snapshot().connected, false);
  activity();
  assert.equal(hub.snapshot().connected, true);
});

test("expired event cursors rebuild the snapshot and reconnect from its boundary", async (t) => {
  const starts = [];
  let views = 0;
  const hub = session(t, {
    threads: async () => page([{ id: "peer" }], ++views === 1 ? "20" : "50"),
    async *watch(_a, after, signal) {
      starts.push(after);
      if (starts.length === 1)
        throw new NativeApiError(410, "event_cursor_expired");
      await new Promise((done) =>
        signal.addEventListener("abort", done, { once: true }),
      );
    },
  });
  await hub.activate("a");
  await new Promise((done) => setTimeout(done, 1700));
  assert.deepEqual(starts, ["20", "50"]);
  assert.equal(hub.snapshot().selected, "peer");
});

test("group.updated refreshes membership and a confirmed leave blocks new sends", async (t) => {
  let joined = true,
    sends = 0;
  const room = "room@chatroom";
  const hub = session(t, {
    group: async () => ({ id: room, roomId: room, isInGroup: joined }),
    sendText: async () => {
      sends++;
      throw Error("should not send after leave");
    },
  });
  await hub.activate("a");
  await hub.select(room);
  assert.equal(hub.snapshot().selectedGroup.isInGroup, true);
  joined = false;
  await hub.consume(
    {
      accountId: "a",
      kind: "group.updated",
      data: { groupId: room },
      cursor: "21",
    },
    "a",
  );
  assert.equal(hub.snapshot().selectedGroup.isInGroup, false);
  await hub.send("After leaving");
  assert.equal(sends, 0);
  assert.match(hub.snapshot().error, /no longer in this group/);
});
test("a late group-info response cannot replace another conversation's membership", async (t) => {
  const late = deferred();
  const hub = session(t, { group: async () => late.promise });
  await hub.activate("a");
  const old = hub.select("room@chatroom");
  await hub.select("peer");
  late.resolve({ id: "room@chatroom", isInGroup: false });
  await old;
  assert.equal(hub.snapshot().selected, "peer");
  assert.equal(hub.snapshot().selectedGroup, null);
});

test("a temporary group lookup failure does not erase a confirmed leave", async (t) => {
  let failed = false;
  const hub = session(t, {
    group: async () => {
      if (failed) throw Error("offline");
      return { id: "room@chatroom", isInGroup: false };
    },
  });
  await hub.activate("a");
  await hub.select("room@chatroom");
  failed = true;
  await hub.refresh();
  assert.equal(hub.snapshot().selectedGroup.isInGroup, false);
  await hub.select("room@chatroom");
  assert.equal(hub.snapshot().selectedGroup.isInGroup, false);
});
