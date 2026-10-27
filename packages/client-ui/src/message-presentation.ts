import type { NativeMessage } from "@agent-wechat/shared";
import { sharedLinkTarget } from "./shared-link-target.ts";

export function textMessageLink(message: NativeMessage) {
  const text = message.body?.trim();
  return messagePresentation(message) === "text" && text && !/\s/.test(text)
    ? sharedLinkTarget(text)
    : null;
}

export function recallText(message: NativeMessage) {
  // The original author controls placement, not the wording of who recalled it.
  return message.recall?.replacementText || "Message recalled";
}

// WeChat's application-message envelope also carries replies and shared cards.
// Files and shared stickers use the validated attachment download flow.
export function messagePresentation(message: NativeMessage) {
  if (message.recall) return "recalled";
  if ([1, 10000, 10002].includes(message.type)) return "text";
  if (message.type === 49) {
    if (
      [1, 74, 87].includes(message.appMessageType ?? 0) &&
      message.body?.trim()
    )
      return "text";
    if (message.appMessageType === 57 || message.quote) return "reply";
    if (message.appMessageType === 19) return "forwarded";
    if (message.link) return "link";
    return [6, 8].includes(message.appMessageType ?? 0)
      ? "media"
      : "unsupported";
  }
  return [3, 34, 43, 47, 62].includes(message.type) ? "media" : "unsupported";
}
