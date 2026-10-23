import test from "node:test";
import assert from "node:assert/strict";
import { MessageRecall, canRecall } from "../src/message-recall.ts";
const message = {
  id: "opaque",
  serverId: "18446744073709551615",
  accountId: "a",
  threadId: "peer",
  direction: "outgoing",
  type: 1,
};
const operation = (state = "succeeded", extra = {}) => ({
  id: "recall1",
  accountId: "a",
  kind: "message_recall",
  state,
  result: {
    messageId: message.id,
    serverId: message.serverId,
    threadId: message.threadId,
  },
  ...extra,
});
const storage = (initial = null) => {
  let saved = initial;
  return {
    read: () => saved,
    write: (value) => {
      saved = { ...value };
    },
  };
};

test("recall is limited to unrecalled own messages with an exact server identity", () => {
  assert.equal(canRecall(message), true);
  for (const override of [
    { direction: "incoming" },
    { recall: {} },
    { serverId: null },
    { serverId: "0" },
    { serverId: "01" },
    { serverId: "18446744073709551616" },
  ])
    assert.equal(canRecall({ ...message, ...override }), false);
});
test("recall uses one admission key, keeps exact IDs and double clicks submit once", async (t) => {
  const calls = [];
  const model = new MessageRecall(
    {
      recallMessage: async (...args) => {
        calls.push(args);
        return operation();
      },
    },
    message,
    storage(),
  );
  t.after(() => model.close());
  await Promise.all([model.recall(), model.recall()]);
  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].slice(0, 3), ["a", "opaque", message.serverId]);
  assert.equal(model.snapshot().operation.state, "succeeded");
});
test("lost recall admission reuses its key on deliberate recovery", async (t) => {
  const calls = [],
    saved = storage();
  const client = {
    recallMessage: async (...args) => {
      calls.push(args);
      if (calls.length === 1) throw new Error("lost response");
      return operation();
    },
  };
  const first = new MessageRecall(client, message, saved);
  await first.recall();
  assert.equal(first.snapshot().uncertain, true);
  first.close();
  const reopened = new MessageRecall(client, message, saved);
  t.after(() => reopened.close());
  assert.equal(calls.length, 1);
  await reopened.recall();
  assert.equal(calls[0][3], calls[1][3]);
});
test("a stored operation is inspected after reload without another recall POST", async (t) => {
  let posts = 0,
    checks = 0;
  const model = new MessageRecall(
    {
      recallMessage: async () => {
        posts++;
      },
      operation: async () => {
        checks++;
        return operation();
      },
    },
    message,
    storage({ key: "key", serverId: message.serverId, operationId: "recall1" }),
  );
  t.after(() => model.close());
  await new Promise((done) => setTimeout(done, 0));
  assert.equal(posts, 0);
  assert.equal(checks, 1);
  assert.equal(model.snapshot().operation.state, "succeeded");
});
test("a receipt for another message is rejected and needs_review is never retried automatically", async (t) => {
  const mismatch = new MessageRecall(
    {
      recallMessage: async () =>
        operation("succeeded", {
          result: {
            messageId: "other",
            threadId: "peer",
            serverId: message.serverId,
          },
        }),
    },
    message,
    storage(),
  );
  t.after(() => mismatch.close());
  await mismatch.recall();
  assert.equal(mismatch.snapshot().operation, null);
  const uncertain = new MessageRecall(
    { recallMessage: async () => operation("needs_review") },
    message,
    storage(),
  );
  t.after(() => uncertain.close());
  await uncertain.recall();
  assert.match(uncertain.snapshot().error, /couldn’t be confirmed/);
  assert.equal(uncertain.snapshot().busy, false);
});
