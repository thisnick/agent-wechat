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

export async function attachmentPathExists(filePath: string | undefined): Promise<boolean> {
  if (!filePath) return false;
  try {
    return (await fs.stat(filePath)).isFile();
  } catch {
    return false;
  }
}
