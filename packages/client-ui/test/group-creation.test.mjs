import test from "node:test";
import assert from "node:assert/strict";
import { GroupCreation } from "../src/group-creation.ts";
const input = { name: "Team", members: ["daniel", "mei"] };
const operation = (state = "succeeded", accountId = "a") => ({
  id: "group-op",
  accountId,
  kind: "group_create",
  state,
  result: { roomId: "room@chatroom" },
});
function setup(t, client, saved = null) {
  const storage = {
    read: () => saved,
    write: (value) => {
      saved = value;
    },
  };
  const creation = new GroupCreation(client, "a", storage);
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
    groupCreationByKey: async (_account, key) => {
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
    { key: "existing", input, operation: operation() },
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
