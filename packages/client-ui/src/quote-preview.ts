import type { NativeMessage } from "@agent-wechat/shared";
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
    ? "Message recalled"
    : valid && state !== "missing" && quote?.embeddedText
    ? quote.embeddedText
    : quote?.type === 3
    ? "Image unavailable"
    : quote?.type === 34
    ? "Voice message unavailable"
    : [43, 62].includes(quote?.type ?? 0)
    ? "Video unavailable"
    : quote?.type === 47 || (quote?.type === 49 && quote.appMessageType === 8)
    ? "Sticker unavailable"
    : quote?.type === 49 && quote.appMessageType === 6
    ? "File unavailable"
    : quote?.type === 48
    ? "Location unavailable"
    : quote?.type === 42
    ? "Shared contact unavailable"
    : quote?.type === 49 && quote.appMessageType === 19
    ? "Forwarded messages unavailable"
    : "Quoted message unavailable";
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
  if (kind === "recalled") return "Message recalled";
  if (kind === "forwarded")
    return message.body?.split(/\r\n|\r|\n/).map(line => line.trim()).find(Boolean) || "Forwarded messages";
  if (["text", "reply"].includes(kind))
    return message.body || "Message text unavailable";
  if (kind === "link") return message.link?.title || "Link";
  if (message.type === 48 && message.location)
    return message.location.name || message.location.address || "Location";
  if (message.type === 42) return "Shared contact";
  if (message.type === 49 && message.appMessageType === 8) return "Sticker";
  return (
    (
      {
        3: "Image",
        34: "Voice message",
        43: "Video",
        47: "Sticker",
        48: "Location",
        49: "Attachment",
        62: "Video",
      } as Record<number, string>
    )[message.type] || "Message preview unavailable"
  );
}
