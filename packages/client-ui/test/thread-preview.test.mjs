import test from "node:test";
import assert from "node:assert/strict";
import { messageSummary, threadPreview } from "../src/thread-preview.ts";

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

test("recalled thread previews show the replacement notice without an inferred sender prefix", () => {
  for (const direction of ["incoming", "outgoing", null]) {
    const message = {
      id: "recalled",
      type: 10000,
      direction,
      senderId: direction === "outgoing" ? "self" : "bot",
      body: "Original text must stay hidden",
      recall: { replacementText: "Nick Bot recalled\n a message" },
    };
    const lastMessage = messageSummary(message, "Bot");
    assert.equal(lastMessage.text, "Nick Bot recalled a message");
    assert.equal(lastMessage.direction, direction);
    assert.equal(
      threadPreview({ id: "room@chatroom", kind: "group", lastMessage }),
      "Nick Bot recalled a message",
    );
    assert.equal(
      messageSummary({ ...message, recall: {} }, "Bot").text,
      "Message recalled",
    );
  }
});
