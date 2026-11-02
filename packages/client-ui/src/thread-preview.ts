import type {
  NativeThread,
  NativeMessage,
  NativeThreadMessage,
} from "@agent-wechat/shared";
import { isZh, t } from "./i18n.ts";
import { messagePresentation, recallText } from "./message-presentation.ts";
import { emojiText } from "./emoji-text.ts";

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
      ? message.body || t("System message", "系统消息")
      : kind === "forwarded"
      ? message.body?.trim() || t("Forwarded messages", "聊天记录")
      : ["text", "reply"].includes(kind)
      ? message.body || t("Message", "消息")
      : kind === "link"
      ? message.link?.title || t("Link", "链接")
      : message.type === 48 && message.location
      ? message.location.name ||
        message.location.address ||
        t("Location", "位置")
      : message.type === 42
      ? t("Shared contact", "个人名片")
      : message.type === 49 && message.appMessageType === 8
      ? t("Sticker", "表情")
      : (
          {
            3: t("Image", "图片"),
            34: t("Voice message", "语音消息"),
            43: t("Video", "视频"),
            47: t("Sticker", "表情"),
            48: t("Location", "位置"),
            49: t("Attachment", "附件"),
            62: t("Video", "视频"),
          } as Record<number, string>
        )[message.type] || t("Unsupported message", "暂不支持的消息");
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

const ZH_PREVIEW_LABELS: Record<string, string> = {
  "Message recalled": "已撤回一条消息",
  "System message": "系统消息",
  "Forwarded messages": "聊天记录",
  Message: "消息",
  Link: "链接",
  "Shared link": "链接",
  Location: "位置",
  "Shared contact": "个人名片",
  Sticker: "表情",
  Photo: "图片",
  Image: "图片",
  "Voice message": "语音消息",
  Video: "视频",
  File: "文件",
  Attachment: "附件",
  "Unsupported message": "暂不支持的消息",
};

export function threadPreview(thread: NativeThread): string {
  const message = thread.lastMessage;
  if (message === null) return t("No messages yet", "暂无消息");
  if (!message) return t("Preview unavailable", "预览不可用");
  const normalized = message.text.replace(/\s+/g, " ").trim();
  const translated =
    isZh() && ZH_PREVIEW_LABELS[normalized]
      ? ZH_PREVIEW_LABELS[normalized]
      : normalized;
  const text = emojiText(translated);
  if (message.type === 10000 || message.type === 10002) return text;
  if (message.direction === "outgoing") return `${t("You: ", "我：")}${text}`;
  if (thread.kind === "group" || thread.id.endsWith("@chatroom"))
    return `${message.senderName || t("Member", "群成员")}${t(": ", "：")}${text}`;
  return text;
}
