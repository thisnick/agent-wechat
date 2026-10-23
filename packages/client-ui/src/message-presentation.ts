import type { NativeMessage } from "@agent-wechat/shared";

// WeChat's application-message envelope also carries replies and shared cards.
// Only file subtype 6 belongs in the ordinary attachment download flow.
export function messagePresentation(message: NativeMessage) {
  if (message.recall) return "recalled";
  if ([1, 10000, 10002].includes(message.type)) return "text";
  if (message.type === 49) {
    if (message.appMessageType === 57 || message.quote) return "reply";
    if (message.appMessageType === 19) return "forwarded";
    if (message.link) return "link";
    return message.appMessageType === 6 ? "media" : "unsupported";
  }
  return [3, 34, 43, 47, 62].includes(message.type) ? "media" : "unsupported";
}
