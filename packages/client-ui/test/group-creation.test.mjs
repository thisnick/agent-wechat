import test from "node:test";
import assert from "node:assert/strict";
import { GroupMutations, validGroupReceipt } from "../src/group-creation.ts";
const input = { name: "Team", members: ["daniel", "mei"] };
const operation = (state = "succeeded", accountId = "a") => ({
  id: "group-op",
  accountId,
  kind: "group_mutation",
  state,
  result: { action: "create", targetId: null, groupId: "room@chatroom" },
});
function setup(t, client, saved = null) {
  const storage = {
    read: () => saved,
    write: (value) => {
      saved = value;
    },
  };
  const creation = new GroupMutations(client, "a", storage);
  t.after(() => creation.close());
  return { creation, saved: () => saved, storage };
}
test("lost group admission is recovered only by key lookup without a second create", async (t) => {
  let creates = 0,
    lookups = 0;
  const { creation, saved } = setup(t, {
    createGroup: async () => {
      creates++;
      throw Error("lost reply");
    },
    groupMutationByKey: async (_account, key) => {
      lookups++;
      assert.equal(key, saved().key);
      return operation();
    },
  });
  await creation.create(input);
  const key = saved().key;
  await creation.create(input);
  await creation.check();
  assert.equal(creates, 1);
  assert.equal(lookups, 1);
  assert.equal(saved().key, key);
  assert.equal(creation.snapshot().receipt.operation.state, "succeeded");
});
test("a reopened group receipt checks its operation and never creates a group", async (t) => {
  let reads = 0;
  const { creation } = setup(
    t,
    {
      operation: async (account, id) => {
        reads++;
        assert.equal(account, "a");
        assert.equal(id, "group-op");
        return operation();
      },
    },
    {
      key: "existing",
      input: { action: "create", ...input },
      operation: operation(),
    },
  );
  for (let i = 0; i < 20 && !reads; i++)
    await new Promise((done) => setTimeout(done, 5));
  assert.equal(reads, 1);
  assert.equal(creation.snapshot().receipt.key, "existing");
});
test("foreign group receipts cannot select another account's room", async (t) => {
  const { creation } = setup(t, {
    createGroup: async () => operation("succeeded", "b"),
  });
  await creation.create(input);
  assert.equal(creation.snapshot().receipt.operation, undefined);
  assert.ok(creation.snapshot().error);
});
test("uncertain group outcomes remain retained and cannot be cleared into another creation", async (t) => {
  const { creation } = setup(t, {
    createGroup: async () => operation("needs_review"),
  });
  await creation.create(input);
  creation.clear();
  assert.equal(creation.snapshot().receipt.operation.state, "needs_review");
});

test("recovered edits verify action and target as well as account", async (t) => {
  const { creation } = setup(t, {
    renameGroup: async () => ({
      ...operation(),
      result: { action: "rename", targetId: "other@chatroom" },
    }),
  });
  await creation.run({
    action: "rename",
    groupId: "room@chatroom",
    name: "Team",
  });
  assert.equal(creation.snapshot().receipt.operation, undefined);
  assert.ok(creation.snapshot().error);
});
test("partial members and per-step results are retained without resubmission", async (t) => {
  const partial = {
    ...operation(),
    result: {
      ...operation().result,
      memberOutcome: "partial",
      members: [{ username: "mei", status: 4 }],
      unconfirmedMembers: ["daniel"],
      steps: [{ accepted: true }],
    },
  };
  let creates = 0;
  const { creation } = setup(t, {
    createGroup: async () => {
      creates++;
      return partial;
    },
  });
  await creation.create(input);
  await creation.create(input);
  assert.equal(creates, 1);
  assert.deepEqual(
    creation.snapshot().receipt.operation.result,
    partial.result,
  );
});
test("a successful change can be acknowledged before a different edit", async (t) => {
  let renames = 0;
  const { creation } = setup(t, {
    renameGroup: async () => ({
      ...operation(),
      id: `rename-${++renames}`,
      result: { action: "rename", targetId: "room@chatroom" },
    }),
  });
  await creation.run({
    action: "rename",
    groupId: "room@chatroom",
    name: "One",
  });
  creation.clear();
  await creation.run({
    action: "rename",
    groupId: "room@chatroom",
    name: "Two",
  });
  assert.equal(renames, 2);
});
test("cancellation uses the recorded operation and keeps its result", async (t) => {
  const queued = operation("queued");
  const { creation } = setup(t, {
    createGroup: async () => queued,
    cancelOperation: async (account, id) => {
      assert.equal(account, "a");
      assert.equal(id, queued.id);
      return operation("cancelled");
    },
  });
  await creation.create(input);
  await creation.cancel();
  assert.equal(creation.snapshot().receipt.operation.state, "cancelled");
});

test("stored group requests are scoped to their target and validate picker fields", () => {
  assert.equal(
    validGroupReceipt({ key: "k", input: { action: "create" } }, "create"),
    false,
  );
  assert.equal(
    validGroupReceipt(
      { key: "k", input: { action: "create", members: ["mei", "daniel"] } },
      "create",
    ),
    true,
  );
  assert.equal(
    validGroupReceipt(
      { key: "k", input: { action: "create", members: null } },
      "create",
    ),
    false,
  );
  assert.equal(
    validGroupReceipt(
      {
        key: "k",
        input: {
          action: "rename",
          groupId: "other@chatroom",
          name: "Wrong target",
        },
      },
      "room@chatroom",
    ),
    false,
  );
});
test("a foreign stored operation is recovered by key without rendering it", async (t) => {
  let reads = 0;
  const { creation } = setup(
    t,
    {
      groupMutationByKey: async () => {
        reads++;
        return operation();
      },
    },
    {
      key: "existing",
      input: { action: "create", ...input },
      operation: operation("succeeded", "b"),
    },
  );
  assert.equal(creation.snapshot().receipt.operation, undefined);
  await creation.check();
  assert.ok(reads >= 1);
  assert.equal(creation.snapshot().receipt.operation.accountId, "a");
});
