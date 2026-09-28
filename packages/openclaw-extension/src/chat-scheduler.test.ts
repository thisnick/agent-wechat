import test from "node:test";
import assert from "node:assert/strict";
import { createChatScheduler, createSerialQueue } from "./chat-scheduler.ts";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => { resolve = done; });
  return { promise, resolve };
}

test("blocked groups cannot consume direct slots, exceed bounds or overlap the same chat", async () => {
  const controller = new AbortController();
  const scheduler = createChatScheduler({ signal: controller.signal });
  const held = deferred();
  const direct = deferred();
  assert.equal(scheduler.schedule("a@chatroom", () => held.promise), true);
  assert.equal(scheduler.schedule("b@chatroom", () => held.promise), true);
  assert.equal(scheduler.schedule("c@chatroom", () => held.promise), false);
  assert.equal(scheduler.schedule("a@chatroom", () => assert.fail("duplicate")), false);
  assert.equal(scheduler.schedule("private", () => { direct.resolve(); return held.promise; }), true);
  assert.equal(scheduler.schedule("private2", () => held.promise), true);
  assert.equal(scheduler.schedule("private3", () => held.promise), false);
  await direct.promise;
  assert.equal(scheduler.has("a@chatroom"), true);
  held.resolve();
  await scheduler.drain();
  assert.equal(scheduler.schedule("c@chatroom", () => {}), true);
  await scheduler.drain();
});

test("failures release slots and shutdown rejects queued and new work", async () => {
  const controller = new AbortController();
  const errors: unknown[] = [];
  const scheduler = createChatScheduler({ signal: controller.signal, onError: (e, id) => errors.push([e, id]) });
  const error = new Error("unavailable");
  scheduler.schedule("private", () => { throw error; });
  await scheduler.drain();
  assert.deepEqual(errors, [[error, "private"]]);
  assert.equal(scheduler.schedule("private", () => assert.fail("cancelled before start")), true);
  controller.abort();
  await scheduler.drain();
  assert.equal(scheduler.schedule("another", () => assert.fail()), false);
});

test("state writes are ordered and a failed save does not poison the queue", async () => {
  const queue = createSerialQueue();
  const held = deferred();
  const entered = deferred();
  const writes: number[] = [];
  let cursor = 1;
  const first = queue.run(async () => { const snapshot = cursor; entered.resolve(); await held.promise; writes.push(snapshot); });
  await entered.promise;
  cursor = 2;
  const second = queue.run(() => writes.push(cursor));
  assert.equal(writes.length, 0);
  held.resolve();
  await Promise.all([first, second]);
  assert.deepEqual(writes, [1, 2]);
  await assert.rejects(queue.run(() => { throw new Error("disk"); }), /disk/);
  await queue.run(() => writes.push(3));
  assert.deepEqual(writes, [1, 2, 3]);
});
