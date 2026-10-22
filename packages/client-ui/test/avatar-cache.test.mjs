import test from "node:test";
import assert from "node:assert/strict";
import { AvatarCache } from "../src/avatar-cache.ts";
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
