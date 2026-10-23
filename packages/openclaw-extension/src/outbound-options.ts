import type { ChannelPlugin } from "openclaw/plugin-sdk/core";
import { createHash } from "node:crypto";
import { sendWeChatMedia } from "./outbound-media.ts";
import { NativeMediaJobInspectionError, validateNativeMediaSendOptions, type NativeMediaSendOptions } from "./native-media.ts";
import type { NativeOutboundDelivery } from "./native-send.ts";

type MediaPayload = { text?: string; mediaUrl?: string; mediaUrls?: string[]; audioAsVoice?: boolean; channelData?: Record<string, unknown> };
type SendContext = { cfg: unknown; to: string; text: string; mediaUrl?: string; audioAsVoice?: boolean; forceDocument?: boolean; signal?: AbortSignal;
  deliveryQueueId?: string; deliveryPartIndex?: number;
  onPlatformSendDispatch?: () => Promise<void>; assertDirectAdapterHandoff?: () => void;
  onDeliveryResult?: (result: NativeOutboundDelivery & { channel: "agent-wechat" }) => Promise<void> | void; payload?: MediaPayload };
type Discovery = NonNullable<ReturnType<NonNullable<ChannelPlugin["actions"]>["describeMessageTool"]>>;

export function nativePayloadMediaOptions(payload: MediaPayload): NativeMediaSendOptions {
  const value = payload.channelData?.["agent-wechat"];
  if (value == null) return {};
  if (typeof value !== "object" || Array.isArray(value) || Object.keys(value).some(key => key !== "videoQuality")
    || !["normal", "original"].includes((value as { videoQuality: string }).videoQuality)) throw Error("Invalid WeChat media options");
  return { videoQuality: (value as { videoQuality: "normal" | "original" }).videoQuality };
}

export const wechatMediaActions: NonNullable<ChannelPlugin["actions"]> = {
  describeMessageTool: () => ({ actions: ["send"], schema: { actions: ["send"], visibility: "all-configured", properties: {
    channelData: { type: "object", description: "WeChat per-send media options. Use agent-wechat.videoQuality for a single video attachment.", additionalProperties: true, properties: {
      "agent-wechat": { type: "object", additionalProperties: false, required: ["videoQuality"], properties: {
        videoQuality: { type: "string", enum: ["normal", "original"], description: "WeChat video mode: normal prepares playback; original also retains the unchanged source video. Cannot combine with forceDocument." },
      } },
    } },
  } } } as Discovery),
  prepareSendPayload: ({ ctx, payload }) => {
    const options = nativePayloadMediaOptions(payload);
    if (options.videoQuality == null) return undefined;
    validateNativeMediaSendOptions({ ...options, forceDocument: (ctx.params.forceDocument ?? ctx.params.asDocument) as boolean | undefined }, payload.audioAsVoice);
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
    ? createHash("sha256").update(JSON.stringify(["wechat-media", ctx.deliveryQueueId, ctx.deliveryPartIndex ?? 0, index])).digest("hex") : undefined;
  const guard = (ctx: SendContext) => async () => {
    ctx.signal?.throwIfAborted(); await ctx.onPlatformSendDispatch?.(); ctx.signal?.throwIfAborted(); ctx.assertDirectAdapterHandoff?.();
  };
  return {
    sendMedia: async (ctx: SendContext) => ({ channel: "agent-wechat" as const,
      ...await send(ctx.cfg, ctx.to, ctx.text, ctx.mediaUrl, ctx.audioAsVoice, key(ctx), { forceDocument: ctx.forceDocument }, guard(ctx)) }),
    sendPayload: async (ctx: SendContext & { payload: MediaPayload }) => {
      const options = { ...nativePayloadMediaOptions(ctx.payload), forceDocument: ctx.forceDocument };
      const audioAsVoice = ctx.payload.audioAsVoice ?? ctx.audioAsVoice;
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
    },
  };
}
