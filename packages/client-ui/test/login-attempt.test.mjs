import test from "node:test";
import assert from "node:assert/strict";
import { NativeApiError } from "@agent-wechat/shared";
import { LoginAttempt } from "../src/login-attempt.ts";

function storage(value = null) {
  return {
    read: () => value,
    write: (next) => {
      value = next;
    },
  };
}
const operation = (state = "awaiting_scan", id = "login-1") => ({
  id,
  accountId: "a",
  kind: "login",
  state,
});

test("a reopened page resumes the persisted QR operation without a second login", async () => {
  const saved = storage();
  let submissions = 0;
  const client = {
    login: async () => {
      submissions++;
      return operation();
    },
    operation: async (account, id) => {
      assert.equal(account, "a");
      assert.equal(id, "login-1");
      return operation("awaiting_approval");
    },
  };
  await new LoginAttempt(client, "a", saved).open();
  const resumed = await new LoginAttempt(client, "a", saved).open();
  assert.equal(resumed.state, "awaiting_approval");
  assert.equal(submissions, 1);
});

test("a lost admission response retains the same login idempotency key", async () => {
  const saved = storage();
  const keys = [];
  const client = {
    login: async (_account, _options, key) => {
      keys.push(key);
      if (keys.length === 1) throw new Error("response lost");
      return operation();
    },
  };
  await assert.rejects(new LoginAttempt(client, "a", saved).open());
  assert.ok(saved.read().key);
  await new LoginAttempt(client, "a", saved).open();
  assert.equal(keys[0], keys[1]);
  assert.equal(saved.read().operationId, "login-1");
});

test("an ended challenge gets a fresh key, while completed phone approval is resumed", async () => {
  const saved = storage({ key: "old", operationId: "login-1" });
  const client = {
    operation: async () => operation("expired"),
    login: async (_account, _options, key) => {
      assert.notEqual(key, "old");
      return operation("awaiting_scan", "login-2");
    },
  };
  assert.equal(
    (await new LoginAttempt(client, "a", saved).open()).id,
    "login-2",
  );
  client.operation = async () => operation("succeeded", "login-2");
  client.login = () => assert.fail("completed approval must not restart login");
  assert.equal(
    (await new LoginAttempt(client, "a", saved).open()).state,
    "succeeded",
  );
});

test("missing operations can restart, but a temporary lookup failure keeps the attempt", async () => {
  const saved = storage({ key: "old", operationId: "login-1" });
  const client = {
    operation: async () => {
      throw new NativeApiError(503, "unavailable");
    },
    login: () => assert.fail("temporary failure must not create another login"),
  };
  await assert.rejects(new LoginAttempt(client, "a", saved).open());
  assert.equal(saved.read().key, "old");
  client.operation = async () => {
    throw new NativeApiError(404, "not_found");
  };
  client.login = async () => operation("awaiting_scan", "replacement");
  assert.equal(
    (await new LoginAttempt(client, "a", saved).open()).id,
    "replacement",
  );
});

test("finishing an older attempt cannot erase the newer saved operation", () => {
  const saved = storage({ key: "new", operationId: "login-2" });
  const login = new LoginAttempt({}, "a", saved);
  login.complete("login-1");
  assert.equal(saved.read().operationId, "login-2");
  login.complete("login-2");
  assert.equal(saved.read(), null);
});
