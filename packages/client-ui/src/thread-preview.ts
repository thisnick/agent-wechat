import type {
  NativeThread,
  NativeMessage,
  NativeThreadMessage,
} from "@agent-wechat/shared";
import { messagePresentation, recallText } from "./message-presentation.ts";

function lastActivity(thread: NativeThread): number {
  const value = thread.lastMessage?.createdAt;
  if (!value) return -Infinity;
  const time = /^\d+$/.test(value) ? Number(value) * 1000 : Date.parse(value);
  return Number.isFinite(time) ? time : -Infinity;
}

export function orderThreads(threads: NativeThread[]): NativeThread[] {
  // Equal or unavailable times keep the server's order without mutating state.
  return [...threads].sort((a, b) => {
    const left = lastActivity(a),
      right = lastActivity(b);
    return left === right ? 0 : left > right ? -1 : 1;
  });
}

export function messageSummary(
  message: NativeMessage,
  senderName: string | null,
): NativeThreadMessage {
  const kind = messagePresentation(message);
  const text =
    kind === "recalled"
      ? recallText(message)
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
