import type { MediaResult } from "@agent-wechat/shared";

export type AttachmentKind = "image" | "file" | "audio" | "video";
export type AttachmentStatus = "ready" | "pending" | "unavailable" | "unsupported" | "too_large" | "error";

export type WeChatAttachment = {
  kind: AttachmentKind;
  status: AttachmentStatus;
  filename: string;
  mime?: string;
  path?: string;
  error?: string;
};

export type MessageWithAttachment = {
  rawBody: string;
  attachment?: WeChatAttachment;
  mediaPath?: string;
  mediaMime?: string;
  hasMedia: boolean;
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

export function attachmentKindForResult(
  result: MediaResult,
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

export function renderAttachmentBody(
  originalBody: string,
  attachment: WeChatAttachment | undefined,
  exposeNonVisualPath = true,
): string {
  if (!attachment) return originalBody;
  const name = safeDisplayName(attachment.filename);
  if (attachment.status !== "ready" || !attachment.path) {
    const label = attachment.kind === "file" ? "File" :
      attachment.kind === "image" ? "Image" :
        attachment.kind === "audio" ? "Voice message" : "Video";
    return appendReference(originalBody, `[${label} unavailable: ${name} (${attachment.status})]`);
  }
  if (attachment.kind === "file") {
    const prefix = originalBody.trim() === name ? "" : originalBody;
    return appendReference(prefix, `[File: ${name}]\n[Local file: ${attachment.path}]`);
  }
  if (exposeNonVisualPath) {
    if (attachment.kind === "audio") {
      return appendReference(originalBody || "<media:audio>", `[Voice message file: ${attachment.path}]`);
    }
    if (attachment.kind === "video") {
      return appendReference(originalBody || "<media:video>", `[Video file: ${attachment.path}]`);
    }
  }
  if (!originalBody) return attachment.kind === "image" ? "<media:image>" : originalBody;
  return originalBody;
}

export function applyLiveAttachment<T extends MessageWithAttachment>(
  message: T,
  originalBody: string,
  attachment: WeChatAttachment | undefined,
): T {
  const attachable = attachment?.status === "ready" && attachment.path && attachment.kind !== "file";
  return {
    ...message,
    rawBody: renderAttachmentBody(originalBody, attachment),
    attachment,
    mediaPath: attachable ? attachment.path : undefined,
    mediaMime: attachable ? attachment.mime : undefined,
    hasMedia: attachment !== undefined,
  };
}

/**
 * Catch-up has one model media slot. Keep every file path in text and select
 * only the latest ready image for that slot. Earlier images and audio/video
 * remain available through explicit local-path references.
 */
export function applyCatchupAttachmentPolicy<T extends MessageWithAttachment>(messages: T[]): T[] {
  let latestImage = -1;
  for (let index = messages.length - 1; index >= 0; index--) {
    const attachment = messages[index].attachment;
    if (attachment?.kind === "image" && attachment.status === "ready" && attachment.path) {
      latestImage = index;
      break;
    }
  }

  return messages.map((message, index) => {
    const attachment = message.attachment;
    if (!attachment) return { ...message, mediaPath: undefined, mediaMime: undefined };
    const selectImage = index === latestImage;
    let rawBody = renderAttachmentBody(message.rawBody, undefined);
    if (attachment.kind === "file") {
      // File paths were already rendered when the message was prepared.
      rawBody = message.rawBody;
    } else if (!selectImage && attachment.status === "ready" && attachment.path) {
      const label = attachment.kind === "image" ? "Image file" :
        attachment.kind === "audio" ? "Voice message file" : "Video file";
      const reference = `[${label}: ${attachment.path}]`;
      rawBody = message.rawBody.includes(reference)
        ? message.rawBody
        : appendReference(message.rawBody, reference);
    }
    return {
      ...message,
      rawBody,
      mediaPath: selectImage ? attachment.path : undefined,
      mediaMime: selectImage ? attachment.mime : undefined,
      hasMedia: attachment !== undefined,
    };
  });
}

export function buildMediaSegments<T extends MessageWithAttachment>(messages: T[]): T[][] {
  const segments: T[][] = [];
  let batch: T[] = [];
  let hasAttachedMedia = false;
  for (const message of messages) {
    if (message.mediaPath && hasAttachedMedia) {
      segments.push(batch);
      batch = [message];
      hasAttachedMedia = true;
    } else {
      batch.push(message);
      if (message.mediaPath) hasAttachedMedia = true;
    }
  }
  if (batch.length > 0) segments.push(batch);
  return segments;
}
