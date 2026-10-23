import type { NativeThread } from "@agent-wechat/shared";

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
