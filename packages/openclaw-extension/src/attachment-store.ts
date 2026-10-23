import { createHash } from "node:crypto";
import fs from "node:fs/promises";

export class AttachmentTooLargeError extends Error {}

function decodedSize(base64: string): number {
  if (!base64 || base64.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(base64)) {
    throw new Error("Invalid attachment encoding");
  }
  const padding = base64.endsWith("==") ? 2 : base64.endsWith("=") ? 1 : 0;
  return base64.length / 4 * 3 - padding;
}

type SaveMediaBuffer = (
  buffer: Buffer,
  contentType: string | undefined,
  subdir: string,
  maxBytes: number,
  originalFilename?: string,
) => Promise<{ path: string; size?: number }>;

export type SaveAttachmentParams = {
  data: string;
  mime: string;
  filename: string;
  maxBytes: number;
  saveMediaBuffer: SaveMediaBuffer;
};

export async function saveManagedAttachment(params: SaveAttachmentParams): Promise<{
  path: string;
  size: number;
  sha256: string;
  filename: string;
}> {
  const size = decodedSize(params.data);
  if (size > params.maxBytes) {
    throw new AttachmentTooLargeError(`Attachment exceeds configured ${params.maxBytes} byte limit`);
  }
  const buffer = Buffer.from(params.data, "base64");
  if (buffer.byteLength !== size) throw new Error("Attachment decoding length mismatch");
  return saveManagedAttachmentBytes({ ...params, bytes: buffer });
}

export async function saveManagedAttachmentBytes(params: Omit<SaveAttachmentParams, "data"> & { bytes: Uint8Array }): Promise<{ path: string; size: number; sha256: string; filename: string }> {
  const size = params.bytes.byteLength;
  if (size > params.maxBytes) throw new AttachmentTooLargeError(`Attachment exceeds configured ${params.maxBytes} byte limit`);
  const buffer = Buffer.from(params.bytes.buffer, params.bytes.byteOffset, size);
  const sha256 = createHash("sha256").update(buffer).digest("hex");
  const saved = await params.saveMediaBuffer(
    buffer,
    params.mime,
    "inbound",
    params.maxBytes,
    params.filename,
  );
  return { path: saved.path, size, sha256, filename: params.filename };
}

type SaveMediaStream = (stream: AsyncIterable<unknown>, contentType: string, subdir: string, maxBytes: number, filename: string, detectionPath: undefined,
  options: { assertCommitAllowed: () => void; durable: boolean }) => Promise<{ path: string; size?: number }>;

export async function saveVerifiedManagedAttachmentStream(params: { stream: AsyncIterable<Uint8Array>; mime: string; filename: string; maxBytes: number;
  expectedBytes: number; expectedSha256: string; assertCurrent: () => void; saveMediaStream: SaveMediaStream }): Promise<{ path: string; size: number; sha256: string; filename: string }> {
  if (!Number.isSafeInteger(params.expectedBytes) || params.expectedBytes < 1 || params.expectedBytes > params.maxBytes || !/^[a-f0-9]{64}$/.test(params.expectedSha256)) throw new Error("Native attachment integrity metadata is invalid");
  let size = 0, finished = false;
  const digest = createHash("sha256");
  const stream = (async function* () {
    for await (const chunk of params.stream) {
      params.assertCurrent(); size += chunk.byteLength;
      if (size > params.maxBytes || size > params.expectedBytes) throw new AttachmentTooLargeError("Native attachment stream exceeds its bound");
      digest.update(chunk); yield chunk;
    }
    params.assertCurrent();
    if (size !== params.expectedBytes || digest.digest("hex") !== params.expectedSha256) throw new Error("Native attachment integrity did not verify");
    finished = true;
  })();
  const saved = await params.saveMediaStream(stream, params.mime, "inbound", params.maxBytes, params.filename, undefined,
    { assertCommitAllowed: params.assertCurrent, durable: true });
  if (!finished || saved.size != null && saved.size !== size) throw new Error("Native attachment stream was not fully saved");
  return { path: saved.path, size, sha256: params.expectedSha256, filename: params.filename };
}

export async function attachmentPathExists(filePath: string | undefined): Promise<boolean> {
  if (!filePath) return false;
  try {
    return (await fs.stat(filePath)).isFile();
  } catch {
    return false;
  }
}
