import { useEffect, useRef, useState, type RefObject } from "react";
import type { Connection } from "./connection.ts";
import type { AttachmentDraft } from "./attachment-drafts.ts";
import { t, useLocale } from "./i18n.ts";
import { Button, Icon } from "./ui.tsx";
import {
  bindComposerAttachments,
  accountAttachmentLimits,
  type ReviewedAttachment,
} from "./composer-attachments.ts";
import { ComposerHelp } from "./composer-help.tsx";

export function AttachmentPicker({
  connection,
  account,
  threadId,
  enabled,
  mediaKinds,
  capabilities,
  composerRef,
  focusMessage,
}: {
  connection: Connection;
  account: string;
  threadId: string | null;
  enabled: boolean;
  mediaKinds: readonly string[];
  capabilities: Record<string, unknown>;
  composerRef: RefObject<HTMLDivElement | null>;
  focusMessage: () => void;
}) {
  useLocale();
  const input = useRef<HTMLInputElement>(null);
  const [error, setError] = useState(""),
    [dragging, setDragging] = useState(false);
  function choose(files: File[]) {
    if (!enabled || !threadId) {
      setError(
        t(
          "Attachments aren't available in this conversation right now.",
          "当前会话暂无法发送附件。",
        ),
      );
      return;
    }
    try {
      connection.attachmentDrafts.add(
        account,
        threadId,
        files,
        mediaKinds,
        accountAttachmentLimits(capabilities),
      );
      setError("");
      focusMessage();
    } catch (error) {
      setError(
        error instanceof Error
          ? error.message
          : t(
              "Couldn't select these files. Choose them again.",
              "无法选择这些文件，请重新选择。",
            ),
      );
    }
  }
  useEffect(() => {
    const root = composerRef.current;
    if (root)
      return bindComposerAttachments(root, choose, (value) =>
        setDragging(value && enabled && !!threadId),
      );
  }, [
    connection,
    account,
    threadId,
    enabled,
    mediaKinds,
    capabilities,
    composerRef,
  ]);
  useEffect(() => {
    if (!enabled) setDragging(false);
  }, [enabled]);
  return (
    <>
      <Button
        icon="attach"
        label={t("Attach file", "添加附件")}
        disabled={!enabled || !threadId}
        onClick={() => input.current?.click()}
      />
      <input
        ref={input}
        className="attachment-input"
        type="file"
        multiple
        aria-label={t("Choose attachment", "选择附件")}
        onChange={(event) => {
          const files = Array.from(event.target.files ?? []);
          event.target.value = "";
          if (files.length) choose(files);
        }}
      />
      {dragging && (
        <span className="composer-drop-hint" role="status">
          <Icon name="attach" /> {t("Drop to attach", "拖放以添加附件")}
        </span>
      )}
      {error && (
        <span className="attachment-error" role="alert">
          {error}
        </span>
      )}
    </>
  );
}

export function AttachmentTray({
  draft,
  remove,
  quality,
}: {
  draft: AttachmentDraft;
  remove: (id: string) => void;
  quality: (original: boolean) => void;
}) {
  useLocale();
  if (!draft.files.length) return null;
  return (
    <section
      className="composer-attachments"
      aria-label={t("Staged attachments", "待发送附件")}
    >
      <div className="composer-attachments-head">
        <span>
          {t(
            `${draft.files.length} ${draft.files.length === 1 ? "attachment" : "attachments"}`,
            `${draft.files.length} 个附件`,
          )}
        </span>
      </div>
      <ul className="composer-attachment-list">
        {draft.files.map((item) => (
          <li key={item.id} className="composer-attachment">
            <AttachmentPreview item={item} />
            <Button
              icon="close"
              label={t(`Remove ${item.file.name}`, `移除 ${item.file.name}`)}
              disabled={!!draft.submission}
              onClick={() => remove(item.id)}
            />
          </li>
        ))}
      </ul>
      {draft.files.some((item) => item.kind === "video") && (
        <div className="composer-original-video">
          <label>
            <input
              type="checkbox"
              checked={draft.videoQuality === "original"}
              disabled={!!draft.submission}
              onChange={(event) => quality(event.target.checked)}
            />
            {t("Original media quality", "原画质发送")}
          </label>
          <ComposerHelp
            label={t("About original media quality", "关于原画质发送")}
          >
            {t(
              "Include the unchanged video file alongside the playback copy.",
              "随播放副本一起附上未经压缩的原始视频文件。",
            )}
          </ComposerHelp>
        </div>
      )}
    </section>
  );
}
function AttachmentPreview({
  item: { file, kind },
}: {
  item: ReviewedAttachment;
}) {
  useLocale();
  const [url, setUrl] = useState(""),
    [previewFailed, setPreviewFailed] = useState(false);
  useEffect(() => {
    setPreviewFailed(false);
    if (kind === "file") return;
    const object = URL.createObjectURL(file);
    setUrl(object);
    return () => URL.revokeObjectURL(object);
  }, [file, kind]);
  const size =
    file.size < 1024 * 1024
      ? `${Math.ceil(file.size / 1024)} KB`
      : `${(file.size / 1024 / 1024).toFixed(1)} MB`;
  return (
    <>
      {url && kind === "image" && (
        <img
          className="attachment-review-preview"
          src={url}
          alt={t("Staged image", "待发送图片")}
        />
      )}
      {url && kind === "video" && !previewFailed && (
        <video
          className="attachment-review-preview"
          src={url}
          muted
          preload="metadata"
          aria-label={t("Staged video", "待发送视频")}
          onError={() => setPreviewFailed(true)}
        />
      )}
      {kind === "video" && previewFailed && (
        <p className="field-help">
          {t("Video preview unavailable.", "视频预览不可用。")}
        </p>
      )}
      <div className="attachment-file">
        <Icon
          name={kind === "image" ? "image" : kind === "video" ? "play" : "file"}
        />
        <span>
          <strong>{file.name}</strong>
          <small>
            {kind === "file"
              ? t("File", "文件")
              : kind === "image"
                ? t("Image", "图片")
                : t("Video", "视频")}{" "}
            · {size}
          </small>
        </span>
      </div>
    </>
  );
}
