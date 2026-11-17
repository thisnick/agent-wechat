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
      "contactPrivacy",
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
  const signal = new AbortController().signal;
  await actions.readPrivacy("peer", signal);
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
    key: "delete-key",
    action: "delete",
    requestId: "request",
    version: "request-v2",
  });
  assert.deepEqual(calls, [
    ["contactPrivacy", "account", "peer", signal],
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
