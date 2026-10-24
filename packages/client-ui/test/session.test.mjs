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
    thread: async (account, id) => ({
      id,
      accountId: account,
      peerId: id,
      kind: id.endsWith("@chatroom") ? "group" : "direct",
      displayName: "Peer",
      unreadCount: 0,
      readStateVersion: "1",
      history: {},
    }),
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

test("opening a chat clears unread status only after history loads, at its captured version", async (t) => {
  const history = deferred(),
    seen = [];
  let unread = 4,
    version = "7";
  const hub = session(t, {
    threads: async () =>
      page([{ id: "peer", unreadCount: unread, readStateVersion: version }]),
    thread: async () => ({
      id: "peer",
      unreadCount: unread,
      readStateVersion: version,
    }),
    messages: async () => history.promise,
    markRead: async (...args) => {
      seen.push(args);
      unread = 0;
      version = "9";
      return {};
    },
  });
  const opening = hub.activate("a");
  await eventually(() => hub.snapshot().selected === "peer");
  assert.equal(seen.length, 0);
  // A new incoming message changes the inventory while this snapshot is loading.
  version = "8";
  await hub.refresh();
  history.resolve(page([message("loaded")]));
  await opening;
  assert.deepEqual(seen, [["a", "peer", "7"]]);
  assert.equal(hub.snapshot().threads[0].unreadCount, 0);
  await hub.select("peer");
  assert.equal(
    seen.length,
    1,
    "already-read chats do not send another request",
  );
});

test("failed or superseded history never clears unread status", async (t) => {
  const late = deferred(),
    seen = [];
  const hub = session(t, {
    threads: async () =>
      page([{ id: "peer", unreadCount: 3, readStateVersion: "1" }]),
    thread: async (_account, id) => ({
      id,
      unreadCount: id === "peer" ? 3 : 0,
      readStateVersion: "1",
    }),
    messages: async (_account, thread) =>
      thread === "peer" ? late.promise : page([]),
    markRead: async (...args) => {
      seen.push(args);
    },
  });
  const opening = hub.activate("a");
  await eventually(() => hub.snapshot().selected === "peer");
  await hub.select("other");
  late.resolve(page([message("late")]));
  await opening;
  assert.deepEqual(seen, []);
  hub.client.messages = async () => {
    throw new Error("offline");
  };
  await hub.select("peer");
  assert.deepEqual(seen, []);
});

test("a repeated read conflict reloads history before retrying and leaves the newer unread count intact", async (t) => {
  let calls = 0;
  const hub = session(t, {
    threads: async () =>
      page([{ id: "peer", unreadCount: 2, readStateVersion: "1" }]),
    thread: async () => ({ id: "peer", unreadCount: 2, readStateVersion: "1" }),
    messages: async () => page([message("loaded")]),
    markRead: async () => {
      calls++;
      throw new NativeApiError(409, "read_state_conflict");
    },
  });
  await hub.activate("a");
  assert.equal(calls, 2);
  assert.equal(hub.snapshot().threads[0].unreadCount, 2);
  assert.equal(hub.snapshot().messages[0].id, "loaded");
  assert.match(hub.snapshot().error, /clear unread/);
});

for (const quoted of [false, true]) {
  test(`${quoted ? "a quoted group reply" : "a text reply"} clears unread arrivals in an already open chat`, async (t) => {
    const threadId = quoted ? "room@chatroom" : "peer";
    let unread = 0,
      version = "1",
      posts = 0;
    const marks = [];
    const thread = () => ({
      id: threadId,
      accountId: "a",
      unreadCount: unread,
      readStateVersion: version,
    });
    const target = message("1", "a", threadId);
    const intent = receipt("queued", { destination: threadId });
    const hub = session(t, {
      threads: async () => page([thread()]),
      thread: async () => thread(),
      capabilities: async () => ({ quotedTextSend: true }),
      messages: async () => page([target]),
      message: async () => message("2", "a", threadId),
      markRead: async (...args) => {
        marks.push(args);
        unread = 0;
        version = "3";
        return {};
      },
      sendText: async (...args) => {
        posts++;
        assert.deepEqual(
          args[4],
          quoted ? { messageId: "1", serverId: "1" } : undefined,
        );
        return intent;
      },
      send: async () => intent,
    });
    await hub.activate("a");
    await eventually(() => hub.snapshot().capabilities.quotedTextSend === true);
    unread = 3;
    version = "2";
    await hub.consume(
      { kind: "message.committed", data: { messageId: "2" } },
      "a",
    );
    assert.equal(hub.snapshot().threads[0].unreadCount, 3);
    assert.equal(await hub.send("A reply", quoted ? target : undefined), true);
    await eventually(() => hub.snapshot().threads[0].unreadCount === 0);
    assert.deepEqual(marks, [["a", threadId, "2"]]);
    assert.equal(posts, 1);
  });
}

test("a reply read conflict keeps newer arrivals unread and never retries the reply or a newer version", async (t) => {
  const reading = deferred(),
    marks = [];
  let unread = 0,
    version = "1",
    posts = 0;
  const thread = () => ({
    id: "peer",
    unreadCount: unread,
    readStateVersion: version,
  });
  const hub = session(t, {
    threads: async () => page([thread()]),
    thread: async () => thread(),
    messages: async () => page([message("1")]),
    markRead: async (...args) => {
      marks.push(args);
      await reading.promise;
      throw new NativeApiError(409, "read_state_conflict");
    },
    sendText: async () => {
      posts++;
      return receipt();
    },
    send: async () => receipt(),
  });
  await hub.activate("a");
  unread = 2;
  version = "2";
  await hub.refresh();
  assert.equal(await hub.send("Reply while new messages arrive"), true);
  unread = 3;
  version = "3";
  reading.resolve();
  await eventually(() => hub.snapshot().threads[0].readStateVersion === "3");
  assert.deepEqual(marks, [["a", "peer", "2"]]);
  assert.equal(hub.snapshot().threads[0].unreadCount, 3);
  assert.equal(hub.snapshot().error, null);
  assert.equal(posts, 1);
});

test("composer read acknowledgments deduplicate focus and reply actions and stay scoped across an account switch", async (t) => {
  const reading = deferred(),
    marks = [],
    reads = [];
  let unread = 0;
  const thread = (account) => ({
    id: "peer",
    accountId: account,
    unreadCount: account === "a" ? unread : 0,
    readStateVersion: account === "a" ? "2" : "b-version",
  });
  const hub = session(t, {
    threads: async (account) => page([thread(account)]),
    thread: async (account) => {
      reads.push(account);
      return thread(account);
    },
    markRead: async (...args) => {
      marks.push(args);
      await reading.promise;
      unread = 0;
      return {};
    },
  });
  await hub.activate("a");
  unread = 4;
  await hub.refresh();
  const first = hub.readOnInteraction("a", "peer");
  await hub.readOnInteraction("a", "peer");
  await hub.readOnInteraction("b", "peer");
  await hub.readOnInteraction("a", "another-chat");
  await hub.activate("b");
  const before = reads.length;
  reading.resolve();
  await first;
  assert.deepEqual(marks, [["a", "peer", "2"]]);
  assert.equal(reads.length, before);
  assert.equal(hub.snapshot().account, "b");
  assert.equal(hub.snapshot().threads[0].readStateVersion, "b-version");
});

test("reply read failures leave unread status intact without blocking or resubmitting the send", async (t) => {
  let unread = 0,
    posts = 0,
    marks = 0;
  const thread = () => ({
    id: "peer",
    unreadCount: unread,
    readStateVersion: "1",
  });
  const hub = session(t, {
    threads: async () => page([thread()]),
    thread: async () => thread(),
    markRead: async () => {
      marks++;
      throw Error("offline");
    },
    sendText: async () => {
      posts++;
      return receipt();
    },
    send: async () => receipt(),
  });
  await hub.activate("a");
  unread = 2;
  await hub.refresh();
  assert.equal(await hub.send("A reply"), true);
  await eventually(() => !!hub.snapshot().error);
  assert.equal(hub.snapshot().threads[0].unreadCount, 2);
  assert.equal(hub.snapshot().sends[0].intent.state, "queued");
  assert.match(hub.snapshot().error, /clear unread/);
  assert.equal(posts, 1);
  assert.equal(marks, 1);
});

test("replying while history is loading leaves read admission to the completed open", async (t) => {
  const history = deferred(),
    marks = [];
  let unread = 3;
  const thread = () => ({
    id: "peer",
    unreadCount: unread,
    readStateVersion: "1",
  });
  const hub = session(t, {
    threads: async () => page([thread()]),
    thread: async () => thread(),
    messages: async () => history.promise,
    markRead: async (...args) => {
      marks.push(args);
      unread = 0;
      return {};
    },
    sendText: async () => receipt(),
    send: async () => receipt(),
  });
  const opening = hub.activate("a");
  await eventually(() => hub.snapshot().selected === "peer");
  await hub.send("Reply before the history finishes loading");
  assert.deepEqual(marks, []);
  history.resolve(page([message("1")]));
  await opening;
  assert.deepEqual(marks, [["a", "peer", "1"]]);
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

test("switching back restores history immediately, revalidates fresh content and retains older pages", async (t) => {
  let refresh = false;
  const late = deferred();
  const hub = session(t, {
    messages: async (_account, thread, options) => {
      if (thread !== "peer") return page([]);
      if (refresh) return late.promise;
      return options.cursor
        ? page([message("old", "a", "peer", "1")])
        : page([message("recent", "a", "peer", "10")], "20", "older-page");
    },
  });
  await hub.activate("a");
  await hub.older();
  await hub.select("other");
  refresh = true;
  const reopening = hub.select("peer");
  assert.deepEqual(
    hub.snapshot().messages.map((m) => m.id),
    ["old", "recent"],
  );
  late.resolve(
    page(
      [message("recent", "a", "peer", "10", "Updated from server")],
      "21",
      "older-page",
    ),
  );
  await reopening;
  assert.deepEqual(
    hub.snapshot().messages.map((m) => m.id),
    ["old", "recent"],
  );
  assert.equal(hub.snapshot().messages[1].body, "Updated from server");
  assert.equal(
    hub.snapshot().nextMessages,
    null,
    "already-loaded history keeps its continuation",
  );
});

test("cached histories are account scoped and a failed revalidation leaves cached content visible", async (t) => {
  let fail = false;
  const hub = session(t, {
    messages: async (account) => {
      if (fail) throw Error("offline");
      return page([message(account, account)]);
    },
  });
  await hub.activate("a");
  await hub.activate("b");
  assert.equal(hub.snapshot().messages[0].body, "b");
  fail = true;
  await hub.activate("a");
  assert.equal(hub.snapshot().messages[0].body, "a");
  assert.ok(hub.snapshot().error);
});

test("recall events retrieve canonical notices and authors for inactive cached chats", async (t) => {
  const late = deferred();
  let refresh = false;
  let reads = 0;
  const recalled = {
    ...message("recent", "a", "peer", "10", "[Message recalled]"),
    type: 10000,
    senderId: "self",
    direction: "outgoing",
    recall: { observedAt: "11", replacementText: "You recalled a message" },
    version: "2",
  };
  const hub = session(t, {
    messages: async (_account, thread) =>
      refresh
        ? late.promise
        : page(thread === "peer" ? [message("recent")] : []),
    message: async () => {
      reads++;
      return recalled;
    },
  });
  await hub.activate("a");
  await hub.select("other");
  await hub.consume(
    {
      kind: "message.recalled",
      data: { messageId: "recent", replacementText: "Stale event notice" },
    },
    "a",
  );
  refresh = true;
  const reopening = hub.select("peer");
  assert.equal(reads, 1);
  assert.deepEqual(hub.snapshot().messages[0], recalled);
  assert.equal(
    hub.snapshot().threads.find((thread) => thread.id === "peer").lastMessage
      .text,
    "You recalled a message",
  );
  late.resolve(page([recalled]));
  await reopening;
});

test("revalidation removes missing current-page messages and retains only new in-flight events", async (t) => {
  let refreshed = false;
  const late = deferred();
  const hub = session(t, {
    messages: async (_account, thread) =>
      thread !== "peer"
        ? page([])
        : refreshed
          ? late.promise
          : page([message("deleted")]),
    message: async () => message("new"),
  });
  await hub.activate("a");
  await hub.select("other");
  refreshed = true;
  const reopening = hub.select("peer");
  await hub.consume(
    { kind: "message.committed", data: { messageId: "new" } },
    "a",
  );
  late.resolve(page([]));
  await reopening;
  assert.deepEqual(
    hub.snapshot().messages.map((m) => m.id),
    ["new"],
  );
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
  await eventually(() => hub.snapshot().contacts.length === 2);
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
  const listing = hub.accounts();
  await hub.activate("b");
  await eventually(() => hub.snapshot().accounts.some((a) => a.id === "b"));
  late.resolve(page([{ id: "a-peer" }], "30"));
  oldAccounts.resolve({ items: [{ id: "a" }] });
  await listing;
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

test("quoted sends retain exact references and their preview across an uncertain response without resubmitting", async (t) => {
  const calls = [];
  const target = { ...message("target"), serverId: "18446744073709551615" };
  const hub = session(t, {
    capabilities: async () => ({ quotedTextSend: true }),
    messages: async () => page([target]),
    sendText: async (...args) => {
      calls.push(args);
      throw new Error("lost response");
    },
    sendByKey: async () => receipt("needs_review"),
  });
  await hub.activate("a");
  await eventually(() => hub.snapshot().capabilities.quotedTextSend === true);
  assert.equal(await hub.send("A reply", target), true);
  const pending = hub.snapshot().sends[0];
  assert.deepEqual(calls[0].slice(0, 3), ["a", "peer", "A reply"]);
  assert.deepEqual(calls[0][4], {
    messageId: target.id,
    serverId: target.serverId,
  });
  assert.deepEqual(pending.quote, calls[0][4]);
  assert.equal(pending.quotePreview.text, "target");
  await hub.checkSend(pending.key);
  assert.equal(calls.length, 1);
  assert.deepEqual(hub.snapshot().sends[0].quote, pending.quote);
});

test("quoted send rejects foreign, recalled and changed targets instead of silently sending ordinary text", async (t) => {
  let posts = 0;
  let current = { ...message("target"), serverId: "99" };
  const hub = session(t, {
    capabilities: async () => ({ quotedTextSend: true }),
    messages: async () => page([current]),
    sendText: async () => {
      posts++;
      throw new Error("must not post");
    },
  });
  await hub.activate("a");
  await eventually(() => hub.snapshot().capabilities.quotedTextSend === true);
  const target = { ...current };
  for (const change of [
    { accountId: "b" },
    { threadId: "another" },
    { serverId: "100" },
  ])
    assert.equal(await hub.send("Reply", { ...target, ...change }), false);
  current = { ...current, recall: { observedAt: "now" } };
  await hub.select("peer");
  assert.equal(await hub.send("Reply", target), false);
  assert.equal(posts, 0);
  assert.equal(hub.snapshot().sends.length, 0);
});

test("definitive quote admission errors preserve the draft without an uncertain receipt or retry", async (t) => {
  let posts = 0;
  const target = { ...message("target"), serverId: "99" };
  const hub = session(t, {
    capabilities: async () => ({ quotedTextSend: true }),
    messages: async () => page([target]),
    sendText: async () => {
      posts++;
      throw new NativeApiError(404, "quote_target_unavailable", false);
    },
  });
  await hub.activate("a");
  await eventually(() => hub.snapshot().capabilities.quotedTextSend === true);
  assert.equal(await hub.send("Keep this draft", target), false);
  assert.equal(posts, 1);
  assert.equal(hub.snapshot().sends.length, 0);
  assert.match(hub.snapshot().error, /no longer available to quote/);
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

test("chat content does not wait for contacts or group details", async (t) => {
  const contacts = deferred(),
    group = deferred();
  const hub = session(t, {
    threads: async () =>
      page([{ id: "room@chatroom", unreadCount: 0, readStateVersion: "1" }]),
    contacts: async () => contacts.promise,
    group: async () => group.promise,
    messages: async () => page([message("ready", "a", "room@chatroom")]),
  });
  await hub.activate("a");
  assert.equal(hub.snapshot().messages[0].id, "ready");
  assert.equal(hub.snapshot().loading, false);
  group.resolve({ id: "room@chatroom", isInGroup: true });
  contacts.resolve(page([]));
});

test("SSE updates affected messages and read badges without reloading inventories", async (t) => {
  let lists = 0,
    contacts = 0,
    unread = 2;
  const targets = [];
  const hub = session(t, {
    threads: async () => {
      lists++;
      return page([{ id: "peer", unreadCount: 0, readStateVersion: "1" }]);
    },
    contacts: async () => {
      contacts++;
      return page([]);
    },
    thread: async (_a, id) => {
      targets.push(id);
      return { id, unreadCount: unread, readStateVersion: String(unread) };
    },
    messages: async () => page([]),
    markRead: async () => {
      unread = 0;
      return {};
    },
    message: async () => message("new"),
  });
  await hub.activate("a");
  await eventually(() => contacts === 1);
  const before = { lists, contacts };
  targets.length = 0;
  await hub.consume(
    { kind: "message.committed", data: { messageId: "new", threadId: "peer" } },
    "a",
  );
  await hub.consume(
    { kind: "thread.read-state", data: { threadId: "peer" } },
    "a",
  );
  await hub.consume(
    { kind: "metadata.changed", data: { kind: "avatar" } },
    "a",
  );
  assert.deepEqual({ lists, contacts }, before);
  assert.deepEqual(targets, ["peer", "peer"]);
  assert.equal(hub.snapshot().threads[0].unreadCount, 0);
  assert.equal(hub.snapshot().messages[0].id, "new");
  assert.equal(hub.snapshot().threads[0].lastMessage.text, "new");
});

test("continuous SSE lets a read cached chat reopen without another history request", async (t) => {
  let reads = 0;
  const hub = session(t, {
    messages: async (a, id) => {
      reads++;
      return page([message(id, a, id)]);
    },
    async *watch(_a, _cursor, signal, options) {
      options.onActivity();
      await new Promise((done) =>
        signal.addEventListener("abort", done, { once: true }),
      );
    },
  });
  await hub.activate("a");
  await hub.select("other");
  const before = reads;
  await hub.select("peer");
  assert.equal(reads, before);
  assert.equal(hub.snapshot().messages[0].id, "peer");
});

test("read conflicts fetch a fresh version and reload history before a second admission", async (t) => {
  const calls = [];
  let version = 2,
    unread = 1;
  const hub = session(t, {
    threads: async () =>
      page([{ id: "peer", unreadCount: 1, readStateVersion: "stale-list" }]),
    thread: async () => {
      calls.push(`thread:${version}`);
      return {
        id: "peer",
        unreadCount: unread,
        readStateVersion: String(version),
      };
    },
    messages: async () => {
      calls.push(`history:${version}`);
      return page([message(`loaded-${version}`)]);
    },
    markRead: async (_a, _t, v) => {
      calls.push(`read:${v}`);
      if (version === 2) {
        version = 3;
        throw new NativeApiError(409, "read_state_conflict");
      }
      unread = 0;
      return {};
    },
  });
  await hub.activate("a");
  assert.deepEqual(calls, [
    "thread:2",
    "history:2",
    "read:2",
    "thread:3",
    "history:3",
    "read:3",
    "thread:3",
  ]);
  assert.equal(hub.snapshot().threads[0].unreadCount, 0);
  assert.equal(hub.snapshot().messages[0].id, "loaded-3");
});

test("a new direct chat can open before the hub has a session row", async (t) => {
  const hub = session(t, {
    thread: async () => {
      throw new NativeApiError(404, "not_found");
    },
  });
  await hub.activate("a");
  await hub.select("new-person");
  assert.equal(hub.snapshot().selected, "new-person");
  assert.equal(hub.snapshot().error, null);
});

test("quote navigation loads intervening history pages until its source is available", async (t) => {
  const cursors = [];
  const hub = session(t, {
    messages: async (_a, _t, options) => {
      cursors.push(options.cursor);
      if (!options.cursor)
        return page([message("new", "a", "peer", "30")], "20", "p2");
      if (options.cursor === "p2")
        return page([message("middle", "a", "peer", "20")], "20", "p3");
      return page([message("source", "a", "peer", "10")]);
    },
  });
  await hub.activate("a");
  assert.equal(
    await hub.revealMessage("source", new AbortController().signal),
    true,
  );
  assert.deepEqual(cursors, [undefined, "p2", "p3"]);
  assert.deepEqual(
    hub.snapshot().messages.map((m) => m.id),
    ["source", "middle", "new"],
  );
});
test("a quote navigation stops on account/chat change and does not follow another chat's cursor", async (t) => {
  const gate = deferred();
  let older = 0;
  const hub = session(t, {
    messages: async (account, thread, options) => {
      if (options.cursor) {
        older++;
        return gate.promise;
      }
      return page([message("new", account, thread)], "20", "older");
    },
  });
  await hub.activate("a");
  const finding = hub.revealMessage("source", new AbortController().signal);
  await hub.select("elsewhere");
  gate.resolve(page([message("source")]));
  assert.equal(await finding, false);
  assert.equal(older, 1);
  assert.equal(hub.snapshot().selected, "elsewhere");
  assert.equal(
    hub.snapshot().messages.some((m) => m.id === "source"),
    false,
  );
});

test("deactivating a profile clears its messages and fences a late chat-open response", async () => {
  const pending = deferred();
  const session = new HubSession(
    client({ threads: async () => pending.promise }),
  );
  const activating = session.activate("a");
  session.deactivate("a");
  pending.resolve(
    page([
      {
        id: "peer",
        accountId: "a",
        peerId: "peer",
        kind: "direct",
        history: {},
      },
    ]),
  );
  await activating;
  assert.equal(session.snapshot().account, null);
  assert.deepEqual(session.snapshot().threads, []);
  assert.deepEqual(session.snapshot().messages, []);
  assert.equal(session.snapshot().connected, false);
  session.close();
});

test("recall events invalidate preview bytes before a canonical lookup completes", async () => {
  const late = deferred();
  const session = new HubSession(client({ message: () => late.promise }));
  await session.activate("a");
  const invalidations = [];
  const stop = session.onMediaInvalidated((account, messageId) =>
    invalidations.push([account, messageId]),
  );
  const consuming = session.consume(
    { kind: "message.recalled", data: { messageId: "photo" } },
    "a",
  );
  assert.deepEqual(invalidations, [["a", "photo"]]);
  late.resolve({
    ...message("photo"),
    recall: { observedAt: "11" },
    type: 10000,
  });
  await consuming;
  stop();
  await session.consume(
    { kind: "message.recalled", data: { messageId: "foreign" } },
    "b",
  );
  assert.ok(
    invalidations.every(([account, id]) => account === "a" && id === "photo"),
  );
  session.close();
});

test("composer acknowledgments stay deduplicated while the updated unread badge is loading", async (t) => {
  const refreshed = deferred();
  let unread = 0,
    posts = 0,
    refreshing = false;
  const view = (count) => ({
    id: "peer",
    accountId: "a",
    unreadCount: count,
    readStateVersion: "2",
  });
  const hub = session(t, {
    threads: async () => page([view(unread)]),
    thread: async () => (refreshing ? refreshed.promise : view(unread)),
    markRead: async () => {
      posts++;
      refreshing = true;
      return {};
    },
  });
  await hub.activate("a");
  unread = 3;
  await hub.refresh();
  const focusing = hub.readOnInteraction("a", "peer");
  await eventually(() => refreshing);
  const clicking = hub.readOnInteraction("a", "peer");
  await Promise.resolve();
  assert.equal(posts, 1);
  refreshed.resolve(view(0));
  await Promise.all([focusing, clicking]);
  assert.equal(hub.snapshot().threads[0].unreadCount, 0);
});
