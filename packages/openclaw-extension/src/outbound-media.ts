import { createHash } from "node:crypto";
import { randomUUID } from "node:crypto";
import { resolveWeChatAccount } from "./types.ts";
import { ensureNativeHost } from "./native-host.ts";
import { sendNativeAttachment, validateNativeMediaSendOptions, type NativeMediaSendOptions } from "./native-media.ts";
import { nativeOutboundDelivery, submitNativeText, type NativeOutboundDelivery } from "./native-send.ts";

export async function sendWeChatMedia(
  cfg: unknown,
  to: string,
  text: string,
  mediaUrl: string | undefined,
  audioAsVoice = false,
  idempotencyKey?: string,
  options: NativeMediaSendOptions = {},
  beforeSend?: () => Promise<void>,
): Promise<NativeOutboundDelivery> {
  validateNativeMediaSendOptions(options, audioAsVoice);
  if (!mediaUrl && (options.forceDocument || options.videoQuality != null)) throw new Error("WeChat media options require an attachment");
  const account = resolveWeChatAccount(cfg as Record<string, unknown>);
  if (!account?.native) throw new Error("No native daemon configured");
  const { client } = await ensureNativeHost(account.native);
  const key = idempotencyKey ?? randomUUID();
  if (!mediaUrl) {
    if (audioAsVoice) throw new Error("Voice sending requires an audio attachment");
    await beforeSend?.();
    const send = await submitNativeText(client, account.native.accountId, to, text, key);
    return nativeOutboundDelivery(client, account.native.accountId, to, send.localMessageId ? [send.localMessageId] : [],
      { sendId: send.id, state: send.state, serverIds: send.serverId ? [send.serverId] : undefined });
  }
  const result = await sendNativeAttachment(client, account, to, mediaUrl, audioAsVoice, key, beforeSend, options);
  if (text) { await beforeSend?.(); await submitNativeText(client, account.native.accountId, to, text, createHash("sha256").update(`${key}\0caption`).digest("hex")); }
  return result;
}
