import test from "node:test";
import assert from "node:assert/strict";
import { sameEmbeddedQuote } from "./native-quote.ts";

test("new quote display fields do not invalidate an older queued excerpt", () => {
  const stored = { serverId: "42", type: 49, appMessageType: 5, state: "embedded",
    senderId: "sender", embeddedText: "[Link] Title\nhttps://example.com/", embeddedImage: false };
  const current = { ...stored, embeddedLink: { title: "Title", url: "https://example.com/", description: null, sourceName: null } };
  assert.equal(sameEmbeddedQuote(stored, current), true);
  for (const change of [
    { serverId: "43" }, { type: 3 }, { appMessageType: 4 }, { senderId: "other" },
    { state: "recalled" }, { embeddedText: "Changed excerpt" }, { embeddedImage: true }, { unknownField: true },
  ]) assert.equal(sameEmbeddedQuote(stored, { ...current, ...change }), false);
  assert.equal(sameEmbeddedQuote(current, { ...current, embeddedLink: { ...current.embeddedLink, title: "Changed title" } }), false);
  assert.equal(sameEmbeddedQuote({ ...stored, embeddedSticker: true }, { ...stored, embeddedSticker: false }), false);
});
