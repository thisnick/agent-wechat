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

test("a quote saved as unsupported stays valid once the daemon can read it", () => {
  // 1.0.0-beta.13 saved quotes of some link cards as unsupported; later daemons
  // read the same source message and return its text and link card.
  const stored = { appMessageType: 4, direction: "incoming", embeddedImage: false, embeddedText: null,
    senderId: "sender", serverId: "42", state: "unsupported", type: 49 } as any;
  const current = { ...stored, state: "embedded", embeddedText: "[Link] Title\nhttps://example.com/",
    embeddedLink: { title: "Title", url: "https://example.com/", description: null, sourceName: null } };
  assert.equal(sameEmbeddedQuote(stored, current), true);
  for (const change of [
    { serverId: "43" }, { type: 3 }, { appMessageType: 5 }, { senderId: "other" }, { direction: "outgoing" },
  ]) assert.equal(sameEmbeddedQuote(stored, { ...current, ...change }), false);
  for (const state of ["recalled", "unavailable"])
    assert.equal(sameEmbeddedQuote(stored, { ...current, state }), false);
  assert.equal(sameEmbeddedQuote({ ...stored, state: "embedded", embeddedText: "Saved" }, current), false);
});
