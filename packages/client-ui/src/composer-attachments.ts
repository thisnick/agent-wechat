import {
  NATIVE_MAX_ORDINARY_SOURCE_BYTES,
  NATIVE_MAX_SOURCE_BYTES,
  NATIVE_MAX_VIDEO_SOURCE_BYTES,
} from "@agent-wechat/shared";

export type AttachmentKind = "image" | "video" | "file";
export const COMPOSER_MAX_SOURCE_BYTES = NATIVE_MAX_SOURCE_BYTES;
export const MAX_COMPOSER_ATTACHMENTS = 10;
export type AttachmentLimits = Record<AttachmentKind, number>;

/** Source limits differ from the smaller prepared playback/image variants. */
export function attachmentSourceLimit(kind: AttachmentKind): number {
  return kind === "file"
    ? NATIVE_MAX_SOURCE_BYTES
    : kind === "video"
    ? NATIVE_MAX_VIDEO_SOURCE_BYTES
    : NATIVE_MAX_ORDINARY_SOURCE_BYTES;
}

/** Older hubs without per-kind limits retain the conservative ordinary limit. */
export function accountAttachmentLimits(
  capabilities: Record<string, unknown>,
): AttachmentLimits {
  const advertised = capabilities.mediaSendMaxBytes as
    | Partial<AttachmentLimits>
    | undefined;
  const positive = (value: unknown): value is number =>
    Number.isSafeInteger(value) && Number(value) > 0;
  return Object.fromEntries(
    (["image", "video", "file"] as const).map((kind) => {
      const limit = advertised?.[kind];
      return [
        kind,
        Math.min(
          attachmentSourceLimit(kind),
          positive(limit) ? limit : NATIVE_MAX_ORDINARY_SOURCE_BYTES,
          positive(capabilities.mediaUploadMaxBytes)
            ? capabilities.mediaUploadMaxBytes
            : NATIVE_MAX_ORDINARY_SOURCE_BYTES,
        ),
      ];
    }),
  ) as AttachmentLimits;
}

export function validateAttachmentSource(
  file: { name: string; size: number },
  kind: AttachmentKind,
  limits?: AttachmentLimits,
) {
  if (!Number.isSafeInteger(file.size) || file.size <= 0)
    throw new Error(`${file.name || "This file"}: choose a nonempty file.`);
  const limit = Math.min(
    attachmentSourceLimit(kind),
    limits?.[kind] ?? attachmentSourceLimit(kind),
  );
  if (file.size > limit) {
    const bound =
      limit === NATIVE_MAX_ORDINARY_SOURCE_BYTES
        ? "just under 25 MiB"
        : limit % (1024 * 1024) === 0
        ? `${limit / 1024 / 1024} MiB`
        : `${limit.toLocaleString("en-US")} bytes`;
    throw new Error(
      `${
        file.name || "This file"
      }: this hub's ${kind} source limit is ${bound}. Choose a smaller file.`,
    );
  }
}

/** Select supported send formats; the hub still validates the actual bytes. */
export function attachmentKind(
  file: Pick<File, "name" | "type">,
): AttachmentKind {
  const mime = file.type.toLowerCase().split(";", 1)[0].trim();
  if (["image/jpeg", "image/png", "image/gif", "image/webp"].includes(mime))
    return "image";
  if (
    [
      "video/mp4",
      "video/quicktime",
      "video/3gpp",
      "video/3gpp2",
      "video/mj2",
    ].includes(mime)
  )
    return "video";
  if (mime && mime !== "application/octet-stream") return "file";
  const extension = file.name.split(".").at(-1)?.toLowerCase();
  if (["jpg", "jpeg", "png", "gif", "webp"].includes(extension ?? ""))
    return "image";
  return ["mov", "mp4", "3gp", "3g2", "mj2"].includes(extension ?? "")
    ? "video"
    : "file";
}

export type ReviewedAttachment = { file: File; kind: AttachmentKind };
export function reviewAttachments(
  files: readonly File[],
  supported: readonly string[],
  limits?: AttachmentLimits,
): ReviewedAttachment[] {
  if (!files.length) throw new Error("Choose a file to attach.");
  if (files.length > MAX_COMPOSER_ATTACHMENTS)
    throw new Error("Choose up to 10 files at a time.");
  return files.map((file) => {
    const kind = attachmentKind(file);
    if (!supported.includes(kind))
      throw new Error(`This hub doesn't support ${kind} attachments.`);
    validateAttachmentSource(file, kind, limits);
    return { file, kind };
  });
}

export function hasTransferredFiles(data: DataTransfer | null): boolean {
  return (
    !!data &&
    (Array.from(data.types).includes("Files") ||
      data.files.length > 0 ||
      Array.from(data.items).some((item) => item.kind === "file"))
  );
}
export function transferredFiles(data: DataTransfer): File[] {
  // Browsers commonly expose the same file in both lists. Never append both.
  if (data.files.length) return Array.from(data.files);
  return Array.from(data.items)
    .filter((item) => item.kind === "file")
    .map((item) => item.getAsFile())
    .filter((file): file is File => file !== null);
}

/** Capture files before either editor handles text; ordinary text stays native. */
export function bindComposerAttachments(
  root: HTMLElement,
  receive: (files: File[]) => void,
  hover: (value: boolean) => void,
): () => void {
  let depth = 0;
  const capture = { capture: true };
  const reset = () => {
    depth = 0;
    hover(false);
  };
  const paste = (event: ClipboardEvent) => {
    if (!hasTransferredFiles(event.clipboardData)) return;
    event.preventDefault();
    event.stopPropagation();
    receive(transferredFiles(event.clipboardData!));
  };
  const enter = (event: DragEvent) => {
    if (!hasTransferredFiles(event.dataTransfer)) return;
    event.preventDefault();
    depth++;
    hover(true);
  };
  const over = (event: DragEvent) => {
    if (!hasTransferredFiles(event.dataTransfer)) return;
    event.preventDefault();
    event.dataTransfer!.dropEffect = "copy";
  };
  const leave = (event: DragEvent) => {
    if (depth && --depth === 0) hover(false);
  };
  const drop = (event: DragEvent) => {
    reset();
    if (!hasTransferredFiles(event.dataTransfer)) return;
    event.preventDefault();
    event.stopPropagation();
    receive(transferredFiles(event.dataTransfer!));
  };
  root.addEventListener("paste", paste, capture);
  root.addEventListener("dragenter", enter, capture);
  root.addEventListener("dragover", over, capture);
  root.addEventListener("dragleave", leave, capture);
  root.addEventListener("drop", drop, capture);
  window.addEventListener("dragend", reset);
  window.addEventListener("blur", reset);
  return () => {
    root.removeEventListener("paste", paste, capture);
    root.removeEventListener("dragenter", enter, capture);
    root.removeEventListener("dragover", over, capture);
    root.removeEventListener("dragleave", leave, capture);
    root.removeEventListener("drop", drop, capture);
    window.removeEventListener("dragend", reset);
    window.removeEventListener("blur", reset);
  };
}
