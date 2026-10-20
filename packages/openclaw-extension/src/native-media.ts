import { createHash, randomUUID } from "node:crypto";
import { open } from "node:fs/promises";
import { basename, extname } from "node:path";
import { NATIVE_MAX_SOURCE_BYTES } from "@agent-wechat/shared";
import type { MediaAsset, MediaJob, MediaSendRequest, NativeMessage, NativeWeChatClient } from "@agent-wechat/shared";
import { saveManagedAttachment } from "./attachment-store.ts";
import { renderAttachmentBody, type WeChatAttachment } from "./media-delivery.ts";
import type { ResolvedWeChatAccount } from "./types.ts";

function kind(type: number): WeChatAttachment["kind"] | undefined {
  return type === 3 || type === 47 ? "image" : type === 34 ? "audio" : type === 43 ? "video" : type === 49 ? "file" : undefined;
}
const terminalJobs = new Set(["accepted", "completed", "succeeded", "available", "failed", "cancelled", "expired", "needs_review"]);
export async function waitNativeMediaJob(client: NativeWeChatClient, accountId: string, initial: MediaJob, timeoutMs = 65000): Promise<MediaJob> {
  const deadline = Date.now() + timeoutMs; let job = initial; let delay = 250;
  while (!terminalJobs.has(job.state) && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, Math.min(delay, Math.max(0, deadline - Date.now()))));
    const remaining = deadline - Date.now(); if (remaining <= 0) break;
    job = await client.job(accountId, job.id, AbortSignal.timeout(remaining)); delay = Math.min(delay * 2, 1000);
  }
  return job;
}
function select(assets: MediaAsset[], type: number): MediaAsset | undefined {
  const preferred = type === 34 ? ["normal"] : type === 43 ? ["normal", "original"] : ["original", "standard", "thumbnail", "encoded"];
  const rank = (asset: MediaAsset) => { const index = preferred.indexOf(asset.actualVariant ?? asset.variant); return index < 0 ? preferred.length : index; };
  return [...assets].sort((a, b) => rank(a) - rank(b))
    .find(asset => asset.state === "available" && (type !== 34 || (asset.actualVariant ?? asset.variant) === "normal"));
}
export async function prepareNativeAttachment(params: { client: NativeWeChatClient; account: ResolvedWeChatAccount; message: NativeMessage; core: any; waitMs?: number }): Promise<{ body: string; attachment?: WeChatAttachment; mediaPath?: string; mediaMime?: string; pending: boolean }> {
  const { client, account, message, core } = params; const attachmentKind = kind(message.type);
  if (!attachmentKind) return { body: message.type === 1 ? message.body ?? "" : `[Unsupported WeChat message type ${message.type}]`, pending: false };
  const accountId = account.native!.accountId; const maxBytes = Math.min(account.mediaMaxMb * 1024 * 1024, 128 * 1024 * 1024);
  let assets = (await client.mediaAssets(accountId, message.id)).items; let asset = select(assets, message.type);
  if (!asset && !assets.some(value => ["expired", "unsupported"].includes(value.state))) {
    try {
      const job = await client.transferMedia(accountId, { messageId: message.id, assetId: null, quality: message.type === 34 || message.type === 43 ? "normal" : "best" });
      const completed = await waitNativeMediaJob(client, accountId, job, params.waitMs ?? 20000);
      if (["queued", "running"].includes(completed.state)) return { body: `[${attachmentKind} pending]`, pending: true };
      assets = (await client.mediaAssets(accountId, message.id)).items; asset = select(assets, message.type);
    } catch { return { body: `[${attachmentKind} pending: retrieval is unavailable]`, pending: true }; }
  }
  const filename = basename(asset?.filename ?? `message-${createHash("sha256").update(message.id).digest("hex").slice(0, 12)}.bin`).replace(/[\r\n\0]/g, "_");
  if (!asset) {
    const state = assets.find(value => value.state === "expired") ? "expired" : assets.find(value => value.state === "unsupported") ? "unsupported" : "unavailable";
    const attachment: WeChatAttachment = { kind: attachmentKind, filename, status: state };
    return { body: renderAttachmentBody("", attachment), attachment, pending: false };
  }
  if (asset.byteLength != null && asset.byteLength > maxBytes) { const attachment: WeChatAttachment = { kind: attachmentKind, filename, status: "too_large" }; return { body: renderAttachmentBody("", attachment), attachment, pending: false }; }
  const content = await client.mediaContent(accountId, asset.id, { maxBytes });
  const hash = createHash("sha256").update(content.bytes).digest("hex");
  if (!asset.sha256 || hash !== asset.sha256 || (asset.byteLength != null && asset.byteLength !== content.bytes.length)) throw new Error("Native attachment integrity did not verify");
  const saved = await saveManagedAttachment({ data: Buffer.from(content.bytes).toString("base64"), mime: content.contentType, filename, maxBytes, saveMediaBuffer: core.channel.media.saveMediaBuffer });
  const actual = asset.actualVariant ?? asset.variant;
  const attachment: WeChatAttachment = { kind: attachmentKind, filename, status: "ready", path: saved.path, mime: content.contentType,
    quality: actual === "original" ? "full" : actual === "standard" ? "standard" : actual === "thumbnail" ? "thumbnail" : undefined };
  return { body: renderAttachmentBody("", attachment), attachment, mediaPath: attachmentKind === "image" || attachmentKind === "video" ? saved.path : undefined, mediaMime: content.contentType, pending: false };
}

export async function sendNativeAttachment(client: NativeWeChatClient, account: ResolvedWeChatAccount, threadId: string, mediaUrl: string, audioAsVoice = false, key: string = randomUUID()): Promise<string> {
  const max = Math.min(account.mediaMaxMb * 1024 * 1024, NATIVE_MAX_SOURCE_BYTES); let bytes: Uint8Array; let filename: string; let mime: string;
  if (/^https?:/.test(mediaUrl)) {
    const response = await fetch(mediaUrl, { redirect: "error", signal: AbortSignal.timeout(30000) }); if (!response.ok) throw new Error(`Native outbound source unavailable (${response.status})`);
    const length = response.headers.get("content-length"); if (length && (!/^\d+$/.test(length) || Number(length) > max)) { await response.body?.cancel(); throw new Error("Native outbound source exceeds configured size"); }
    const reader = response.body?.getReader(); if (!reader) throw new Error("Native outbound source has no bytes");
    const chunks: Uint8Array[] = []; let size = 0; try { while (true) { const { value, done } = await reader.read(); if (done) break; size += value.length; if (size > max) throw new Error("Native outbound source exceeds configured size"); chunks.push(value); } } catch (error) { await reader.cancel(); throw error; } finally { reader.releaseLock(); }
    bytes = new Uint8Array(size); let offset = 0; for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
    filename = basename(new URL(mediaUrl).pathname) || "attachment"; mime = response.headers.get("content-type")?.split(";")[0] ?? "application/octet-stream";
  } else {
    const path = mediaUrl.startsWith("file:") ? new URL(mediaUrl) : mediaUrl; const file = await open(path, "r");
    try { const stat = await file.stat(); if (!stat.isFile() || stat.size === 0 || stat.size > max) throw new Error("Native outbound source must be a bounded regular file"); const chunks: Buffer[] = []; let size = 0; while (true) { const chunk = Buffer.alloc(Math.min(65536, max + 1 - size)); const { bytesRead } = await file.read(chunk, 0, chunk.length, null); if (!bytesRead) break; size += bytesRead; if (size > max) throw new Error("Native outbound source exceeds configured size"); chunks.push(chunk.subarray(0, bytesRead)); } bytes = Buffer.concat(chunks, size); } finally { await file.close(); }
    filename = basename(path instanceof URL ? path.pathname : path); const extension = extname(filename).toLowerCase(); mime = [".png", ".jpg", ".jpeg", ".gif", ".webp"].includes(extension) ? `image/${extension === ".jpg" ? "jpeg" : extension.slice(1)}` : extension === ".mp4" ? "video/mp4" : [".wav", ".mp3", ".ogg", ".m4a", ".flac", ".aac"].includes(extension) ? `audio/${extension.slice(1)}` : "application/octet-stream";
  }
  if (audioAsVoice && !mime.startsWith("audio/")) throw new Error("Voice sending requires an explicit audio attachment");
  const accountId = account.native!.accountId; const source = await client.upload(accountId, bytes, filename, mime);
  const input: MediaSendRequest = { threadId, kind: audioAsVoice ? "voice" : mime.startsWith("image/") ? "image" : mime.startsWith("video/") ? "video" : "file", sourceId: source.id, standardSourceId: null, thumbnailSourceId: null, playSeconds: null, durationMs: null };
  const job = await waitNativeMediaJob(client, accountId, await client.sendMedia(accountId, input, key), audioAsVoice ? 25 * 60000 : 65000);
  if (!["accepted", "completed", "succeeded"].includes(job.state)) throw new Error(`Native media job ${job.id} requires inspection (${job.state}); retain completed items and do not automatically resend`);
  return `agent-wechat-native:${accountId}:${job.id}`;
}
