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
test("Channels quote cards use embedded metadata and notices retain their text", () => {
  const link = {
    title: "Channels post",
    description: "Description",
    sourceName: "Publisher",
    url: "https://example.invalid/channels",
  };
  const reply = {
    ...parent,
    quote: {
      ...parent.quote,
      state: "embedded",
      appMessageType: 51,
      embeddedLink: link,
      embeddedText: "[Link] Channels post",
    },
  };
  assert.deepEqual(embeddedQuotePreview(reply).content.link, link);
  assert.equal(
    embeddedQuotePreview({
      ...reply,
      quote: { ...reply.quote, state: "recalled" },
    }).content,
    undefined
  );
  for (const appMessageType of [74, 87]) {
    const notice = {
      ...parent,
      quote: {
        ...parent.quote,
        state: "embedded",
        appMessageType,
        embeddedText: "Notice text",
      },
    };
    assert.equal(embeddedQuotePreview(notice).text, "Notice text");
  }
});
test("application text shares keep their embedded quote text and recall suppression", () => {
  const share = {
    ...original,
    appMessageType: 1,
    body: "https://example.com/shared",
  };
  const reply = {
    ...parent,
    quote: {
      ...parent.quote,
      appMessageType: 1,
      state: "embedded",
      embeddedText: share.body,
    },
  };
  assert.equal(quotedText(share), share.body);
  assert.equal(embeddedQuotePreview(reply, share).text, share.body);
  assert.equal(
    embeddedQuotePreview(reply, { ...share, recall: {} }).text,
    "Message recalled"
  );
});
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
  assert.equal(preview.content.id, reply.id);
  assert.equal(preview.content.accountId, reply.accountId);
  assert.equal(preview.content.threadId, reply.threadId);
  assert.equal(preview.content.type, 3);
  assert.equal(preview.content.quote, undefined);
  assert.equal(preview.content.appMessageType, undefined);
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
    assert.equal(embeddedQuotePreview(changed).content, undefined);
  }
  assert.equal(
    embeddedQuotePreview(reply, {
      ...original,
      type: 10000,
      recall: { observedAt: "10" },
    }).content,
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
    }).content,
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

test("quoted stickers and location pins keep readable labels when media is unavailable", () => {
  const raw = "<msg>transport metadata</msg>";
  for (const message of [{ type: 47 }, { type: 49, appMessageType: 8 }]) {
    assert.equal(quotedText({ ...message, body: raw }), "Sticker");
    assert.equal(embeddedQuotePreview({ type: 49, quote: { ...message, state: "unsupported" } }).text, "Sticker unavailable");
  }
  for (const [location, expected] of [[{ name: "Park", address: "Main Street" }, "Park"],
    [{ name: null, address: "Main Street" }, "Main Street"], [{ name: null, address: null }, "Location"]]) {
    assert.equal(quotedText({ type: 48, body: raw, location }), expected);
  }
  assert.equal(quotedText({ type: 48, body: raw }), "Location");
  assert.equal(embeddedQuotePreview({ type: 49, quote: { type: 48, state: "unsupported" } }).text, "Location unavailable");
  assert.equal(quotedText({ type: 42, body: raw }), "Shared contact");
  assert.equal(embeddedQuotePreview({ type: 49, quote: { type: 42, state: "unsupported" } }).text, "Shared contact unavailable");
  assert.equal(quotedText({ type: 49, appMessageType: 19, body: null }), "Forwarded messages");
  assert.equal(quotedText({ type: 49, appMessageType: 19, body: "[Forwarded messages]\nShared summary" }), "[Forwarded messages]");
  assert.equal(embeddedQuotePreview({ type: 49, quote: { type: 49, appMessageType: 19, state: "unsupported" } }).text, "Forwarded messages unavailable");
});

test("forwarded reply banners keep only the outer summary without nested message text", () => {
  const forwarded = { type: 49, appMessageType: 19, body: "[Chat History] Summary\nAlice: Nested private text" };
  assert.equal(quotedText(forwarded), "[Chat History] Summary");
  assert.equal(quotedText({ ...forwarded, body: "\n  [Chat History] Summary\r\nAlice: Nested private text" }), "[Chat History] Summary");
  assert.equal(quotedText({ ...forwarded, body: "\n \r\n" }), "Forwarded messages");
});

test("compact link quotes use embedded structured cards and keep the parent media identity", () => {
  for (const appMessageType of [3, 4, 5]) {
    const link = {
      title: "Received title",
      url: "https://example.com/received",
    };
    const reply = {
      ...parent,
      type: 49,
      quote: {
        ...parent.quote,
        state: "embedded",
        appMessageType,
        embeddedLink: link,
      },
    };
    const preview = embeddedQuotePreview(reply, {
      ...original,
      link: {
        title: "Different stored title",
        url: "https://example.com/stored",
      },
    });
    assert.equal(preview.content.id, reply.id);
    assert.equal(preview.content.link, link);
    assert.equal(preview.content.appMessageType, appMessageType);
    assert.equal(preview.content.quote, undefined);
    for (const state of ["missing", "unsupported", "recalled", "invalid"]) {
      assert.equal(
        embeddedQuotePreview({ ...reply, quote: { ...reply.quote, state } })
          .content,
        undefined
      );
    }
    assert.equal(
      embeddedQuotePreview({ ...reply, recall: {} }).content,
      undefined
    );
    assert.equal(
      embeddedQuotePreview(reply, { ...original, recall: {} }).content,
      undefined
    );
  }
});

test("ordinary and shared sticker quotes reuse sticker rendering without looking up an original", () => {
  for (const type of [47, 49]) {
    const reply = {
      ...parent,
      type: 49,
      quote: {
        ...parent.quote,
        type,
        appMessageType: type === 49 ? 8 : undefined,
        state: "embedded",
        embeddedSticker: true,
      },
    };
    const preview = embeddedQuotePreview(reply);
    assert.equal(preview.content.type, 47);
    assert.equal(preview.content.id, reply.id);
    assert.equal(preview.content.quote, undefined);
    assert.equal(preview.content.link, undefined);
    assert.equal(
      embeddedQuotePreview({
        ...reply,
        quote: { ...reply.quote, embeddedSticker: false },
      }).content,
      undefined
    );
    assert.equal(
      embeddedQuotePreview({
        ...reply,
        quote: { ...reply.quote, state: "recalled" },
      }).content,
      undefined
    );
  }
  assert.equal(quotedText({ type: 49, appMessageType: 8 }), "Sticker");
});
