import { createHash } from "node:crypto";
import { randomUUID } from "node:crypto";
import { resolveWeChatAccount } from "./types.ts";
import { ensureNativeHost } from "./native-host.ts";
import { sendNativeAttachment, validateNativeMediaSendOptions, type NativeMediaSendOptions } from "./native-media.ts";
import { nativeOutboundDelivery, submitNativeText, type NativeOutboundDelivery } from "./native-send.ts";
import { validateWeChatMentionSend, type WeChatTextMentions } from "./outbound-mentions.ts";
import { nativeReplyMessageId, nativeReplyReference, parseWeChatReplyTo, validateWeChatReplySend } from "./outbound-reply.ts";

export type WeChatOutboundSendOptions = NativeMediaSendOptions & { mentions?: WeChatTextMentions; replyToId?: string };

export async function sendWeChatMedia(
  cfg: unknown,
  to: string,
  text: string,
  mediaUrl: string | undefined,
  audioAsVoice = false,
  idempotencyKey?: string,
  options: WeChatOutboundSendOptions = {},
  beforeSend?: () => Promise<void>,
): Promise<NativeOutboundDelivery> {
  validateWeChatMentionSend(options.mentions, { to, text, mediaUrl, audioAsVoice, ...options });
  const replyToId = parseWeChatReplyTo(options.replyToId);
  validateWeChatReplySend(replyToId, { text, mediaUrl, audioAsVoice, ...options });
  validateNativeMediaSendOptions(options, audioAsVoice);
  if (!mediaUrl && (options.forceDocument || options.videoQuality != null)) throw new Error("WeChat media options require an attachment");
  const account = resolveWeChatAccount(cfg as Record<string, unknown>);
  if (!account?.native) throw new Error("No native daemon configured");
  const { client } = await ensureNativeHost(account.native);
  const key = idempotencyKey ?? randomUUID();
  if (!mediaUrl) {
    if (audioAsVoice) throw new Error("Voice sending requires an audio attachment");
    const original = replyToId ? await client.message(account.native.accountId, nativeReplyMessageId(replyToId, account.native.accountId)) : undefined;
    const quote = replyToId && original ? nativeReplyReference(original, account.native.accountId, to, replyToId) : undefined;
    if (original?.recall) throw Error("The WeChat reply target has been recalled; choose another message");
    await beforeSend?.();
    const send = await submitNativeText(client, account.native.accountId, to, text, key, { mentions: options.mentions, quote });
    return nativeOutboundDelivery(client, account.native.accountId, to, send.localMessageId ? [send.localMessageId] : [],
      { sendId: send.id, state: send.state, serverIds: send.serverId ? [send.serverId] : undefined });
  }
  const result = await sendNativeAttachment(client, account, to, mediaUrl, audioAsVoice, key, beforeSend, options);
  if (text) { await beforeSend?.(); await submitNativeText(client, account.native.accountId, to, text, createHash("sha256").update(`${key}\0caption`).digest("hex")); }
  return result;
}
