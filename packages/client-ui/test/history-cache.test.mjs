import test from "node:test";
import assert from "node:assert/strict";
import { HistoryCache } from "../src/history-cache.ts";
const history = (n) => ({
  messages: Array.from({ length: n }, (_, id) => ({ id: String(id) })),
  nextMessages: "opaque",
  sourceGeneration: "1",
  group: null,
});

test("history cache evicts least recently used chats without truncating pages", () => {
  const cache = new HistoryCache(2, 4);
  cache.set("a", "one", history(2));
  cache.set("a", "two", history(2));
  cache.get("a", "one");
  cache.set("a", "three", history(1));
  assert.equal(cache.get("a", "two"), undefined);
  assert.equal(cache.get("a", "one").nextMessages, "opaque");
  cache.set("a", "huge", history(5));
  assert.equal(cache.get("a", "huge"), undefined);
});
test("history keys cannot collide across accounts and account invalidation is selective", () => {
  const cache = new HistoryCache();
  cache.set("a", "b:c", history(1));
  cache.set("a:b", "c", history(2));
  cache.clear("a");
  assert.equal(cache.get("a", "b:c"), undefined);
  assert.equal(cache.get("a:b", "c").messages.length, 2);
  cache.clear();
  assert.equal(cache.get("a:b", "c"), undefined);
});
