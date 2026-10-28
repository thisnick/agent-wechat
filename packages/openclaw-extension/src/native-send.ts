import { NativeApiError, type NativeSendIntent, type NativeWeChatClient, type QuoteSendReference } from "@agent-wechat/shared";
import { parseWeChatTextMentions, validateWeChatMentionSend, type WeChatTextMentions } from "./outbound-mentions.ts";
export type NativeOutboundDelivery = { messageId: string; meta: Record<string, unknown> };
export type NativeVerifiedMessageReceipt = { messageId: string; serverId: string; accountId: string; threadId: string };

/** IDs used for later mutations must name verified stored messages, never jobs. */
export async function nativeOutboundDelivery(client: NativeWeChatClient, accountId: string, threadId: string,
  messageIds: unknown, source: { sendId?: string; jobId?: string; state: string; serverIds?: string[] }): Promise<NativeOutboundDelivery> {
  const receipts: NativeVerifiedMessageReceipt[] = [];
  try {
    if (!Array.isArray(messageIds) || !messageIds.length || messageIds.length > 256 || messageIds.some(id => typeof id !== "string" || !id || id.length > 4096) || new Set(messageIds).size !== messageIds.length) throw Error("Message receipts unavailable");
    for (const [index, messageId] of messageIds.entries()) {
      const message = await client.message(accountId, messageId);
      if (message.id !== messageId || message.accountId !== accountId || message.threadId !== threadId || message.direction !== "outgoing"
        || typeof message.serverId !== "string" || !/^[1-9]\d{0,19}$/.test(message.serverId)
        || BigInt(message.serverId) > 18446744073709551615n
        || source.serverIds?.[index] != null && source.serverIds[index] !== message.serverId) throw Error("Message receipt identity mismatch");
      receipts.push({ messageId, serverId: message.serverId, accountId, threadId });
    }
  } catch {
    // Submission already occurred; return an explicit uncertain receipt without
    // manufacturing a message ID or turning it into an invitation to resend.
    return { messageId: "", meta: { accountId, threadId, ...source, messageIds: receipts, outcome: "unknown",
      receiptState: "needs_review", retryable: false, note: "Send submitted. Inspect the durable send/job and retained receipts; do not resend automatically." } };
  }
  return { messageId: receipts[0].messageId, meta: { accountId, threadId, serverId: receipts[0].serverId, outcome: "sent",
    ...source, messageIds: receipts, receiptState: "verified" } };
}

export class NativeTextSendInspectionError extends Error {
  readonly code = "native_text_send_inspection_required";
  readonly retryable = false;
  readonly accountId: string;
  readonly threadId: string;
  readonly idempotencyKey: string;
  readonly state: string;
  readonly sendId?: string;
  constructor(accountId: string, threadId: string, idempotencyKey: string, state: string, sendId?: string) {
    super(`Native text send requires inspection (${state}); retain the original idempotency key and do not automatically resend`);
    this.name = "NativeTextSendInspectionError";
    this.accountId = accountId; this.threadId = threadId; this.idempotencyKey = idempotencyKey; this.state = state; this.sendId = sendId;
  }
}

/** Submit once, then inspect the durable intent without repeating its side effect. */
export async function submitNativeText(client: NativeWeChatClient, accountId: string, threadId: string, text: string, key: string,
  options: { timeoutMs?: number; initialDelayMs?: number; mentions?: WeChatTextMentions; quote?: QuoteSendReference } = {}): Promise<NativeSendIntent> {
  const mentions = options.mentions === undefined ? undefined : parseWeChatTextMentions(options.mentions);
  validateWeChatMentionSend(mentions, { to: threadId, text, replyToId: options.quote?.messageId });
  const inspect = (value: unknown, sendId?: string): NativeSendIntent => {
    const intent = value as Partial<NativeSendIntent> & { kind?: unknown; threadId?: unknown };
    if (!intent || typeof intent !== "object" || Array.isArray(intent) || typeof intent.id !== "string" || !intent.id
      || (intent.accountId !== undefined && intent.accountId !== accountId)
      || (intent.destination !== undefined && intent.destination !== threadId)
      || intent.kind !== undefined || intent.threadId !== undefined
      || typeof intent.state !== "string" || (sendId !== undefined && intent.id !== sendId)) throw new NativeTextSendInspectionError(accountId, threadId, key, "receipt_identity_mismatch", sendId);
    // Older acknowledgements contain only id/state. Missing scope comes from
    // this authenticated request, never from another account or conversation.
    // This records acceptance only; nativeOutboundDelivery still requires an
    // exact canonical message readback before returning a verified delivery.
    return { ...intent, accountId, destination: threadId } as NativeSendIntent;
  };
  let intent: NativeSendIntent;
  try { intent = inspect(await client.sendText(accountId, threadId, text, key, options.quote ?? (mentions ? { mentions } : undefined))); }
  catch (error) {
    if (error instanceof NativeTextSendInspectionError || error instanceof NativeApiError && error.status < 500 && error.code !== "timeout") throw error;
    // The POST may have reached the daemon. Recover only by the original key;
    // an absent or unreadable receipt is not permission to send again.
    try { intent = inspect(await client.sendByKey(accountId, key)); }
    catch (lookupError) {
      if (lookupError instanceof NativeTextSendInspectionError) throw lookupError;
      throw new NativeTextSendInspectionError(accountId, threadId, key, "submission_unknown");
    }
  }
  const deadline = Date.now() + Math.max(1, Math.min(options.timeoutMs ?? 65000, 65000));
  let delay = Math.max(1, Math.min(options.initialDelayMs ?? 250, 1000));
  while (["queued", "submitting"].includes(intent.state) && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, Math.min(delay, deadline - Date.now())));
    const remaining = deadline - Date.now();
    if (remaining <= 0) break;
    try { intent = inspect(await client.send(accountId, intent.id, AbortSignal.timeout(remaining)), intent.id); }
    catch (error) {
      if (error instanceof NativeTextSendInspectionError) throw error;
      throw new NativeTextSendInspectionError(accountId, threadId, key, "status_unavailable", intent.id);
    }
    delay = Math.min(delay * 2, 1000);
  }
  if (!["accepted", "sent", "server_accepted"].includes(intent.state)) throw new NativeTextSendInspectionError(accountId, threadId, key, intent.state, intent.id);
  return intent;
}
