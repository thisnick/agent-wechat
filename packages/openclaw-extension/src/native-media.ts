import { createHash, randomUUID } from "node:crypto";
import { open } from "node:fs/promises";
import { basename, extname } from "node:path";
import { getImageMetadata, resizeToJpeg, IMAGE_REDUCE_QUALITY_STEPS } from "openclaw/plugin-sdk/media-runtime";
import { NATIVE_MAX_SOURCE_BYTES } from "@agent-wechat/shared";
import type { MediaAsset, MediaJob, MediaSendRequest, NativeMessage, NativeWeChatClient } from "@agent-wechat/shared";
import { saveManagedAttachment } from "./attachment-store.ts";
import { renderAttachmentBody, type WeChatAttachment } from "./media-delivery.ts";
import type { NativeSuppliedAsset } from "./native-consumer-state.ts";
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
async function prepareImageRepresentation(bytes: Buffer, mime: string, cfg: any): Promise<{ bytes: Buffer; mime: string; preview: boolean }> {
  const configured = cfg?.agents?.defaults?.imageMaxDimensionPx;
  const side = typeof configured === "number" && Number.isFinite(configured) ? Math.max(1, Math.min(1200, Math.floor(configured))) : 1200;
  const metadata = await getImageMetadata(bytes);
  if (!metadata?.width || !metadata.height) throw Error("Native model image dimensions are unavailable");
  if (["image/png", "image/jpeg"].includes(mime) && metadata.width <= side && metadata.height <= side && bytes.length <= 5 * 1024 * 1024) return { bytes, mime, preview: false };
  for (const quality of IMAGE_REDUCE_QUALITY_STEPS) {
    const prepared = await resizeToJpeg({ buffer: bytes, maxSide: side, quality, withoutEnlargement: true });
    if (prepared.length <= 5 * 1024 * 1024) return { bytes: prepared, mime: "image/jpeg", preview: true };
  }
  throw Error("Native model image representation exceeds its bound");
}
export async function prepareNativeAttachment(params: { client: NativeWeChatClient; account: ResolvedWeChatAccount; message: NativeMessage; core: any; cfg?: any; waitMs?: number }): Promise<{ body: string; attachment?: WeChatAttachment; mediaPath?: string; mediaMime?: string; suppliedAssets?: NativeSuppliedAsset[]; pending: boolean }> {
  const { client, account, message, core } = params; const attachmentKind = kind(message.type);
  if (!attachmentKind) return { body: message.type === 1 ? message.body ?? "" : `[Unsupported WeChat message type ${message.type}]`, pending: false };
  const accountId = account.native!.accountId; const maxBytes = Math.min(account.mediaMaxMb * 1024 * 1024, 128 * 1024 * 1024);
  const scopedAssets = async () => {
    const items = (await client.mediaAssets(accountId, message.id)).items;
    if (items.some(asset => asset.accountId !== accountId || asset.messageId !== message.id)) throw Error("Native attachment identity does not match its message");
    return items;
  };
  let assets = await scopedAssets(); let asset = select(assets, message.type);
  if (!asset && !assets.some(value => ["expired", "unsupported"].includes(value.state))) {
    try {
      const job = await client.transferMedia(accountId, { messageId: message.id, assetId: null, quality: message.type === 34 || message.type === 43 ? "normal" : "best" });
      const completed = await waitNativeMediaJob(client, accountId, job, params.waitMs ?? 20000);
      if (["queued", "running"].includes(completed.state)) return { body: `[${attachmentKind} pending]`, pending: true };
      assets = await scopedAssets(); asset = select(assets, message.type);
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
  const mime = content.contentType.split(";", 1)[0].trim().toLowerCase();
  if (!/^[a-z0-9.+-]+\/[a-z0-9.+-]+$/.test(mime)) throw Error("Native attachment MIME type is unsupported");
  const hash = createHash("sha256").update(content.bytes).digest("hex");
  if (!asset.sha256 || hash !== asset.sha256 || (asset.byteLength != null && asset.byteLength !== content.bytes.length)) throw new Error("Native attachment integrity did not verify");
  const saved = await saveManagedAttachment({ data: Buffer.from(content.bytes).toString("base64"), mime: mime, filename, maxBytes, saveMediaBuffer: core.channel.media.saveMediaBuffer });
  const actual = asset.actualVariant ?? asset.variant;
  const attachment: WeChatAttachment = { kind: attachmentKind, filename, status: "ready", path: saved.path, mime: mime,
    quality: actual === "original" ? "full" : actual === "standard" ? "standard" : actual === "thumbnail" ? "thumbnail" : undefined };
  let suppliedPath = saved.path, suppliedMime = mime, suppliedHash = hash, suppliedLength = content.bytes.length;
  let body = renderAttachmentBody("", attachment);
  if (attachmentKind === "image" && params.cfg) {
    const representation = await prepareImageRepresentation(Buffer.from(content.bytes), mime, params.cfg);
    if (representation.preview) {
      const normalized = await saveManagedAttachment({ data: representation.bytes.toString("base64"), mime: representation.mime,
        filename: `${filename}.model.jpg`, maxBytes: 5 * 1024 * 1024, saveMediaBuffer: core.channel.media.saveMediaBuffer });
      suppliedPath = normalized.path; suppliedMime = representation.mime; suppliedHash = normalized.sha256; suppliedLength = normalized.size;
      body += `\n[Model image: prepared still preview; original retained separately]`;
    }
  }
  return { body, attachment, suppliedAssets: [{ id: asset.id, sha256: suppliedHash, byteLength: suppliedLength, mime: suppliedMime, path: suppliedPath,
    role: attachmentKind === "image" ? "model_image" : "file_reference", kind: attachmentKind, runtimeMedia: attachmentKind === "image" || attachmentKind === "video", contextRole: "trigger",
    sourceSha256: hash, sourceByteLength: content.bytes.length, sourceMime: mime, sourcePath: saved.path }],
    mediaPath: attachmentKind === "image" || attachmentKind === "video" ? suppliedPath : undefined, mediaMime: suppliedMime, pending: false };
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
