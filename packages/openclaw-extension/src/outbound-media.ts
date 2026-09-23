import { randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { basename, extname } from "node:path";
import { WeChatClient, type VoiceJob } from "@agent-wechat/shared";
import { resolveWeChatAccount } from "./types.ts";

const EXT_MIME: Record<string, string> = {
  png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp",
  mp3: "audio/mpeg", wav: "audio/wav", ogg: "audio/ogg", opus: "audio/ogg",
  m4a: "audio/mp4", flac: "audio/flac", aac: "audio/aac",
};

function isTerminal(job: VoiceJob): boolean {
  return ["completed", "failed", "cancelled", "needs_review"].includes(job.status);
}

export async function waitForVoiceJob(client: WeChatClient, jobId: string): Promise<VoiceJob> {
  const deadline = Date.now() + 25 * 60_000;
  let lastStatusAt = Date.now();
  while (Date.now() < deadline) {
    let job: VoiceJob;
    try {
      job = await client.getVoiceJob(jobId);
      lastStatusAt = Date.now();
    } catch (error) {
      if (Date.now() - lastStatusAt > 30_000) {
        throw new Error(`Voice job ${jobId} status is unavailable: ${String(error)}. Inspect the job before retrying.`);
      }
      await new Promise((resolve) => setTimeout(resolve, 1000));
      continue;
    }
    if (isTerminal(job)) {
      if (job.status === "completed") return job;
      const sent = job.chunks.filter((chunk) => chunk.status === "verified").length;
      throw new Error(`Voice job ${jobId} ${job.status}: ${sent}/${job.chunks.length} notes verified. ${job.error ?? ""} Do not resend the whole recording.`);
    }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  throw new Error(`Voice job ${jobId} exceeded the 25-minute wait. Inspect the job before retrying.`);
}

export async function sendWeChatMedia(
  cfg: unknown,
  to: string,
  text: string,
  mediaUrl: string | undefined,
  audioAsVoice = false,
  idempotencyKey?: string,
): Promise<string> {
  const account = resolveWeChatAccount(cfg as Record<string, unknown>);
  if (!account?.serverUrl) throw new Error("No serverUrl configured");
  const client = new WeChatClient({ baseUrl: account.serverUrl, token: account.token });
  if (!mediaUrl) {
    if (audioAsVoice) throw new Error("Voice sending requires an audio attachment");
    const result = await client.sendMessage({ chatId: to, text: text || undefined });
    if (!result.success) throw new Error(result.error ?? "Send failed");
    return `agent-wechat:${to}:${Date.now()}`;
  }

  let bytes: Uint8Array;
  let mimeType: string;
  let filename: string;
  if (mediaUrl.startsWith("http://") || mediaUrl.startsWith("https://")) {
    const res = await fetch(mediaUrl);
    if (!res.ok) throw new Error(`Media fetch failed: ${res.status}`);
    bytes = new Uint8Array(await res.arrayBuffer());
    mimeType = res.headers.get("content-type")?.split(";")[0] ?? "application/octet-stream";
    filename = basename(new URL(mediaUrl).pathname) || "file";
    if (mimeType === "application/octet-stream") {
      mimeType = EXT_MIME[extname(filename).toLowerCase().slice(1)] ?? mimeType;
    }
  } else {
    const localPath = mediaUrl.startsWith("file://") ? new URL(mediaUrl) : mediaUrl;
    const pathText = localPath instanceof URL ? localPath.pathname : localPath;
    bytes = new Uint8Array(await readFile(localPath));
    filename = basename(pathText);
    mimeType = EXT_MIME[extname(filename).toLowerCase().slice(1)] ?? "application/octet-stream";
  }

  if (audioAsVoice) {
    if (!mimeType.startsWith("audio/")) throw new Error(`Voice media must be audio, got ${mimeType}`);
    const job = await client.createVoiceJob(to, bytes, idempotencyKey ?? randomUUID());
    await waitForVoiceJob(client, job.jobId);
    if (text) {
      const caption = await client.sendMessage({ chatId: to, text });
      if (!caption.success) throw new Error(`Voice job ${job.jobId} completed, but caption failed: ${caption.error ?? "unknown error"}`);
    }
    return `agent-wechat:voice:${job.jobId}`;
  }

  const base64 = Buffer.from(bytes).toString("base64");
  const result = mimeType.startsWith("image/")
    ? await client.sendMessage({ chatId: to, text: text || undefined, image: { data: base64, mimeType } })
    : await client.sendMessage({ chatId: to, text: text || undefined, file: { data: base64, filename } });
  if (!result.success) throw new Error(result.error ?? "Send media failed");
  return `agent-wechat:${to}:${Date.now()}`;
}
