import test from "node:test";
import assert from "node:assert/strict";
import { IDBFactory } from "fake-indexeddb";
import { BrowserCache } from "../src/browser-cache.ts";
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
  return { open, storage };
}

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
  assert.equal(
    await (await cache.blob("media", "b", "three")).blob.text(),
    "3333",
  );
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
