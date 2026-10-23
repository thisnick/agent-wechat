import type { NativeMessage } from "@agent-wechat/shared";
import { messagePresentation } from "./message-presentation.ts";

export function localQuote(
  parent: NativeMessage,
  messages: NativeMessage[],
): NativeMessage | undefined {
  if (!parent.quote) return;
  const found = messages.filter(
    (message) =>
      message.id !== parent.id &&
      message.accountId === parent.accountId &&
      message.threadId === parent.threadId &&
      message.serverId === parent.quote!.serverId &&
      message.type === parent.quote!.type,
  );
  return found.length === 1 ? found[0] : undefined;
}
export function quotedText(message: NativeMessage): string {
  const kind = messagePresentation(message);
  if (kind === "recalled") return "Message recalled";
  if (["text", "reply", "forwarded"].includes(kind))
    return message.body || "Message text unavailable";
  if (kind === "link") return message.link?.title || "Link";
  return (
    (
      {
        3: "Image",
        34: "Voice message",
        43: "Video",
        47: "Sticker",
        49: "Attachment",
        62: "Video",
      } as Record<number, string>
    )[message.type] || "Message preview unavailable"
  );
}
