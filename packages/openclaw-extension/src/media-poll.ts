import { setTimeout as delay } from "node:timers/promises";
import type { MediaResult, WeChatClient } from "@agent-wechat/shared";

type MediaClient = Pick<WeChatClient, "getMedia" | "signal">;
type Log = { info?: (...args: any[]) => void };
const MAX_REQUESTS = 3;
// Scope the budget to a monitor client, including later buffered-history refreshes.
// Bound retention; a restarted monitor can retry previously unavailable media.
const budgets = new WeakMap<MediaClient, Map<string, number>>();

function rejectsBestQuality(error: unknown): boolean {
  return error instanceof Error && /^(400|422)\b/.test(error.message) && /\bbest\b/i.test(error.message);
}

/** Poll with a shared request budget, including legacy image-quality fallbacks. */
export async function pollMedia(
  client: MediaClient,
  chatId: string,
  localId: number,
  quality: "full" | "best",
  log?: Log,
  maxAttempts = MAX_REQUESTS,
  intervalMs = 1000,
): Promise<MediaResult | null> {
  let attempts = budgets.get(client);
  if (!attempts) {
    attempts = new Map();
    budgets.set(client, attempts);
  }
  const key = `${chatId}:${localId}`;
  let remaining = Math.min(maxAttempts, MAX_REQUESTS - (attempts.get(key) ?? 0));
  let lastResult: MediaResult | null = null;
  let legacyImageServer = false;
  const request = async (requestedQuality: "full" | "best" | "thumbnail") => {
    client.signal?.throwIfAborted();
    remaining--;
    const used = (attempts.get(key) ?? 0) + 1;
    attempts.delete(key);
    attempts.set(key, used);
    if (attempts.size > 1000) attempts.delete(attempts.keys().next().value!);
    const result = await client.getMedia(chatId, localId, requestedQuality);
    client.signal?.throwIfAborted();
    return result;
  };
  client.signal?.throwIfAborted();
  while (remaining > 0) {
    let result: MediaResult;
    if (legacyImageServer) {
      const full = await request("full");
      if (full.data || full.type === "unsupported") return full;
      if (remaining <= 0) return full;
      const preview = await request("thumbnail");
      result = preview.data ? { ...preview, quality: "thumbnail" as const } : full;
    } else {
      try {
        result = await request(quality);
      } catch (error) {
        client.signal?.throwIfAborted();
        if (quality !== "best" || !rejectsBestQuality(error)) throw error;
        legacyImageServer = true;
        log?.info?.("[media] Server does not support best image quality; trying full then thumbnail");
        continue;
      }
    }
    lastResult = result;
    if (result.type === "unsupported" || result.data) return result;
    if (remaining > 0) {
      log?.info?.(`[media] Attempt ${attempts.get(key)}/${MAX_REQUESTS} for ${chatId}:${localId} returned no data, retrying...`);
      await delay(intervalMs, undefined, { signal: client.signal });
    }
  }
  return lastResult;
}
