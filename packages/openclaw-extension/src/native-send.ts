import type { NativeSendIntent, NativeWeChatClient } from "@agent-wechat/shared";

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
