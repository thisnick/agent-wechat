import type {
  NativeThread,
  NativeThreadAvatarMember,
  NativeMessage,
  NativeThreadMessage,
} from "@agent-wechat/shared";
import { t } from "./i18n.ts";
import { messagePresentation, recallText } from "./message-presentation.ts";
import { emojiText } from "./emoji-text.ts";

export function conversationName(
  id: string,
  displayName: string | null | undefined,
  members: NativeThreadAvatarMember[] | undefined,
  ownUsername?: string | null,
): string {
  if (!id.endsWith("@chatroom"))
    return displayName || (id === "filehelper" ? t("threadPreview.fileTransfer") : id);
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
  return t("threadPreview.groupChatNames", { names: names.length ? names.join(", ") : `…${suffix}` });
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
      ? message.body || t("threadPreview.systemMessage")
      : kind === "forwarded"
      ? message.body?.trim() || t("threadPreview.forwardedMessages")
      : ["text", "reply"].includes(kind)
      ? message.body || t("threadPreview.message")
      : kind === "mini-program"
      ? message.miniProgram?.title || t("miniPrograms.singular")
      : kind === "link"
      ? message.link?.title || t("threadPreview.link")
      : message.type === 48 && message.location
      ? message.location.name ||
        message.location.address ||
        t("threadPreview.location")
      : message.type === 42
      ? t("threadPreview.sharedContact")
      : message.type === 49 && message.appMessageType === 8
      ? t("threadPreview.sticker")
      : (
          {
            3: t("threadPreview.image"),
            34: t("threadPreview.voiceMessage"),
            43: t("threadPreview.video"),
            47: t("threadPreview.sticker"),
            48: t("threadPreview.location"),
            49: t("threadPreview.attachment"),
            62: t("threadPreview.video"),
          } as Record<number, string>
        )[message.type] || t("threadPreview.unsupportedMessage");
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

// The hub sends these placeholder previews in English; show them in the UI language.
const PREVIEW_LABELS: Record<string, () => string> = {
  "Message recalled": () => t("threadPreview.labelMessageRecalled"),
  "System message": () => t("threadPreview.labelSystemMessage"),
  "Forwarded messages": () => t("threadPreview.labelForwardedMessages"),
  Message: () => t("threadPreview.labelMessage"),
  Link: () => t("threadPreview.labelLink"),
  "Shared link": () => t("threadPreview.labelSharedLink"),
  Location: () => t("threadPreview.labelLocation"),
  "Shared contact": () => t("threadPreview.labelSharedContact"),
  Sticker: () => t("threadPreview.labelSticker"),
  Photo: () => t("threadPreview.labelPhoto"),
  Image: () => t("threadPreview.labelImage"),
  "Voice message": () => t("threadPreview.labelVoiceMessage"),
  Video: () => t("threadPreview.labelVideo"),
  File: () => t("threadPreview.labelFile"),
  Attachment: () => t("threadPreview.labelAttachment"),
  "Unsupported message": () => t("threadPreview.labelUnsupportedMessage"),
};

export function threadPreview(thread: NativeThread): string {
  const message = thread.lastMessage;
  if (message === null) return t("threadPreview.noMessagesYet");
  if (!message) return t("threadPreview.previewUnavailable");
  const normalized = message.text.replace(/\s+/g, " ").trim();
  const translated = Object.hasOwn(PREVIEW_LABELS, normalized)
    ? PREVIEW_LABELS[normalized]()
    : normalized;
  const text = emojiText(translated);
  if (message.type === 10000 || message.type === 10002) return text;
  if (message.direction === "outgoing") return t("threadPreview.ownMessage", { text });
  if (thread.kind === "group" || thread.id.endsWith("@chatroom"))
    return t("threadPreview.memberMessage", {
      name: message.senderName || t("threadPreview.member"),
      text,
    });
  return text;
}
