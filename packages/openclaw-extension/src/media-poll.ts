import type { MediaResult, WeChatClient } from "@agent-wechat/shared";

type MediaClient = Pick<WeChatClient, "getMedia">;
type Log = { info?: (...args: any[]) => void };

function rejectsBestQuality(error: unknown): boolean {
  return error instanceof Error && /^(400|422)\b/.test(error.message) && /\bbest\b/i.test(error.message);
}

/** Poll the server for media; older servers reject the new best-image query. */
export async function pollMedia(
  client: MediaClient,
  chatId: string,
  localId: number,
  quality: "full" | "best",
  log?: Log,
  maxAttempts = 15,
  intervalMs = 1000,
): Promise<MediaResult | null> {
  let lastResult: MediaResult | null = null;
  let legacyImageServer = false;
  for (let attempt = 1; attempt <= maxAttempts; attempt++) {
    let result: MediaResult;
    if (legacyImageServer) {
      const full = await client.getMedia(chatId, localId, "full");
      if (full.data || full.type === "unsupported") return full;
      const preview = await client.getMedia(chatId, localId, "thumbnail");
      result = preview.data ? { ...preview, quality: "thumbnail" } : full;
    } else {
      try {
        result = await client.getMedia(chatId, localId, quality);
      } catch (error) {
        if (quality !== "best" || !rejectsBestQuality(error)) throw error;
        legacyImageServer = true;
        log?.info?.("[media] Server does not support best image quality; trying full then thumbnail");
        attempt--;
        continue;
      }
    }
    lastResult = result;
    if (result.type === "unsupported" || result.data) return result;
    if (attempt < maxAttempts) {
      log?.info?.(`[media] Attempt ${attempt}/${maxAttempts} for ${chatId}:${localId} returned no data, retrying...`);
      await new Promise(resolve => setTimeout(resolve, intervalMs));
    }
  }
  return lastResult;
}
