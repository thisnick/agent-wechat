import test from "node:test";
import assert from "node:assert/strict";
import { IDBFactory, IDBIndex, IDBObjectStore } from "fake-indexeddb";
import { BrowserCache } from "../src/browser-cache.ts";
import { savedConversationCategories } from "../src/conversation-filters.ts";
import { MediaPreviewCache } from "../src/media-preview-cache.ts";
import { AvatarCache } from "../src/avatar-cache.ts";

export function memoryStorage() {
  const values = new Map();
  return {
    getItem: (key) => values.get(key) ?? null,
    setItem: (key, value) => values.set(key, value),
    removeItem: (key) => values.delete(key),
  };
}
const snapshot = (account = "a", cursor = "90071992547409930001") => ({
  threads: [{ id: "peer", accountId: account }],
  contacts: [{ id: "peer", displayName: "Peer" }],
  histories: [
    [
      "peer",
      {
        messages: [
          {
            id: "local-1",
            serverId: "18446744073709551615",
            accountId: account,
            threadId: "peer",
            body: "hello",
            quote: {
              serverId: "18446744073709551614",
              state: "available",
              content: "quote",
            },
          },
        ],
        nextMessages: "opaque-page",
        sourceGeneration: "profile-1",
        validatedAt: 1793060000000,
        group: null,
      },
    ],
  ],
  cursor,
});
function setup(t, options = {}) {
  const factory = new IDBFactory(),
    storage = memoryStorage();
  const open = (origin = "https://hub.test", hub = "hub-1") => {
    const cache = new BrowserCache(origin, hub, {
      factory,
      storage,
      broadcast: false,
      ...options,
    });
    t.after(() => cache.close());
    return cache;
  };
  return { factory, open, storage };
}

const idbResult = (request) =>
  new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });

test("restart reconstructs normalized history and preserves opaque and 64-bit IDs", async (t) => {
  const { open } = setup(t);
  const first = open();
  await first.writeAccount("a", snapshot());
  first.rememberAccount("a");
  first.saveView("a", { thread: "peer", filter: "unread" });
  await first.close();
  const second = open();
  assert.deepEqual(await second.readAccount("a"), snapshot());
  assert.equal(second.lastAccount(), "a");
  assert.deepEqual(second.view("a"), { thread: "peer", filter: "unread" });
  assert.equal(await second.readAccount("b"), undefined);
  assert.equal(await open("https://other.test").readAccount("a"), undefined);
  assert.equal(
    await open("https://hub.test", "hub-2").readAccount("a"),
    undefined,
  );
  assert.equal(open("https://other.test").lastAccount(), undefined);
});

test("saved group filters carry over and an empty category selection takes precedence", (t) => {
  const cache = setup(t).open();
  cache.saveView("a", { filter: "groups" });
  assert.deepEqual(savedConversationCategories(cache.view("a")), ["group"]);
  cache.saveView("a", { categories: [] });
  assert.deepEqual(savedConversationCategories(cache.view("a")), []);
  cache.saveView("b", { filter: "unread" });
  assert.deepEqual(savedConversationCategories(cache.view("b")), []);
});

test("an older tab cannot overwrite a newer atomic snapshot or rewind its cursor", async (t) => {
  const { open } = setup(t);
  const fast = open(),
    slow = open();
  await fast.writeAccount("a", snapshot());
  await slow.writeAccount("a", snapshot("a", "90071992547409930000"));
  await slow.checkpoint("a", "90071992547409929999");
  assert.equal((await fast.readAccount("a")).cursor, "90071992547409930001");
  await fast.checkpoint("a", "90071992547409930002");
  assert.equal((await slow.readAccount("a")).cursor, "90071992547409930002");
});

test("clear fetched cache preserves drafts and independent pending-send receipts across tabs", async (t) => {
  const { open } = setup(t);
  const one = open(),
    two = open();
  await one.writeAccount("a", snapshot());
  one.writeState(
    "a",
    "peer",
    {
      text: "unfinished",
      quote: { messageId: "m", serverId: "18446744073709551615" },
    },
    "drafts",
  );
  one.writeState("a", "send:one", { key: "one", uncertain: true });
  two.writeState("a", "send:two", { key: "two", uncertain: true });
  one.saveBlob(
    "media",
    "a",
    "image",
    { blob: new Blob(["photo"]), checkedAt: 1 },
    "m",
  );
  await Promise.all([one.flush(), two.flush()]);
  await one.clearAccount("a");
  const restarted = open();
  await restarted.primeRecovery(["a"]);
  assert.equal(await restarted.readAccount("a"), undefined);
  assert.equal(await restarted.blob("media", "a", "image"), undefined);
  assert.equal(restarted.readState("a", "peer", "drafts").text, "unfinished");
  assert.deepEqual(
    restarted
      .states("a", "send:")
      .map((value) => value.key)
      .sort(),
    ["one", "two"],
  );
  assert.equal(restarted.readState("b", "send:one"), undefined);
});

test("blob budget evicts least recently used bytes across media and avatars", async (t) => {
  let now = 1;
  const { open } = setup(t, { mediaBudget: 8, now: () => now++ });
  const cache = open();
  cache.saveBlob(
    "media",
    "a",
    "one",
    { blob: new Blob(["1111"]), checkedAt: 1 },
    "m1",
  );
  cache.saveBlob("avatars", "a", "peer", {
    blob: new Blob(["2222"]),
    checkedAt: 2,
  });
  await cache.blob("media", "a", "one");
  cache.saveBlob(
    "media",
    "b",
    "three",
    { blob: new Blob(["3333"]), checkedAt: 3 },
    "m3",
  );
  await cache.flush();
  assert.equal(await cache.blob("avatars", "a", "peer"), undefined);
  assert.equal(
    await (await cache.blob("media", "a", "one")).blob.text(),
    "1111",
  );
  cache.invalidateMessage("a", "m1");
  await cache.flush();
  assert.equal(await cache.blob("media", "a", "one"), undefined);
  assert.equal((await cache.stats("a")).bytes, 0);
  assert.equal((await cache.stats("b")).bytes, 4);
  assert.equal(
    await (await cache.blob("media", "b", "three")).blob.text(),
    "3333",
  );
});

test("persisted media and avatar hits update only recency rows and never load payloads for stats, eviction, or invalidation", async (t) => {
  let now = 1;
  const { factory, open } = setup(t, { mediaBudget: 16, now: () => now });
  const cache = open();
  cache.saveBlob(
    "media",
    "a",
    "image",
    { blob: new Blob(["photo"], { type: "image/png" }), checkedAt: 50 },
    "m",
  );
  cache.saveBlob("avatars", "a", "peer", {
    blob: new Blob(["face!"], { type: "image/png" }),
    checkedAt: 50,
  });
  await cache.flush();
  const db = await idbResult(factory.open("agent-wechat-cache", 2));
  t.after(() => db.close());
  const mediaKey = ["https://hub.test", "hub-1", "a", "image"];
  const avatarKey = ["https://hub.test", "hub-1", "a", "peer"];
  const peek = (store, key) =>
    idbResult(db.transaction(store).objectStore(store).get(key));
  const mediaBytes = await peek("media", mediaKey);
  const avatarBytes = await peek("avatars", avatarKey);
  assert.equal("lastUsed" in mediaBytes, false);
  assert.equal("lastUsed" in avatarBytes, false);
  assert.equal("data" in (await peek("mediaMeta", mediaKey)), false);
  assert.equal("data" in (await peek("avatarMeta", avatarKey)), false);

  const puts = t.mock.method(IDBObjectStore.prototype, "put");
  now = 10;
  assert.equal(
    await (await cache.blob("media", "a", "image")).blob.text(),
    "photo",
  );
  now = 11;
  assert.equal(
    await (await cache.blob("avatars", "a", "peer")).blob.text(),
    "face!",
  );
  assert.deepEqual(
    puts.mock.calls.map(({ this: store }) => store.name),
    ["mediaMeta", "avatarMeta"],
  );
  assert.deepEqual(await peek("media", mediaKey), mediaBytes);
  assert.deepEqual(await peek("avatars", avatarKey), avatarBytes);
  assert.deepEqual(await peek("mediaMeta", mediaKey), {
    key: mediaKey,
    scope: JSON.stringify(mediaKey.slice(0, -1)),
    lastUsed: 10,
    bytes: 5,
    messageId: "m",
  });
  assert.deepEqual(await peek("avatarMeta", avatarKey), {
    key: avatarKey,
    scope: JSON.stringify(avatarKey.slice(0, -1)),
    lastUsed: 11,
    bytes: 5,
    messageId: undefined,
  });

  // stats(), LRU eviction, and invalidateMessage() must never read payload stores.
  const payloadReads = [];
  const trackRead = (storeName, op) => {
    if (storeName === "media" || storeName === "avatars")
      payloadReads.push(`${storeName}.${op}`);
  };
  const origStoreGetAll = IDBObjectStore.prototype.getAll;
  t.mock.method(IDBObjectStore.prototype, "getAll", function (...args) {
    trackRead(this.name, "getAll");
    return origStoreGetAll.apply(this, args);
  });
  const origIndexGetAll = IDBIndex.prototype.getAll;
  t.mock.method(IDBIndex.prototype, "getAll", function (...args) {
    trackRead(this.objectStore.name, `index(${this.name}).getAll`);
    return origIndexGetAll.apply(this, args);
  });
  const origStoreGet = IDBObjectStore.prototype.get;
  t.mock.method(IDBObjectStore.prototype, "get", function (...args) {
    trackRead(this.name, "get");
    return origStoreGet.apply(this, args);
  });
  const origOpenCursor = IDBObjectStore.prototype.openCursor;
  t.mock.method(IDBObjectStore.prototype, "openCursor", function (...args) {
    trackRead(this.name, "openCursor");
    return origOpenCursor.apply(this, args);
  });

  assert.equal((await cache.stats("a")).bytes, 10);
  now = 20;
  cache.saveBlob(
    "media",
    "a",
    "second",
    { blob: new Blob(["1234567890"]), checkedAt: 60 },
    "m2",
  );
  await cache.flush();
  cache.invalidateMessage("a", "m2");
  await cache.flush();
  assert.deepEqual(payloadReads, []);
  // "image" (lastUsed=10) was evicted by 10+10>16, "peer" (lastUsed=11) survived, "second" was invalidated.
  assert.equal((await cache.stats("a")).bytes, 5);

  await cache.clearAccount("a");
  assert.equal(await peek("media", mediaKey), undefined);
  assert.equal(await peek("mediaMeta", mediaKey), undefined);
  assert.equal(await peek("avatars", avatarKey), undefined);
  assert.equal(await peek("avatarMeta", avatarKey), undefined);
});

test("an unreadable legacy media or avatar Blob removes its payload and metadata", async (t) => {
  for (const [kind, metaName] of [
    ["media", "mediaMeta"],
    ["avatars", "avatarMeta"],
  ]) {
    await t.test(kind, async (t) => {
      const { factory, open } = setup(t);
      const cache = open();
      const db = await idbResult(factory.open("agent-wechat-cache", 2));
      t.after(() => db.close());
      const key = ["https://hub.test", "hub-1", "a", "item"];
      const scope = JSON.stringify(key.slice(0, -1));
      const blob = new Blob(["photo"]);
      const tx = db.transaction([kind, metaName], "readwrite");
      await Promise.all([
        idbResult(
          tx.objectStore(kind).put({ key, scope, data: { blob, checkedAt: 1 } }),
        ),
        idbResult(
          tx
            .objectStore(metaName)
            .put({ key, scope, lastUsed: 1, bytes: blob.size }),
        ),
      ]);
      assert.equal((await cache.stats("a")).bytes, blob.size);
      const arrayBuffer = t.mock.method(
        Blob.prototype,
        "arrayBuffer",
        async () => {
          throw new DOMException(
            "Backing file is unreadable",
            "NotReadableError",
          );
        },
      );

      assert.equal(await cache.blob(kind, "a", "item"), undefined);
      assert.equal(arrayBuffer.mock.callCount(), 1);
      for (const store of [kind, metaName]) {
        const row = db.transaction(store).objectStore(store).get(key);
        assert.equal(
          await idbResult(row),
          undefined,
          `${store} row was not removed`,
        );
      }
      const stats = await cache.stats("a");
      assert.equal(stats.available, true);
      assert.equal(stats.bytes, 0);
    });
  }
});

test("a blob hit repairs missing metadata for an orphan payload row", async (t) => {
  const { factory, open } = setup(t, { now: () => 42 });
  const cache = open();
  cache.saveBlob(
    "media",
    "a",
    "orphan",
    { blob: new Blob(["hello"], { type: "text/plain" }), checkedAt: 1 },
    "m-orphan",
  );
  await cache.flush();
  const db = await idbResult(factory.open("agent-wechat-cache", 2));
  t.after(() => db.close());
  const key = ["https://hub.test", "hub-1", "a", "orphan"];
  // Remove only the metadata row to simulate an orphan v2 payload entry.
  await idbResult(
    db.transaction("mediaMeta", "readwrite").objectStore("mediaMeta").delete(key),
  );
  assert.equal((await cache.stats("a")).bytes, 0);
  assert.equal(
    await (await cache.blob("media", "a", "orphan")).blob.text(),
    "hello",
  );
  assert.equal((await cache.stats("a")).bytes, 5);
  cache.invalidateMessage("a", "m-orphan");
  await cache.flush();
  assert.equal(await cache.blob("media", "a", "orphan"), undefined);
  assert.equal((await cache.stats("a")).bytes, 0);
});

test("v1 upgrade preserves media and avatar payloads, normalizes metadata, and supports cross-store LRU and invalidation", async (t) => {
  let now = 30;
  const { factory, open } = setup(t, { mediaBudget: 12, now: () => now++ });
  const v1 = factory.open("agent-wechat-cache", 1);
  const key = (account, id) => ["https://hub.test", "hub-1", account, id];
  const storedBytes = (text) => new TextEncoder().encode(text).buffer;
  v1.onupgradeneeded = () => {
    for (const name of [
      "threads",
      "contacts",
      "messages",
      "chatState",
      "syncState",
      "avatars",
      "media",
      "drafts",
      "receipts",
    ])
      v1.result
        .createObjectStore(name, { keyPath: "key" })
        .createIndex("scope", "scope");
    const media = v1.transaction.objectStore("media");
    // Legacy Blob format
    media.put({
      key: key("a", "older"),
      scope: JSON.stringify(key("a", "older").slice(0, -1)),
      data: { blob: new Blob(["old!"]), checkedAt: 10 },
      lastUsed: 10,
      bytes: 4,
      messageId: "same-message",
    });
    // Current StoredBlob (ArrayBuffer) format
    media.put({
      key: key("a", "newer"),
      scope: JSON.stringify(key("a", "newer").slice(0, -1)),
      data: {
        content: storedBytes("keep"),
        contentType: "image/png",
        checkedAt: 10,
      },
      lastUsed: 20,
      bytes: 4,
      messageId: "same-message",
    });
    // Row missing lastUsed and bytes: upgrade must derive bytes and clamp lastUsed to 0.
    media.put({
      key: key("a", "incomplete"),
      scope: JSON.stringify(key("a", "incomplete").slice(0, -1)),
      data: {
        content: storedBytes("zero"),
        contentType: "image/png",
        checkedAt: 10,
      },
      messageId: "m-incomplete",
    });
    const avatars = v1.transaction.objectStore("avatars");
    avatars.put({
      key: key("a", "peer"),
      scope: JSON.stringify(key("a", "peer").slice(0, -1)),
      data: {
        content: storedBytes("face"),
        contentType: "image/png",
        checkedAt: 10,
      },
      lastUsed: 15,
      bytes: 4,
    });
  };
  (await idbResult(v1)).close();
  const puts = t.mock.method(IDBObjectStore.prototype, "put");
  const cache = open();
  assert.equal((await cache.stats("a")).bytes, 16);
  assert.deepEqual(
    puts.mock.calls.map(({ this: store }) => store.name).sort(),
    ["avatarMeta", "mediaMeta", "mediaMeta", "mediaMeta"],
  );

  // Saving an avatar triggers cross-store eviction: incomplete (0) and older (10) are evicted first.
  cache.saveBlob("avatars", "b", "other-peer", {
    blob: new Blob(["avtr"]),
    checkedAt: 1,
  });
  await cache.flush();
  assert.equal(await cache.blob("media", "a", "incomplete"), undefined);
  assert.equal(await cache.blob("media", "a", "older"), undefined);
  assert.equal(
    await (await cache.blob("avatars", "a", "peer")).blob.text(),
    "face",
  );
  assert.equal(
    await (await cache.blob("media", "a", "newer")).blob.text(),
    "keep",
  );

  // Saving media for account b evicts oldest remaining ("other-peer" at lastUsed=30) while keeping newer items.
  cache.saveBlob(
    "media",
    "b",
    "other",
    { blob: new Blob(["safe"]), checkedAt: 1 },
    "same-message",
  );
  await cache.flush();
  assert.equal(await cache.blob("avatars", "b", "other-peer"), undefined);

  cache.invalidateMessage("a", "same-message");
  await cache.flush();
  assert.equal(await cache.blob("media", "a", "newer"), undefined);
  assert.equal(
    await (await cache.blob("media", "b", "other")).blob.text(),
    "safe",
  );
  assert.equal((await cache.stats("a")).bytes, 4);
  assert.equal((await cache.stats("b")).bytes, 4);
});

test("a v1 backfill error clears the disposable blob cache instead of locking out drafts and receipts", async (t) => {
  for (const mode of ["put-async-error", "put-sync-throw", "cursor-error"]) {
    await t.test(mode, async (t) => {
      const { factory, open } = setup(t);
      const v1 = factory.open("agent-wechat-cache", 1);
      const key = (id) => ["https://hub.test", "hub-1", "a", id];
      const scope = JSON.stringify(["https://hub.test", "hub-1", "a"]);
      v1.onupgradeneeded = () => {
        for (const name of [
          "threads",
          "contacts",
          "messages",
          "chatState",
          "syncState",
          "avatars",
          "media",
          "drafts",
          "receipts",
        ])
          v1.result
            .createObjectStore(name, { keyPath: "key" })
            .createIndex("scope", "scope");
        v1.transaction.objectStore("drafts").put({
          key: key("peer"),
          scope,
          data: { text: "keep my draft" },
          lastUsed: 1,
          bytes: 20,
        });
        v1.transaction.objectStore("receipts").put({
          key: key("send:k1"),
          scope,
          data: { key: "k1", uncertain: true },
          lastUsed: 1,
          bytes: 20,
        });
        for (const [id, text, msg] of [
          ["img-1", "photo", "m1"],
          ["img-2", "second", "m2"],
        ]) {
          v1.transaction.objectStore("media").put({
            key: key(id),
            scope,
            data: {
              content: new TextEncoder().encode(text).buffer,
              contentType: "image/png",
              checkedAt: 1,
            },
            lastUsed: 10,
            bytes: text.length,
            messageId: msg,
          });
        }
        v1.transaction.objectStore("avatars").put({
          key: key("av-1"),
          scope,
          data: {
            content: new TextEncoder().encode("face").buffer,
            contentType: "image/png",
            checkedAt: 1,
          },
          lastUsed: 10,
          bytes: 4,
        });
      };
      (await idbResult(v1)).close();

      if (mode === "put-async-error") {
        const origPut = IDBObjectStore.prototype.put;
        const origAdd = IDBObjectStore.prototype.add;
        let failedOnce = false;
        t.mock.method(IDBObjectStore.prototype, "put", function (...args) {
          if (this.name === "mediaMeta" && !failedOnce) {
            failedOnce = true;
            // Real async ConstraintError IDBRequest that aborts the upgrade tx
            // unless write.onerror calls event.preventDefault().
            origAdd.call(this, args[0]);
            return origAdd.call(this, args[0]);
          }
          return origPut.apply(this, args);
        });
      } else if (mode === "put-sync-throw") {
        const origPut = IDBObjectStore.prototype.put;
        let failedOnce = false;
        t.mock.method(IDBObjectStore.prototype, "put", function (...args) {
          if (this.name === "mediaMeta" && !failedOnce) {
            failedOnce = true;
            return origPut.call(this, { key: null, scope });
          }
          return origPut.apply(this, args);
        });
      } else {
        const origOpenCursor = IDBObjectStore.prototype.openCursor;
        t.mock.method(
          IDBObjectStore.prototype,
          "openCursor",
          function (...args) {
            if (this.name === "media") {
              const req = origOpenCursor.apply(this, args);
              const origDesc = Object.getOwnPropertyDescriptor(
                Object.getPrototypeOf(req),
                "onerror",
              );
              let onerrorHandler = null;
              Object.defineProperty(req, "onerror", {
                configurable: true,
                get: () => onerrorHandler,
                set: (fn) => {
                  onerrorHandler = fn;
                  origDesc?.set?.call(req, fn);
                },
              });
              queueMicrotask(() => {
                if (!onerrorHandler) return;
                const ev = new Event("error", { cancelable: true });
                onerrorHandler.call(req, ev);
              });
              return req;
            }
            return origOpenCursor.apply(this, args);
          },
        );
      }

      const cache = open();
      await cache.primeRecovery(["a"]);
      assert.equal(cache.available, true);
      assert.deepEqual(cache.readState("a", "peer", "drafts"), {
        text: "keep my draft",
      });
      assert.deepEqual(cache.readState("a", "send:k1", "receipts"), {
        key: "k1",
        uncertain: true,
      });
      // Both media rows were cleared on backfill failure while avatars survived cleanly.
      assert.equal(await cache.blob("media", "a", "img-1"), undefined);
      assert.equal(await cache.blob("media", "a", "img-2"), undefined);
      assert.equal(
        await (await cache.blob("avatars", "a", "av-1")).blob.text(),
        "face",
      );
      assert.equal((await cache.stats("a")).bytes, 4);
      // Cache remains fully usable for new media saves.
      cache.saveBlob(
        "media",
        "a",
        "fresh",
        { blob: new Blob(["ok"]), checkedAt: 2 },
        "m3",
      );
      await cache.flush();
      assert.equal(
        await (await cache.blob("media", "a", "fresh")).blob.text(),
        "ok",
      );
    });
  }
});

test("a v2 upgrade waits for a v1 tab up to the blocked timeout", async (t) => {
  for (const closesInTime of [true, false]) {
    const name = closesInTime
      ? "tab closes before timeout"
      : "tab stays open past timeout";
    await t.test(name, async (t) => {
      const blockedTimeoutMs = 20;
      const { factory, open } = setup(t, { blockedTimeoutMs });
      const v1 = await idbResult(factory.open("agent-wechat-cache", 1));
      t.after(() => v1.close());
      t.mock.timers.enable({ apis: ["setTimeout"] });
      const blocked = Promise.withResolvers();
      const upgraded = Promise.withResolvers();
      const openDatabase = factory.open.bind(factory);
      t.mock.method(factory, "open", (...args) => {
        const request = openDatabase(...args);
        request.addEventListener("blocked", blocked.resolve, { once: true });
        request.addEventListener(
          "success",
          () => upgraded.resolve(request.result),
          {
            once: true,
          },
        );
        return request;
      });

      const cache = open();
      const stats = cache.stats("a");
      await blocked.promise;
      t.mock.timers.tick(blockedTimeoutMs - 1);
      assert.equal(cache.available, true);

      if (closesInTime) {
        v1.close();
        assert.equal((await stats).available, true);
        t.mock.timers.tick(blockedTimeoutMs);
        assert.equal(cache.available, true);
      } else {
        t.mock.timers.tick(1);
        assert.equal((await stats).available, false);
        assert.equal(cache.available, false);
        v1.close();
        const lateDb = await upgraded.promise;
        assert.throws(() => lateDb.transaction("media"), {
          name: "InvalidStateError",
        });
        assert.equal((await cache.stats("a")).available, false);
      }
    });
  }
});

test("closing a cache cancels a blocked v2 upgrade immediately", async (t) => {
  for (const closeBeforeBlocked of [false, true]) {
    const name = closeBeforeBlocked
      ? "close called before blocked event with queued write"
      : "close called while blocked timer is active";
    await t.test(name, async (t) => {
      const blockedTimeoutMs = 5000;
      const { factory } = setup(t, { blockedTimeoutMs });
      const v1 = await idbResult(factory.open("agent-wechat-cache", 1));
      t.after(() => v1.close());
      t.mock.timers.enable({ apis: ["setTimeout"] });
      const blocked = Promise.withResolvers();
      const upgraded = Promise.withResolvers();
      const openDatabase = factory.open.bind(factory);
      t.mock.method(factory, "open", (...args) => {
        const request = openDatabase(...args);
        request.addEventListener("blocked", blocked.resolve, { once: true });
        request.addEventListener(
          "success",
          () => upgraded.resolve(request.result),
          { once: true },
        );
        return request;
      });

      const cache = new BrowserCache("https://hub.test", "hub-1", {
        factory,
        broadcast: false,
        blockedTimeoutMs,
      });
      if (closeBeforeBlocked) {
        cache.writeState("a", "draft-1", { text: "pending" }, "drafts");
        // Must resolve without advancing the mocked 5000 ms timer.
        await cache.close();
      } else {
        await blocked.promise;
        await cache.close();
      }
      v1.close();
      const lateDb = await upgraded.promise;
      assert.throws(() => lateDb.transaction("media"), {
        name: "InvalidStateError",
      });
    });
  }
});

test("blocked browser storage falls back without losing this session's draft or receipt", async (t) => {
  const cache = new BrowserCache("https://hub.test", "hub", {
    factory: {
      open() {
        throw new DOMException("denied", "SecurityError");
      },
    },
    broadcast: false,
  });
  t.after(() => cache.close());
  cache.writeState("a", "send:key", { uncertain: true });
  await cache.flush();
  assert.equal(cache.readState("a", "send:key").uncertain, true);
  assert.equal(await cache.readAccount("a"), undefined);
  assert.equal((await cache.stats("a")).available, false);
});

test("clearing a cache notifies another open tab in the same hub namespace", async (t) => {
  const previous = globalThis.window;
  globalThis.window = {};
  t.after(() => {
    if (previous === undefined) delete globalThis.window;
    else globalThis.window = previous;
  });
  const factory = new IDBFactory();
  const one = new BrowserCache("https://hub.test", "broadcast-test", {
    factory,
  });
  const two = new BrowserCache("https://hub.test", "broadcast-test", {
    factory,
  });
  t.after(async () => {
    await one.close();
    await two.close();
  });
  const received = new Promise((resolve) => two.onInvalidated(resolve));
  await one.clearAccount("a");
  assert.equal(
    await Promise.race([
      received,
      new Promise((_, reject) => {
        const timer = setTimeout(
          () => reject(Error("No cache invalidation")),
          1000,
        );
        timer.unref();
      }),
    ]),
    "a",
  );
});

test("verified media bytes survive reload and are shared by chat and viewer", async (t) => {
  const { open } = setup(t);
  const persistent = open();
  const asset = {
    id: "asset",
    accountId: "a",
    messageId: "m",
    kind: "image",
    variant: "standard",
    actualVariant: "standard",
    state: "available",
    sha256: "verified-hash",
  };
  let calls = 0;
  const client = {
    mediaContent: async () => {
      calls++;
      return {
        bytes: new TextEncoder().encode("photo"),
        contentType: "image/png",
      };
    },
  };
  const first = new MediaPreviewCache(client, undefined, undefined, persistent);
  const lease = first.acquire(asset);
  await lease.ready;
  lease.release();
  first.close();
  await persistent.close();
  const second = new MediaPreviewCache(client, undefined, undefined, open());
  t.after(() => second.close());
  const chat = second.acquire(asset),
    viewer = second.acquire(asset);
  assert.equal((await chat.ready).url, (await viewer.ready).url);
  assert.equal(calls, 1);
  const original = second.acquire({
    ...asset,
    variant: "original",
    actualVariant: "original",
    sha256: "original-hash",
  });
  await original.ready;
  assert.equal(calls, 2);
  second.invalidateMessage("a", "m");
  await assert.rejects(fetch((await chat.ready).url));
  chat.release();
  viewer.release();
  original.release();
});

test("visible cached avatars keep their age across reloads and only stale visible ones refresh", async (t) => {
  const { open } = setup(t);
  const persistent = open();
  for (const username of ["fresh", "stale", "offscreen"])
    persistent.saveBlob("avatars", "a", username, {
      blob: new Blob([username], { type: "image/png" }),
      checkedAt: username === "fresh" ? 3600001 : 1,
    });
  await persistent.flush();
  const refreshed = [],
    read = [];
  const avatars = new AvatarCache(
    {
      avatar: async (_a, username) => {
        read.push(username);
        return {
          bytes: new TextEncoder().encode(username),
          contentType: "image/png",
        };
      },
      refreshAvatar: async (_a, username) => {
        refreshed.push(username);
        return { id: username };
      },
      operation: async () => ({ state: "succeeded" }),
    },
    () => 3600002,
    () => true,
    persistent,
  );
  t.after(() => avatars.close());
  const stopFresh = avatars.show("a", "fresh"),
    stopStale = avatars.show("a", "stale");
  await avatars.ensure("a", "fresh", true);
  await avatars.ensure("a", "stale", true);
  // ensure() may join the initial show() refresh after its cached read resolves.
  for (let i = 0; i < 20 && !read.includes("stale"); i++)
    await new Promise((resolve) => setTimeout(resolve, 5));
  assert.deepEqual(refreshed, ["stale"]);
  assert.deepEqual(read, ["stale"]);
  assert.ok(avatars.get("a", "fresh"));
  assert.equal(avatars.get("a", "offscreen"), "");
  stopFresh();
  stopStale();
});
