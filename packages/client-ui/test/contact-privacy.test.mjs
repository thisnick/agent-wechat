import test from "node:test";
import assert from "node:assert/strict";
import {
  contactDetailActions,
  restoreContactChange,
  sameContactPrivacy,
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
