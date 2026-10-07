import type { MediaResult } from "@agent-wechat/shared";

export function expiredMediaMessage(result: MediaResult): string | undefined {
  if (result.type !== "expired" || result.data) return undefined;
  const expiry = typeof result.expiresAt === "number" ? new Date(result.expiresAt * 1000) : undefined;
  const when = expiry && Number.isFinite(expiry.getTime()) ? ` (${expiry.toISOString()})` : "";
  return `Media has expired${when} and no cached copy is available. Ask the sender to resend; automatic retries will not recover it.`;
}
