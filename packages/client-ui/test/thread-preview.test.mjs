import test from "node:test";
import assert from "node:assert/strict";
import {
  messageSummary,
  threadPreview,
  orderThreads,
} from "../src/thread-preview.ts";

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
  assert.equal(messageSummary({ type: 49, appMessageType: 8 }, "Bot").text, "Sticker");
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

test("chat order follows the latest message time across incoming, own and live updates", () => {
  const thread = (id, time, direction = "incoming") => ({
    id,
    lastMessage: { id: `message-${id}`, createdAt: time, direction },
  });
  const threads = [
    thread("old", "10"),
    thread("new", "30", "outgoing"),
    thread("middle", "1970-01-01T00:00:20Z"),
  ];
  assert.deepEqual(
    orderThreads(threads).map((t) => t.id),
    ["new", "middle", "old"],
  );
  assert.deepEqual(
    threads.map((t) => t.id),
    ["old", "new", "middle"],
  );
  assert.deepEqual(
    orderThreads([
      {
        ...threads[0],
        lastMessage: { ...threads[0].lastMessage, createdAt: "40" },
      },
      ...threads.slice(1),
    ]).map((t) => t.id),
    ["old", "new", "middle"],
  );
});

test("equal and unknown activity times keep their existing relative order", () => {
  const threads = [
    { id: "missing" },
    { id: "first", lastMessage: { createdAt: "10" } },
    { id: "invalid", lastMessage: { createdAt: "invalid" } },
    { id: "second", lastMessage: { createdAt: "10" } },
    { id: "empty", lastMessage: null },
  ];
  assert.deepEqual(
    orderThreads(threads).map((t) => t.id),
    ["first", "second", "missing", "invalid", "empty"],
  );
});
