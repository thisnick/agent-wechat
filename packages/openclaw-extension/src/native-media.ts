import { createHash, randomUUID } from "node:crypto";
import { openAsBlob } from "node:fs";
import { mkdtemp, open, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, extname, join } from "node:path";
import { getImageMetadata, resizeToJpeg, IMAGE_REDUCE_QUALITY_STEPS } from "openclaw/plugin-sdk/media-runtime";
import { saveMediaStream } from "openclaw/plugin-sdk/media-store";
import { NATIVE_MAX_SOURCE_BYTES, NATIVE_MAX_ORDINARY_SOURCE_BYTES, NATIVE_MAX_VIDEO_SOURCE_BYTES, NativeApiError } from "@agent-wechat/shared";
import type { MediaAsset, MediaJob, MediaSendRequest, MediaVariant, NativeMessage, NativeWeChatClient } from "@agent-wechat/shared";
import { saveManagedAttachment, saveManagedAttachmentBytes, saveVerifiedManagedAttachmentStream } from "./attachment-store.ts";
import { renderAttachmentBody, type WeChatAttachment } from "./media-delivery.ts";
import type { NativeSuppliedAsset } from "./native-consumer-state.ts";
import type { ResolvedWeChatAccount } from "./types.ts";
import { nativeOutboundDelivery, type NativeOutboundDelivery } from "./native-send.ts";
import { nativeRecallInfo } from "./native-recall-state.ts";
import { renderEmbeddedQuote } from "./native-quote.ts";

function kind(type: number): WeChatAttachment["kind"] | undefined {
  return type === 3 || type === 47 ? "image" : type === 34 ? "audio" : type === 43 ? "video" : type === 49 ? "file" : undefined;
}
const terminalJobs = new Set(["accepted", "completed", "succeeded", "available", "failed", "partial", "cancelled", "expired", "needs_review"]);
const successfulSendJobs = new Set(["accepted", "completed", "succeeded"]);
const terminalIdentityErrors = new Set(["media_identity_unavailable", "media_source_changed"]);
export class NativeMediaJobInspectionError extends Error {
  readonly code = "native_media_job_inspection_required";
  readonly retryable = false;
  readonly jobId: string;
  readonly accountId: string;
  readonly threadId: string;
  readonly idempotencyKey: string;
  readonly state: string;
  readonly reason: "status_unavailable" | "job_not_completed";
  constructor(job: MediaJob, accountId: string, threadId: string, key: string, reason: "status_unavailable" | "job_not_completed") {
    super(`Native media job ${job.id} requires inspection (${job.state}); retain the original job and idempotency key and do not automatically resend`);
    this.name = "NativeMediaJobInspectionError";
    this.jobId = job.id; this.accountId = accountId; this.threadId = threadId;
    this.idempotencyKey = key; this.state = job.state; this.reason = reason;
  }
}
function mediaSendResult(job: MediaJob): Record<string, unknown> { return job.result && typeof job.result === "object" && !Array.isArray(job.result) ? job.result as Record<string, unknown> : {}; }
function mediaSendReceiptIds(job: MediaJob, audioAsVoice: boolean): string[] | undefined {
  const result = mediaSendResult(job), ids = result.messageIds;
  if (!successfulSendJobs.has(job.state) || result.profileProjection === "unknown" || !Array.isArray(ids) || !ids.length || ids.length > 256
    || ids.some(id => typeof id !== "string" || !id || id.length > 4096) || new Set(ids).size !== ids.length) return undefined;
  if (audioAsVoice) {
    const chunks = result.chunks;
    if (!Array.isArray(chunks) || !chunks.length || chunks.length !== ids.length
      || chunks.some(chunk => !chunk || chunk.state !== "accepted" || typeof chunk.serverMessageId !== "string")) return undefined;
  } else if (ids.length !== 1) return undefined;
  return ids;
}
/** Acceptance precedes local receipt projection. Inspect the submitted job only. */
export async function waitNativeMediaSendReceipts(client: NativeWeChatClient, accountId: string, threadId: string, initial: MediaJob, audioAsVoice: boolean, timeoutMs = 10000): Promise<MediaJob> {
  if (initial.accountId !== accountId || initial.threadId !== threadId || initial.kind !== "send") throw Error("Native media receipt job identity mismatch; inspect the submitted job and do not resend");
  const deadline = Date.now() + Math.max(0, Math.min(timeoutMs, 10000)); let job = initial, delay = 100;
  while (successfulSendJobs.has(job.state) && !mediaSendReceiptIds(job, audioAsVoice) && Date.now() < deadline) {
    const result = mediaSendResult(job);
    if (["unknown", "not_applicable"].includes(String(result.profileProjection))
      || audioAsVoice && Array.isArray(result.chunks) && result.chunks.some(chunk => !chunk || chunk.state !== "accepted")) break;
    await new Promise(resolve => setTimeout(resolve, Math.min(delay, Math.max(0, deadline - Date.now()))));
    const remaining = deadline - Date.now(); if (remaining <= 0) break;
    let next: MediaJob;
    try { next = await client.job(accountId, initial.id, AbortSignal.timeout(remaining)); }
    catch { break; }
    if (next.id !== initial.id || next.accountId !== accountId || next.threadId !== threadId || next.kind !== "send"
      || next.sourceId !== initial.sourceId || initial.serverMessageId != null && next.serverMessageId !== initial.serverMessageId) break;
    job = next; delay = Math.min(delay * 2, 1000);
  }
  return job;
}
export async function waitNativeMediaJob(client: NativeWeChatClient, accountId: string, initial: MediaJob, timeoutMs = 65000, signal?: AbortSignal, assertCurrent?: () => void): Promise<MediaJob> {
  return waitNativeMediaStatus(initial, timeoutMs, signal, assertCurrent, (job, pollSignal) => client.job(accountId, job.id, pollSignal));
}
async function waitNativeMediaStatus(initial: MediaJob, timeoutMs: number, signal: AbortSignal | undefined, assertCurrent: (() => void) | undefined,
  readStatus: (job: MediaJob, signal: AbortSignal) => Promise<MediaJob | undefined>): Promise<MediaJob> {
  const deadline = Date.now() + timeoutMs; let job = initial; let delay = 250;
  while (!terminalJobs.has(job.state) && Date.now() < deadline) {
    signal?.throwIfAborted(); assertCurrent?.();
    await new Promise(resolve => setTimeout(resolve, Math.min(delay, Math.max(0, deadline - Date.now()))));
    const remaining = deadline - Date.now(); if (remaining <= 0) break;
    signal?.throwIfAborted(); assertCurrent?.();
    const next = await readStatus(job, signal ? AbortSignal.any([signal, AbortSignal.timeout(remaining)]) : AbortSignal.timeout(remaining));
    if (!next) break;
    job = next; delay = Math.min(delay * 2, 1000);
  }
  return job;
}
async function incomingMediaJobStatus(client: NativeWeChatClient, accountId: string, jobId: string, assetId: string | undefined,
  signal: AbortSignal | undefined, assertCurrent: () => void): Promise<MediaJob | undefined> {
  let job: MediaJob;
  // Only this known job's read may become pending. Scope, identity and integrity
  // failures stay outside the transport catch, and no transfer is resubmitted.
  assertCurrent();
  try { job = await client.job(accountId, jobId, signal); }
  catch (error) {
    assertCurrent();
    if (error instanceof NativeApiError ? error.retryable && !terminalIdentityErrors.has(error.code) && error.code !== "message_recalled"
      : error instanceof TypeError || error instanceof DOMException && ["TimeoutError", "NetworkError", "AbortError"].includes(error.name)) return undefined;
    throw error;
  }
  assertCurrent();
  if (job.id !== jobId || job.accountId !== accountId || job.kind !== "download" || (assetId != null && job.assetId != null && job.assetId !== assetId)) throw Error("Native download job identity does not match its attachment");
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
export type NativePrepareMediaParams = { client: NativeWeChatClient; account: ResolvedWeChatAccount; message: NativeMessage; core: any; cfg?: any; waitMs?: number; download?: boolean; quality?: MediaVariant; signal?: AbortSignal; assertCurrent?: () => void; beforeMediaAccess?: () => Promise<void>; allowQuotedMessage?: (message: Pick<NativeMessage, "senderId" | "direction">) => Promise<boolean>; saveMediaStream?: typeof saveMediaStream };
async function prepareNativeLink(params: NativePrepareMediaParams): Promise<NativePreparedMedia> {
  const { message } = params;
  const link = message.link!;
  const body = [`[Link]${link.title ? ` ${link.title}` : ""}`, link.description, link.url].filter(Boolean).join("\n").slice(0, 65536);
  const unavailable = (status: "unsupported" | "unavailable", reason?: string): NativePreparedMedia => ({
    body: `${body}\n[Link preview ${status}]`, pending: false, retryable: false, reason,
    attachment: { kind: "image", status, filename: "link-preview" },
  });
  if (params.quality && !["best", "thumbnail"].includes(params.quality)) return unavailable("unsupported", "unsupported_media_variant");
  let prepared: NativePreparedMedia;
  try {
    // Only the renderer changes. The daemon re-parses the stored card using
    // this parent message ID and owns its thumbnail transfer plan.
    prepared = await prepareNativeAttachment({ ...params, quality: "thumbnail",
      message: { ...message, type: 3, appMessageType: undefined, quote: undefined, link: undefined } });
  } catch (error) {
    params.signal?.throwIfAborted(); params.assertCurrent?.();
    if (!(error instanceof NativeApiError)) throw error;
    if (terminalIdentityErrors.has(error.code) || error.code === "message_recalled") throw error;
    return { ...unavailable("unavailable", error.code), retryable: error.retryable };
  }
  if (prepared.reason === "message_recalled" || terminalIdentityErrors.has(prepared.reason ?? "")) return prepared;
  // A thumbnail is optional: a queued or failed preview must not hold the
  // card's title, description and URL behind media retries.
  return { ...prepared, body: `${body}\n[Link preview]\n${prepared.body}`, pending: false,
    ...(prepared.pending ? { attachment: { kind: "image" as const, status: "pending" as const, filename: "link-preview" } } : {}) };
}
async function prepareNativeQuote(params: NativePrepareMediaParams): Promise<NativePreparedMedia> {
  const { message } = params;
  const guard = () => { params.signal?.throwIfAborted(); params.assertCurrent?.(); }; guard();
  const author = { senderId: message.quote?.senderId ?? null, direction: message.quote?.direction ?? null };
  const permitted = !message.quote || !params.allowQuotedMessage || await params.allowQuotedMessage(author);
  guard();
  const rendered = renderEmbeddedQuote(message, permitted);
  if (!rendered.image) return { body: rendered.body, quote: rendered.quote, pending: false, retryable: false,
    ...(rendered.quote.state === "recalled" ? { reason: "message_recalled" } : {}) };
  const beforeMediaAccess = async () => {
    await params.beforeMediaAccess?.(); guard();
    if (params.allowQuotedMessage && !await params.allowQuotedMessage(author)) throw Error("Quoted message is no longer permitted");
    guard();
  };
  // The received parent owns the descriptor, cache identity and transfer plan.
  const prepared = await prepareNativeAttachment({ ...params, message: { ...message, type: 3, appMessageType: undefined, quote: undefined, link: undefined }, beforeMediaAccess });
  if (prepared.reason === "message_recalled" || terminalIdentityErrors.has(prepared.reason ?? "")) return prepared;
  return { ...prepared, body: renderEmbeddedQuote(message, true, prepared.body).body, quote: rendered.quote, pending: false,
    ...(prepared.pending ? { attachment: { kind: "image" as const, status: "pending" as const, filename: "quoted-image" } } : {}) };
}
export async function prepareNativeAttachment(params: NativePrepareMediaParams): Promise<NativePreparedMedia> {
  try { return await prepareNativeAttachmentInput(params); }
  catch (error) {
    params.signal?.throwIfAborted(); params.assertCurrent?.();
    if (!(error instanceof NativeApiError) || !terminalIdentityErrors.has(error.code)) throw error;
    if (params.message.type === 49 && params.message.appMessageType === 57) return {
      body: `${(params.message.body ?? "").slice(0, 16384)}\n[Quoted message unavailable: stored source identity could not be verified]`,
      quote: { state: "unavailable" }, pending: false, retryable: false, reason: error.code,
    };
    const attachment: WeChatAttachment = { kind: kind(params.message.type) ?? "file", filename: "attachment", status: "unavailable" };
    return { body: renderAttachmentBody("", attachment), attachment, pending: false, retryable: false, reason: error.code };
  }
}
async function prepareNativeAttachmentInput(params: NativePrepareMediaParams): Promise<NativePreparedMedia> {
  const { client, account, message, core } = params; const attachmentKind = kind(message.type);
  const guard = () => { params.signal?.throwIfAborted(); params.assertCurrent?.(); }; guard();
  if (nativeRecallInfo(message)) return { body: "[Message recalled]", pending: false, retryable: false, reason: "message_recalled" };
  const authorize = async () => { guard(); await params.beforeMediaAccess?.(); guard(); };
  if (message.type === 49 && message.appMessageType === 57) return prepareNativeQuote(params);
  if (message.type === 49 && [1, 74, 87].includes(message.appMessageType ?? 0)) return { body: message.body?.slice(0, 32768) || "[Shared text unavailable]", pending: false };
  if (message.type === 49 && message.appMessageType === 19) return { body: message.body?.slice(0, 65536) || "[Forwarded chat history unavailable]", pending: false };
  if (message.type === 49 && [3, 4, 5, 51].includes(message.appMessageType ?? 0) && message.link) return prepareNativeLink(params);
  if (message.type === 49 && message.appMessageType != null && message.appMessageType !== 6) return { body: `[Unsupported WeChat app message subtype ${message.appMessageType}]`, pending: false, retryable: false };
  if (!attachmentKind) return { body: message.type === 1 ? message.body ?? "" : `[Unsupported WeChat message type ${message.type}]`, pending: false };
  const accountId = account.native!.accountId; const maxBytes = Math.min(account.mediaMaxMb * 1024 * 1024,
    attachmentKind === "file" ? Math.max(NATIVE_MAX_SOURCE_BYTES, 128 * 1024 * 1024) : 128 * 1024 * 1024);
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
  if (!asset && initialCandidate?.jobId && initialCandidate.state !== "recalled") {
    await authorize();
    job = await incomingMediaJobStatus(client, accountId, initialCandidate.jobId, initialCandidate.id, params.signal, guard);
    guard();
    if (!job) return { body: `[${attachmentKind} pending: retrieval is unavailable]`, pending: true, jobId: initialCandidate.jobId, retryable: true };
    assets = await scopedAssets(); asset = select(assets, message.type, quality);
    if (!asset && transferCandidate()?.state === "recalled") return { body: "[Message recalled]", pending: false, retryable: false, reason: "message_recalled" };
    if (!asset && ["queued", "running"].includes(job.state)) return { body: `[${attachmentKind} pending]`, pending: true, jobId: job.id, retryable: true };
  }
  if (!asset && !job && params.download !== false && !["expired", "unsupported", "recalled"].includes(transferCandidate()?.state ?? "")) {
    await authorize();
    try {
      job = await client.transferMedia(accountId, { messageId: message.id, assetId: null, quality: transferQuality });
      guard();
      job = await waitNativeMediaStatus(job, params.waitMs ?? 20000, params.signal, guard,
        (known, pollSignal) => incomingMediaJobStatus(client, accountId, known.id, initialCandidate?.id, pollSignal, guard));
      guard();
    } catch (error) {
      guard();
      if (!(error instanceof NativeApiError) || terminalIdentityErrors.has(error.code)) throw error;
      if (error.retryable) return { body: `[${attachmentKind} pending: retrieval is unavailable]`, pending: true, jobId: job?.id, retryable: true };
      transferError = error;
    }
    // Refresh even while queued: a cached copy may have arrived during the wait.
    // Identity/integrity errors must propagate rather than becoming pending.
    assets = await scopedAssets(); asset = select(assets, message.type, quality);
    if (!asset && transferCandidate()?.state === "recalled") return { body: "[Message recalled]", pending: false, retryable: false, reason: "message_recalled" };
    if (!asset && job && ["queued", "running"].includes(job.state)) return { body: `[${attachmentKind} pending]`, pending: true, jobId: job.id, retryable: true };
  }
  const candidate = transferCandidate();
  const filename = basename(asset?.filename ?? candidate?.filename ?? `message-${createHash("sha256").update(message.id).digest("hex").slice(0, 12)}.bin`).replace(/[\r\n\0]/g, "_");
  if (!asset) {
    if (candidate?.state === "recalled" || transferError?.code === "message_recalled") return { body: "[Message recalled]", pending: false, retryable: false, reason: "message_recalled" };
    const state = candidate?.state === "expired" ? "expired" : candidate?.state === "unsupported" ? "unsupported" : "unavailable";
    const attachment: WeChatAttachment = { kind: attachmentKind, filename, status: state, expiresAt: candidate?.expiresAt ?? undefined };
    return { body: renderAttachmentBody("", attachment), attachment, pending: false, jobId: job?.id,
      retryable: state === "expired" || state === "unsupported" ? false : transferError ? false : job?.retryable ?? params.download === false,
      reason: transferError?.code ?? (job?.reason && /^[a-z0-9_]{1,80}$/.test(job.reason) ? job.reason : undefined) };
  }
  if (asset.byteLength != null && asset.byteLength > maxBytes) { const attachment: WeChatAttachment = { kind: attachmentKind, filename, status: "too_large" }; return { body: renderAttachmentBody("", attachment), attachment, pending: false }; }
  await authorize();
  if (attachmentKind === "file" && asset.byteLength != null && asset.byteLength > 128 * 1024 * 1024) {
    const content = await client.mediaContentStream(accountId, asset.id, { maxBytes, signal: params.signal });
    const mime = content.contentType.split(";", 1)[0].trim().toLowerCase();
    if (!/^[a-z0-9.+-]+\/[a-z0-9.+-]+$/.test(mime)) { await content.cancel(); throw Error("Native attachment MIME type is unsupported"); }
    let saved;
    try {
      saved = await saveVerifiedManagedAttachmentStream({ stream: content.stream, mime, filename, maxBytes, expectedBytes: asset.byteLength,
        expectedSha256: asset.sha256 ?? "", assertCurrent: guard, saveMediaStream: params.saveMediaStream ?? saveMediaStream });
    } finally { await content.cancel(); }
    await authorize();
    const attachment: WeChatAttachment = { kind: "file", filename, status: "ready", path: saved.path, mime };
    return { body: renderAttachmentBody("", attachment), attachment, mediaMime: mime, pending: false, jobId: job?.id, retryable: false, variant: asset.actualVariant ?? asset.variant,
      suppliedAssets: [{ id: asset.id, contextRole: "trigger", role: "file_reference", kind: "file", path: saved.path, mime, sha256: saved.sha256, byteLength: saved.size, runtimeMedia: false,
        sourceSha256: saved.sha256, sourceByteLength: saved.size, sourceMime: mime, sourcePath: saved.path }] };
  }
  const content = await client.mediaContent(accountId, asset.id, { maxBytes, signal: params.signal });
  guard();
  const mime = content.contentType.split(";", 1)[0].trim().toLowerCase();
  if (!/^[a-z0-9.+-]+\/[a-z0-9.+-]+$/.test(mime)) throw Error("Native attachment MIME type is unsupported");
  const hash = createHash("sha256").update(content.bytes).digest("hex");
  if (!asset.sha256 || hash !== asset.sha256 || (asset.byteLength != null && asset.byteLength !== content.bytes.length)) throw new Error("Native attachment integrity did not verify");
  await authorize();
  const saved = await saveManagedAttachmentBytes({ bytes: content.bytes, mime, filename, maxBytes, saveMediaBuffer: core.channel.media.saveMediaBuffer });
  const actual = asset.actualVariant ?? asset.variant;
  const attachment: WeChatAttachment = { kind: attachmentKind, filename, status: "ready", path: saved.path, mime: mime,
    quality: actual === "original" ? "full" : actual === "standard" ? "standard" : actual === "thumbnail" ? "thumbnail" : undefined };
  let suppliedPath = saved.path, suppliedMime = mime, suppliedHash = hash, suppliedLength = content.bytes.length;
  let body = renderAttachmentBody("", attachment);
  if (attachmentKind === "image" && (params.cfg || mime === "image/x-wxgf")) {
    let imageBytes = Buffer.from(content.bytes), imageMime = mime;
    const wxgf = mime === "image/x-wxgf";
    if (wxgf) {
      const preview = asset.preview;
      if (!preview || preview.sourceSha256 !== hash || !/^[a-f0-9]{64}$/.test(preview.sha256)
        || !Number.isInteger(preview.byteLength) || preview.byteLength <= 0 || preview.byteLength > 16 * 1024 * 1024
        || !["image/jpeg", "image/png"].includes(preview.contentType)) throw Error("Native WXGF preview metadata did not verify");
      await authorize();
      const derived = await client.mediaContent(accountId, asset.id, { maxBytes: preview.byteLength, representation: "preview", signal: params.signal });
      guard();
      const derivedMime = derived.contentType.split(";", 1)[0].trim().toLowerCase();
      if (derived.sourceSha256 !== hash || derivedMime !== preview.contentType || derived.bytes.length !== preview.byteLength
        || createHash("sha256").update(derived.bytes).digest("hex") !== preview.sha256) throw Error("Native WXGF preview integrity did not verify");
      imageBytes = Buffer.from(derived.bytes); imageMime = derivedMime;
    }
    const representation = await prepareImageRepresentation(imageBytes, imageMime, params.cfg);
    if (wxgf || representation.preview) {
      const extension = representation.mime === "image/png" ? "png" : "jpg";
      const normalized = await saveManagedAttachment({ data: representation.bytes.toString("base64"), mime: representation.mime,
        filename: `${filename}.model.${extension}`, maxBytes: 5 * 1024 * 1024, saveMediaBuffer: core.channel.media.saveMediaBuffer });
      suppliedPath = normalized.path; suppliedMime = representation.mime; suppliedHash = normalized.sha256; suppliedLength = normalized.size;
      body += wxgf ? `\n[Model image: WXGF still preview; original retained separately; animation and transparency are not represented]`
        : `\n[Model image: prepared still preview; original retained separately]`;
    }
  }
  guard();
  return { body, attachment, jobId: job?.id, retryable: false, variant: actual, suppliedAssets: [{ id: asset.id, sha256: suppliedHash, byteLength: suppliedLength, mime: suppliedMime, path: suppliedPath,
    role: attachmentKind === "image" ? "model_image" : "file_reference", kind: attachmentKind, runtimeMedia: attachmentKind === "image" || attachmentKind === "video", contextRole: "trigger",
    sourceSha256: hash, sourceByteLength: content.bytes.length, sourceMime: mime, sourcePath: saved.path }],
    mediaPath: attachmentKind === "image" || attachmentKind === "video" ? suppliedPath : undefined, mediaMime: suppliedMime, pending: false };
}

export type NativeMediaSendOptions = { forceDocument?: boolean; videoQuality?: "normal" | "original" };
export function validateNativeMediaSendOptions(options: NativeMediaSendOptions, audioAsVoice = false): void {
  if (options.forceDocument != null && typeof options.forceDocument !== "boolean") throw new Error("Invalid WeChat document option");
  if (options.videoQuality != null && !["normal", "original"].includes(options.videoQuality)) throw new Error("Invalid WeChat video quality");
  if (options.forceDocument && (audioAsVoice || options.videoQuality != null)) throw new Error("Document sending cannot also request voice or video quality");
}
function outboundMediaKind(mime: string, audioAsVoice: boolean, options: NativeMediaSendOptions): MediaSendRequest["kind"] {
  if (options.forceDocument) return "file";
  if (audioAsVoice && !mime.startsWith("audio/")) throw new Error("Voice sending requires an explicit audio attachment");
  const selected = audioAsVoice ? "voice" : mime.startsWith("image/") ? "image" : mime.startsWith("video/") ? "video" : "file";
  if (options.videoQuality != null && selected !== "video") throw new Error("Video quality requires a video attachment");
  return selected;
}
function outboundSourceLimit(account: ResolvedWeChatAccount, selectedKind: MediaSendRequest["kind"]): number {
  return Math.min(account.mediaMaxMb * 1024 * 1024, selectedKind === "file" ? NATIVE_MAX_SOURCE_BYTES : selectedKind === "video" ? NATIVE_MAX_VIDEO_SOURCE_BYTES : NATIVE_MAX_ORDINARY_SOURCE_BYTES);
}
export async function sendNativeAttachment(client: NativeWeChatClient, account: ResolvedWeChatAccount, threadId: string, mediaUrl: string, audioAsVoice = false, key: string = randomUUID(), beforeSend?: () => Promise<void>, options: NativeMediaSendOptions = {}): Promise<NativeOutboundDelivery> {
  validateNativeMediaSendOptions(options, audioAsVoice);
  await beforeSend?.();
  let filename: string, mime: string, selectedKind: MediaSendRequest["kind"], sourceBytes: number, source: Awaited<ReturnType<NativeWeChatClient["upload"]>>;
  let temporary: string | undefined;
  const accountId = account.native!.accountId;
  try {
    let blob: Blob;
    if (/^https?:/.test(mediaUrl)) {
      const response = await fetch(mediaUrl, { redirect: "error", signal: AbortSignal.timeout(60 * 60000) }); if (!response.ok) throw new Error(`Native outbound source unavailable (${response.status})`);
      filename = basename(new URL(mediaUrl).pathname) || "attachment"; mime = response.headers.get("content-type")?.split(";")[0].trim().toLowerCase() ?? "application/octet-stream";
      try { selectedKind = outboundMediaKind(mime, audioAsVoice, options); } catch (error) { await response.body?.cancel(); throw error; }
      const max = outboundSourceLimit(account, selectedKind);
      const length = response.headers.get("content-length"); if (length && (!/^\d+$/.test(length) || Number(length) > max)) { await response.body?.cancel(); throw new Error("Native outbound source exceeds configured size"); }
      const reader = response.body?.getReader(); if (!reader) throw new Error("Native outbound source has no bytes");
      let path: string, file: Awaited<ReturnType<typeof open>> | undefined, size = 0;
      try {
        temporary = await mkdtemp(join(tmpdir(), "wechat-outbound-")); path = join(temporary, "source"); file = await open(path, "wx", 0o600);
        while (true) { const { value, done } = await reader.read(); if (done) break; size += value.byteLength; if (size > max) throw new Error("Native outbound source exceeds configured size"); await file.writeFile(value); }
        if (!size || length && size !== Number(length)) throw new Error("Native outbound source length mismatch");
      } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); await file?.close(); }
      blob = await openAsBlob(path, { type: mime });
    } else {
      const path = mediaUrl.startsWith("file:") ? new URL(mediaUrl) : mediaUrl;
      filename = basename(path instanceof URL ? path.pathname : path); const extension = extname(filename).toLowerCase(); mime = [".png", ".jpg", ".jpeg", ".gif", ".webp"].includes(extension) ? `image/${extension === ".jpg" ? "jpeg" : extension.slice(1)}` : extension === ".mp4" ? "video/mp4" : [".wav", ".mp3", ".ogg", ".m4a", ".flac", ".aac"].includes(extension) ? `audio/${extension.slice(1)}` : "application/octet-stream";
      selectedKind = outboundMediaKind(mime, audioAsVoice, options); const max = outboundSourceLimit(account, selectedKind), file = await open(path, "r");
      let size: number;
      try { const stat = await file.stat(); if (!stat.isFile() || !stat.size || stat.size > max) throw new Error("Native outbound source must be a bounded regular file"); size = stat.size; } finally { await file.close(); }
      blob = await openAsBlob(path, { type: mime });
      if (blob.size !== size) throw new Error("Native outbound source changed before staging");
    }
    sourceBytes = blob.size;
    if (sourceBytes > outboundSourceLimit(account, selectedKind)) throw new Error("Native outbound source exceeds selected media kind size");
    await beforeSend?.();
    source = await client.upload(accountId, blob, filename, mime);
  } finally { if (temporary) await rm(temporary, { recursive: true, force: true }); }
  const input: MediaSendRequest = { threadId, kind: selectedKind, sourceId: source.id, standardSourceId: null, thumbnailSourceId: null, playSeconds: null, durationMs: null,
    ...(selectedKind === "video" ? { videoQuality: options.videoQuality ?? "normal" } : {}) };
  await beforeSend?.();
  const waitMs = audioAsVoice ? 25 * 60000 : selectedKind === "video" || selectedKind === "file" && sourceBytes > NATIVE_MAX_ORDINARY_SOURCE_BYTES ? 60 * 60000 : 65000;
  const submitted = await client.sendMedia(accountId, input, key);
  let job: MediaJob;
  try { job = await waitNativeMediaJob(client, accountId, submitted, waitMs); }
  catch {
    // Keep only the known intent identity: transport errors may contain URLs,
    // response content or credentials, and cannot authorize another submission.
    throw new NativeMediaJobInspectionError(submitted, accountId, threadId, key, "status_unavailable");
  }
  if (successfulSendJobs.has(job.state)) job = await waitNativeMediaSendReceipts(client, accountId, threadId, job, audioAsVoice);
  if (!successfulSendJobs.has(job.state)) throw new NativeMediaJobInspectionError(job, accountId, threadId, key, "job_not_completed");
  const result = mediaSendResult(job);
  const chunks = Array.isArray(result.chunks) ? result.chunks.filter(chunk => chunk && typeof chunk === "object") as Array<{ state?: string; serverMessageId?: string }> : [];
  const serverIds = audioAsVoice ? chunks.filter(chunk => chunk.state === "accepted" && typeof chunk.serverMessageId === "string").map(chunk => chunk.serverMessageId!) : job.serverMessageId ? [job.serverMessageId] : undefined;
  return nativeOutboundDelivery(client, accountId, threadId, mediaSendReceiptIds(job, audioAsVoice), { jobId: job.id, state: job.state, serverIds });
}
