import type { NativeMessage } from "@agent-wechat/shared";
import { buildHistoryContextFromEntries, buildInboundHistoryFromEntries, DEFAULT_GROUP_HISTORY_LIMIT } from "openclaw/plugin-sdk/reply-history";
import type { NativeSuppliedAsset } from "./native-consumer-state.ts";
import type { prepareNativeAttachment } from "./native-media.ts";

export type NativePreparedAttachment = Awaited<ReturnType<typeof prepareNativeAttachment>>;
export function nativeGroupHistoryLimit(cfg: any): number {
  const limit = cfg?.messages?.groupChat?.historyLimit;
  return Number.isInteger(limit) && limit >= 0 ? Math.min(limit, 200) : DEFAULT_GROUP_HISTORY_LIMIT;
}
export async function transcribeNativeAttachment(prepared: NativePreparedAttachment, core: any, cfg: any): Promise<NativePreparedAttachment> {
  const attachment = prepared.attachment;
  if (attachment?.kind !== "audio" || attachment.status !== "ready" || !attachment.path) return prepared;
  let transcript = "";
  try { transcript = (await core.mediaUnderstanding.transcribeAudioFile({ filePath: attachment.path, cfg, mime: attachment.mime })).text?.trim() ?? ""; } catch { /* The verified voice file remains available without transcription. */ }
  return { ...prepared, body: `${prepared.body}\n${transcript ? `[Voice transcript: ${transcript.slice(0, 10000)}]` : "[Voice transcription unavailable]"}` };
}

/** Prepare only sender-admitted context, after the triggering mention gate.
 * Historical media failures remain explicit references/statuses and do not hide
 * the current message. Three workers bound transfer/transcription concurrency. */
export async function prepareNativeHistory(messages: NativeMessage[], prepare: (message: NativeMessage) => Promise<NativePreparedAttachment>): Promise<Array<{ message: NativeMessage; prepared: NativePreparedAttachment }>> {
  const result: Array<{ message: NativeMessage; prepared: NativePreparedAttachment }> = new Array(messages.length);
  let next = 0;
  await Promise.all(Array.from({ length: Math.min(3, messages.length) }, async () => {
    for (let index = next++; index < messages.length; index = next++) {
      const message = messages[index]; let prepared: NativePreparedAttachment;
      try { prepared = await prepare(message); }
      catch { prepared = { body: `[Attachment unavailable for message ${message.id}: retrieval or integrity verification failed]`, pending: false }; }
      if (message.type === 1 && prepared.body.length > 4096) prepared = { ...prepared, body: `${prepared.body.slice(0, 4096)}\n[Message truncated; use wechat_history or wechat_search for the stored message]` };
      result[index] = { message, prepared };
    }
  }));
  return result;
}

export function buildNativeContext(history: Array<{ message: NativeMessage; prepared: NativePreparedAttachment }>, message: NativeMessage, prepared: NativePreparedAttachment) {
  const all = [...history, { message, prepared }];
  // Catch-up has one vision slot. Keep the prepared representation of the last
  // ready image; every original (including earlier images) remains a text path.
  const image = history.length ? [...all].reverse().find(item => item.prepared.attachment?.kind === "image" && item.prepared.attachment.status === "ready" && item.prepared.mediaPath) : undefined;
  const selected = history.length ? image : all.at(-1);
  const assets: NativeSuppliedAsset[] = all.flatMap(item => (item.prepared.suppliedAssets ?? []).map(asset => {
    const contextRole = item.message.id === message.id ? "trigger" as const : "history" as const;
    if (item === selected) return { ...asset, contextRole, messageId: item.message.id };
    return { ...asset, contextRole, messageId: item.message.id, runtimeMedia: false, role: "file_reference" as const,
      path: asset.sourcePath ?? asset.path, sha256: asset.sourceSha256 ?? asset.sha256,
      byteLength: asset.sourceByteLength ?? asset.byteLength, mime: asset.sourceMime ?? asset.mime };
  }));
  const entries = history.map(item => ({ sender: item.message.senderId!, body: item.prepared.body,
    timestamp: item.message.createdAt ? Date.parse(item.message.createdAt) : undefined, messageId: item.message.id }));
  const inputBody = entries.length ? buildHistoryContextFromEntries({ entries, currentMessage: prepared.body, excludeLast: false,
    formatEntry: entry => `${entry.sender}: ${entry.body}` }) : prepared.body;
  const attachments = all.flatMap(item => {
    const attachment = item.prepared.attachment;
    return attachment?.status === "ready" && attachment.path ? [{ path: attachment.path, filename: attachment.filename,
      kind: attachment.kind, messageId: item.message.id, sender: item.message.senderId }] : [];
  });
  return { inputBody, attachments, assets, history: buildInboundHistoryFromEntries({ entries, limit: entries.length }),
    mediaPath: selected?.prepared.mediaPath, mediaMime: selected?.prepared.mediaMime };
}
