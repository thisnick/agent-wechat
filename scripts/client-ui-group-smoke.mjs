import assert from "node:assert/strict";
import { GroupCreation } from "../packages/client-ui/src/group-creation.ts";
export async function groupJourney({ client, wait, account }) {
  assert.equal((await client.capabilities(account)).groupCreation, true);
  const input = {
    name: "Simulator project",
    members: ["sim_mei", "sim_daniel"],
  };
  const original = client.createGroup.bind(client);
  let submissions = 0,
    saved = null;
  const storage = {
    read: () => saved,
    write: (value) => {
      saved = value;
    },
  };
  const dropped = Object.create(client);
  dropped.createGroup = async (...args) => {
    submissions++;
    await original(...args);
    throw Error("Lost group admission response");
  };
  const creation = new GroupCreation(dropped, account, storage);
  let room;
  try {
    await creation.create(input);
    const key = saved.key;
    assert.ok(creation.snapshot().error);
    await creation.check();
    const operation = await wait(
      "created group receipt",
      () =>
        creation.snapshot().receipt.operation?.state === "succeeded" &&
        creation.snapshot().receipt.operation,
    );
    assert.equal(submissions, 1);
    room = operation.result.roomId;
    assert.match(room, /^simroom_[a-f0-9]+@chatroom$/);
    assert.equal(
      (
        await client.createGroup(
          account,
          { ...input, members: [...input.members].reverse() },
          key,
        )
      ).id,
      operation.id,
    );
    await assert.rejects(
      client.createGroup(account, { ...input, name: "Conflicting" }, key),
      (error) => error.code === "idempotency_conflict",
    );
    await assert.rejects(
      client.createGroup(
        account,
        { ...input, members: ["sim_mei", "sim_mei"] },
        "invalid-duplicate",
      ),
    );
    await assert.rejects(
      client.createGroup(
        account,
        { ...input, members: ["sim_mei", "filehelper"] },
        "invalid-helper",
      ),
    );
    await assert.rejects(client.groupCreationByKey("work", key));
    creation.close();
    const reopened = new GroupCreation(client, account, storage);
    try {
      await reopened.check();
      assert.equal(reopened.snapshot().receipt.operation.id, operation.id);
      assert.equal(submissions, 1);
    } finally {
      reopened.close();
    }
    const thread = await wait("created group thread projection", async () => {
      const result = await client.thread(account, room);
      return result.displayName === input.name && result;
    });
    assert.equal(thread.kind, "group");
    const members = await client.refreshGroupMembers(account, room);
    await wait("created group roster projection", async () => {
      const operation = await client.operation(account, members.id);
      assert.notEqual(operation.state, "failed");
      return operation.state === "succeeded";
    });
    const roster = (await client.groupMembers(account, room)).items;
    assert.equal(roster.length, 3);
    assert.ok(roster.some((member) => member.id === "sim_mei"));
    assert.ok(roster.some((member) => member.id === "sim_daniel"));
    assert.ok(roster.some((member) => member.id === `sim_${account}`));
    const message = await client.sendText(
      account,
      room,
      "Synthetic group message",
      "group-text",
    );
    await wait(
      "group text acceptance",
      async () => (await client.send(account, message.id)).state === "accepted",
    );
    await wait("group text history", async () =>
      (await client.messages(account, room)).items.some(
        (item) =>
          item.body === "Synthetic group message" &&
          item.direction === "outgoing",
      ),
    );
    await assert.rejects(
      client.refreshGroupMembers("work", room).then(async (operation) => {
        const final = await wait("foreign group lookup", async () => {
          const item = await client.operation("work", operation.id);
          return ["succeeded", "failed"].includes(item.state) && item;
        });
        if (final.state === "failed") throw Error("foreign group refused");
      }),
    );
  } finally {
    creation.close();
  }
  return room;
}
