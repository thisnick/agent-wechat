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
    ? t("quotePreview.messageRecalled")
    : valid && state !== "missing" && quote?.embeddedText
    ? quote.embeddedText
    : quote?.type === 3
    ? t("quotePreview.imageUnavailable")
    : quote?.type === 34
    ? t("quotePreview.voiceMessageUnavailable")
    : [43, 62].includes(quote?.type ?? 0)
    ? t("quotePreview.videoUnavailable")
    : quote?.type === 47 || (quote?.type === 49 && quote.appMessageType === 8)
    ? t("quotePreview.stickerUnavailable")
    : quote?.type === 49 && quote.appMessageType === 6
    ? t("quotePreview.fileUnavailable")
    : quote?.type === 48
    ? t("quotePreview.locationUnavailable")
    : quote?.type === 42
    ? t("quotePreview.sharedContactUnavailable")
    : quote?.type === 49 && quote.appMessageType === 19
    ? t("quotePreview.forwardedMessagesUnavailable")
    : t("quotePreview.quotedMessageUnavailable");
  return {
    text,
    miniProgram: !recalled && state === "embedded" ? quote?.embeddedMiniProgram : undefined,
    senderId: recalled ? undefined : quote?.senderId,
    senderName: !recalled && typeof quote?.senderName === "string"
      && quote.senderName.length <= 4096
      && !/[\u0000-\u001f\u007f-\u009f]/.test(quote.senderName)
      ? quote.senderName.trim() || undefined : undefined,
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
  if (kind === "recalled") return t("quotePreview.messageRecalled");
  if (kind === "forwarded")
    return (
      message.body
        ?.split(/\r\n|\r|\n/)
        .map((line) => line.trim())
        .find(Boolean) || t("quotePreview.forwardedMessages")
    );
  if (["text", "reply"].includes(kind))
    return message.body || t("quotePreview.messageTextUnavailable");
  if (kind === "mini-program") return message.miniProgram?.title || t("miniPrograms.singular");
  if (kind === "link") return message.link?.title || t("quotePreview.link");
  if (message.type === 48 && message.location)
    return (
      message.location.name ||
      message.location.address ||
      t("quotePreview.location")
    );
  if (message.type === 42) return t("quotePreview.sharedContact");
  if (message.type === 49 && message.appMessageType === 8)
    return t("quotePreview.sticker");
  return (
    (
      {
        3: t("quotePreview.image"),
        34: t("quotePreview.voiceMessage"),
        43: t("quotePreview.video"),
        47: t("quotePreview.sticker"),
        48: t("quotePreview.location"),
        49: t("quotePreview.attachment"),
        62: t("quotePreview.video"),
      } as Record<number, string>
    )[message.type] || t("quotePreview.messagePreviewUnavailable")
  );
}
