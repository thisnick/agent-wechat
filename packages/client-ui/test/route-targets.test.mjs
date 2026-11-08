import test from "node:test";
import assert from "node:assert/strict";
import { NativeApiError } from "@agent-wechat/shared";
import { conversationAvailable, messageAvailable } from "../src/route-targets.ts";

const notFound = () => Promise.reject(new NativeApiError(404, "not_found"));
const client = (overrides = {}) => ({
  thread: notFound,
  contact: notFound,
  group: notFound,
  message: notFound,
  ...overrides,
});

test("a listed conversation and File Transfer need no lookup", async () => {
  const fail = () => assert.fail("no lookup expected");
  const lookups = client({ thread: fail, contact: fail, group: fail });
  assert.equal(await conversationAvailable(lookups, "a", "peer", [{ id: "peer" }]), true);
  assert.equal(await conversationAvailable(lookups, "a", "filehelper", []), true);
});

test("a thread, contact or group the hub knows can be opened", async () => {
  const seen = [];
  const record = (name) => (...args) => {
    seen.push([name, ...args]);
    return Promise.resolve({});
  };
  assert.equal(await conversationAvailable(client({ thread: record("thread") }), "a", "p", []), true);
  assert.equal(await conversationAvailable(client({ contact: record("contact") }), "a", "p", []), true);
  assert.equal(await conversationAvailable(client({ group: record("group") }), "a", "r@chatroom", []), true);
  assert.deepEqual(seen, [["thread", "a", "p"], ["contact", "a", "p"], ["group", "a", "r@chatroom"]]);
});

test("an identifier the hub has never heard of is unavailable", async () => {
  assert.equal(await conversationAvailable(client(), "a", "nobody", []), false);
  assert.equal(await conversationAvailable(client(), "a", "nobody@chatroom", []), false);
});

test("a groups-only lookup is not made for a direct chat", async () => {
  const lookups = client({ group: () => assert.fail("not a group") });
  assert.equal(await conversationAvailable(lookups, "a", "nobody", []), false);
});

test("a failed lookup other than not-found does not discard the link", async () => {
  const down = () => Promise.reject(new NativeApiError(503, "unavailable", true));
  assert.equal(await conversationAvailable(client({ thread: down }), "a", "peer", []), true);
  assert.equal(await conversationAvailable(client({ thread: notFound, contact: () => Promise.reject(new Error("offline")) }), "a", "peer", []), true);
});

test("a message must exist and belong to the conversation named with it", async () => {
  const message = (threadId, accountId = "a") => () => Promise.resolve({ id: "m", accountId, threadId });
  assert.equal(await messageAvailable(client({ message: message("t") }), "a", "t", "m"), true);
  assert.equal(await messageAvailable(client({ message: message("other") }), "a", "t", "m"), false);
  assert.equal(await messageAvailable(client({ message: message("t", "b") }), "a", "t", "m"), false);
  assert.equal(await messageAvailable(client(), "a", "t", "m"), false);
  const down = () => Promise.reject(new NativeApiError(503, "unavailable", true));
  assert.equal(await messageAvailable(client({ message: down }), "a", "t", "m"), true);
});
