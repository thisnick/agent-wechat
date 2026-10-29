import type { NativeMessage, QuoteSendReference } from "@agent-wechat/shared";

export function validQuoteReference(
  value: unknown
): value is QuoteSendReference {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const quote = value as Record<string, unknown>;
  return (
    Object.keys(quote).every(
      (key) => key === "messageId" || key === "serverId"
    ) &&
    typeof quote.messageId === "string" &&
    !!quote.messageId &&
    quote.messageId.length <= 8192 &&
    typeof quote.serverId === "string" &&
    /^[1-9]\d{0,19}$/.test(quote.serverId) &&
    BigInt(quote.serverId) <= 18446744073709551615n
  );
}

export function canReply(message: NativeMessage): boolean {
  return (
    !message.recall &&
    !!message.senderId &&
    ["incoming", "outgoing"].includes(message.direction ?? "") &&
    validQuoteReference({
      messageId: message.id,
      serverId: message.serverId,
    }) &&
    ((message.type === 1 && !!message.body?.trim()) ||
      (message.type === 42 && !!message.body?.trim()) ||
      [3, 34, 43, 47].includes(message.type) ||
      (message.type === 48 && !!message.location) ||
      (message.type === 49 &&
        [1, 19, 87].includes(message.appMessageType ?? 0) &&
        !!message.body?.trim()) ||
      (message.type === 49 &&
        message.appMessageType === 51 &&
        !!message.link) ||
      (message.type === 49 &&
        [3, 4, 5, 6, 8, 57].includes(
          message.appMessageType ?? (message.quote ? 57 : 0)
        )))
  );
}
