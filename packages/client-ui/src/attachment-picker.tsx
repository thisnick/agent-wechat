import { useEffect, useRef, useState, type RefObject } from "react";
import type { Connection } from "./connection.ts";
import type { AttachmentDraft } from "./attachment-drafts.ts";
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
  const input = useRef<HTMLInputElement>(null);
  const [error, setError] = useState(""),
    [dragging, setDragging] = useState(false);
  function choose(files: File[]) {
    if (!enabled || !threadId) {
      setError("Attachments aren't available in this conversation right now.");
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
          : "Couldn't select these files. Choose them again.",
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
        label="Attach file"
        disabled={!enabled || !threadId}
        onClick={() => input.current?.click()}
      />
      <input
        ref={input}
        className="attachment-input"
        type="file"
        multiple
        aria-label="Choose attachment"
        onChange={(event) => {
          const files = Array.from(event.target.files ?? []);
          event.target.value = "";
          if (files.length) choose(files);
        }}
      />
      {dragging && (
        <span className="composer-drop-hint" role="status">
          <Icon name="attach" /> Drop to attach
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
  if (!draft.files.length) return null;
  return (
    <section className="composer-attachments" aria-label="Staged attachments">
      <div className="composer-attachments-head">
        <span>
          {draft.files.length}{" "}
          {draft.files.length === 1 ? "attachment" : "attachments"}
        </span>
      </div>
      <ul className="composer-attachment-list">
        {draft.files.map((item) => (
          <li key={item.id} className="composer-attachment">
            <AttachmentPreview item={item} />
            <Button
              icon="close"
              label={`Remove ${item.file.name}`}
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
            Original media quality
          </label>
          <ComposerHelp label="About original media quality">
            Include the unchanged video file alongside the playback copy.
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
          alt="Staged image"
        />
      )}
      {url && kind === "video" && !previewFailed && (
        <video
          className="attachment-review-preview"
          src={url}
          muted
          preload="metadata"
          aria-label="Staged video"
          onError={() => setPreviewFailed(true)}
        />
      )}
      {kind === "video" && previewFailed && (
        <p className="field-help">Video preview unavailable.</p>
      )}
      <div className="attachment-file">
        <Icon
          name={kind === "image" ? "image" : kind === "video" ? "play" : "file"}
        />
        <span>
          <strong>{file.name}</strong>
          <small>
            {kind === "file" ? "File" : kind === "image" ? "Image" : "Video"} ·{" "}
            {size}
          </small>
        </span>
      </div>
    </>
  );
}
