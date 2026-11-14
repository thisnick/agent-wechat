import type { MediaJob, NativeWeChatClient } from "@agent-wechat/shared";
import { t } from "./i18n.ts";

const ended = new Set(["available", "accepted", "failed", "cancelled", "needs_review", "partial", "expired", "unsupported"]);

/** A known job result, distinct from failure to reach the hub. */
export class StickerJobError extends Error {
  constructor(job: Pick<MediaJob, "id" | "state" | "reason">, stillRunning = false) {
    const id = /^[a-zA-Z0-9_-]{1,100}$/.test(job.id) ? job.id : "";
    const state = /^[a-z_]{1,40}$/.test(job.state) ? job.state : "unknown";
    const reason = job.reason && /^[a-z0-9_]{1,80}$/.test(job.reason) ? job.reason : "";
    super(t(stillRunning ? "stickers.jobStillRunning" : "stickers.jobFailed", { id, state, reason }));
    this.name = "StickerJobError";
  }
}

/** Watch only the submitted job; failure never submits it again. */
export async function waitStickerJob(client: NativeWeChatClient, account: string, job: MediaJob, signal?: AbortSignal): Promise<MediaJob> {
  const until = Date.now() + 60_000;
  while (!ended.has(job.state)) {
    signal?.throwIfAborted();
    if (Date.now() >= until) throw new StickerJobError(job, true);
    await new Promise(resolve => setTimeout(resolve, 400));
    job = await client.job(account, job.id, signal);
  }
  if (!["available", "accepted"].includes(job.state)) throw new StickerJobError(job);
  return job;
}
