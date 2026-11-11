export type AttachmentKind = "image" | "file" | "audio" | "video";
export type AttachmentStatus = "ready" | "pending" | "expired" | "unavailable" | "unsupported" | "too_large" | "error";

export type WeChatAttachment = {
  kind: AttachmentKind;
  status: AttachmentStatus;
  filename: string;
  mime?: string;
  path?: string;
  quality?: "full" | "standard" | "thumbnail";
  error?: string;
  expiresAt?: number;
};

const MIME_BY_FORMAT: Record<string, string> = {
  jpeg: "image/jpeg",
  jpg: "image/jpeg",
  png: "image/png",
  gif: "image/gif",
  webp: "image/webp",
  mp3: "audio/mpeg",
  silk: "audio/silk",
  mp4: "video/mp4",
  pdf: "application/pdf",
  doc: "application/msword",
  docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  xls: "application/vnd.ms-excel",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  ppt: "application/vnd.ms-powerpoint",
  pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  zip: "application/zip",
  txt: "text/plain",
};

export function attachmentKindForMessageType(baseType: number): AttachmentKind | undefined {
  if (baseType === 3) return "image";
  if (baseType === 34) return "audio";
  if (baseType === 43) return "video";
  if (baseType === 49) return "file";
  return undefined;
}

export function mediaRequestQuality(baseType: number): "best" | "full" {
  return baseType === 3 ? "best" : "full";
}

export function attachmentKindForResult(
  result: { type: string },
  fallback: AttachmentKind,
): AttachmentKind {
  if (result.type === "image" || result.type === "emoji") return "image";
  if (result.type === "voice") return "audio";
  if (result.type === "video") return "video";
  if (result.type === "file") return "file";
  return fallback;
}

export function mediaMime(format: string, kind: AttachmentKind): string {
  const normalized = format.toLowerCase().replace(/^\./, "");
  return MIME_BY_FORMAT[normalized] ?? (
    kind === "image" ? "image/*" :
      kind === "audio" ? "audio/*" :
        kind === "video" ? "video/*" :
          "application/octet-stream"
  );
}

function appendReference(body: string, reference: string): string {
  return body ? `${body}\n${reference}` : reference;
}

function safeDisplayName(name: string): string {
  return name.replace(/[\r\n\0]/g, " ").trim() || "attachment";
}

export function attachmentFallbackFilename(
  kind: AttachmentKind,
  localId: number,
  originalBody = "",
): string {
  if (kind === "file" && originalBody && !originalBody.trimStart().startsWith("<")) {
    return safeDisplayName(originalBody);
  }
  const extension = kind === "image" ? "jpg" : kind === "audio" ? "mp3" : kind === "video" ? "mp4" : "bin";
  return `message-${localId}.${extension}`;
}

export function attachmentSourceBody(originalBody: string, kind: AttachmentKind | undefined): string {
  if (!kind || kind === "file") return originalBody;
  return originalBody.trimStart().startsWith("<") ? "" : originalBody;
}

/**
 * Text that stands in for an attachment in the agent's message. Ready files are
 * passed to OpenClaw separately as media facts, so this text names them but never
 * includes a local path: a gateway path is meaningless inside a sandboxed agent.
 */
export function renderAttachmentBody(
  originalBody: string,
  attachment: WeChatAttachment | undefined,
): string {
  if (!attachment) return originalBody;
  const name = safeDisplayName(attachment.filename);
  if (attachment.status !== "ready" || !attachment.path) {
    const label = attachment.kind === "file" ? "File" :
      attachment.kind === "image" ? "Image" :
        attachment.kind === "audio" ? "Voice message" : "Video";
    if (attachment.status === "pending") {
      return appendReference(originalBody, `[${label} pending: ${name}]`);
    }
    if (attachment.status === "expired") {
      return appendReference(originalBody, `[${label} expired: ${name}; no cached copy available. Ask the sender to resend.]`);
    }
    return appendReference(originalBody, `[${label} unavailable: ${name} (${attachment.status})]`);
  }
  if (attachment.kind === "file") {
    const prefix = originalBody.trim() === name ? "" : originalBody;
    return appendReference(prefix, `[File: ${name}]`);
  }
  const body = originalBody || `<media:${attachment.kind}>`;
  if (attachment.kind === "image" && attachment.quality === "thumbnail") {
    return appendReference(body, "[Image preview only: thumbnail quality]");
  }
  return body;
}
