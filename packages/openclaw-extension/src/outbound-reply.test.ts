import test from "node:test";
import assert from "node:assert/strict";
import { createChannelMessageAdapterFromOutbound } from "openclaw/plugin-sdk/channel-outbound";
import { NativeApiError, type NativeMessage } from "@agent-wechat/shared";
import { createWeChatMediaHandlers, wechatMediaActions } from "./outbound-options.ts";
import { sendWeChatMedia } from "./outbound-media.ts";
import { stopNativeHost } from "./native-host.ts";
import { nativeReplyMessageId, nativeReplyReference, parseWeChatReplyTo } from "./outbound-reply.ts";
import { nativeSourceMessageSid } from "./native-prepared-boundary.ts";
import { nativeOutboundKey } from "./native-send-key.ts";

const original = { id: "msg_source", accountId: "account", threadId: "g@chatroom", serverId: "18446744073709551615",
  senderId: "wxid_member", direction: "incoming", type: 1, body: "Original" } as NativeMessage;

test("reply references use exact stored identities and decimal server IDs without number conversion", () => {
  assert.equal(parseWeChatReplyTo(null), undefined);
  for (const value of ["", ".", "..", 42, "bad\nID", "x".repeat(4097)]) assert.throws(() => parseWeChatReplyTo(value), /exact stored/);
  assert.deepEqual(nativeReplyReference(original, "account", "g@chatroom", "msg_source"), { messageId: "msg_source", serverId: "18446744073709551615" });
  const sourceId = nativeSourceMessageSid(original);
  assert.equal(nativeReplyMessageId(sourceId, "account"), original.id);
  assert.deepEqual(nativeReplyReference(original, "account", "g@chatroom", sourceId), { messageId: original.id, serverId: original.serverId });
  assert.throws(() => nativeReplyMessageId(sourceId, "other-account"), /invalid for this account/);
  assert.throws(() => nativeReplyMessageId("wechat-native:account:msg_source:invalid", "account"), /invalid for this account/);
  assert.throws(() => nativeReplyReference({ ...original, serverId: "42" }, "account", "g@chatroom", sourceId), /unavailable or outside/);
  for (const change of [{ id: "other" }, { accountId: "other" }, { threadId: "other@chatroom" }, { serverId: null },
    { serverId: "0" }, { serverId: "18446744073709551616" }]) {
    assert.throws(() => nativeReplyReference({ ...original, ...change }, "account", "g@chatroom", "msg_source"), /unavailable or outside/);
  }
});

test("tool, outbound and queued adapter paths retain replyTo and the same send key", async () => {
  const calls: any[][] = [];
  const handlers = createWeChatMediaHandlers((async (...args: any[]) => {
    calls.push(args); return { messageId: "sent", meta: { outcome: "sent" } };
  }) as typeof sendWeChatMedia);
  const adapter = createChannelMessageAdapterFromOutbound({ id: "agent-wechat", outbound: handlers });
  const payload = { text: "Agreed", replyToId: original.id };
  assert.equal(wechatMediaActions.prepareSendPayload!({ ctx: { params: {} } as any, to: "g@chatroom", payload }), payload);
  const ctx = { cfg: {}, to: "g@chatroom", text: payload.text, deliveryQueueId: "reply-queue", payload };
  await handlers.sendPayload(ctx);
  await adapter.send!.text!({ ...ctx, replyToId: original.id });
  await adapter.send!.payload!({ ...ctx, payload: JSON.parse(JSON.stringify(payload)) });
  assert.equal(calls.length, 3);
  for (const call of calls) {
    assert.deepEqual(call[6], { forceDocument: undefined, replyToId: original.id });
    assert.equal(call[5], nativeOutboundKey("reply-queue"));
  }
});

test("invalid and attachment replies fail at every adapter boundary before dispatch", async () => {
  let sends = 0;
  const handlers = createWeChatMediaHandlers((async () => { sends++; assert.fail("must not send"); }) as typeof sendWeChatMedia);
  for (const extra of [{ text: " " }, { replyToId: "" }, { mediaUrl: "/fixture/photo.jpg" }, { mediaUrls: ["/fixture/file.pdf"] },
    { audioAsVoice: true }, { forceDocument: true }, { channelData: { "agent-wechat": { videoQuality: "normal" } } }]) {
    const payload = { text: "Reply", replyToId: original.id, ...extra };
    const ctx = { cfg: {}, to: "g@chatroom", text: payload.text, ...extra, payload };
    assert.throws(() => wechatMediaActions.prepareSendPayload!({ ctx: { params: extra } as any, to: ctx.to, payload }), /WeChat quoted replies|exact stored/);
    await assert.rejects(handlers.sendText(ctx), /WeChat quoted replies|exact stored/);
    await assert.rejects(handlers.sendPayload(ctx), /WeChat quoted replies|exact stored/);
    await assert.rejects(handlers.sendMedia(ctx), /WeChat quoted replies|exact stored/);
  }
  assert.equal(sends, 0);
});

test("real SDK submits quote identities and refuses unsafe targets without a plain-text fallback", async t => {
  await stopNativeHost(); t.after(stopNativeHost);
  const cfg = { channels: { "agent-wechat": { enabled: true, backend: "native", native: { mode: "external", accountId: "account",
    endpoint: "http://127.0.0.1:6174", token: "synthetic-reply-token-".repeat(3), consumerStateDir: "/unused/synthetic" } } } };
  const submissions: any[] = []; let source: any = original, sourceMissing = false, rejectQuote = false, loseResponse = false;
  let recoveries = 0, dispatches = 0;
  const send = { id: "send_reply", accountId: "account", destination: "g@chatroom", state: "accepted", localMessageId: "msg_reply", serverId: "9007199254740999" };
  t.mock.method(globalThis, "fetch", async (url: URL, init: RequestInit) => {
    const path = url.pathname;
    if (path === "/v1/status") return Response.json({ version: { product: "agent-wechat", backend: "native", version: "1.0.0", apiVersion: 1,
      capabilities: ["serve", "events", "profile-read", "text-send", "native-login"] } });
    if (path.endsWith("/messages/msg_source")) return sourceMissing ? Response.json({ error: { code: "not_found" } }, { status: 404 }) : Response.json(source);
    if (path.endsWith("/messages/msg_reply")) return Response.json({ id: "msg_reply", accountId: "account", threadId: "g@chatroom", direction: "outgoing", serverId: send.serverId });
    if (path.endsWith("/sends/by-idempotency-key")) { recoveries++; assert.equal(url.searchParams.get("key"), "retained-reply-key"); return Response.json(send); }
    if (path.endsWith("/sends") && init.method === "POST") {
      submissions.push({ body: JSON.parse(init.body as string), key: new Headers(init.headers).get("Idempotency-Key") });
      if (loseResponse) throw Error("synthetic lost response");
      return rejectQuote ? Response.json({ error: { code: "unsupported_quote_target" } }, { status: 400 }) : Response.json(send);
    }
    assert.fail(`Unexpected synthetic path: ${path}`);
  });
  const sendReply = (replyToId = original.id) => sendWeChatMedia(cfg, "g@chatroom", "Agreed", undefined, false, "retained-reply-key", { replyToId }, async () => { dispatches++; });
  assert.equal((await sendReply()).messageId, "msg_reply");
  assert.deepEqual(submissions, [{ key: "retained-reply-key", body: { kind: "text", threadId: "g@chatroom", text: "Agreed", quote: { messageId: original.id, serverId: original.serverId } } }]);
  assert.equal((await sendReply(nativeSourceMessageSid(original))).messageId, "msg_reply");
  assert.deepEqual(submissions[1], submissions[0]);
  for (const change of [{ accountId: "other" }, { threadId: "other@chatroom" }, { id: "other" }, { serverId: null }, { recall: { replacementText: "Recalled" } }]) {
    source = { ...original, ...change };
    await assert.rejects(sendReply(), /WeChat reply target|recalled|Invalid native message identity/);
  }
  source = original; sourceMissing = true;
  await assert.rejects(sendReply(), error => error instanceof NativeApiError && error.code === "not_found");
  sourceMissing = false;
  assert.equal(submissions.length, 2); assert.equal(dispatches, 2);
  rejectQuote = true;
  await assert.rejects(sendReply(), error => error instanceof NativeApiError && error.code === "unsupported_quote_target");
  assert.equal(submissions.length, 3); assert.equal(recoveries, 0);
  rejectQuote = false; loseResponse = true;
  assert.equal((await sendReply()).messageId, "msg_reply");
  assert.equal(submissions.length, 4); assert.equal(recoveries, 1);
  assert.ok(submissions.every(item => item.body.quote.messageId === original.id && item.key === "retained-reply-key"));
  for (const mentions of [{ userIds: ["wxid_member"] }, { all: true }]) {
    const delivered = await sendWeChatMedia(cfg, "g@chatroom", "Agreed", undefined, false, "retained-reply-key", { replyToId: original.id, mentions });
    assert.equal(delivered.messageId, "msg_reply");
    assert.deepEqual(submissions.at(-1), { key: "retained-reply-key", body: {
      kind: "text", threadId: "g@chatroom", text: "Agreed", quote: { messageId: original.id, serverId: original.serverId }, mentions,
    } });
  }
  assert.equal(recoveries, 3, "combined sends recover the same key after a lost POST response");
});
