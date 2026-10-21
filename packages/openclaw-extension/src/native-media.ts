import { createHash, randomUUID } from "node:crypto";
import { open } from "node:fs/promises";
import { basename, extname } from "node:path";
import { getImageMetadata, resizeToJpeg, IMAGE_REDUCE_QUALITY_STEPS } from "openclaw/plugin-sdk/media-runtime";
import { NATIVE_MAX_SOURCE_BYTES, NativeApiError } from "@agent-wechat/shared";
import type { MediaAsset, MediaJob, MediaSendRequest, MediaVariant, NativeMessage, NativeWeChatClient } from "@agent-wechat/shared";
import { saveManagedAttachment } from "./attachment-store.ts";
import { renderAttachmentBody, type WeChatAttachment } from "./media-delivery.ts";
import type { NativeSuppliedAsset } from "./native-consumer-state.ts";
import type { ResolvedWeChatAccount } from "./types.ts";

function kind(type: number): WeChatAttachment["kind"] | undefined {
  return type === 3 || type === 47 ? "image" : type === 34 ? "audio" : type === 43 ? "video" : type === 49 ? "file" : undefined;
}
const terminalJobs = new Set(["accepted", "completed", "succeeded", "available", "failed", "cancelled", "expired", "needs_review"]);
export async function waitNativeMediaJob(client: NativeWeChatClient, accountId: string, initial: MediaJob, timeoutMs = 65000, signal?: AbortSignal, assertCurrent?: () => void): Promise<MediaJob> {
  const deadline = Date.now() + timeoutMs; let job = initial; let delay = 250;
  while (!terminalJobs.has(job.state) && Date.now() < deadline) {
    signal?.throwIfAborted(); assertCurrent?.();
    await new Promise(resolve => setTimeout(resolve, Math.min(delay, Math.max(0, deadline - Date.now()))));
    const remaining = deadline - Date.now(); if (remaining <= 0) break;
    signal?.throwIfAborted(); assertCurrent?.();
    job = await client.job(accountId, job.id, signal ? AbortSignal.any([signal, AbortSignal.timeout(remaining)]) : AbortSignal.timeout(remaining)); delay = Math.min(delay * 2, 1000);
  }
  return job;
}
function select(assets: MediaAsset[], type: number, quality: MediaVariant = "best"): MediaAsset | undefined {
  const preferred = type === 34 ? ["normal"] : type === 43 ? ["normal", "original"] : ["original", "standard", "thumbnail", "encoded"];
  const rank = (asset: MediaAsset) => { const index = preferred.indexOf(asset.actualVariant ?? asset.variant); return index < 0 ? preferred.length : index; };
  return [...assets].sort((a, b) => rank(a) - rank(b))
    .find(asset => asset.state === "available" && (quality === "best" ? type !== 34 || (asset.actualVariant ?? asset.variant) === "normal" : (asset.actualVariant ?? asset.variant) === quality));
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
export type NativePreparedMedia = { body: string; attachment?: WeChatAttachment; mediaPath?: string; mediaMime?: string; suppliedAssets?: NativeSuppliedAsset[]; pending: boolean; jobId?: string; retryable?: boolean; reason?: string; variant?: MediaVariant; quote?: { state: string; messageId?: string; senderId?: string; type?: number } };
export type NativePrepareMediaParams = { client: NativeWeChatClient; account: ResolvedWeChatAccount; message: NativeMessage; core: any; cfg?: any; waitMs?: number; download?: boolean; quality?: MediaVariant; signal?: AbortSignal; assertCurrent?: () => void; beforeMediaAccess?: () => Promise<void>; allowQuotedMessage?: (message: Pick<NativeMessage, "senderId" | "direction">) => Promise<boolean> };
async function prepareNativeQuote(params: NativePrepareMediaParams): Promise<NativePreparedMedia> {
  const { client, account, message } = params;
  const guard = () => { params.signal?.throwIfAborted(); params.assertCurrent?.(); }; guard();
  const comment = (message.body ?? "").slice(0, 16384);
  const unavailable = (state: string): NativePreparedMedia => ({ body: `${comment}${comment ? "\n\n" : ""}[Quoted message ${state}; original lookup is confined to this chat]`, pending: false, retryable: state === "unavailable", quote: { state } });
  if (!message.quote) return unavailable("invalid");
  await params.beforeMediaAccess?.(); guard();
  let resolution;
  try { resolution = await client.quotedMessage(account.native!.accountId, message.id); }
  catch (error) { guard(); if (!(error instanceof NativeApiError)) throw error; return unavailable("unavailable"); }
  guard();
  if (resolution.accountId !== message.accountId || resolution.messageId !== message.id || resolution.threadId !== message.threadId) throw Error("Native quote identity does not match its message");
  if (resolution.state === "embedded") {
    if (resolution.message != null || (message.quote.type === 1 ? typeof resolution.embeddedText !== "string" || resolution.embeddedText.length > 16384
      : message.quote.type !== 3 || resolution.embeddedImage !== true)) throw Error("Invalid embedded quote resolution");
    const author = { senderId: resolution.senderId ?? null, direction: null };
    if (params.allowQuotedMessage && !await params.allowQuotedMessage(author)) return unavailable("not permitted");
    guard();
    const quote = { state: "embedded", senderId: resolution.senderId ?? undefined, type: message.quote.type };
    const prefix = `${comment}${comment ? "\n\n" : ""}[Quoted ${message.quote.type === 3 ? "image" : "text"} from ${resolution.senderId ?? "unknown sender"}; embedded in quote, original not stored]\n`;
    if (message.quote.type === 1) return { body: `${prefix}${resolution.embeddedText}\n[/Quoted message]`, pending: false, quote };
    const beforeMediaAccess = async () => {
      await params.beforeMediaAccess?.(); guard();
      if (params.allowQuotedMessage && !await params.allowQuotedMessage(author)) throw Error("Quoted message is no longer permitted");
      guard();
    };
    // The daemon owns the quote's media plan. This changes only the renderer;
    // the API still receives the stored parent ID and re-parses its metadata.
    const prepared = await prepareNativeAttachment({ ...params, message: { ...message, type: 3, appMessageType: undefined, quote: undefined }, beforeMediaAccess });
    return { ...prepared, body: `${prefix}${prepared.body}\n[/Quoted message]`, pending: false, quote,
      ...(prepared.pending ? { attachment: { kind: "image" as const, status: "pending" as const, filename: "quoted-image" } } : {}) };
  }
  if (resolution.state !== "resolved") {
    if (!["missing", "invalid", "unsupported", "ambiguous"].includes(resolution.state) || resolution.message != null) throw Error("Invalid native quote resolution");
    return unavailable(resolution.state);
  }
  const original = resolution.message;
  if (!original || original.accountId !== message.accountId || original.threadId !== message.threadId || original.id === message.id
    || original.serverId !== message.quote.serverId || original.type !== message.quote.type || ![1, 3].includes(original.type)) throw Error("Native quoted original crosses its message boundary");
  if (params.allowQuotedMessage && !await params.allowQuotedMessage(original)) return unavailable("not permitted");
  guard();
  const quote = { state: "resolved", messageId: original.id, senderId: original.senderId ?? undefined, type: original.type };
  const prefix = `${comment}${comment ? "\n\n" : ""}[Quoted ${original.type === 3 ? "image" : "text"} from ${original.senderId ?? "unknown sender"}; message ${original.id}]\n`;
  if (original.type === 1) return { body: `${prefix}${(original.body ?? "").slice(0, 16384)}\n[/Quoted message]`, pending: false, quote };
  const beforeMediaAccess = async () => {
    await params.beforeMediaAccess?.(); guard();
    if (params.allowQuotedMessage && !await params.allowQuotedMessage(original)) throw Error("Quoted message is no longer permitted");
    guard();
  };
  const prepared = await prepareNativeAttachment({ ...params, message: original, beforeMediaAccess });
  // A delayed quoted image must not hide the current comment. Its explicit
  // status/job remains available through the ordinary media lookup tool.
  return { ...prepared, body: `${prefix}${prepared.body}\n[/Quoted message]`, pending: false, quote,
    ...(prepared.pending ? { attachment: { kind: "image" as const, status: "pending" as const, filename: "quoted-image" } } : {}),
    suppliedAssets: prepared.suppliedAssets?.map(asset => ({ ...asset, sourceMessageId: original.id })) };
}
export async function prepareNativeAttachment(params: NativePrepareMediaParams): Promise<NativePreparedMedia> {
  const { client, account, message, core } = params; const attachmentKind = kind(message.type);
  const guard = () => { params.signal?.throwIfAborted(); params.assertCurrent?.(); }; guard();
  const authorize = async () => { guard(); await params.beforeMediaAccess?.(); guard(); };
  if (message.type === 49 && message.appMessageType === 57) return prepareNativeQuote(params);
  if (message.type === 49 && message.appMessageType === 19) return { body: message.body?.slice(0, 65536) || "[Forwarded chat history unavailable]", pending: false };
  if (message.type === 49 && message.appMessageType != null && message.appMessageType !== 6) return { body: `[Unsupported WeChat app message subtype ${message.appMessageType}]`, pending: false, retryable: false };
  if (!attachmentKind) return { body: message.type === 1 ? message.body ?? "" : `[Unsupported WeChat message type ${message.type}]`, pending: false };
  const accountId = account.native!.accountId; const maxBytes = Math.min(account.mediaMaxMb * 1024 * 1024, 128 * 1024 * 1024);
  const scopedAssets = async () => {
    await authorize();
    const items = (await client.mediaAssets(accountId, message.id)).items;
    guard();
    if (items.some(asset => asset.accountId !== accountId || asset.messageId !== message.id)) throw Error("Native attachment identity does not match its message");
    return items;
  };
  const quality = params.quality ?? "best";
  const transferQuality = quality === "best" && (message.type === 34 || message.type === 43) ? "normal" : quality;
  let assets = await scopedAssets(); let asset = select(assets, message.type, quality);
  const relevant = () => assets.filter(value => transferQuality === "best" || value.variant === transferQuality);
  const transferCandidate = () => {
    const order = ["original", "normal", "standard", "thumbnail", "encoded"];
    return [...relevant()].sort((a, b) => order.indexOf(a.variant) - order.indexOf(b.variant))[0];
  };
  const initialCandidate = transferCandidate();
  if (!asset && initialCandidate?.byteLength != null && initialCandidate.byteLength > maxBytes) {
    const attachment: WeChatAttachment = { kind: attachmentKind, filename: basename(initialCandidate.filename ?? "attachment"), status: "too_large" };
    return { body: renderAttachmentBody("", attachment), attachment, pending: false, retryable: false };
  }
  let job: MediaJob | undefined;
  let transferError: NativeApiError | undefined;
  if (!asset && initialCandidate?.jobId) {
    await authorize();
    job = await client.job(accountId, initialCandidate.jobId, params.signal);
    guard();
    if (job.accountId !== accountId || job.kind !== "download" || (job.assetId != null && job.assetId !== initialCandidate.id)) throw Error("Native download job identity does not match its attachment");
    assets = await scopedAssets(); asset = select(assets, message.type, quality);
    if (!asset && ["queued", "running"].includes(job.state)) return { body: `[${attachmentKind} pending]`, pending: true, jobId: job.id, retryable: true };
  }
  if (!asset && !job && params.download !== false && !["expired", "unsupported"].includes(transferCandidate()?.state ?? "")) {
    try {
      await authorize();
      job = await client.transferMedia(accountId, { messageId: message.id, assetId: null, quality: transferQuality });
      guard();
      job = await waitNativeMediaJob(client, accountId, job, params.waitMs ?? 20000, params.signal, guard);
      guard();
    } catch (error) {
      guard();
      if (!(error instanceof NativeApiError) || error.retryable) return { body: `[${attachmentKind} pending: retrieval is unavailable]`, pending: true, jobId: job?.id, retryable: true };
      transferError = error;
    }
    // Refresh even while queued: a cached copy may have arrived during the wait.
    // Identity/integrity errors must propagate rather than becoming pending.
    assets = await scopedAssets(); asset = select(assets, message.type, quality);
    if (!asset && job && ["queued", "running"].includes(job.state)) return { body: `[${attachmentKind} pending]`, pending: true, jobId: job.id, retryable: true };
  }
  const candidate = transferCandidate();
  const filename = basename(asset?.filename ?? candidate?.filename ?? `message-${createHash("sha256").update(message.id).digest("hex").slice(0, 12)}.bin`).replace(/[\r\n\0]/g, "_");
  if (!asset) {
    const state = candidate?.state === "expired" ? "expired" : candidate?.state === "unsupported" ? "unsupported" : "unavailable";
    const attachment: WeChatAttachment = { kind: attachmentKind, filename, status: state, expiresAt: candidate?.expiresAt ?? undefined };
    return { body: renderAttachmentBody("", attachment), attachment, pending: false, jobId: job?.id,
      retryable: state === "expired" || state === "unsupported" ? false : transferError ? false : job?.retryable ?? params.download === false,
      reason: transferError?.code ?? (job?.reason && /^[a-z0-9_]{1,80}$/.test(job.reason) ? job.reason : undefined) };
  }
  if (asset.byteLength != null && asset.byteLength > maxBytes) { const attachment: WeChatAttachment = { kind: attachmentKind, filename, status: "too_large" }; return { body: renderAttachmentBody("", attachment), attachment, pending: false }; }
  await authorize();
  const content = await client.mediaContent(accountId, asset.id, { maxBytes, signal: params.signal });
  guard();
  const mime = content.contentType.split(";", 1)[0].trim().toLowerCase();
  if (!/^[a-z0-9.+-]+\/[a-z0-9.+-]+$/.test(mime)) throw Error("Native attachment MIME type is unsupported");
  const hash = createHash("sha256").update(content.bytes).digest("hex");
  if (!asset.sha256 || hash !== asset.sha256 || (asset.byteLength != null && asset.byteLength !== content.bytes.length)) throw new Error("Native attachment integrity did not verify");
  await authorize();
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
  guard();
  return { body, attachment, jobId: job?.id, retryable: false, variant: actual, suppliedAssets: [{ id: asset.id, sha256: suppliedHash, byteLength: suppliedLength, mime: suppliedMime, path: suppliedPath,
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
