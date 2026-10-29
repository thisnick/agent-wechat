import test from "node:test";
import assert from "node:assert/strict";
import { parseWeChatTextMentions } from "./outbound-mentions.ts";
import { createWeChatMediaHandlers, nativePayloadMediaOptions, wechatMediaActions } from "./outbound-options.ts";
import { createChannelMessageAdapterFromOutbound } from "openclaw/plugin-sdk/channel-outbound";
import type { sendWeChatMedia } from "./outbound-media.ts";

test("mention recipients retain exact IDs and reject ambiguous or editor-only input", () => {
  assert.deepEqual(parseWeChatTextMentions({ userIds: ["wxid_b", "wxid_a"] }), { userIds: ["wxid_b", "wxid_a"] });
  assert.deepEqual(parseWeChatTextMentions({ all: true }), { all: true });
  for (const value of [null, [], {}, { userIds: [] }, { all: false }, { all: "true" }, { all: true, userIds: [] },
    { userIds: ["wxid_a", "wxid_a"] }, { userIds: ["A name"] }, { userIds: ["notify@all"] }, { userIds: [3] },
    { userIds: Array.from({ length: 129 }, (_, index) => `wxid_${index}`) }, { userIds: ["wxid_a"], positions: [] }]) {
    assert.throws(() => parseWeChatTextMentions(value), /Invalid WeChat mentions/);
  }
});

test("ordinary and replayed mention payloads preserve options and stable queue-derived keys", async () => {
  const calls: any[][] = [];
  const handlers = createWeChatMediaHandlers((async (...args: any[]) => {
    calls.push(args); await args[7]?.(); return { messageId: "sent", meta: { outcome: "sent" } };
  }) as typeof sendWeChatMedia);
  const payload = { text: "please check", channelData: { "agent-wechat": { mentions: { userIds: ["wxid_a"] } } } };
  assert.equal(wechatMediaActions.prepareSendPayload!({ ctx: { params: {} } as any, to: "g@chatroom", payload }), payload);
  const ctx = { cfg: {}, to: "g@chatroom", text: "please check", deliveryQueueId: "queue-mentions", deliveryPartIndex: 2, payload };
  await handlers.sendPayload(ctx);
  await handlers.sendText({ ...ctx, payload: JSON.parse(JSON.stringify(payload)) });
  assert.deepEqual(calls[0][6], { mentions: { userIds: ["wxid_a"] }, forceDocument: undefined });
  assert.equal(calls[0][5], calls[1][5]); assert.match(calls[0][5], /^[a-f0-9]{64}$/);
  await handlers.sendPayload({ ...ctx, deliveryPartIndex: 3, payload: { ...payload, channelData: { "agent-wechat": { mentions: { all: true } } } } });
  assert.notEqual(calls[0][5], calls[2][5]); assert.deepEqual(calls[2][6].mentions, { all: true });
});

test("mention combinations fail at prepare and forced-core boundaries before any send", async () => {
  let sends = 0;
  const handlers = createWeChatMediaHandlers((async () => { sends++; throw Error("must not dispatch"); }) as typeof sendWeChatMedia);
  const payload = { text: "hi", channelData: { "agent-wechat": { mentions: { userIds: ["wxid_a"] } } } };
  for (const extra of [{ to: "wxid_dm" }, { mediaUrl: "/fixture/a.png" }, { mediaUrls: ["/fixture/a.mp4"] },
    { audioAsVoice: true }, { forceDocument: true },
    { channelData: { "agent-wechat": { mentions: { all: true }, videoQuality: "original" } } }, { text: " " }]) {
    const ctx = { cfg: {}, to: "g@chatroom", text: "hi", ...extra, payload: { ...payload, ...extra } };
    assert.throws(() => wechatMediaActions.prepareSendPayload!({ ctx: { params: extra } as any, to: ctx.to, payload: ctx.payload, replyToId: (extra as any).replyToId }), /WeChat mentions/);
    await assert.rejects(handlers.sendPayload(ctx), /WeChat mentions/);
    await assert.rejects(handlers.sendText(ctx), /WeChat mentions/);
    await assert.rejects(handlers.sendMedia(ctx), /WeChat mentions/);
  }
  assert.throws(() => nativePayloadMediaOptions({ channelData: { "agent-wechat": { mentions: { userIds: ["wxid_a"] }, invalid: true } } }), /Invalid WeChat/);
  assert.equal(sends, 0);
});


test("quoted mentions survive prepared and queued adapter payloads with one send key", async () => {
  const calls: any[][] = [];
  const handlers = createWeChatMediaHandlers((async (...args: any[]) => {
    calls.push(args); return { messageId: "sent", meta: { outcome: "sent" } };
  }) as typeof sendWeChatMedia);
  const adapter = createChannelMessageAdapterFromOutbound({ id: "agent-wechat", outbound: handlers });
  for (const mentions of [{ userIds: ["wxid_a"] }, { all: true }]) {
    const payload = { text: "please check", replyToId: "msg_source", channelData: { "agent-wechat": { mentions } } };
    assert.equal(wechatMediaActions.prepareSendPayload!({ ctx: { params: {} } as any, to: "g@chatroom", payload }), payload);
    const ctx = { cfg: {}, to: "g@chatroom", text: payload.text, deliveryQueueId: "quoted-mention-queue", payload };
    await handlers.sendPayload(ctx);
    await adapter.send!.text!({ ...ctx, replyToId: payload.replyToId });
    await adapter.send!.payload!({ ...ctx, payload: JSON.parse(JSON.stringify(payload)) });
    for (const call of calls.slice(-3)) {
      assert.deepEqual(call[6], { mentions, forceDocument: undefined, replyToId: payload.replyToId });
      assert.equal(call[5], calls.at(-1)![5]);
    }
  }
});
