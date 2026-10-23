import test from "node:test";
import assert from "node:assert/strict";
import {
  messagePresentation,
  recallText,
} from "../src/message-presentation.ts";

test("quoted replies and forwarded history never enter the media download flow", () => {
  assert.equal(
    messagePresentation({ type: 49, appMessageType: 57, body: "A reply" }),
    "reply",
  );
  assert.equal(
    messagePresentation({
      type: 49,
      quote: { serverId: "12", type: 1 },
      body: "Older hub reply",
    }),
    "reply",
  );
  assert.equal(
    messagePresentation({
      type: 49,
      appMessageType: 19,
      body: "Forwarded text",
    }),
    "forwarded",
  );
  assert.equal(
    messagePresentation({
      type: 49,
      appMessageType: 5,
      link: { title: "Shared page" },
    }),
    "link",
  );
  assert.equal(messagePresentation({ type: 49, appMessageType: 6 }), "media");
});

test("supported media keep their renderer, unsupported cards do not request downloads", () => {
  for (const type of [3, 34, 43, 47, 62])
    assert.equal(messagePresentation({ type }), "media");
  for (const type of [42, 48, 999])
    assert.equal(messagePresentation({ type }), "unsupported");
  for (const appMessageType of [undefined, 33, 74, 2000])
    assert.equal(
      messagePresentation({ type: 49, appMessageType }),
      "unsupported",
    );
  for (const type of [1, 10000, 10002])
    assert.equal(messagePresentation({ type }), "text");
});

test("recall markers override every body and content renderer", () => {
  assert.equal(
    messagePresentation({
      type: 49,
      appMessageType: 57,
      body: "Hidden text",
      recall: { observedAt: "now" },
    }),
    "recalled",
  );
});

test("recall wording uses the recorded actor rather than assuming the original author recalled it", () => {
  const name = (id) => (id === "bot" ? "Nick Bot" : "Someone");
  const own = { direction: "outgoing", senderId: "self" };
  assert.equal(
    recallText({ ...own, recall: { actorId: "self" } }, name),
    "You recalled this message",
  );
  assert.equal(
    recallText({ ...own, recall: { actorId: "bot" } }, name),
    "Nick Bot recalled this message",
  );
  assert.equal(recallText({ ...own, recall: {} }, name), "Message recalled");
  assert.equal(
    recallText(
      { direction: "incoming", senderId: "bot", recall: { actorId: "bot" } },
      name,
    ),
    "Message recalled",
  );
});
