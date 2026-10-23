import test from "node:test";
import assert from "node:assert/strict";
import { threadPreview } from "../src/thread-preview.ts";

test("thread previews distinguish incoming, own, group and system messages", () => {
  const message = {
    type: 1,
    text: "See you\n  tomorrow",
    direction: "incoming",
    senderName: "Mei Chen",
  };
  const thread = { id: "mei", kind: "direct", lastMessage: message };
  assert.equal(threadPreview(thread), "See you tomorrow");
  assert.equal(
    threadPreview({
      ...thread,
      lastMessage: { ...message, direction: "outgoing" },
    }),
    "You: See you tomorrow",
  );
  assert.equal(
    threadPreview({ ...thread, kind: "group" }),
    "Mei Chen: See you tomorrow",
  );
  assert.equal(
    threadPreview({
      ...thread,
      kind: "group",
      lastMessage: { ...message, type: 47, text: "Sticker" },
    }),
    "Mei Chen: Sticker",
  );
  assert.equal(
    threadPreview({
      ...thread,
      kind: "group",
      lastMessage: { ...message, type: 10000, text: "Message recalled" },
    }),
    "Message recalled",
  );
});

test("missing history and missing member names have honest fallbacks", () => {
  assert.equal(threadPreview({ lastMessage: null }), "No messages yet");
  assert.equal(threadPreview({}), "Preview unavailable");
  assert.equal(
    threadPreview({
      id: "room@chatroom",
      lastMessage: { type: 1, text: "Hi", senderName: null },
    }),
    "Member: Hi",
  );
});
