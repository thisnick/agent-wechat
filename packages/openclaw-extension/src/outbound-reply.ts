import type { NativeMessage, QuoteSendReference } from "@agent-wechat/shared";
import { nativeSourceMessageSid } from "./native-prepared-boundary.ts";

export function parseWeChatReplyTo(value: unknown): string | undefined {
  if (value == null) return undefined;
  if (typeof value !== "string" || !value || value.length > 4096 || value === "." || value === ".."
    || /[\u0000-\u001f\u007f]/.test(value)) throw Error("Use an exact stored WeChat message ID for replyTo");
  return value;
}

/** Host reply-to-current IDs include an account and source fingerprint. */
export function nativeReplyMessageId(replyToId: string, accountId: string): string {
  if (!replyToId.startsWith("wechat-native:")) return replyToId;
  const prefix = `wechat-native:${accountId}:`;
  const end = replyToId.lastIndexOf(":");
  const id = replyToId.slice(prefix.length, end);
  if (!replyToId.startsWith(prefix) || !/^[a-f0-9]{64}$/.test(replyToId.slice(end + 1))
    || !parseWeChatReplyTo(id)) throw Error("WeChat reply target identity is invalid for this account");
  return id;
}

export function validateWeChatReplySend(replyToId: unknown, input: {
  text?: string; mediaUrl?: string; mediaUrls?: string[]; audioAsVoice?: boolean; forceDocument?: boolean; videoQuality?: string;
}): void {
  if (parseWeChatReplyTo(replyToId) === undefined) return;
  if (input.mediaUrl || input.mediaUrls?.length || input.audioAsVoice || input.forceDocument || input.videoQuality != null) {
    throw Error("WeChat quoted replies support text only; send attachments separately");
  }
  if (typeof input.text !== "string" || !input.text.trim()) throw Error("WeChat quoted replies require message text");
}

/** Resolve the exact server ID from the authenticated hub, never from model-supplied quote content. */
export function nativeReplyReference(message: NativeMessage, accountId: string, threadId: string, replyToId: string): QuoteSendReference {
  const messageId = nativeReplyMessageId(replyToId, accountId);
  if (message.id !== messageId || message.accountId !== accountId || message.threadId !== threadId
    || typeof message.serverId !== "string" || !/^[1-9]\d{0,19}$/.test(message.serverId)
    || BigInt(message.serverId) > 18446744073709551615n
    || replyToId.startsWith("wechat-native:") && nativeSourceMessageSid(message) !== replyToId) throw Error("WeChat reply target is unavailable or outside this conversation");
  return { messageId, serverId: message.serverId };
}
