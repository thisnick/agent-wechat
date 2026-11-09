import type {
  NativeThread,
  NativeThreadAvatarMember,
  NativeMessage,
  NativeThreadMessage,
} from "@agent-wechat/shared";
import { messagePresentation, recallText } from "./message-presentation.ts";
import { emojiText } from "./emoji-text.ts";

export function conversationName(
  id: string,
  displayName: string | null | undefined,
  members: NativeThreadAvatarMember[] | undefined,
  ownUsername?: string | null,
): string {
  if (!id.endsWith("@chatroom"))
    return displayName || (id === "filehelper" ? "File Transfer" : id);
  const savedName = displayName?.trim();
  if (savedName && savedName !== id) return savedName;
  const names = (members ?? [])
    .filter((member) => member.id !== ownUsername)
    .map((member) => {
      const name = member.displayName.trim();
      return name === member.id ? "" : name;
    })
    .filter(Boolean)
    .slice(0, 3);
  const suffix = id.slice(0, -"@chatroom".length).slice(-4);
  return `Group chat · ${names.length ? names.join(", ") : `…${suffix}`}`;
}

export function orderThreads(threads: NativeThread[]): NativeThread[] {
  // The hub orders both pinned and ordinary chats. Never rank its saved page again.
  return [...threads];
}

export function messageSummary(
  message: NativeMessage,
  senderName: string | null,
): NativeThreadMessage {
  const kind = messagePresentation(message);
  const text =
    kind === "recalled"
      ? recallText(message)
      : kind === "system"
      ? message.body || "System message"
      : kind === "forwarded"
      ? message.body?.trim() || "Forwarded messages"
      : ["text", "reply"].includes(kind)
      ? message.body || "Message"
      : kind === "link"
      ? message.link?.title || "Link"
      : message.type === 48 && message.location
      ? message.location.name || message.location.address || "Location"
      : message.type === 42
      ? "Shared contact"
      : message.type === 49 && message.appMessageType === 8
      ? "Sticker"
      : (
          {
            3: "Image",
            34: "Voice message",
            43: "Video",
            47: "Sticker",
            48: "Location",
            49: "Attachment",
            62: "Video",
          } as Record<number, string>
        )[message.type] || "Unsupported message";
  return {
    id: message.id,
    senderId: kind === "system" ? null : message.senderId,
    senderName: kind === "system" ? null : senderName,
    direction: kind === "system" ? null : message.direction,
    type: kind === "recalled" ? 10000 : message.type,
    text: text.replace(/\s+/g, " ").slice(0, 240),
    createdAt: message.createdAt,
  };
}

export function threadPreview(thread: NativeThread): string {
  const message = thread.lastMessage;
  if (message === null) return "No messages yet";
  if (!message) return "Preview unavailable";
  const text = emojiText(message.text.replace(/\s+/g, " ").trim());
  if (message.type === 10000 || message.type === 10002) return text;
  if (message.direction === "outgoing") return `You: ${text}`;
  if (thread.kind === "group" || thread.id.endsWith("@chatroom"))
    return `${message.senderName || "Member"}: ${text}`;
  return text;
}
