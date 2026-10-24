import test from "node:test";
import assert from "node:assert/strict";
import {
  embeddedQuotePreview,
  localQuote,
  quotedText,
} from "../src/quote-preview.ts";
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
test("quote navigation uses only a unique original in the same account and conversation", () => {
  assert.equal(
    localQuote(parent, [
      { ...original, threadId: "other" },
      { ...original, accountId: "b" },
      original,
    ]),
    original
  );
  assert.equal(
    localQuote(parent, [original, { ...original, id: "duplicate" }]),
    undefined
  );
  assert.equal(localQuote(parent, [{ ...original, type: 1 }]), undefined);
});
test("embedded excerpts and authors never come from a stored original", () => {
  const reply = {
    ...parent,
    body: "My comment",
    quote: {
      ...parent.quote,
      state: "embedded",
      senderId: "embedded-author",
      direction: "outgoing",
      embeddedText: "Received excerpt",
    },
  };
  const preview = embeddedQuotePreview(reply, {
    ...original,
    body: "Different stored content",
    senderId: "other-author",
  });
  assert.equal(preview.text, "Received excerpt");
  assert.equal(preview.senderId, "embedded-author");
  assert.equal(preview.own, true);
  assert.equal(embeddedQuotePreview(reply).text, "Received excerpt");
  assert.equal(reply.body, "My comment");
  assert.equal(
    embeddedQuotePreview(
      {
        ...reply,
        quote: { ...reply.quote, embeddedText: undefined, state: "missing" },
      },
      original
    ).text,
    "Quoted message unavailable"
  );
});
test("embedded image media keeps the reply identity even when an original is loaded", () => {
  const reply = {
    ...parent,
    type: 49,
    appMessageType: 57,
    quote: { ...parent.quote, type: 3, state: "embedded", embeddedImage: true },
  };
  const preview = embeddedQuotePreview(reply, { ...original, type: 3 });
  assert.equal(preview.image.id, reply.id);
  assert.equal(preview.image.accountId, reply.accountId);
  assert.equal(preview.image.threadId, reply.threadId);
  assert.equal(preview.image.type, 3);
  assert.equal(preview.image.quote, undefined);
  assert.equal(preview.image.appMessageType, undefined);
});
test("recalls suppress embedded text and images; other states keep supported summaries", () => {
  const reply = {
    ...parent,
    quote: {
      ...parent.quote,
      type: 3,
      state: "embedded",
      embeddedText: "Old text",
      embeddedImage: true,
    },
  };
  for (const changed of [
    { ...reply, recall: { observedAt: "10" } },
    { ...reply, quote: { ...reply.quote, state: "recalled" } },
  ]) {
    assert.equal(embeddedQuotePreview(changed).text, "Message recalled");
    assert.equal(embeddedQuotePreview(changed).image, undefined);
  }
  assert.equal(
    embeddedQuotePreview(reply, {
      ...original,
      type: 10000,
      recall: { observedAt: "10" },
    }).image,
    undefined
  );
  assert.equal(
    embeddedQuotePreview({
      ...reply,
      quote: {
        ...reply.quote,
        type: 34,
        state: "unsupported",
        embeddedText: undefined,
      },
    }).text,
    "Voice message unavailable"
  );
  assert.equal(
    embeddedQuotePreview({
      ...reply,
      quote: {
        ...reply.quote,
        type: 49,
        appMessageType: 6,
        state: "unsupported",
        embeddedText: "report.pdf",
      },
    }).text,
    "report.pdf"
  );
  assert.equal(
    embeddedQuotePreview({
      ...reply,
      quote: { ...reply.quote, state: "invalid" },
    }).image,
    undefined
  );
});
test("quoted replies show normalized text and recalls suppress their old content", () => {
  assert.equal(quotedText(original), "Quoted reply text");
  assert.equal(
    quotedText({ ...original, recall: { observedAt: "10" } }),
    "Message recalled"
  );
  assert.equal(
    quotedText({
      ...original,
      appMessageType: 99,
      body: "<msg>private transport</msg>",
    }),
    "Attachment"
  );
});
