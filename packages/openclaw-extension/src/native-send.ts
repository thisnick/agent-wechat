import type { NativeSendIntent, NativeWeChatClient } from "@agent-wechat/shared";
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

/** Submit once, then inspect the durable intent without repeating its side effect. */
export async function submitNativeText(client: NativeWeChatClient, accountId: string, threadId: string, text: string, key: string, options: { timeoutMs?: number; initialDelayMs?: number } = {}): Promise<NativeSendIntent> {
  let intent = await client.sendText(accountId, threadId, text, key);
  const deadline = Date.now() + Math.max(1, Math.min(options.timeoutMs ?? 65000, 65000));
  let delay = Math.max(1, Math.min(options.initialDelayMs ?? 250, 1000));
  while (["queued", "submitting"].includes(intent.state) && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, Math.min(delay, deadline - Date.now())));
    const remaining = deadline - Date.now();
    if (remaining <= 0) break;
    intent = await client.send(accountId, intent.id, AbortSignal.timeout(remaining));
    delay = Math.min(delay * 2, 1000);
  }
  if (!["accepted", "sent", "server_accepted"].includes(intent.state)) throw new Error(`Native send ${intent.id} requires inspection (${intent.state}); do not automatically resend`);
  return intent;
}
