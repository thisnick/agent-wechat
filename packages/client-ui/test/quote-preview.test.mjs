import test from "node:test";
import assert from "node:assert/strict";
import { localQuote, quotedText } from "../src/quote-preview.ts";
const parent = {
  id: "parent",
  accountId: "a",
  threadId: "room",
  quote: { serverId: "18446744073709551615", type: 49 },
};
const original = {
  id: "original",
  accountId: "a",
  threadId: "room",
  serverId: "18446744073709551615",
  type: 49,
  appMessageType: 57,
  body: "Quoted reply text",
};
test("quote previews use only a unique original in the same account and conversation", () => {
  assert.equal(
    localQuote(parent, [
      { ...original, threadId: "other" },
      { ...original, accountId: "b" },
      original,
    ]),
    original,
  );
  assert.equal(
    localQuote(parent, [original, { ...original, id: "duplicate" }]),
    undefined,
  );
  assert.equal(localQuote(parent, [{ ...original, type: 1 }]), undefined);
});
test("quoted replies show normalized text and recalls suppress their old content", () => {
  assert.equal(quotedText(original), "Quoted reply text");
  assert.equal(
    quotedText({ ...original, recall: { observedAt: "10" } }),
    "Message recalled",
  );
  assert.equal(
    quotedText({
      ...original,
      appMessageType: 99,
      body: "<msg>private transport</msg>",
    }),
    "Attachment",
  );
});
