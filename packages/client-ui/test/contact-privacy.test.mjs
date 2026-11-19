import { ContactQueries, contactKey } from "../src/contact-queries.ts";
import { QueryObserver } from "@tanstack/react-query";
import { NativeApiError } from "@agent-wechat/shared";
import test from "node:test";
import assert from "node:assert/strict";
import {
  contactDetailActions,
  restoreContactChange,
  sameContactPrivacy,
  readContactPrivacyWithPendingRetry,
} from "../src/contact-privacy.ts";

const privacy = { chatsOnly: true, hideMyPosts: true, hideTheirPosts: false };
test("the card adapter keeps privacy with the send and acceptance, and versions existing-friend changes", async () => {
  const calls = [];
  const client = Object.fromEntries(
    [
      "updateContact",
      "sendContactRequest",
      "acceptContactRequest",
      "deleteContactRequest",
      "removeContact",
    ].map((name) => [
      name,
      async (...args) => {
        calls.push([name, ...args]);
        return {};
      },
    ]),
  );
  const actions = contactDetailActions({ session: { client } }, "account");
  await actions.submit({
    key: "send-key",
    action: "send",
    input: { recipientRef: "discovery", message: "Hello", privacy },
  });
  await actions.submit({
    key: "accept-key",
    action: "accept",
    requestId: "request",
    version: "request-v1",
    privacy,
  });
  await actions.submit({
    key: "privacy-key",
    action: "privacy",
    contactId: "peer",
    input: { privacy, expectedVersion: "privacy-v1" },
  });
  await actions.submit({
    key: "remark-key",
    action: "remark",
    contactId: "peer",
    input: { remark: null, expectedVersion: "privacy-v1" },
  });
  await actions.submit({
    key: "delete-key",
    action: "delete",
    requestId: "request",
    version: "request-v2",
  });
  assert.deepEqual(calls, [
    [
      "sendContactRequest",
      "account",
      { recipientRef: "discovery", message: "Hello", privacy },
      "send-key",
    ],
    [
      "acceptContactRequest",
      "account",
      "request",
      { expectedVersion: "request-v1", privacy },
      "accept-key",
    ],
    [
      "updateContact",
      "account",
      "peer",
      { privacy, expectedVersion: "privacy-v1" },
      "privacy-key",
    ],
    [
      "updateContact",
      "account",
      "peer",
      { remark: null, expectedVersion: "privacy-v1" },
      "remark-key",
    ],
    ["deleteContactRequest", "account", "request", "request-v2", "delete-key"],
  ]);
});

test("older uncertain receipts retain the original key and omit newly introduced privacy inputs", () => {
  const send = {
    action: "send",
    key: "original-send",
    input: { recipientRef: "original-ref", message: "Original greeting" },
  };
  const restored = restoreContactChange(send, {});
  assert.deepEqual(restored, send);
  assert.equal("privacy" in restored.input, false);
  const accepted = restoreContactChange(
    { action: "accept", key: "original-accept" },
    { requestId: "original-request", version: "original-version" },
  );
  assert.deepEqual(accepted, {
    action: "accept",
    key: "original-accept",
    requestId: "original-request",
    version: "original-version",
  });
  assert.equal("privacy" in accepted, false);
  const frozen = { ...accepted, privacy, version: "frozen-version" };
  assert.deepEqual(
    restoreContactChange(frozen, {
      requestId: "new-request",
      version: "new-version",
    }),
    frozen,
  );
});

test("hidden Moments choices still count as privacy changes while Chats Only is selected", () => {
  assert.equal(sameContactPrivacy(privacy, { ...privacy }), true);
  assert.equal(
    sameContactPrivacy(privacy, { ...privacy, hideTheirPosts: true }),
    false,
  );
});

test("canonical privacy readback retries only pending GETs before returning the fresh version", async () => {
  let reads = 0,
    pending = 0;
  const pauses = [];
  const fresh = { privacy, version: "canonical-v2" };
  const value = await readContactPrivacyWithPendingRetry(
    async (signal) => {
      assert.equal(signal.aborted, false);
      if (++reads < 3) throw new NativeApiError(409, "contact_privacy_pending");
      return fresh;
    },
    new AbortController().signal,
    {
      pending: () => pending++,
      wait: async (milliseconds) => {
        pauses.push(milliseconds);
      },
    },
  );
  assert.equal(value, fresh);
  assert.equal(reads, 3);
  assert.equal(pending, 2);
  assert.deepEqual(pauses, [250, 500]);
});

test("privacy readback stops on forbidden, unresolved and other errors", async () => {
  for (const error of [
    new NativeApiError(403, "forbidden"),
    new NativeApiError(409, "contact_privacy_unresolved"),
    new NativeApiError(404, "not_found"),
    new NativeApiError(500, "internal", true),
    new TypeError("fetch failed"),
  ]) {
    let reads = 0;
    await assert.rejects(
      readContactPrivacyWithPendingRetry(
        async () => {
          reads++;
          throw error;
        },
        new AbortController().signal,
        {
          wait: async () => {
            assert.fail("Unrelated errors must not schedule a retry");
          },
        },
      ),
      (value) => value === error,
    );
    assert.equal(reads, 1);
  }
});

test("account-scope cancellation stops pending privacy reads and discards late results", async () => {
  const scope = new AbortController();
  let reads = 0;
  await assert.rejects(
    readContactPrivacyWithPendingRetry(
      async () => {
        reads++;
        throw new NativeApiError(409, "contact_privacy_pending");
      },
      scope.signal,
      {
        wait: async () => {
          scope.abort();
        },
      },
    ),
    { name: "AbortError" },
  );
  assert.equal(reads, 1);
  const late = new AbortController();
  await assert.rejects(
    readContactPrivacyWithPendingRetry(async () => {
      late.abort();
      return { privacy, version: "old-account" };
    }, late.signal),
    { name: "AbortError" },
  );
});

test("pending privacy readback has both a deadline and a finite GET budget", async () => {
  let reads = 0;
  await assert.rejects(
    readContactPrivacyWithPendingRetry(
      async () => {
        reads++;
        throw new NativeApiError(409, "contact_privacy_pending");
      },
      new AbortController().signal,
      { timeoutMs: 20 },
    ),
    (error) => error.code === "contact_privacy_pending",
  );
  assert.equal(reads, 1);
  reads = 0;
  await assert.rejects(
    readContactPrivacyWithPendingRetry(
      async () => {
        reads++;
        throw new NativeApiError(409, "contact_privacy_pending");
      },
      new AbortController().signal,
      { wait: async () => {} },
    ),
    (error) => error.code === "contact_privacy_pending",
  );
  assert.equal(reads, 8);
});


const pointContact = version => ({ id: "peer", privacy, privacyVersion: version });

/** One point read through the query cache, as a card's query runs it. */
const load = (cache, account, id) => cache.client.fetchQuery({
  queryKey: contactKey(account, id),
  queryFn: ({ signal }) => cache.read(account, id, signal),
  staleTime: 0,
});

test("an invalidated inventory cannot overwrite privacy after a contact update or account switch", async () => {
  const cache = new ContactQueries({ contact: async () => pointContact("current") });
  cache.reset("first");
  const applyInventory = cache.captureCatalog("first");
  cache.invalidate("first", "peer");
  await load(cache, "first", "peer");
  applyInventory([pointContact("stale")]);
  assert.equal(cache.get("first", "peer").version, "current");
  const beforeSwitch = cache.captureCatalog("first");
  cache.reset("first");
  beforeSwitch([pointContact("prior-login")]);
  assert.equal(cache.get("first", "peer"), undefined);
});

test("the live inventory warms a closed contact, and a newer inventory clears unavailable settings", async () => {
  const cache = new ContactQueries({
    contact: (_account, _id, signal) => new Promise((_, reject) => {
      signal.addEventListener("abort", () => reject(signal.reason), { once: true });
    }),
  });
  cache.reset("one");
  const dialog = new AbortController();
  const point = cache.read("one", "peer", dialog.signal);
  dialog.abort();
  await assert.rejects(point, { name: "AbortError" });
  cache.captureCatalog("one")([pointContact("warm")]);
  assert.equal(cache.get("one", "peer").version, "warm");
  cache.captureCatalog("one")([{ id: "peer", privacyPending: true }]);
  assert.equal(cache.get("one", "peer"), undefined);
  cache.reset("one", new NativeApiError(401, "unauthorized"));
  cache.captureCatalog("one")([pointContact("forbidden")]);
  assert.equal(cache.denied("one"), true);
  assert.equal(cache.get("one", "peer"), undefined);
});

test("a remark change replaces the snapshot even though the privacy version is unchanged", async () => {
  let point = { ...pointContact("v1"), remark: "Old", remarkPending: true };
  const cache = new ContactQueries({ contact: async () => point });
  cache.reset("one");
  assert.deepEqual(await load(cache, "one", "peer"), { privacy, version: "v1", remark: "Old", remarkPending: true });
  point = { ...pointContact("v1"), remark: "New" };
  await load(cache, "one", "peer");
  assert.deepEqual(cache.get("one", "peer"), { privacy, version: "v1", remark: "New", remarkPending: false });
  const before = cache.get("one", "peer");
  cache.captureCatalog("one")([{ ...pointContact("v1"), remark: "New" }]);
  assert.equal(cache.get("one", "peer"), before, "an identical catalog row keeps the same snapshot");
  cache.captureCatalog("one")([{ ...pointContact("v1"), remark: null }]);
  assert.equal(cache.get("one", "peer").remark, null);
});

test("missing contact privacy reads as none and pending privacy is retried", async () => {
  for (const pending of [undefined, true]) {
    const cache = new ContactQueries({
      contact: async () => ({ id: "peer", relation: "friend", privacyPending: pending }),
    });
    cache.reset("one");
    const read = cache.read("one", "peer", new AbortController().signal);
    if (pending) {
      await assert.rejects(read, { code: "contact_privacy_pending" });
    } else {
      assert.equal(await read, null);
      assert.equal(cache.get("one", "peer"), undefined);
    }
  }
});

test("a forbidden read denies the account's contact reads until it is reset", async () => {
  let calls = 0;
  const cache = new ContactQueries({ contact: async () => { calls++; throw new NativeApiError(403, "forbidden"); } });
  cache.reset("one");
  let notified = 0;
  cache.subscribe(() => notified++);
  await assert.rejects(load(cache, "one", "peer"), { status: 403 });
  assert.equal(cache.denied("one"), true);
  assert.equal(notified, 1);
  await assert.rejects(load(cache, "one", "other"), { status: 403 });
  assert.equal(calls, 1, "a denied account doesn't ask again");
  cache.reset("one");
  assert.equal(cache.denied("one"), false);
});

for (const sameSession of [true, false]) {
  test(sameSession ? "an open card's read restarts after a contact catalog refresh and keeps the snapshot"
    : "an open card's read stops after a session change even if the account ID is reused", async () => {
    let notifyStarted;
    const started = new Promise(resolve => { notifyStarted = resolve; });
    let calls = 0;
    const cache = new ContactQueries({
      contact: async (_account, _id, signal) => {
        if (++calls !== 2) return pointContact(calls === 1 ? "first" : "fresh");
        return new Promise((_, reject) => {
          signal.addEventListener("abort", () => reject(signal.reason), { once: true });
          notifyStarted();
        });
      },
    });
    cache.reset("one");
    // An open card observes the query, so invalidation refetches it in the background.
    const observer = new QueryObserver(cache.client, {
      queryKey: contactKey("one", "peer"),
      queryFn: ({ signal }) => cache.read("one", "peer", signal),
    });
    const seen = [];
    const unsubscribe = observer.subscribe((result) => seen.push(result.data?.version));
    try {
      await load(cache, "one", "peer");
      seen.length = 0;
      cache.invalidate("one", "peer");
      await started;
      assert.equal(cache.get("one", "peer").version, "first", "the old snapshot stays while it reloads");
      if (sameSession) {
        cache.invalidateAccount("one");
        // The account invalidation cancels the hanging read and starts a fresh one.
        for (let i = 0; i < 100 && cache.get("one", "peer").version !== "fresh"; i++)
          await new Promise((resolve) => setTimeout(resolve, 5));
        assert.equal(cache.get("one", "peer").version, "fresh");
        assert.equal(calls, 3);
        assert.ok(!seen.includes(undefined), "the card never loses its snapshot");
      } else {
        cache.reset("one");
        await new Promise((resolve) => setTimeout(resolve, 10));
        assert.equal(cache.get("one", "peer"), undefined);
        assert.equal(calls, 2);
      }
    } finally {
      unsubscribe();
    }
  });
}
