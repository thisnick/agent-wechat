import type { NativeMessage, NativeRecallInfo } from "@agent-wechat/shared";
import { validateNativeRecallMetadata } from "@agent-wechat/shared";
export type NativeRecallObservation = { messageId?: string; threadId: string; serverId: string; observedAt: string; recalledAt?: string;
  originalSenderId?: string | null; originalDirection?: string | null; replacementText?: string | null };
export function nativeRecallInfo(message: NativeMessage): NativeRecallInfo | undefined {
  if (message.recall) validateNativeRecallMetadata(message.recall);
  return message.recall;
}
export function nativeRecallIdentity(threadId: string, serverId: string): string { return JSON.stringify([threadId, serverId]); }
export function validateNativeRecall(value: NativeRecallObservation): void {
  validateNativeRecallMetadata(value);
  if (!value || typeof value.threadId !== "string" || !value.threadId || value.threadId.length > 4096
    || typeof value.serverId !== "string" || !/^[1-9]\d{0,19}$/.test(value.serverId) || BigInt(value.serverId) > 18446744073709551615n
    || typeof value.observedAt !== "string" || value.observedAt.length > 128 || !Number.isFinite(Date.parse(value.observedAt))
    || (value.recalledAt !== undefined && (typeof value.recalledAt !== "string" || value.recalledAt.length > 128 || !Number.isFinite(Date.parse(value.recalledAt))))
    || (value.messageId !== undefined && (typeof value.messageId !== "string" || !value.messageId || value.messageId.length > 4096))) throw Error("Invalid native recall observation; cursor retained");
}
export function mergeNativeRecall(prior: NativeRecallObservation, next: NativeRecallObservation): NativeRecallObservation {
  validateNativeRecall(prior); validateNativeRecall(next);
  if (prior.threadId !== next.threadId || prior.serverId !== next.serverId) throw Error("Conflicting native recall identity; cursor retained");
  for (const field of ["originalSenderId", "originalDirection"] as const) {
    if (prior[field] != null && next[field] != null && prior[field] !== next[field]) throw Error("Conflicting native recall author; cursor retained");
  }
  return { threadId: prior.threadId, serverId: prior.serverId, observedAt: prior.observedAt,
    ...(prior.messageId ?? next.messageId ? { messageId: prior.messageId ?? next.messageId } : {}),
    ...(prior.recalledAt ?? next.recalledAt ? { recalledAt: prior.recalledAt ?? next.recalledAt } : {}),
    ...((prior.originalSenderId ?? next.originalSenderId) != null ? { originalSenderId: prior.originalSenderId ?? next.originalSenderId } : {}),
    ...((prior.originalDirection ?? next.originalDirection) != null ? { originalDirection: prior.originalDirection ?? next.originalDirection } : {}),
    ...((prior.replacementText ?? next.replacementText) != null ? { replacementText: prior.replacementText ?? next.replacementText } : {}),
  };
}
export function recalledNativeMessage(message: NativeMessage, recall: NativeRecallObservation): NativeMessage {
  validateNativeRecall(recall);
  const previous = nativeRecallInfo(message);
  const originalSenderId = recall.originalSenderId ?? previous?.originalSenderId;
  const originalDirection = recall.originalDirection ?? previous?.originalDirection;
  const replacementText = previous?.replacementText ?? recall.replacementText;
  return { ...message, type: 10000, body: "[Message recalled]", quote: undefined, link: undefined, media: [],
    senderId: originalSenderId ?? (message.senderId || null), direction: originalDirection ?? (message.senderId ? message.direction : null),
    mentions: { state: "unknown", userIds: [], self: null, all: null, source: null },
    recall: { observedAt: previous?.observedAt ?? recall.observedAt, ...(previous?.recalledAt ?? recall.recalledAt ? { recalledAt: previous?.recalledAt ?? recall.recalledAt } : {}),
      ...(originalSenderId != null ? { originalSenderId } : {}), ...(originalDirection != null ? { originalDirection } : {}),
      ...(replacementText != null ? { replacementText } : {}) },
  } as NativeMessage;
}
