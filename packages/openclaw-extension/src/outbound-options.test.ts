import test from "node:test";
import assert from "node:assert/strict";
import { createChannelMessageAdapterFromOutbound } from "openclaw/plugin-sdk/channel-outbound";
import { createWeChatMediaHandlers, nativePayloadMediaOptions, NativeMediaPayloadInspectionError, wechatMediaActions } from "./outbound-options.ts";
import { wechatPlugin } from "./channel.ts";
import type { sendWeChatMedia } from "./outbound-media.ts";

test("actual WeChat channel bindings expose persisted quality options and document-aware payload/media hooks", () => {
  assert.equal(wechatPlugin.actions, wechatMediaActions);
  assert.equal(typeof wechatPlugin.outbound?.sendPayload, "function"); assert.equal(typeof wechatPlugin.message?.send?.payload, "function");
  const discovery = wechatMediaActions.describeMessageTool({ cfg: {} })!;
  const schema = (Array.isArray(discovery.schema) ? discovery.schema[0] : discovery.schema)!;
  assert.equal(schema.visibility, "all-configured"); assert.equal(schema.properties.videoQuality, undefined);
  assert.deepEqual((schema.properties.channelData as any).properties["agent-wechat"].properties.videoQuality.enum, ["normal", "original"]);
});
test("pinned outbound bridge forwards forceDocument and persisted quality with stable queue identities", async () => {
  const calls: any[][] = [], handlers = createWeChatMediaHandlers((async (...args: any[]) => {
    calls.push(args); await args[7]?.(); return { messageId: "stored", meta: { outcome: "sent" } };
  }) as typeof sendWeChatMedia);
  const adapter = createChannelMessageAdapterFromOutbound({ id: "agent-wechat", outbound: handlers });
  let guards = 0;
  const context = { cfg: {}, to: "peer", text: "", mediaUrl: "/fixture/movie.mp4", forceDocument: true, deliveryQueueId: "queue", deliveryPartIndex: 0,
    assertDirectAdapterHandoff: () => { guards++; } };
  await adapter.send!.media!(context);
  assert.deepEqual(calls[0][6], { forceDocument: true }); assert.equal(guards, 1);
  const payload = JSON.parse(JSON.stringify({ text: "", mediaUrl: "/fixture/movie.mp4", channelData: { "agent-wechat": { videoQuality: "original" } } }));
  // The default host payload already contains channelData, including when the
  // host skips prepareSendPayload to force core queue persistence.
  await adapter.send!.payload!({ ...context, forceDocument: false, payload });
  await handlers.sendPayload({ ...context, forceDocument: false, payload });
  assert.deepEqual(calls[1][6], { forceDocument: false, videoQuality: "original" });
  assert.equal(calls[1][5], calls[2][5]); assert.match(calls[1][5], /^[a-f0-9]{64}$/);
  assert.equal(payload.channelData["agent-wechat"].videoQuality, "original");
});
test("document aliases, invalid quality and multi-attachment quality fail before handing off media", async () => {
  const payload = { mediaUrl: "/fixture/movie.mp4", channelData: { "agent-wechat": { videoQuality: "original" } } };
  for (const params of [{ forceDocument: true }, { asDocument: true }]) {
    assert.throws(() => wechatMediaActions.prepareSendPayload!({ ctx: { params } as any, to: "peer", payload }), /cannot also request/);
  }
  assert.throws(() => nativePayloadMediaOptions({ channelData: { "agent-wechat": { videoQuality: "automatic" } } }), /Invalid WeChat/);
  assert.throws(() => nativePayloadMediaOptions({ channelData: { "agent-wechat": { videoQuality: "normal", extra: true } } }), /Invalid WeChat/);
  let calls = 0; const handlers = createWeChatMediaHandlers((async () => { calls++; assert.fail("Invalid options cannot stage media"); }) as typeof sendWeChatMedia);
  await assert.rejects(handlers.sendPayload({ cfg: {}, to: "peer", text: "", payload: { ...payload, mediaUrls: ["/fixture/second.mp4"] } }), /one attachment/);
  await assert.rejects(handlers.sendPayload({ cfg: {}, to: "peer", text: "", forceDocument: true, payload }), /cannot also request/);
  assert.equal(calls, 0);
});
test("a later payload failure retains completed receipts and never includes private error content", async () => {
  const reports: any[] = []; let calls = 0;
  const handlers = createWeChatMediaHandlers((async () => {
    calls++; if (calls === 2) throw Error("synthetic private source detail"); return { messageId: "completed-first", meta: { outcome: "sent" } };
  }) as typeof sendWeChatMedia);
  await assert.rejects(handlers.sendPayload({ cfg: {}, to: "peer", text: "", payload: { mediaUrls: ["/fixture/a.pdf", "/fixture/b.pdf"] },
    onDeliveryResult: result => { reports.push(result); } }), error => {
    assert.ok(error instanceof NativeMediaPayloadInspectionError); assert.equal(error.retryable, false);
    assert.equal(error.deliveries[0].messageId, "completed-first"); assert.ok(!String(error).includes("synthetic private")); return true;
  });
  assert.equal(calls, 2); assert.equal(reports.length, 1); assert.equal(reports[0].messageId, "completed-first");
});
test("a receipt persistence failure stops the batch with the completed message identity", async () => {
  let calls = 0;
  const handlers = createWeChatMediaHandlers((async () => { calls++; return { messageId: "completed-first", meta: { outcome: "sent" } }; }) as typeof sendWeChatMedia);
  await assert.rejects(handlers.sendPayload({ cfg: {}, to: "peer", text: "", payload: { mediaUrls: ["/fixture/a.pdf", "/fixture/b.pdf"] },
    onDeliveryResult: () => { throw Error("synthetic private persistence error"); } }), error => {
    assert.ok(error instanceof NativeMediaPayloadInspectionError); assert.equal(error.deliveries[0].messageId, "completed-first"); return true;
  });
  assert.equal(calls, 1);
});

test("cancellation after a completed part retains its receipt", async () => {
  const controller = new AbortController(); let calls = 0;
  const handlers = createWeChatMediaHandlers((async () => {
    calls++; controller.signal.throwIfAborted();
    controller.abort(); return { messageId: "completed-before-cancel", meta: { outcome: "sent" } };
  }) as typeof sendWeChatMedia);
  await assert.rejects(handlers.sendPayload({ cfg: {}, to: "peer", text: "", signal: controller.signal,
    payload: { mediaUrls: ["/fixture/a.pdf", "/fixture/b.pdf"] } }), error => {
    assert.ok(error instanceof NativeMediaPayloadInspectionError);
    assert.equal(error.deliveries[0].messageId, "completed-before-cancel"); assert.equal(error.retryable, false); return true;
  });
  assert.equal(calls, 2);
});
