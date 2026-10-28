import { createHash } from "node:crypto";

// Keep the existing media derivation so queue recovery after an upgrade uses
// the same key. Text payloads share this persisted queue/part identity.
export function nativeOutboundKey(queueId: string, partIndex = 0, attachmentIndex = 0): string {
  return createHash("sha256").update(JSON.stringify(["wechat-media", queueId, partIndex, attachmentIndex])).digest("hex");
}
