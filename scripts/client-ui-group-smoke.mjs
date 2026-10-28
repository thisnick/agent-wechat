import assert from "node:assert/strict";
import { GroupMutations } from "../packages/client-ui/src/group-creation.ts";
export async function groupJourney({ client, wait, account, control, state }) {
  const capability = await client.capabilities(account);
  assert.ok(capability.groupMutations.includes("create"));
  assert.equal(capability.groupMutationMaxSelectedMembers, 40);
  const input = {
    name: "Simulator project",
    members: ["sim_daniel", "sim_mei"],
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
  const creation = new GroupMutations(dropped, account, storage);
  const finish = async (label, operation) =>
    wait(label, async () => {
      const item = await client.operation(account, operation.id);
      if (["failed", "needs_review", "cancelled"].includes(item.state))
        throw Error(`${label}: ${JSON.stringify(item)}`);
      return item.state === "succeeded" && item;
    });
  let room;
  try {
    await creation.create(input);
    const key = saved.key;
    assert.ok(creation.snapshot().error);
    await creation.check();
    const operation = await wait("created group receipt", async () => {
      // Status recovery is read-only even if a transient read stopped watching.
      await creation.check();
      const snapshot = creation.snapshot();
      if (snapshot.error) throw Error(snapshot.error);
      const current = snapshot.receipt.operation;
      if (
        current &&
        ["failed", "needs_review", "cancelled"].includes(current.state)
      )
        throw Error(JSON.stringify(current));
      return current?.state === "succeeded" && current;
    });
    assert.equal(operation.kind, "group_mutation");
    assert.equal(submissions, 1);
    room = operation.result.groupId;
    assert.match(room, /^simroom_[a-f0-9]+@chatroom$/);
    assert.equal(
      (await client.createGroup(account, input, key)).id,
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
    await assert.rejects(client.groupMutationByKey("work", key));
    creation.close();
    const reopened = new GroupMutations(client, account, storage);
    try {
      await reopened.check();
      assert.equal(reopened.snapshot().receipt.operation.id, operation.id);
      assert.equal(submissions, 1);
    } finally {
      reopened.close();
    }
    const thread = await wait("created group thread", async () => {
      const value = await client.thread(account, room);
      return value.displayName === input.name && value;
    });
    assert.equal(thread.kind, "group");
    await finish(
      "group detail refresh",
      await client.refreshGroup(account, room),
    );
    await finish(
      "group roster refresh",
      await client.refreshGroupMembers(account, room),
    );
    const info = await client.group(account, room);
    assert.equal(info.isInGroup, true);
    assert.equal(info.muted, false);
    assert.equal((await client.groupMembers(account, room)).items.length, 3);
    const groupThread = await client.thread(account, room);
    assert.equal(groupThread.avatarMembers.length, 3);
    assert.ok(
      groupThread.avatarMembers.some((member) => member.id === "sim_mei"),
    );
    assert.equal(groupThread.lastMessage.text, "System message");
    const text = await client.sendText(
      account,
      room,
      "Synthetic group message",
      "group-text",
    );
    await wait(
      "group text acceptance",
      async () => (await client.send(account, text.id)).state === "accepted",
    );
    await wait("group text history", async () =>
      (await client.messages(account, room)).items.some(
        (item) => item.body === "Synthetic group message",
      ),
    );
    const latest = (await client.thread(account, room)).lastMessage;
    assert.equal(latest.text, "Synthetic group message");
    assert.equal(latest.direction, "outgoing");
    assert.equal(
      latest.id,
      (await client.messages(account, room, { limit: 1 })).items[0].id,
    );
    assert.equal(capability.inlineTextMentions, true);
    const mentioned = (await client.groupMembers(account, room)).items.find(member => member.username === "sim_mei");
    assert.ok(mentioned.mentionName);
    const inlineText = "😀 Hi @Draft\u2005, check this";
    const mentionOptions = { mentions: { userIds: ["sim_mei"], positions: [{ start: 6, end: 13, index: 0 }] } };
    const inline = await client.sendText(account, room, inlineText, "group-inline-mention", mentionOptions);
    await wait("inline mention acceptance", async () => (await client.send(account, inline.id)).state === "accepted");
    const rendered = `😀 Hi @${mentioned.mentionName}\u2005, check this`;
    await wait("inline mention canonical history", async () => {
      const message = (await client.messages(account, room)).items.find(item => item.body === rendered);
      return message?.mentions?.userIds.includes("sim_mei");
    });
    assert.equal((await client.sendText(account, room, inlineText, "group-inline-mention", mentionOptions)).id, inline.id);
    const rename = await finish(
      "rename group",
      await client.renameGroup(account, room, "Design team", "group-rename"),
    );
    assert.equal(rename.result.remoteOutcome, "accepted");
    assert.equal((await client.group(account, room)).name, "Design team");
    await finish(
      "my group name",
      await client.setMyGroupName(account, room, "Team lead", "group-my-name"),
    );
    assert.equal(
      (await client.group(account, room)).myDisplayName,
      "Team lead",
    );
    for (const muted of [true, false]) {
      await finish("direct notifications", await client.setThreadMuted(account, "sim_mei", muted, `direct-notifications-${muted}`));
      assert.equal((await client.thread(account,"sim_mei")).muted,muted);
      assert.equal((await client.threads(account)).items.find((thread) => thread.id === "sim_mei").muted,muted);
      await finish("verify direct notifications", await client.refreshContact(account,"sim_mei"));
      assert.equal((await client.thread(account,"sim_mei")).muted,muted);
    }
    await finish(
      "mute group",
      await client.setThreadMuted(account, room, true, "group-mute"),
    );
    assert.equal((await client.group(account, room)).muted, true);
    assert.equal((await client.thread(account, room)).muted, true);
    assert.equal(
      (await client.threads(account)).items.find((thread) => thread.id === room).muted,
      true,
    );
    await finish(
      "unmute group",
      await client.setThreadMuted(account, room, false, "group-unmute"),
    );
    assert.equal((await client.group(account, room)).muted, false);
    assert.equal((await client.thread(account, room)).muted, false);
    await finish(
      "remove member",
      await client.removeGroupMembers(
        account,
        room,
        ["sim_mei"],
        "group-remove",
      ),
    );
    assert.ok(
      !(await client.groupMembers(account, room)).items.some(
        (member) => member.id === "sim_mei",
      ),
    );
    await finish(
      "refresh removed roster",
      await client.refreshGroupMembers(account, room),
    );
    assert.ok(
      !(await client.groupMembers(account, room)).items.some(
        (member) => member.id === "sim_mei",
      ),
    );
    await finish(
      "add member",
      await client.addGroupMembers(account, room, ["sim_mei"], "group-add"),
    );
    assert.ok(
      (await client.groupMembers(account, room)).items.some(
        (member) => member.id === "sim_mei",
      ),
    );
    await finish(
      "clear announcement",
      await client.clearGroupAnnouncement(account, room, "group-clear"),
    );
    assert.equal((await client.group(account, room)).announcement, "");
    await finish(
      "refresh edited group",
      await client.refreshGroup(account, room),
    );
    await finish(
      "refresh edited members",
      await client.refreshGroupMembers(account, room),
    );
    const refreshed = await client.group(account, room);
    assert.equal(refreshed.name, "Design team");
    assert.equal(refreshed.myDisplayName, "Team lead");
    assert.equal(refreshed.muted, false);
    assert.equal((await client.groupMembers(account, room)).items.length, 3);
    await assert.rejects(
      client.renameGroup("work", room, "Foreign group", "foreign-rename"),
    );
    await assert.rejects(client.groupMutationByKey("work", "group-rename"));
    const leave = await finish(
      "leave group",
      await client.leaveGroup(account, room, "group-leave"),
    );
    assert.equal(leave.result.steps.length, 3);
    assert.equal((await client.group(account, room)).isInGroup, false);
    assert.ok(
      (await client.messages(account, room)).items.some(
        (item) => item.body === "Synthetic group message",
      ),
    );
    await assert.rejects(
      client.renameGroup(account, room, "After leaving", "after-leave"),
      (error) => error.code === "ordinary_group_required",
    );

    // Partial member results come from the simulator RPC; the actual Core
    // decoder/projector must preserve a nonzero member status without assuming acceptance.
    await control({ op: "group_fault", account, fault: "partial_members" });
    const partial = await finish(
      "partial group creation",
      await client.createGroup(
        account,
        { ...input, name: "Partial group" },
        "partial-group",
      ),
    );
    assert.equal(partial.result.memberOutcome, "partial");
    assert.deepEqual(
      partial.result.members.map((member) => member.status),
      [0, 4],
    );
    const partialRoom = partial.result.groupId;
    assert.deepEqual(
      (await client.groupMembers(account, partialRoom)).items
        .map((member) => member.id)
        .sort(),
      [`sim_${account}`, "sim_daniel"].sort(),
    );
    await finish(
      "partial roster refresh",
      await client.refreshGroupMembers(account, partialRoom),
    );
    assert.equal(
      (await client.groupMembers(account, partialRoom)).items.length,
      2,
    );
    await control({ op: "group_fault", account, fault: "reject" });
    const rejected = await client.renameGroup(
      account,
      partialRoom,
      "Rejected name",
      "group-reject",
    );
    const rejection = await wait("group rejection", async () => {
      const value = await client.operation(account, rejected.id);
      return value.state === "failed" && value;
    });
    assert.equal(rejection.result.remoteOutcome, "rejected");
    assert.equal(
      (await client.group(account, partialRoom)).name,
      "Partial group",
    );
    const before = (await state()).accounts.find(
      (item) => item.id === account,
    ).groupCount;
    await control({ op: "group_fault", account, fault: "lost_acknowledgment" });
    const uncertain = await client.createGroup(
      account,
      { ...input, name: "Uncertain group" },
      "uncertain-group",
    );
    const review = await wait("uncertain group outcome", async () => {
      const value = await client.operation(account, uncertain.id);
      return value.state === "needs_review" && value;
    });
    assert.equal(review.result.remoteOutcome, "unknown");
    assert.equal(
      (await state()).accounts.find((item) => item.id === account).groupCount,
      before + 1,
    );
    assert.equal(
      (await client.groupMutationByKey(account, "uncertain-group")).id,
      uncertain.id,
    );
    assert.equal(
      (
        await client.createGroup(
          account,
          { ...input, name: "Uncertain group" },
          "uncertain-group",
        )
      ).id,
      uncertain.id,
    );
    assert.equal(
      (await state()).accounts.find((item) => item.id === account).groupCount,
      before + 1,
    );
  } finally {
    creation.close();
  }
  return room;
}
