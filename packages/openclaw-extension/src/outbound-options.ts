import type { ChannelPlugin } from "openclaw/plugin-sdk/core";
import { sendWeChatMedia } from "./outbound-media.ts";
import { NativeMediaJobInspectionError, validateNativeMediaSendOptions } from "./native-media.ts";
import type { NativeOutboundDelivery } from "./native-send.ts";
import { parseWeChatTextMentions, validateWeChatMentionSend } from "./outbound-mentions.ts";
import type { WeChatOutboundSendOptions } from "./outbound-media.ts";
import { nativeOutboundKey } from "./native-send-key.ts";

type MediaPayload = { text?: string; mediaUrl?: string; mediaUrls?: string[]; audioAsVoice?: boolean; channelData?: Record<string, unknown>; replyToId?: string };
type SendContext = { cfg: unknown; to: string; text: string; mediaUrl?: string; audioAsVoice?: boolean; forceDocument?: boolean; signal?: AbortSignal;
  replyToId?: string | null;
  deliveryQueueId?: string; deliveryPartIndex?: number;
  onPlatformSendDispatch?: () => Promise<void>; assertDirectAdapterHandoff?: () => void;
  onDeliveryResult?: (result: NativeOutboundDelivery & { channel: "agent-wechat" }) => Promise<void> | void; payload?: MediaPayload };
type Discovery = NonNullable<ReturnType<NonNullable<ChannelPlugin["actions"]>["describeMessageTool"]>>;

export function nativePayloadMediaOptions(payload: MediaPayload): WeChatOutboundSendOptions {
  const value = payload.channelData?.["agent-wechat"];
  if (value == null) return {};
  if (typeof value !== "object" || Array.isArray(value) || Object.keys(value).some(key => key !== "videoQuality" && key !== "mentions")) throw Error("Invalid WeChat send options");
  const options = value as Record<string, unknown>;
  if (!Object.keys(options).length || options.videoQuality !== undefined && !["normal", "original"].includes(options.videoQuality as string)) throw Error("Invalid WeChat media options");
  return { ...(options.videoQuality !== undefined ? { videoQuality: options.videoQuality as "normal" | "original" } : {}),
    ...(options.mentions !== undefined ? { mentions: parseWeChatTextMentions(options.mentions) } : {}) };
}

export const wechatMediaActions: NonNullable<ChannelPlugin["actions"]> = {
  describeMessageTool: () => ({ actions: ["send"], schema: { actions: ["send"], visibility: "all-configured", properties: {
    channelData: { type: "object", description: "WeChat send options: mentions for ordinary group text, or videoQuality for one video attachment.", additionalProperties: true, properties: {
      "agent-wechat": { type: "object", additionalProperties: false, anyOf: [{ required: ["videoQuality"] }, { required: ["mentions"] }], properties: {
        videoQuality: { type: "string", enum: ["normal", "original"], description: "WeChat video mode: normal prepares playback; original also retains the unchanged source video. Cannot combine with forceDocument." },
        mentions: { type: "object", additionalProperties: false,
          description: "Mention exact members in an ordinary group text message. Get userIds from wechat_group_info, not display names. The hub adds visible @names and metadata. Use all=true only when the sender has permission. Cannot combine with DMs, media, voice, video quality or quoted replies.",
          properties: { userIds: { type: "array", minItems: 1, maxItems: 128, uniqueItems: true, items: { type: "string", minLength: 1, maxLength: 256, pattern: "^[A-Za-z0-9_@.-]+$" } },
            all: { type: "boolean", const: true } },
          oneOf: [{ required: ["userIds"], not: { required: ["all"] } }, { required: ["all"], not: { required: ["userIds"] } }],
        },
      } },
    } },
  } } } as Discovery),
  prepareSendPayload: ({ ctx, to, payload, replyToId }) => {
    const options = nativePayloadMediaOptions(payload);
    const forceDocument = (ctx.params.forceDocument ?? ctx.params.asDocument) as boolean | undefined;
    validateWeChatMentionSend(options.mentions, { ...payload, ...options, to, forceDocument, replyToId: replyToId ?? payload.replyToId });
    if (options.videoQuality == null && options.mentions == null) return undefined;
    validateNativeMediaSendOptions({ ...options, forceDocument }, payload.audioAsVoice);
    return payload;
  },
};

/** Completed parts remain inspectable if a later attachment cannot finish. */
export class NativeMediaPayloadInspectionError extends Error {
  readonly retryable = false;
  readonly code = "native_media_payload_inspection_required";
  readonly deliveries: readonly NativeOutboundDelivery[];
  readonly pendingJob?: { jobId: string; accountId: string; threadId: string; idempotencyKey: string };
  constructor(deliveries: readonly NativeOutboundDelivery[], pendingJob?: { jobId: string; accountId: string; threadId: string; idempotencyKey: string }) {
    super("WeChat attachment batch requires inspection; retain completed receipts and do not automatically resend");
    this.name = "NativeMediaPayloadInspectionError";
    this.deliveries = deliveries; this.pendingJob = pendingJob;
  }
}

export function createWeChatMediaHandlers(send: typeof sendWeChatMedia = sendWeChatMedia) {
  const key = (ctx: SendContext, index = 0) => ctx.deliveryQueueId
    ? nativeOutboundKey(ctx.deliveryQueueId, ctx.deliveryPartIndex ?? 0, index) : undefined;
  const guard = (ctx: SendContext) => async () => {
    ctx.signal?.throwIfAborted(); await ctx.onPlatformSendDispatch?.(); ctx.signal?.throwIfAborted(); ctx.assertDirectAdapterHandoff?.();
  };
  const sendPayload = async (ctx: SendContext & { payload: MediaPayload }) => {
    const options = { ...nativePayloadMediaOptions(ctx.payload), forceDocument: ctx.forceDocument };
    const audioAsVoice = ctx.payload.audioAsVoice ?? ctx.audioAsVoice;
    validateWeChatMentionSend(options.mentions, { ...ctx.payload, ...options, to: ctx.to, text: ctx.payload.text ?? ctx.text,
      mediaUrl: ctx.payload.mediaUrl ?? ctx.mediaUrl, audioAsVoice, replyToId: ctx.replyToId ?? ctx.payload.replyToId });
    validateNativeMediaSendOptions(options, audioAsVoice);
    const urls = [...new Set([...(ctx.payload.mediaUrls ?? []), ctx.payload.mediaUrl ?? ctx.mediaUrl].filter((url): url is string => typeof url === "string" && !!url))];
    if (urls.length > 16 || options.videoQuality != null && urls.length !== 1) throw Error("WeChat video quality requires one attachment per send");
    const deliveries: NativeOutboundDelivery[] = [];
    for (const [index, mediaUrl] of (urls.length ? urls : [undefined]).entries()) {
      let delivered: NativeOutboundDelivery;
      try { delivered = await send(ctx.cfg, ctx.to, index === 0 ? ctx.payload.text ?? ctx.text : "", mediaUrl, audioAsVoice, key(ctx, index), options, guard(ctx)); }
      catch (error) {
        if (!deliveries.length) throw error;
        throw new NativeMediaPayloadInspectionError(deliveries, error instanceof NativeMediaJobInspectionError
          ? { jobId: error.jobId, accountId: error.accountId, threadId: error.threadId, idempotencyKey: error.idempotencyKey } : undefined);
      }
      deliveries.push(delivered);
      if (urls.length > 1) {
        try { await ctx.onDeliveryResult?.({ channel: "agent-wechat", ...delivered }); }
        catch { throw new NativeMediaPayloadInspectionError(deliveries); }
      }
      if (delivered.meta.outcome === "unknown") break;
    }
    const first = deliveries[0];
    return { channel: "agent-wechat" as const, ...first,
      ...(deliveries.length > 1 ? { meta: { ...first.meta, parts: deliveries, retryable: false,
        ...(deliveries.some(part => part.meta.outcome === "unknown") ? { outcome: "unknown", receiptState: "needs_review" } : {}) } } : {}) };
  };
  return {
    sendText: async (ctx: SendContext) => sendPayload({ ...ctx, payload: ctx.payload ?? { text: ctx.text } }),
    sendMedia: async (ctx: SendContext) => sendPayload({ ...ctx, payload: ctx.payload ?? { text: ctx.text, mediaUrl: ctx.mediaUrl } }),
    sendPayload,
  };
}
