import type { NativeMessage } from "@agent-wechat/shared";
import { t } from "./i18n.ts";
import { messagePresentation } from "./message-presentation.ts";

export function localQuote(
  parent: NativeMessage,
  messages: NativeMessage[]
): NativeMessage | undefined {
  const found = messages.filter((message) =>
    quoteTargetMatches(parent, message)
  );
  return found.length === 1 ? found[0] : undefined;
}

// A stored original is used for navigation and recall suppression, never to
// replace the excerpt delivered inside the reply.
export function quoteTargetMatches(
  parent: NativeMessage,
  message: NativeMessage
): boolean {
  return (
    !!parent.quote &&
    message.id !== parent.id &&
    message.accountId === parent.accountId &&
    message.threadId === parent.threadId &&
    message.serverId === parent.quote.serverId &&
    (message.type === parent.quote.type || !!message.recall)
  );
}

export function embeddedQuotePreview(
  parent: NativeMessage,
  original?: NativeMessage
) {
  const quote = parent.quote;
  const recalled =
    !!parent.recall ||
    quote?.state === "recalled" ||
    (!!original?.recall && quoteTargetMatches(parent, original));
  const state = quote?.state ?? "missing";
  const valid = ["embedded", "missing", "unsupported"].includes(state);
  const image =
    !recalled &&
    state === "embedded" &&
    quote?.type === 3 &&
    quote.embeddedImage === true;
  const sticker =
    !recalled &&
    state === "embedded" &&
    (quote?.type === 47 ||
      (quote?.type === 49 && quote.appMessageType === 8)) &&
    quote.embeddedSticker === true;
  const link =
    !recalled &&
    state === "embedded" &&
    quote?.type === 49 &&
    [3, 4, 5, 51].includes(quote.appMessageType ?? 0)
      ? quote.embeddedLink
      : undefined;
  const text = recalled
    ? t("Message recalled", "消息已撤回")
    : valid && state !== "missing" && quote?.embeddedText
    ? quote.embeddedText
    : quote?.type === 3
    ? t("Image unavailable", "图片不可用")
    : quote?.type === 34
    ? t("Voice message unavailable", "语音消息不可用")
    : [43, 62].includes(quote?.type ?? 0)
    ? t("Video unavailable", "视频不可用")
    : quote?.type === 47 || (quote?.type === 49 && quote.appMessageType === 8)
    ? t("Sticker unavailable", "表情不可用")
    : quote?.type === 49 && quote.appMessageType === 6
    ? t("File unavailable", "文件不可用")
    : quote?.type === 48
    ? t("Location unavailable", "位置不可用")
    : quote?.type === 42
    ? t("Shared contact unavailable", "分享名片不可用")
    : quote?.type === 49 && quote.appMessageType === 19
    ? t("Forwarded messages unavailable", "聊天记录不可用")
    : t("Quoted message unavailable", "引用消息不可用");
  return {
    text,
    senderId: recalled ? undefined : quote?.senderId,
    own: !recalled && quote?.direction === "outgoing",
    // Assets belong to the reply carrying the embedded descriptor. Changing
    // the renderer does not change the account/message sent to the media API.
    content:
      image || sticker || link
        ? {
            ...parent,
            type: image ? 3 : sticker ? 47 : 49,
            appMessageType: link ? quote?.appMessageType : undefined,
            quote: undefined,
            link: link || undefined,
            body: quote?.embeddedText ?? null,
          }
        : undefined,
  };
}
export function quotedText(message: NativeMessage): string {
  const kind = messagePresentation(message);
  if (kind === "recalled") return t("Message recalled", "消息已撤回");
  if (kind === "forwarded")
    return (
      message.body
        ?.split(/\r\n|\r|\n/)
        .map((line) => line.trim())
        .find(Boolean) || t("Forwarded messages", "聊天记录")
    );
  if (["text", "reply"].includes(kind))
    return message.body || t("Message text unavailable", "消息内容不可用");
  if (kind === "link") return message.link?.title || t("Link", "链接");
  if (message.type === 48 && message.location)
    return (
      message.location.name ||
      message.location.address ||
      t("Location", "位置")
    );
  if (message.type === 42) return t("Shared contact", "个人名片");
  if (message.type === 49 && message.appMessageType === 8)
    return t("Sticker", "表情");
  return (
    (
      {
        3: t("Image", "图片"),
        34: t("Voice message", "语音消息"),
        43: t("Video", "视频"),
        47: t("Sticker", "表情"),
        48: t("Location", "位置"),
        49: t("Attachment", "附件"),
        62: t("Video", "视频"),
      } as Record<number, string>
    )[message.type] || t("Message preview unavailable", "消息预览不可用")
  );
}
