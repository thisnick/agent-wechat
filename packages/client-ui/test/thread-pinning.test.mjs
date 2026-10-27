import test from "node:test";
import assert from "node:assert/strict";
import { NativeApiError } from "@agent-wechat/shared";
import { GroupMutations, validGroupReceipt } from "../src/group-creation.ts";
import { ThreadPinning } from "../src/thread-pinning.ts";
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
};
const op = (extra = {}) => ({
  id: "pin-op",
  accountId: "a",
  kind: "group_mutation",
  state: "succeeded",
  result: {
    action: "pin",
    targetId: "peer",
    pinned: true,
    remoteOutcome: "accepted",
    profileProjection: "complete",
  },
  ...extra,
});
async function until(fn) {
  for (let i = 0; i < 100; i++) {
    if (fn()) return;
    await new Promise((done) => setTimeout(done, 5));
  }
  assert.ok(fn());
}
function setup(t, overrides = {}, initial = {}) {
  let saved = initial.receipt ?? null,
    posts = 0,
    reads = 0;
  let state = {
    account: "a",
    connected: true,
    selected: "active",
    messages: [{ id: "kept" }],
    threads: [{ id: initial.thread ?? "peer", pinned: false, unreadCount: 3 }],
    capabilities: { threadPinningTargets: ["direct", "group"] },
    ...initial.state,
  };
  const listeners = new Set();
  const publish = (patch) => {
    state = { ...state, ...patch };
    for (const listener of listeners) listener();
  };
  const session = {
    snapshot: () => state,
    subscribe: (fn) => {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    refreshConversations: async () => {
      reads++;
      if (overrides.refresh) await overrides.refresh();
      else publish({ threads: [{ ...state.threads[0], pinned: true }] });
    },
  };
  const mutations = new GroupMutations(
    {
      setThreadPinned: async (account, thread, pinned, key) => {
        posts++;
        assert.equal(saved.key, key, "key must be stored before submission");
        assert.deepEqual(
          [account, thread, pinned],
          ["a", initial.thread ?? "peer", true],
        );
        return overrides.submit ? overrides.submit() : op();
      },
      operation: async () =>
        overrides.operation ? overrides.operation() : op(),
      groupMutationByKey: async (_a, key) => {
        assert.equal(key, saved.key);
        return overrides.lookup ? overrides.lookup() : op();
      },
    },
    "a",
    {
      read: () => saved,
      write: (value) => {
        saved = value;
      },
    },
  );
  const pin = new ThreadPinning(
    session,
    mutations,
    "a",
    initial.thread ?? "peer",
  );
  t.after(() => {
    pin.close();
    mutations.close();
  });
  return {
    pin,
    mutations,
    publish,
    saved: () => saved,
    posts: () => posts,
    reads: () => reads,
    state: () => state,
  };
}
test("pin controls share authoritative state and one busy receipt without opening an unread chat", async (t) => {
  const response = deferred(),
    view = deferred();
  const f = setup(t, {
    submit: () => response.promise,
    refresh: () => view.promise,
  });
  const changes = [];
  const stop = f.pin.subscribe(() => changes.push(f.pin.snapshot()));
  t.after(stop);
  const saving = f.pin.toggle();
  assert.equal(f.pin.snapshot().busy, true);
  assert.equal(f.pin.snapshot().pinned, false);
  await f.pin.toggle();
  assert.equal(f.posts(), 1);
  response.resolve(op());
  await until(() => f.reads() === 1);
  assert.equal(
    f.pin.snapshot().disabled,
    true,
    "accepted operation awaits canonical list",
  );
  f.publish({ threads: [{ id: "peer", pinned: true, unreadCount: 3 }] });
  view.resolve();
  await saving;
  assert.equal(f.pin.snapshot().pinned, true);
  assert.equal(f.pin.snapshot().disabled, false);
  assert.equal(f.state().selected, "active");
  assert.equal(f.state().threads[0].unreadCount, 3);
  assert.deepEqual(f.state().messages, [{ id: "kept" }]);
  assert.ok(changes.length > 1);
});
test("accepted changes with pending local application keep polling the same operation", async (t) => {
  const pending = op({
    result: { ...op().result, profileProjection: "pending" },
  });
  const f = setup(t, { submit: () => pending });
  await f.pin.toggle();
  assert.equal(f.pin.snapshot().busy, true);
  assert.equal(f.pin.snapshot().pinned, false);
  const key = f.saved().key;
  f.mutations.clear();
  assert.equal(f.saved().key, key);
  await f.pin.check();
  assert.equal(f.pin.snapshot().pinned, true);
  assert.equal(f.posts(), 1);
  assert.equal(f.saved().key, key);
});
test("rejection keeps the old pin state and exposes a retryable error", async (t) => {
  const f = setup(t, {
    submit: () =>
      op({
        state: "rejected",
        result: {
          ...op().result,
          remoteOutcome: "rejected",
          profileProjection: "pending",
        },
      }),
  });
  await f.pin.toggle();
  assert.equal(f.pin.snapshot().pinned, false);
  assert.match(f.pin.snapshot().error, /wasn’t applied/);
  assert.equal(f.pin.snapshot().disabled, false);
  assert.equal(f.reads(), 0);
});
test("lost admission and needs_review retain the key across reload without retrying", async (t) => {
  const unknown = op({
    state: "needs_review",
    result: {
      ...op().result,
      remoteOutcome: "unknown",
      profileProjection: "pending",
    },
  });
  const f = setup(t, {
    submit: () => {
      throw Error("lost response");
    },
    lookup: () => unknown,
  });
  await f.pin.toggle();
  const receipt = structuredClone(f.saved());
  assert.equal(f.pin.snapshot().uncertain, true);
  await f.pin.toggle();
  assert.equal(f.posts(), 1);
  const reopened = setup(
    t,
    { lookup: () => unknown, operation: () => unknown },
    { receipt },
  );
  await until(() => reopened.saved().operation?.state === "needs_review");
  assert.equal(reopened.posts(), 0);
  assert.equal(reopened.pin.snapshot().uncertain, true);
  reopened.mutations.clear();
  await reopened.pin.toggle();
  assert.equal(reopened.saved().key, receipt.key);
  assert.equal(reopened.posts(), 0);
  await reopened.pin.check();
  assert.equal(reopened.saved().key, receipt.key);
});
for (const [name, state] of [
  ["null", null],
  ["legacy", undefined],
])
  test(`unknown ${name} state is not Off and can be refreshed`, async (t) => {
    const f = setup(
      t,
      {},
      { state: { threads: [{ id: "peer", pinned: state }] } },
    );
    assert.equal(f.pin.snapshot().pinned, null);
    assert.equal(f.pin.snapshot().disabled, true);
    await f.pin.toggle();
    assert.equal(f.posts(), 0);
    await f.pin.check();
    assert.equal(f.pin.snapshot().pinned, true);
  });
for (const initial of [
  { thread: "filehelper" },
  { state: { capabilities: { threadPinningTargets: ["group"] } } },
])
  test("unsupported target cannot submit", async (t) => {
    const f = setup(t, {}, initial);
    assert.equal(f.pin.snapshot().supported, false);
    await f.pin.toggle();
    assert.equal(f.posts(), 0);
  });
for (const status of [403, 409])
  test(`${status} retains the prior state and never automatically resubmits`, async (t) => {
    const f = setup(t, {
      submit: () => {
        throw new NativeApiError(status, "forbidden");
      },
    });
    await f.pin.toggle();
    const key = f.saved().key;
    assert.equal(f.pin.snapshot().pinned, false);
    assert.equal(f.pin.snapshot().disabled, true);
    if (status === 403) assert.match(f.pin.snapshot().error, /permission/);
    else assert.equal(f.pin.snapshot().uncertain, true);
    await f.pin.toggle();
    assert.equal(f.posts(), 1);
    assert.equal(f.saved().key, key);
  });
for (const bad of [
  op({ accountId: "b" }),
  op({ kind: "other" }),
  op({ result: { ...op().result, targetId: "other" } }),
  op({ result: { ...op().result, pinned: false } }),
  op({ result: { ...op().result, action: "mute" } }),
])
  test("pin receipts validate account, kind, target, action and desired state", async (t) => {
    const f = setup(t, { submit: () => bad });
    await f.pin.toggle();
    assert.equal(f.saved().operation, undefined);
    assert.equal(f.pin.snapshot().pinned, false);
  });
test("stored pin requests validate their target and desired boolean", () => {
  for (const pinned of [null, undefined, "true", 1])
    assert.equal(
      validGroupReceipt(
        { key: "k", input: { action: "pin", groupId: "peer", pinned } },
        "peer",
      ),
      false,
    );
  assert.equal(
    validGroupReceipt(
      { key: "k", input: { action: "pin", groupId: "peer", pinned: true } },
      "other",
    ),
    false,
  );
  assert.equal(
    validGroupReceipt(
      { key: "k", input: { action: "pin", groupId: "peer", pinned: false } },
      "peer",
    ),
    true,
  );
});
test("an older operation read cannot overwrite a newer confirmed pin", async (t) => {
  const old = deferred();
  let calls = 0;
  const f = setup(t, {
    submit: () =>
      op({
        state: "needs_review",
        result: { ...op().result, remoteOutcome: "unknown" },
      }),
    operation: () => (++calls === 1 ? old.promise : op()),
  });
  await f.pin.toggle();
  const first = f.mutations.check();
  const second = f.mutations.check();
  await second;
  old.resolve(op({ state: "needs_review" }));
  await first;
  assert.equal(f.saved().operation.state, "succeeded");
  await until(() => !f.pin.snapshot().busy);
  assert.equal(f.pin.snapshot().pinned, true);
});

test("a failed status read exposes Check status instead of a permanently busy control", async (t) => {
  const pending = op({
    state: "queued",
    result: {
      ...op().result,
      remoteOutcome: "not_attempted",
      profileProjection: "pending",
    },
  });
  const f = setup(t, {
    submit: () => pending,
    operation: () => {
      throw new Error("offline");
    },
  });
  await f.pin.toggle();
  await f.pin.check();
  assert.equal(f.pin.snapshot().uncertain, true);
  assert.equal(f.pin.snapshot().busy, false);
  const key = f.saved().key;
  await f.pin.toggle();
  assert.equal(f.saved().key, key);
  assert.equal(f.posts(), 1);
});

test("known unsupported-target conflicts are safe admission rejections, not uncertain sends", async (t) => {
  const f = setup(t, {
    submit: () => {
      throw new NativeApiError(409, "ordinary_thread_required");
    },
  });
  await f.pin.toggle();
  assert.equal(f.saved().notAdmitted, true);
  assert.equal(f.pin.snapshot().uncertain, false);
  assert.match(f.pin.snapshot().error, /unavailable/);
  assert.equal(f.pin.snapshot().pinned, false);
});

test("confirmed receipts reuse the current inventory after reload without per-row operation reads", async (t) => {
  let lookups = 0;
  const receipt = {
    key: "completed",
    input: { action: "pin", groupId: "peer", pinned: true },
    operation: op(),
  };
  const f = setup(
    t,
    {
      operation: () => {
        lookups++;
        return op();
      },
      lookup: () => {
        lookups++;
        return op();
      },
    },
    { receipt },
  );
  await new Promise((done) => setTimeout(done, 10));
  assert.equal(
    f.pin.snapshot().pinned,
    false,
    "a later phone unpin wins over an older completed receipt",
  );
  assert.equal(f.pin.snapshot().busy, false);
  assert.equal(f.reads(), 0);
  assert.equal(lookups, 0);
});

test("a running operation with an unsettled remote outcome is saving, not uncertain", async (t) => {
  const running = op({
    state: "running",
    result: {
      ...op().result,
      remoteOutcome: "unknown",
      profileProjection: "pending",
    },
  });
  const f = setup(t, { submit: () => running });
  await f.pin.toggle();
  assert.equal(f.pin.snapshot().uncertain, false);
  assert.equal(f.pin.snapshot().busy, true);
  const key = f.saved().key;
  await f.pin.check();
  assert.equal(f.pin.snapshot().pinned, true);
  assert.equal(f.saved().key, key);
  assert.equal(f.posts(), 1);
});
