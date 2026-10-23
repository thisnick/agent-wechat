import {
  useEffect,
  useId,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import {
  NATIVE_MAX_SOURCE_BYTES,
  type MediaAsset,
  type MediaJob,
  type MediaKind,
  type NativeMessage,
} from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { readableError } from "./session.ts";
import { Avatar, Button, Icon } from "./ui.tsx";
import type { PendingMedia } from "./media-outbox.ts";
import {
  standardImage,
  cachedVariant,
  canRetrieve,
  downloadAsset,
} from "./media-variants.ts";
import { useVisible } from "./visible.tsx";
import { usePhotoViewer } from "./photo-viewer.tsx";
import type { IncomingMedia } from "./media-inbox.ts";

const ended = new Set([
  "available",
  "accepted",
  "failed",
  "cancelled",
  "needs_review",
  "partial",
  "expired",
  "unsupported",
]);
const size = (bytes: number) =>
  bytes < 1024 * 1024
    ? `${Math.ceil(bytes / 1024)} KB`
    : `${(bytes / 1024 / 1024).toFixed(1)} MB`;

export function AttachmentPicker({
  connection,
  account,
  threadId,
  recipient,
  enabled,
}: {
  connection: Connection;
  account: string;
  threadId: string | null;
  recipient: string;
  enabled: boolean;
}) {
  const input = useRef<HTMLInputElement>(null);
  const [review, setReview] = useState<{
    file: File;
    threadId: string;
    recipient: string;
    kind: MediaKind;
  } | null>(null);
  const [error, setError] = useState("");
  const outbox = connection.mediaOutbox(account);
  useEffect(() => {
    if (!review) return;
    const close = (event: KeyboardEvent) => {
      if (event.key === "Escape") setReview(null);
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [review]);
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
        aria-label="Choose attachment"
        onChange={(event) => {
          const file = event.target.files?.[0];
          event.target.value = "";
          if (!file || !threadId) return;
          if (!file.size || file.size > NATIVE_MAX_SOURCE_BYTES) {
            setError("Choose a nonempty file smaller than 25 MB.");
            return;
          }
          setError("");
          setReview({
            file,
            threadId,
            recipient,
            kind: file.type.startsWith("image/")
              ? "image"
              : file.type.startsWith("video/")
                ? "video"
                : "file",
          });
        }}
      />
      {error && (
        <span className="attachment-error" role="alert">
          {error}
        </span>
      )}
      {review && (
        <div className="overlay-backdrop" onClick={() => setReview(null)}>
          <section
            className="modal attachment-review"
            role="dialog"
            aria-modal="true"
            aria-label="Send attachment"
            onClick={(event) => event.stopPropagation()}
          >
            <div className="modal-head">
              <h2>Send attachment</h2>
              <Button
                icon="close"
                label="Close attachment"
                onClick={() => setReview(null)}
              />
            </div>
            <div className="modal-body">
              <p>
                To <strong>{review.recipient}</strong>
              </p>
              <div className="attachment-file">
                <Icon name="file" />
                <span>
                  <strong>{review.file.name}</strong>
                  <small>{size(review.file.size)}</small>
                </span>
              </div>
              <label className="setting">
                Send as
                <select
                  autoFocus
                  value={review.kind}
                  onChange={(event) =>
                    setReview({
                      ...review,
                      kind: event.target.value as MediaKind,
                    })
                  }
                >
                  <option value="file">File</option>
                  <option value="image">Image</option>
                  <option value="video">Video</option>
                  <option value="voice">Voice message</option>
                </select>
              </label>
              {review.kind === "voice" && (
                <p className="field-help">
                  Audio over 60 seconds is split into separate voice messages.
                </p>
              )}
              <div className="attachment-actions">
                <button className="secondary" onClick={() => setReview(null)}>
                  Cancel
                </button>
                <button
                  className="primary"
                  disabled={!enabled}
                  onClick={() => {
                    const target = review;
                    setReview(null);
                    void outbox
                      .send(target.threadId, target.file, target.kind)
                      .catch((error) => setError(error.message));
                  }}
                >
                  Send {review.kind === "voice" ? "voice message" : review.kind}
                </button>
              </div>
            </div>
          </section>
        </div>
      )}
    </>
  );
}

export function PendingAttachments({
  connection,
  account,
  threadId,
  messages,
}: {
  connection: Connection;
  account: string;
  threadId: string | null;
  messages: NativeMessage[];
}) {
  const outbox = connection.mediaOutbox(account);
  const pending = useSyncExternalStore(outbox.subscribe, outbox.snapshot);
  return (
    <>
      {pending
        .filter(
          (item) => item.threadId === threadId && !committed(item, messages),
        )
        .map((item) => {
          const job = item.job;
          const uncertain =
            item.uncertain ||
            job?.state === "needs_review" ||
            job?.state === "partial";
          const label = uncertain
            ? "Check the conversation before sending again"
            : job?.state === "accepted"
              ? "Accepted · loading message…"
              : item.stage === "uploading"
                ? "Uploading…"
                : item.stage === "submitting"
                  ? "Submitting…"
                  : item.stage === "cancelled" || job?.state === "cancelled"
                    ? "Cancelled"
                    : item.stage === "failed" || job?.state === "failed"
                      ? "Attachment failed"
                      : job?.phase || "Checking submission…";
          return (
            <article
              className="message own"
              key={item.key}
              data-media-state={job?.state ?? item.stage}
            >
              <Avatar name="You" />
              <div className="message-body">
                <div className="bubble attachment-file">
                  <Icon name="file" />
                  <span>
                    <strong>{item.name}</strong>
                    <small>
                      {size(item.size)} · {item.kind}
                    </small>
                  </span>
                </div>
                <div
                  className={`delivery ${
                    uncertain || item.error ? "warn" : ""
                  }`}
                  role="status"
                >
                  {label}
                  {job?.totalBytes && !ended.has(job.state)
                    ? ` · ${Math.min(
                        100,
                        Math.round((job.completedBytes / job.totalBytes) * 100),
                      )}%`
                    : ""}
                </div>
                {item.error && (
                  <small className="send-explanation">{item.error}</small>
                )}
                {Array.isArray(
                  (job?.result as { chunks?: unknown[] })?.chunks,
                ) && (
                  <small className="send-explanation">
                    {(
                      job!.result as {
                        chunks: {
                          index: number;
                          state: string;
                          durationMs: number;
                        }[];
                      }
                    ).chunks
                      .map((chunk) => `Note ${chunk.index + 1}: ${chunk.state}`)
                      .join(" · ")}
                  </small>
                )}
                <div className="attachment-job-actions">
                  {(item.sourceId || job) && (
                    <button onClick={() => void outbox.check(item.key)}>
                      Check status
                    </button>
                  )}
                  {(item.stage === "uploading" ||
                    (job && !ended.has(job.state))) && (
                    <button
                      disabled={job?.cancelRequested}
                      onClick={() => void outbox.cancel(item.key)}
                    >
                      {job?.cancelRequested ? "Cancelling…" : "Cancel"}
                    </button>
                  )}
                  {job?.retryable && !uncertain && (
                    <button onClick={() => void outbox.retry(item.key)}>
                      Retry
                    </button>
                  )}
                  {(item.stage === "failed" ||
                    item.stage === "cancelled" ||
                    (job && ended.has(job.state))) && (
                    <button onClick={() => outbox.dismiss(item.key)}>
                      Dismiss
                    </button>
                  )}
                </div>
              </div>
            </article>
          );
        })}
    </>
  );
}
function committed(item: PendingMedia, messages: NativeMessage[]): boolean {
  if (item.job?.state !== "accepted") return false;
  const ids = (item.job.result as { messageIds?: string[] })?.messageIds;
  return Array.isArray(ids) && ids.length > 0
    ? ids.every((id) => messages.some((message) => message.id === id))
    : !!item.job.serverMessageId &&
        messages.some(
          (message) => message.serverId === item.job!.serverMessageId,
        );
}

export function MediaCard({
  message,
  connection,
  previewOnly = false,
  viewer = false,
}: {
  message: NativeMessage;
  connection: Connection;
  previewOnly?: boolean;
  viewer?: boolean;
}) {
  const { element, visible } = useVisible<HTMLDivElement>();
  const opening = useSyncExternalStore(
    connection.session.subscribe,
    connection.session.snapshot,
  ).opening;
  const model = connection.incomingMedia.get(message.accountId, message.id);
  const state = useSyncExternalStore(model.subscribe, model.snapshot);
  const gallery = usePhotoViewer();
  const image = previewOnly || message.type === 3;
  const sticker = message.type === 47;
  const voice = message.type === 34;
  const visual = image || sticker;
  const asset = image
    ? standardImage(state.assets)
    : (state.assets.find(
        (asset) => asset.variant === (voice ? "normal" : "standard"),
      ) ?? state.assets[0]);
  const working = !!asset && model.working(asset);
  useEffect(() => {
    if (visible || viewer) void model.load();
  }, [model, visible, viewer]);
  useEffect(() => {
    if ((visual || voice) && !opening && (visible || viewer) && asset)
      model.ensure(asset);
  }, [model, asset, opening, visible, viewer, visual, voice]);
  const available = asset && cachedVariant(asset);
  return (
    <div
      ref={element}
      className={`bubble media-card${visual ? " visual-media" : ""}${sticker ? " sticker-card" : ""}${voice ? " voice-card" : ""}${viewer ? " viewer-media" : ""}`}
    >
      {!visual && !voice && (
        <div className="attachment-file">
          <Icon name="file" />
          <strong>
            {asset?.filename || (message.type === 43 ? "Video" : "Attachment")}
          </strong>
        </div>
      )}
      {available ? (
        <AssetContent
          key={`${asset.id}:${asset.sha256}`}
          asset={asset}
          connection={connection}
          open={
            image && !previewOnly && !viewer && gallery
              ? () => gallery.open(message)
              : undefined
          }
        />
      ) : visual || voice ? (
        <div
          className="media-placeholder"
          role="status"
          aria-label={
            !state.loaded || working
              ? voice
                ? "Loading voice message"
                : "Loading image"
              : voice
                ? "Voice message unavailable"
                : "Image unavailable"
          }
        >
          <Icon name={voice ? "play" : "image-off"} />
          {!state.loaded || working ? (
            <span className="media-spinner" />
          ) : asset && canRetrieve(asset) ? (
            <button onClick={() => void model.retrieve(asset)}>Retry</button>
          ) : null}
        </div>
      ) : null}
      {!previewOnly && !voice && !image && state.loaded && (
        <MediaDownload
          model={model}
          connection={connection}
          asset={downloadAsset(state.assets, asset)}
        />
      )}
      {voice &&
        !available &&
        state.loaded &&
        asset &&
        !working &&
        canRetrieve(asset) && (
          <button onClick={() => void model.retrieve(asset)}>
            Retry voice message
          </button>
        )}
      {state.error && (
        <span
          className="media-error-indicator"
          role="status"
          title={state.error}
        >
          <Icon name="help" />
          <span className="sr-only">{state.error}</span>
        </span>
      )}
    </div>
  );
}

export function AssetContent({
  asset,
  connection,
  open,
}: {
  asset: MediaAsset;
  connection: Connection;
  open?: () => void;
}) {
  const [url, setUrl] = useState("");
  const [error, setError] = useState("");
  const preview = asset.preview;
  const type = preview?.contentType ?? asset.contentType ?? "";
  const inline =
    /^image\/(jpeg|png|gif|webp)$/.test(type) ||
    /^video\/mp4$/.test(type) ||
    /^audio\/(wav|mpeg|ogg)$/.test(type);
  useEffect(() => {
    if (!inline) return;
    const controller = new AbortController();
    let objectUrl = "";
    void connection.session.client
      .mediaContent(asset.accountId, asset.id, {
        representation: preview ? "preview" : "original",
        maxBytes: NATIVE_MAX_SOURCE_BYTES,
        signal: controller.signal,
      })
      .then((content) => {
        if (controller.signal.aborted) return;
        objectUrl = URL.createObjectURL(
          new Blob([new Uint8Array(content.bytes).buffer], {
            type: content.contentType,
          }),
        );
        setUrl(objectUrl);
      })
      .catch((error) => {
        if (!controller.signal.aborted) setError(readableError(error));
      });
    return () => {
      controller.abort();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [asset.accountId, asset.id, asset.sha256]);
  const image = (
    <img
      className="media-preview"
      src={url}
      alt={
        asset.kind === "sticker" ? "Sticker" : asset.filename || "Shared image"
      }
    />
  );
  return (
    <>
      {!inline && (
        <div
          className="media-placeholder"
          role="status"
          aria-label="Preview unavailable"
        >
          <Icon name="image-off" />
        </div>
      )}
      {inline && !url && !error && (
        <div
          className="media-placeholder"
          role="status"
          aria-label="Loading media"
        >
          <span className="media-spinner" />
        </div>
      )}
      {url &&
        (type.startsWith("image/") ? (
          open ? (
            <button
              className="photo-open"
              onClick={open}
              aria-label="Open photo"
            >
              {image}
            </button>
          ) : (
            image
          )
        ) : type.startsWith("video/") ? (
          <video
            className="media-preview"
            src={url}
            controls
            preload="metadata"
          />
        ) : (
          <audio
            className="media-audio"
            src={url}
            controls
            preload="metadata"
            aria-label="Voice message"
          />
        ))}
      {error && (
        <div
          className="media-placeholder"
          role="status"
          aria-label="Media unavailable"
          title={error}
        >
          <Icon name="image-off" />
        </div>
      )}
    </>
  );
}

export function MediaDownload({
  asset,
  model,
  connection,
}: {
  asset?: MediaAsset;
  model: IncomingMedia;
  connection: Connection;
}) {
  const state = useSyncExternalStore(model.subscribe, model.snapshot);
  const id = useId();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  if (!asset) return null;
  const ready = cachedVariant(asset);
  const working = model.working(asset);
  const job = state.jobs[asset.id];
  const original = asset.variant === "original";
  const downloadBytes =
    (ready && asset.kind === "image" ? asset.preview?.byteLength : null) ??
    asset.byteLength ??
    job?.totalBytes;
  const quality = original
    ? "original"
    : asset.kind === "image"
      ? "standard"
      : "";
  const label = saving
    ? "Saving…"
    : working
      ? "Downloading…"
      : ready
        ? `Download${quality ? ` ${quality}` : ""}`
        : original && canRetrieve(asset)
          ? "Request original"
          : canRetrieve(asset)
            ? "Download"
            : `${original ? "Original" : "Download"} unavailable`;
  const progress = job?.totalBytes
    ? Math.min(100, Math.round((job.completedBytes / job.totalBytes) * 100))
    : undefined;
  async function save() {
    if (!asset || saving || working) return;
    if (!ready) {
      void model.retrieve(asset);
      return;
    }
    setSaving(true);
    setError("");
    try {
      const representation =
        asset.preview && asset.kind === "image" ? "preview" : "original";
      const content = await connection.session.client.mediaContent(
        asset.accountId,
        asset.id,
        { representation },
      );
      const href = URL.createObjectURL(
        new Blob([new Uint8Array(content.bytes).buffer], {
          type: content.contentType,
        }),
      );
      const link = document.createElement("a");
      link.href = href;
      link.download =
        (representation === "preview"
          ? asset.preview?.filename
          : asset.filename) || "attachment";
      link.click();
      setTimeout(() => URL.revokeObjectURL(href), 30000);
    } catch (error) {
      setError(readableError(error));
    } finally {
      setSaving(false);
    }
  }
  return (
    <div className="media-download">
      <button
        className="media-download-button"
        aria-describedby={id}
        disabled={saving || working || (!ready && !canRetrieve(asset))}
        onClick={() => void save()}
      >
        <Icon name="download" />
        <span>{label}</span>
      </button>
      <span className="download-disclosure" id={id} role="tooltip">
        {quality
          ? `${quality[0].toUpperCase()}${quality.slice(1)}`
          : asset.filename || "File"}
        {downloadBytes ? ` · ${size(downloadBytes)}` : " · Size unknown"}
        {!ready && canRetrieve(asset) ? " · Keep on agent-wechat hub" : ""}
      </span>
      {working && (
        <>
          <progress aria-label="Download progress" value={progress} max={100} />
          {progress !== undefined && (
            <span className="download-percent">{progress}%</span>
          )}
          <Button
            icon="close"
            label="Cancel download"
            disabled={job?.cancelRequested}
            onClick={() => void model.cancel(asset)}
          />
        </>
      )}
      {error && (
        <small className="attachment-error" role="alert">
          {error}
        </small>
      )}
    </div>
  );
}
