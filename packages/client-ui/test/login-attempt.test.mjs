import test from "node:test";
import assert from "node:assert/strict";
import { NativeApiError } from "@agent-wechat/shared";
import { LoginAttempt, openWhenFree } from "../src/login-attempt.ts";

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

test("sign-in tries saved credentials with QR fallback through the auto strategy", async () => {
  const saved = storage();
  let submissions = 0;
  const client = {
    login: async (account, options) => {
      submissions++;
      assert.equal(account, "a");
      assert.equal(options.strategy, "auto");
      assert.deepEqual(options.qrRefresh, { mode: "auto", maxRefreshes: 3 });
      return operation("awaiting_approval");
    },
    operation: async () => operation("awaiting_approval"),
  };
  assert.equal(
    (await new LoginAttempt(client, "a", saved).open()).state,
    "awaiting_approval"
  );
  await new LoginAttempt(client, "a", saved).open();
  assert.equal(submissions, 1);
});

test("choosing a QR code skips the saved sign-in for the same account", async () => {
  const saved = storage();
  const client = {
    login: async (account, options) => {
      assert.equal(account, "a");
      assert.equal(options.strategy, "qr");
      return operation();
    },
  };
  await new LoginAttempt(client, "a", saved).open("qr");
  assert.equal(saved.read().strategy, "qr");
  assert.equal(saved.read().operationId, "login-1");
});

test("a lost submission is repeated with its own strategy, not a newly chosen one", async () => {
  const saved = storage();
  const submissions = [];
  const client = {
    login: async (_account, options, key) => {
      submissions.push({ strategy: options.strategy, key });
      if (submissions.length === 1) throw new Error("response lost");
      return operation();
    },
  };
  await assert.rejects(new LoginAttempt(client, "a", saved).open("auto"));
  // The hub refuses a reused key with different options, so the retry keeps both.
  const retry = new LoginAttempt(client, "a", saved);
  await retry.open("qr");
  assert.deepEqual(submissions[1], submissions[0]);
  assert.equal(submissions[1].strategy, "auto");
  assert.equal(retry.strategy, "auto");
});

test("a resumed attempt reports the strategy it was started with", async () => {
  const saved = storage({ key: "k", operationId: "login-1", strategy: "qr" });
  const client = {
    operation: async () => operation("awaiting_scan"),
    login: () => assert.fail("a running attempt must be resumed"),
  };
  const resumed = new LoginAttempt(client, "a", saved);
  await resumed.open("auto");
  assert.equal(resumed.strategy, "qr");
  // Attempts saved before the strategy was recorded always used auto.
  const older = new LoginAttempt(client, "a", storage({ key: "k", operationId: "login-1" }));
  await older.open("qr");
  assert.equal(older.strategy, "auto");
});

test("a QR attempt after a failed saved sign-in gets a fresh key", async () => {
  const saved = storage({ key: "old", operationId: "login-1", strategy: "auto" });
  const client = {
    operation: async () => operation("failed"),
    login: async (_account, options, key) => {
      assert.equal(options.strategy, "qr");
      assert.notEqual(key, "old");
      return operation("awaiting_scan", "login-2");
    },
  };
  assert.equal((await new LoginAttempt(client, "a", saved).open("qr")).id, "login-2");
  assert.equal(saved.read().strategy, "qr");
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
    "login-2"
  );
  client.operation = async () => operation("succeeded", "login-2");
  client.login = () => assert.fail("completed approval must not restart login");
  assert.equal(
    (await new LoginAttempt(client, "a", saved).open()).state,
    "succeeded"
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
    "replacement"
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

const busy = () => new NativeApiError(409, "operation_in_progress");
const noWait = async () => {};

test("a sign-in refused by a busy account clears its key instead of replaying it", async () => {
  const saved = storage();
  const keys = [];
  const client = {
    login: async (_account, _options, key) => {
      keys.push(key);
      if (keys.length === 1) throw busy();
      return operation();
    },
  };
  await assert.rejects(new LoginAttempt(client, "a", saved).open("auto"), /operation_in_progress/);
  // The hub admitted nothing, so the next attempt may change its options.
  assert.equal(saved.read(), null);
  await new LoginAttempt(client, "a", saved).open("qr");
  assert.notEqual(keys[1], keys[0]);
  assert.equal(saved.read().strategy, "qr");
});

test("a busy refusal keeps another window's accepted sign-in", async () => {
  const saved = storage();
  const client = {
    login: async () => {
      // Another window's request was accepted while this one was in flight.
      saved.write({ key: "other-window", operationId: "login-9", strategy: "auto" });
      throw busy();
    },
  };
  await assert.rejects(new LoginAttempt(client, "a", saved).open("auto"), /operation_in_progress/);
  assert.deepEqual(saved.read(), { key: "other-window", operationId: "login-9", strategy: "auto" });
});

test("a busy account is retried until it accepts the sign-in", async () => {
  let refusals = 2;
  const waits = [];
  let waiting = 0;
  const client = {
    login: async () => {
      if (refusals-- > 0) throw busy();
      return operation("starting");
    },
  };
  const opened = await openWhenFree(new LoginAttempt(client, "a", storage()), "auto", {
    signedIn: async () => false,
    waiting: () => waiting++,
    wait: async (milliseconds) => waits.push(milliseconds),
    delayMs: 1500,
  });
  assert.equal(opened.operation.state, "starting");
  assert.equal(opened.alreadySignedIn, undefined);
  assert.deepEqual(waits, [1500, 1500]);
  assert.equal(waiting, 2);
});

test("a busy account that is already signed in opens without another sign-in", async () => {
  let submissions = 0;
  const client = {
    login: async () => {
      submissions++;
      throw busy();
    },
  };
  const opened = await openWhenFree(new LoginAttempt(client, "a", storage()), "auto", {
    signedIn: async () => true,
    wait: () => assert.fail("a signed-in account must not wait"),
  });
  assert.deepEqual(opened, { alreadySignedIn: true });
  assert.equal(submissions, 1);
});

test("a sign-in passes on a busy refusal after its attempts and other errors at once", async () => {
  let submissions = 0;
  const client = {
    login: async () => {
      submissions++;
      throw busy();
    },
  };
  await assert.rejects(
    openWhenFree(new LoginAttempt(client, "a", storage()), "auto", {
      signedIn: async () => false,
      wait: noWait,
      attempts: 3,
    }),
    /operation_in_progress/,
  );
  assert.equal(submissions, 3);
  client.login = async () => {
    submissions++;
    throw new NativeApiError(409, "auth_required");
  };
  await assert.rejects(
    openWhenFree(new LoginAttempt(client, "a", storage()), "auto", {
      signedIn: () => assert.fail("only a busy refusal checks the account"),
      wait: noWait,
    }),
    /auth_required/,
  );
  assert.equal(submissions, 4);
});

test("leaving the sign-in screen stops the retries", async () => {
  const controller = new AbortController();
  let submissions = 0;
  const client = {
    login: async () => {
      submissions++;
      throw busy();
    },
  };
  await assert.rejects(
    openWhenFree(new LoginAttempt(client, "a", storage()), "auto", {
      signal: controller.signal,
      signedIn: async () => false,
      wait: async () => controller.abort(),
    }),
    (error) => error.name === "AbortError",
  );
  assert.equal(submissions, 1);
});

test("resuming picks up a running sign-in and never submits one", async () => {
  const client = {
    login: () => assert.fail("resume must not start a sign-in"),
    operation: async () => operation("awaiting_approval"),
  };
  const running = new LoginAttempt(client, "a", storage({ key: "k", operationId: "login-1", strategy: "qr" }));
  assert.equal((await running.resume()).state, "awaiting_approval");
  assert.equal(running.strategy, "qr");
  assert.equal(await new LoginAttempt(client, "a", storage()).resume(), null);
  // A lost submission stays saved for an explicit sign-in to repeat.
  const lost = storage({ key: "k" });
  assert.equal(await new LoginAttempt(client, "a", lost).resume(), null);
  assert.deepEqual(lost.read(), { key: "k" });
  const ended = storage({ key: "k", operationId: "login-1" });
  client.operation = async () => operation("failed");
  assert.equal(await new LoginAttempt(client, "a", ended).resume(), null);
  assert.equal(ended.read(), null);
});
