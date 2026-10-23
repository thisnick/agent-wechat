import test from "node:test";
import assert from "node:assert/strict";
import { canReply, validQuoteReference } from "../src/reply.ts";

const message = {
  id: "m",
  serverId: "18446744073709551615",
  senderId: "peer",
  direction: "incoming",
  type: 1,
  body: "Hello",
};
test("reply eligibility matches native supported types and preserves full unsigned identities", () => {
  for (const type of [1, 3, 34, 43])
    assert.equal(canReply({ ...message, type }), true);
  for (const appMessageType of [5, 6, 57])
    assert.equal(canReply({ ...message, type: 49, appMessageType }), true);
  for (const change of [
    { type: 47 },
    { type: 10000 },
    { type: 49, appMessageType: 19 },
    { recall: {} },
    { senderId: null },
    { direction: null },
    { serverId: null },
    { serverId: "18446744073709551616" },
    { serverId: "01" },
    { body: "" },
  ])
    assert.equal(canReply({ ...message, ...change }), false);
  assert.equal(canReply({ ...message, direction: "outgoing" }), true);
});
test("stored reply references carry only exact identities, never caller-supplied quote content", () => {
  const reference = { messageId: "m", serverId: message.serverId };
  assert.equal(validQuoteReference(reference), true);
  for (const value of [
    null,
    [],
    {},
    { ...reference, content: "forged XML" },
    { ...reference, serverId: 12 },
    { ...reference, serverId: "0" },
  ])
    assert.equal(validQuoteReference(value), false);
});
