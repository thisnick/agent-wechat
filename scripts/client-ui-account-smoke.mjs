import assert from "node:assert/strict";
import { connect } from "../packages/client-ui/src/connection.ts";

export async function accountJourney({
  client,
  control,
  wait,
  account,
  endpoint,
  token,
}) {
  const other = await client.account("work");
  await wait("signed-in profile cannot be removed", async () => {
    try {
      await client.removeAccount(account, account);
      assert.fail("signed-in profile was removed");
    } catch (error) {
      if (error.code === "operation_in_progress") return false;
      assert.equal(error.code, "account_sign_out_required");
      return true;
    }
  });
  await client.updateAccount(account, { alias: "ui-personal" });
  assert.equal(
    (await client.accounts()).items.find((item) => item.id === account).alias,
    "ui-personal",
  );
  await control({
    op: "inbound",
    account,
    peer: "sim_mei",
    body: "Read-state fixture",
  });
  await wait("read fixture committed before marking", async () =>
    (await client.messages(account, "sim_mei")).items.some(
      (message) => message.body === "Read-state fixture",
    ),
  );
  let priorVersion;
  const read = await wait("read stable profile generation", async () => {
    const thread = await client.thread(account, "sim_mei");
    if (!thread.unreadCount) return false;
    priorVersion = thread.readStateVersion;
    try {
      return await client.markRead(account, "sim_mei", priorVersion);
    } catch (error) {
      // Concurrent initial contact/sync projection can change the profile
      // generation. A conflict has not admitted a remote receipt.
      if (error.code === "read_state_conflict") return false;
      throw error;
    }
  });
  assert.equal(read.phoneReceiptSent, true);
  assert.equal((await client.thread(account, "sim_mei")).unreadCount, 0);
  await assert.rejects(
    client.markRead(account, "sim_mei", priorVersion),
    (error) => error.code === "read_state_conflict",
  );
  const avatar = await client.refreshAvatar(account, "sim_mei");
  await wait("cached avatar projection", async () => {
    const operation = await client.operation(account, avatar.id);
    assert.notEqual(
      operation.state,
      "failed",
      `avatar refresh: ${JSON.stringify(operation.result)}`,
    );
    return operation.state === "succeeded";
  });
  const bytes = await client.avatar(account, "sim_mei");
  assert.equal(bytes.contentType, "image/png");
  assert.deepEqual(
    [...bytes.bytes.slice(0, 8)],
    [137, 80, 78, 71, 13, 10, 26, 10],
  );
  const history = (await client.messages(account, "sim_mei")).items.map(
    (item) => item.id,
  );
  const connection = await connect(endpoint, token, true);
  try {
    const real = connection.session.client.logout.bind(
      connection.session.client,
    );
    let submissions = 0;
    connection.session.client.logout = async (account, key) => {
      const operation = await real(account, key);
      submissions++;
      if (submissions === 1) throw new Error("Lost logout admission response");
      return operation;
    };
    await assert.rejects(connection.logout(account));
    const recovered = await connection.logout(account);
    const done = await wait("logout recovery", async () => {
      const operation = await client.operation(account, recovered.id);
      assert.notEqual(operation.state, "failed");
      return operation.state === "succeeded" && operation;
    });
    assert.equal((await connection.logout(account)).id, done.id);
    assert.equal(submissions, 2);
    assert.equal((await client.account(account)).authenticated, false);
    assert.deepEqual(
      (await client.messages(account, "sim_mei")).items.map((item) => item.id),
      history,
    );
    assert.deepEqual(
      (await client.avatar(account, "sim_mei")).bytes,
      bytes.bytes,
    );
    assert.equal(
      (await client.account("work")).authenticated,
      other.authenticated,
    );
    await assert.rejects(
      client.sendText(
        account,
        "sim_mei",
        "must not send after logout",
        "after-logout",
      ),
    );
    const removal = await wait("signed-out idle profile removal", () =>
      client.removeAccount(account, account),
    );
    assert.equal(removal.filesRetained, true);
    assert.equal(removal.removed, true);
    assert.ok(
      !(await client.accounts()).items.some((item) => item.id === account),
    );
    await assert.rejects(
      client.account(account),
      (error) => error.code === "account_not_found",
    );
    assert.equal(
      (await client.account("work")).authenticated,
      other.authenticated,
    );
  } finally {
    connection.close();
  }
}
