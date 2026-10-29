import test from "node:test";
import assert from "node:assert/strict";
import aliases from "../src/emoji-aliases.json" with { type: "json" };
import cases from "./fixtures/emoji-text.json" with { type: "json" };
import { emojiText } from "../src/emoji-text.ts";
import { threadPreview, messageSummary } from "../src/thread-preview.ts";
import { embeddedQuotePreview, quotedText } from "../src/quote-preview.ts";

for (const [input, expected] of cases) {
  test(`display emoji text: ${JSON.stringify(input)}`, () => {
    assert.equal(emojiText(input), expected);
  });
}
test("the curated aliases are exact tokens and are consumed in full", () => {
  for (const [alias, emoji] of Object.entries(aliases)) {
    assert.ok(/^\[[^\[\]]+\]$|^\/\S+$/.test(alias), alias);
    assert.ok(emoji && !emoji.includes("["), alias);
    assert.equal(emojiText(`甲${alias}乙`), `甲${emoji}乙`, alias);
  }
});
test("display conversion leaves source messages, cached summaries and quote excerpts intact", () => {
  const source = Object.freeze({
    id: "emoji-message", accountId: "a", threadId: "room@chatroom",
    serverId: "123", type: 1, body: "你好[微笑] 😂", direction: "incoming",
    senderId: "peer", createdAt: "2026-10-28T12:00:00Z",
  });
  const summary = Object.freeze(messageSummary(source, "Peer"));
  assert.equal(summary.text, source.body);
  assert.equal(threadPreview({ id: source.threadId, kind: "group", lastMessage: summary }), "Peer: 你好🙂 😂");
  assert.equal(threadPreview({ id: "peer", lastMessage: { ...summary, direction: "outgoing" } }), "You: 你好🙂 😂");
  assert.equal(quotedText(source), source.body);
  const reply = Object.freeze({ ...source, type: 49, appMessageType: 57, quote: {
    type: 1, state: "embedded", embeddedText: source.body,
  } });
  const quote = embeddedQuotePreview(reply);
  assert.equal(emojiText(quote.text), "你好🙂 😂");
  assert.equal(quote.text, source.body);
  assert.equal(source.body, "你好[微笑] 😂");
  assert.equal(summary.text, source.body);
  assert.equal(reply.quote.embeddedText, source.body);
});
