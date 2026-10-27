import test from "node:test";
import assert from "node:assert/strict";
import { GroupMutations } from "../src/group-creation.ts";
import { ThreadMuting, ThreadPinning } from "../src/thread-pinning.ts";

for (const target of ["peer", "room@chatroom"])
  test(`mute/unmute shares pending state with pin and refreshes canonical ${target}`, async (t) => {
    let saved,
      resolve,
      desired,
      posts = 0,
      reads = 0;
    let state = {
      account: "a",
      connected: true,
      threads: [{ id: target, pinned: false, muted: false }],
      capabilities: {
        threadPinningTargets: ["direct", "group"],
        threadNotificationTargets: ["direct", "group"],
      },
    };
    const listeners = new Set();
    const publish = () => {
      for (const fn of listeners) fn();
    };
    const session = {
      snapshot: () => state,
      subscribe: (fn) => {
        listeners.add(fn);
        return () => listeners.delete(fn);
      },
      refreshConversations: async () => {
        reads++;
        state = {
          ...state,
          threads: [{ ...state.threads[0], muted: desired }],
        };
        publish();
      },
    };
    const operation = () => ({
      id: "op" + posts,
      accountId: "a",
      kind: "group_mutation",
      state: "succeeded",
      result: {
        action: "mute",
        targetId: target,
        remoteOutcome: "accepted",
        profileProjection: "partial",
      },
    });
    const mutations = new GroupMutations(
      {
        setThreadMuted: async (_account, thread, muted, key) => {
          assert.equal(thread, target);
          assert.equal(saved.key, key);
          posts++;
          desired = muted;
          return new Promise((done) => (resolve = done));
        },
      },
      "a",
      { read: () => null, write: (value) => (saved = value) },
    );
    const mute = new ThreadMuting(session, mutations, "a", target);
    const pin = new ThreadPinning(session, mutations, "a", target);
    t.after(() => {
      mute.close();
      pin.close();
      mutations.close();
    });
    for (const value of [true, false]) {
      const saving = mute.toggle();
      assert.equal(saved.input.muted, value);
      assert.equal(mute.snapshot().saving, true);
      assert.equal(pin.snapshot().disabled, true);
      assert.equal(pin.snapshot().saving, false);
      await mute.toggle();
      assert.equal(posts, value ? 1 : 2, "no duplicate submit while busy");
      resolve(operation());
      await saving;
      assert.equal(mute.snapshot().muted, value);
      assert.equal(mute.snapshot().saving, false);
      assert.equal(pin.snapshot().disabled, false);
    }
    assert.equal(reads, 2);
  });

test("unknown mute is not treated as unmuted", (t) => {
  const state = {
    account: "a",
    connected: true,
    threads: [{ id: "peer", muted: null }],
    capabilities: { threadNotificationTargets: ["direct"] },
  };
  const mutations = new GroupMutations({}, "a", {
    read: () => null,
    write: () => {},
  });
  const mute = new ThreadMuting(
    { snapshot: () => state, subscribe: () => () => {} },
    mutations,
    "a",
    "peer",
  );
  t.after(() => {
    mute.close();
    mutations.close();
  });
  assert.equal(mute.snapshot().muted, null);
  assert.equal(mute.snapshot().disabled, true);
});
