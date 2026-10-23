import type {
  NativeThread,
  NativeMessage,
  NativeThreadMessage,
} from "@agent-wechat/shared";
import { messagePresentation } from "./message-presentation.ts";

export function messageSummary(
  message: NativeMessage,
  senderName: string | null,
): NativeThreadMessage {
  const kind = messagePresentation(message);
  const text =
    kind === "recalled"
      ? "Message recalled"
      : ["text", "reply", "forwarded"].includes(kind)
        ? message.body || "Message"
        : kind === "link"
          ? message.link?.title || "Link"
          : (
              {
                3: "Image",
                34: "Voice message",
                43: "Video",
                47: "Sticker",
                49: "Attachment",
                62: "Video",
              } as Record<number, string>
            )[message.type] || "Unsupported message";
  return {
    id: message.id,
    senderId: message.senderId,
    senderName,
    direction: message.direction,
    type: kind === "recalled" ? 10000 : message.type,
    text: text.replace(/\s+/g, " ").slice(0, 240),
    createdAt: message.createdAt,
  };
}

export function threadPreview(thread: NativeThread): string {
  const message = thread.lastMessage;
  if (message === null) return "No messages yet";
  if (!message) return "Preview unavailable";
  const text = message.text.replace(/\s+/g, " ").trim();
  if (message.type === 10000 || message.type === 10002) return text;
  if (message.direction === "outgoing") return `You: ${text}`;
  if (thread.kind === "group" || thread.id.endsWith("@chatroom"))
    return `${message.senderName || "Member"}: ${text}`;
  return text;
}
