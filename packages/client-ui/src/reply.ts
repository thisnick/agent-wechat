import type { NativeMessage, QuoteSendReference } from "@agent-wechat/shared";

export function validQuoteReference(
  value: unknown,
): value is QuoteSendReference {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const quote = value as Record<string, unknown>;
  return (
    Object.keys(quote).every(
      (key) => key === "messageId" || key === "serverId",
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
      [3, 34, 43].includes(message.type) ||
      (message.type === 49 &&
        [5, 6, 57].includes(
          message.appMessageType ?? (message.quote ? 57 : 0),
        )))
  );
}
