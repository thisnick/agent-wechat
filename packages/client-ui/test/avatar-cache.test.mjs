import test from "node:test";
import assert from "node:assert/strict";
import { AvatarCache } from "../src/avatar-cache.ts";
import { NativeApiError } from "@agent-wechat/shared";
const response = (text) => ({
  bytes: new TextEncoder().encode(text),
  contentType: "image/png",
});
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
};
test("avatar cache shares requests within an account and keeps account bytes separate", async (t) => {
  let calls = 0;
  const cache = new AvatarCache({
    avatar: async (account) => {
      calls++;
      return response(account);
    },
  });
  t.after(() => cache.close());
  await Promise.all([
    cache.load("a", "mei"),
    cache.load("a", "mei"),
    cache.load("b", "mei"),
  ]);
  assert.equal(calls, 2);
  assert.notEqual(cache.get("a", "mei"), cache.get("b", "mei"));
  assert.equal(await (await fetch(cache.get("a", "mei"))).text(), "a");
});
test("a late cached response cannot replace a newer avatar refresh", async (t) => {
  const late = deferred();
  let calls = 0;
  const cache = new AvatarCache({
    avatar: () =>
      ++calls === 1 ? late.promise : Promise.resolve(response("new")),
  });
  t.after(() => cache.close());
  const initial = cache.load("a", "mei");
  await cache.load("a", "mei", true);
  late.resolve(response("old"));
  await initial;
  assert.equal(await (await fetch(cache.get("a", "mei"))).text(), "new");
});

test("lazy avatars refresh only missing bytes, deduplicate members and keep cached bytes", async (t) => {
  const ready = new Set(["cached"]),
    requested = [];
  const cache = new AvatarCache({
    avatar: async (_account, username) => {
      if (ready.has(username)) return response(username);
      throw new NativeApiError(404, "media_not_cached");
    },
    refreshAvatar: async (account, username) => {
      requested.push({ account, username });
      return { id: username };
    },
    operation: async (_account, id) => {
      ready.add(id);
      return { state: "succeeded" };
    },
  });
  t.after(() => cache.close());
  // A conversation-list lookup must not start any network download.
  await cache.load("a", "missing");
  assert.equal(requested.length, 0);
  await Promise.all([
    cache.ensure("a", "missing"),
    cache.ensure("a", "missing"),
    cache.ensure("a", "cached"),
  ]);
  assert.deepEqual(requested, [{ account: "a", username: "missing" }]);
  assert.ok(cache.get("a", "missing"));
  await cache.ensure("a", "missing");
  assert.equal(requested.length, 1);
});

test("lazy avatar requests are limited and stop queued work when the connection closes", async () => {
  const gate = deferred(),
    requested = [];
  const cache = new AvatarCache({
    avatar: async () => {
      throw new NativeApiError(404, "media_not_cached");
    },
    refreshAvatar: async (_account, username) => {
      requested.push(username);
      return { id: username };
    },
    operation: async () => gate.promise,
  });
  const requests = Array.from({ length: 12 }, (_, n) =>
    cache.ensure("a", `member${n}`),
  );
  await new Promise((done) => setTimeout(done, 0));
  assert.equal(requested.length, 4);
  cache.close();
  gate.resolve({ state: "cancelled" });
  await Promise.all(requests);
  assert.equal(requested.length, 4);
});

test("authorization and connection errors do not trigger avatar downloads", async (t) => {
  let downloads = 0;
  const cache = new AvatarCache({
    avatar: async () => {
      throw new NativeApiError(403, "forbidden");
    },
    refreshAvatar: async () => {
      downloads++;
    },
  });
  t.after(() => cache.close());
  await cache.ensure("a", "member");
  assert.equal(downloads, 0);
});
