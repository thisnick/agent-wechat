import test from "node:test";
import assert from "node:assert/strict";
import {
  messagePresentation,
  recallText,
  textMessageLink,
} from "../src/message-presentation.ts";

test("application text shares display as text and only whole safe URLs become links", () => {
  const message = {
    type: 49,
    appMessageType: 1,
    body: "https://example.com/article?a=1&b=2",
  };
  assert.equal(messagePresentation(message), "text");
  assert.equal(textMessageLink(message).href, message.body);
  for (const body of [
    "Shared text",
    "https://example.com/ with a comment",
    "javascript:alert(1)",
    "https://user:password@example.com/",
  ])
    assert.equal(textMessageLink({ ...message, body }), null);
  for (const body of [null, "", "   "])
    assert.equal(messagePresentation({ ...message, body }), "unsupported");
  assert.equal(messagePresentation({ ...message, recall: {} }), "recalled");
  assert.equal(textMessageLink({ ...message, recall: {} }), null);
});

test("Channels cards, upload notices, and announcements have bounded presentation", () => {
  const channel = {
    type: 49,
    appMessageType: 51,
    body: "[Link] Channels post",
    link: { title: "Channels post", url: "https://example.invalid/channels" },
  };
  assert.equal(messagePresentation(channel), "link");
  for (const appMessageType of [74, 87]) {
    const message = { type: 49, appMessageType, body: "Notice text" };
    assert.equal(messagePresentation(message), "text");
    assert.equal(messagePresentation({ ...message, recall: {} }), "recalled");
    assert.equal(
      messagePresentation({ ...message, body: null }),
      "unsupported"
    );
  }
  assert.equal(messagePresentation({ ...channel, recall: {} }), "recalled");
});

test("quoted replies and forwarded history never enter the media download flow", () => {
  assert.equal(
    messagePresentation({ type: 49, appMessageType: 57, body: "A reply" }),
    "reply"
  );
  assert.equal(
    messagePresentation({
      type: 49,
      quote: { serverId: "12", type: 1 },
      body: "Older hub reply",
    }),
    "reply"
  );
  assert.equal(
    messagePresentation({
      type: 49,
      appMessageType: 19,
      body: "Forwarded text",
    }),
    "forwarded"
  );
  assert.equal(
    messagePresentation({
      type: 49,
      appMessageType: 5,
      link: { title: "Shared page" },
    }),
    "link"
  );
  assert.equal(messagePresentation({ type: 49, appMessageType: 6 }), "media");
  assert.equal(messagePresentation({ type: 49, appMessageType: 8 }), "media");
});

test("supported media keep their renderer, unsupported cards do not request downloads", () => {
  for (const type of [3, 34, 43, 47, 62])
    assert.equal(messagePresentation({ type }), "media");
  for (const type of [42, 48, 999])
    assert.equal(messagePresentation({ type }), "unsupported");
  for (const appMessageType of [undefined, 33, 74, 2000])
    assert.equal(
      messagePresentation({ type: 49, appMessageType }),
      "unsupported"
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
    "recalled"
  );
});

test("recall wording preserves the server notice independently of the original author", () => {
  for (const direction of ["incoming", "outgoing", null]) {
    const message = {
      direction,
      senderId: direction === "outgoing" ? "self" : "bot",
      body: "Original text must stay hidden",
      recall: { replacementText: 'Nick Bot recalled a message <&> "notice"' },
    };
    assert.equal(
      recallText(message),
      'Nick Bot recalled a message <&> "notice"'
    );
    assert.equal(
      recallText({
        ...message,
        recall: { replacementText: "你撤回了一条消息" },
      }),
      "你撤回了一条消息"
    );
  }
});

test("missing notices never infer a recall initiator from sender or direction", () => {
  for (const direction of ["incoming", "outgoing", null])
    for (const replacementText of [undefined, null, ""])
      assert.equal(
        recallText({
          direction,
          senderId: "self",
          body: "Original text must stay hidden",
          recall: { replacementText },
        }),
        "Message recalled"
      );
});
