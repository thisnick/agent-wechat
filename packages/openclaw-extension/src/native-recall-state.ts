import type { NativeMessage, NativeRecallInfo } from "@agent-wechat/shared";
export type NativeRecallObservation = { messageId?: string; threadId: string; serverId: string; observedAt: string; recalledAt?: string };
export function nativeRecallInfo(message: NativeMessage): NativeRecallInfo | undefined { return message.recall; }
export function nativeRecallIdentity(threadId: string, serverId: string): string { return JSON.stringify([threadId, serverId]); }
export function validateNativeRecall(value: NativeRecallObservation): void {
  if (!value || typeof value.threadId !== "string" || !value.threadId || value.threadId.length > 4096
    || typeof value.serverId !== "string" || !/^[1-9]\d{0,19}$/.test(value.serverId) || BigInt(value.serverId) > 18446744073709551615n
    || typeof value.observedAt !== "string" || value.observedAt.length > 128 || !Number.isFinite(Date.parse(value.observedAt))
    || (value.recalledAt !== undefined && (typeof value.recalledAt !== "string" || value.recalledAt.length > 128 || !Number.isFinite(Date.parse(value.recalledAt))))
    || (value.messageId !== undefined && (typeof value.messageId !== "string" || !value.messageId || value.messageId.length > 4096))) throw Error("Invalid native recall observation; cursor retained");
}
export function recalledNativeMessage(message: NativeMessage, recall: NativeRecallObservation): NativeMessage {
  return { ...message, type: 10000, body: "[Message recalled]", quote: undefined, media: [],
    mentions: { state: "unknown", userIds: [], self: null, all: null, source: null },
    recall: { observedAt: recall.observedAt, ...(recall.recalledAt ? { recalledAt: recall.recalledAt } : {}) },
  } as NativeMessage;
}
