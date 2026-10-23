import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import {
  NATIVE_MAX_SOURCE_BYTES,
  type MediaAsset,
  type MediaJob,
  type MediaKind,
  type NativeMessage,
} from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { pause, readableError } from "./session.ts";
import { Avatar, Button, Icon } from "./ui.tsx";
import type { PendingMedia } from "./media-outbox.ts";
import { standardImage, cachedVariant, canRetrieve } from "./media-variants.ts";

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
}: {
  message: NativeMessage;
  connection: Connection;
}) {
  const client = connection.session.client;
  const [assets, setAssets] = useState<MediaAsset[]>([]);
  const [selected, setSelected] = useState("");
  const [job, setJob] = useState<MediaJob | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const lifetime = useRef(new AbortController());
  const currentAssets = useRef<MediaAsset[]>([]);
  const request = useRef(0);
  const automaticallyRequested = useRef(new Set<string>());
  const reload = async () => {
    const generation = ++request.current;
    const response = await client.mediaAssets(message.accountId, message.id);
    if (!lifetime.current.signal.aborted && generation === request.current) {
      currentAssets.current = response.items;
      setAssets(response.items);
      setSelected((current) =>
        response.items.some((asset) => asset.id === current)
          ? current
          : ((
              response.items.find(
                (asset) =>
                  asset.variant ===
                  (message.type === 34 ? "normal" : "standard"),
              ) ?? response.items[0]
            )?.id ?? ""),
      );
      setLoading(false);
    }
  };
  useEffect(() => {
    void reload().catch((error) => {
      if (!lifetime.current.signal.aborted) {
        setError(readableError(error));
        setLoading(false);
      }
    });
    return () => lifetime.current.abort();
  }, [message.accountId, message.id]);
  useEffect(
    () =>
      connection.session.onMediaReady((assetId) => {
        if (
          !currentAssets.current.length ||
          currentAssets.current.some((asset) => asset.id === assetId)
        )
          void reload().catch(() => {});
      }),
    [connection.session, message.accountId, message.id],
  );
  useEffect(() => {
    if (!job || ended.has(job.state)) return;
    const controller = new AbortController();
    void (async () => {
      try {
        while (!controller.signal.aborted) {
          await pause(700, controller.signal);
          if (controller.signal.aborted) return;
          const current = await client.job(
            message.accountId,
            job.id,
            controller.signal,
          );
          if (controller.signal.aborted) return;
          setJob(current);
          if (ended.has(current.state)) {
            await reload();
            return;
          }
        }
      } catch (error) {
        if (!controller.signal.aborted) setError(readableError(error));
      }
    })();
    return () => controller.abort();
  }, [job?.id, job?.state]);
  const image = message.type === 3;
  const asset = image
    ? standardImage(assets)
    : assets.find((asset) => asset.id === selected);
  const working = !!job && !ended.has(job.state);
  async function retrieve() {
    if (!asset) return;
    setError("");
    try {
      const current = await client.transferMedia(message.accountId, {
        messageId: message.id,
        assetId: asset.id,
        quality: asset.variant,
      });
      if (lifetime.current.signal.aborted) return;
      setJob(current);
      if (ended.has(current.state)) await reload();
    } catch (error) {
      if (!lifetime.current.signal.aborted) setError(readableError(error));
    }
  }
  useEffect(() => {
    if (
      (image || message.type === 47) &&
      asset &&
      canRetrieve(asset) &&
      !working &&
      !automaticallyRequested.current.has(asset.id)
    ) {
      automaticallyRequested.current.add(asset.id);
      void retrieve();
    }
  }, [message.type, asset?.id, asset?.state, working]);
  return (
    <div
      className={`bubble media-card${
        message.type === 47 ? " sticker-card" : ""
      }`}
    >
      <div className="attachment-file">
        <Icon
          name={message.type === 3 || message.type === 47 ? "image" : "file"}
        />
        <span>
          <strong>
            {asset?.filename ||
              (message.type === 3
                ? "Image"
                : message.type === 43
                  ? "Video"
                  : message.type === 34
                    ? "Voice message"
                    : message.type === 47
                      ? "Sticker"
                      : "Attachment")}
          </strong>
          <small>
            {asset?.byteLength ? `${size(asset.byteLength)} · ` : ""}
            {loading ? "Loading…" : asset?.state || "Unavailable"}
          </small>
        </span>
      </div>
      {!image && assets.length > 1 && (
        <select
          aria-label="Attachment quality"
          value={selected}
          disabled={working}
          onChange={(event) => {
            setSelected(event.target.value);
            setError("");
            setJob(null);
          }}
        >
          {assets.map((asset) => (
            <option key={asset.id} value={asset.id}>
              {asset.variant}
            </option>
          ))}
        </select>
      )}
      {asset?.state === "available" && (!image || cachedVariant(asset)) ? (
        <AssetContent
          key={`${asset.accountId}:${asset.id}:${asset.sha256}`}
          asset={asset}
          connection={connection}
          saveLabel={image ? "Save image" : "Save attachment"}
        />
      ) : (
        asset && (
          <div className="attachment-job-actions">
            <button
              disabled={
                working ||
                !asset.retryable ||
                asset.state === "expired" ||
                asset.state === "unsupported"
              }
              onClick={() => void retrieve()}
            >
              {working
                ? "Downloading…"
                : image
                  ? "Retry image download"
                  : "Load attachment"}
            </button>
          </div>
        )
      )}
      {image && !loading && !asset && (
        <small>Standard image is unavailable.</small>
      )}
      {image && (
        <OriginalImage
          asset={assets.find((asset) => asset.variant === "original")}
          connection={connection}
          message={message}
          reload={reload}
        />
      )}
      {working && (
        <div className="attachment-job-actions">
          <span role="status">
            {job.phase}
            {job.totalBytes
              ? ` · ${Math.min(
                  100,
                  Math.round((job.completedBytes / job.totalBytes) * 100),
                )}%`
              : ""}
          </span>
          <button
            disabled={job.cancelRequested}
            onClick={() =>
              void client
                .cancelJob(message.accountId, job.id)
                .then(setJob)
                .catch((error) => setError(readableError(error)))
            }
          >
            Cancel
          </button>
        </div>
      )}
      {job?.retryable && ended.has(job.state) && (
        <button
          onClick={() =>
            void client
              .retryJob(message.accountId, job.id)
              .then(setJob)
              .catch((error) => setError(readableError(error)))
          }
        >
          Retry download
        </button>
      )}
      {error && (
        <small className="attachment-error" role="alert">
          {error}
        </small>
      )}
    </div>
  );
}

function AssetContent({
  asset,
  connection,
  saveLabel,
}: {
  asset: MediaAsset;
  connection: Connection;
  saveLabel: string;
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
  return (
    <>
      {inline && !url && !error && <small>Loading preview…</small>}
      {url &&
        (type.startsWith("image/") ? (
          <img
            className="media-preview"
            src={url}
            alt={
              asset.kind === "sticker"
                ? "Sticker"
                : asset.filename || "Shared image"
            }
          />
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
          />
        ))}
      <SaveAsset
        asset={asset}
        connection={connection}
        label={saveLabel}
        representation={
          preview && asset.kind === "image" ? "preview" : "original"
        }
      />
      {error && (
        <small className="attachment-error" role="alert">
          {error}
        </small>
      )}
    </>
  );
}

function SaveAsset({
  asset,
  connection,
  label,
  representation = "original",
}: {
  asset: MediaAsset;
  connection: Connection;
  label: string;
  representation?: "original" | "preview";
}) {
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  async function save() {
    if (saving) return;
    setSaving(true);
    setError("");
    try {
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
    <>
      <div className="attachment-job-actions">
        <button disabled={saving} onClick={() => void save()}>
          {saving ? "Saving…" : label}
        </button>
      </div>
      {error && (
        <small className="attachment-error" role="alert">
          {error}
        </small>
      )}
    </>
  );
}

function OriginalImage({
  asset,
  connection,
  message,
  reload,
}: {
  asset?: MediaAsset;
  connection: Connection;
  message: NativeMessage;
  reload: () => Promise<void>;
}) {
  const [job, setJob] = useState<MediaJob | null>(null);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState("");
  const currentReload = useRef(reload);
  currentReload.current = reload;
  const client = connection.session.client;
  useEffect(() => {
    if (!job || ended.has(job.state)) return;
    const controller = new AbortController();
    void (async () => {
      while (!controller.signal.aborted) {
        await pause(700, controller.signal);
        if (controller.signal.aborted) return;
        const current = await client.job(
          message.accountId,
          job.id,
          controller.signal,
        );
        if (controller.signal.aborted) return;
        setJob(current);
        if (ended.has(current.state)) {
          await currentReload.current();
          return;
        }
      }
    })().catch((error) => {
      if (!controller.signal.aborted) setError(readableError(error));
    });
    return () => controller.abort();
  }, [client, message.accountId, job?.id, job?.state]);
  if (!asset) return null;
  const working = starting || (!!job && !ended.has(job.state));
  async function retrieve() {
    if (!asset || working) return;
    setStarting(true);
    setError("");
    try {
      const current =
        job?.retryable && ended.has(job.state)
          ? await client.retryJob(message.accountId, job.id)
          : await client.transferMedia(message.accountId, {
              messageId: message.id,
              assetId: asset.id,
              quality: "original",
            });
      setJob(current);
      if (ended.has(current.state)) await currentReload.current();
    } catch (error) {
      setError(readableError(error));
    } finally {
      setStarting(false);
    }
  }
  return (
    <div className="original-image-actions">
      {cachedVariant(asset) ? (
        <>
          <small>
            Original downloaded to agent-wechat hub
            {asset.byteLength ? ` · ${size(asset.byteLength)}` : ""}
          </small>
          <SaveAsset
            asset={asset}
            connection={connection}
            label="Save original"
          />
        </>
      ) : (
        <>
          <button
            disabled={working || (!canRetrieve(asset) && !job?.retryable)}
            onClick={() => void retrieve()}
          >
            {working
              ? "Downloading original…"
              : job?.retryable
                ? "Retry original download"
                : "Download original"}
          </button>
          {job && working && (
            <button
              disabled={job.cancelRequested}
              onClick={() =>
                void client
                  .cancelJob(message.accountId, job.id)
                  .then(setJob)
                  .catch((error) => setError(readableError(error)))
              }
            >
              {job.cancelRequested ? "Cancelling…" : "Cancel original download"}
            </button>
          )}
          <small>
            {working
              ? job?.totalBytes
                ? `${Math.min(
                    100,
                    Math.round((job.completedBytes / job.totalBytes) * 100),
                  )}%`
                : "Keeping a copy on the agent-wechat hub."
              : job && ended.has(job.state) && job.state !== "available"
                ? `Original download ${job.state.replaceAll("_", " ")}.`
                : asset.state === "expired"
                  ? "Original expired."
                  : asset.state === "unsupported"
                    ? "Original unavailable."
                    : "Keeps a copy on the agent-wechat hub; then you can save it to this device."}
          </small>
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
