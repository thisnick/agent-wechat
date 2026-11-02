import { desktop, saveDesktopFile } from "./desktop.ts";
import {
  useEffect,
  useId,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import {
  type MediaAsset,
  type MediaJob,
  type NativeMessage,
} from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { t, useLocale } from "./i18n.ts";
import { mediaPreviewIdentity } from "./media-preview-cache.ts";
import { readableError } from "./session.ts";
import { Avatar, Button, Icon } from "./ui.tsx";
import type { PendingMedia } from "./media-outbox.ts";
import {
  standardImage,
  imagePreview,
  cachedVariant,
  canRetrieve,
  downloadAsset,
  cachedOriginal,
  videoAsset,
} from "./media-variants.ts";
import { useVisible } from "./visible.tsx";
import { useMediaViewer } from "./media-viewer-context.ts";
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
  useLocale();
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
            ? t(
                "Check the conversation before sending again",
                "请先检查会话，再重新发送",
              )
            : job?.state === "accepted"
              ? t("Accepted · loading message…", "已接收 · 正在加载消息…")
              : item.stage === "uploading"
                ? t("Uploading…", "正在上传…")
                : item.stage === "submitting"
                  ? t("Submitting…", "正在提交…")
                  : item.stage === "cancelled" || job?.state === "cancelled"
                    ? t("Cancelled", "已取消")
                    : item.stage === "failed" || job?.state === "failed"
                      ? t("Attachment failed", "附件发送失败")
                      : job?.phase || t("Checking submission…", "正在检查提交…");
          const kindLabel =
            item.kind === "image"
              ? t("image", "图片")
              : item.kind === "video"
                ? t("video", "视频")
                : item.kind === "voice"
                  ? t("voice", "语音")
                  : t("file", "文件");
          return (
            <article
              className="message own"
              key={item.key}
              data-media-state={job?.state ?? item.stage}
            >
              <Avatar name={t("You", "我")} />
              <div className="message-body">
                <div className="bubble attachment-file">
                  <Icon name="file" />
                  <span>
                    <strong>{item.name}</strong>
                    <small>
                      {size(item.size)} · {kindLabel}
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
                      .map((chunk) =>
                        t(
                          `Note ${chunk.index + 1}: ${chunk.state}`,
                          `第 ${chunk.index + 1} 段：${chunk.state}`,
                        ),
                      )
                      .join(" · ")}
                  </small>
                )}
                <div className="attachment-job-actions">
                  {(item.sourceId || job) && (
                    <button onClick={() => void outbox.check(item.key)}>
                      {t("Check status", "检查状态")}
                    </button>
                  )}
                  {(item.stage === "uploading" ||
                    (job && !ended.has(job.state))) && (
                    <button
                      disabled={job?.cancelRequested}
                      onClick={() => void outbox.cancel(item.key)}
                    >
                      {job?.cancelRequested
                        ? t("Cancelling…", "正在取消…")
                        : t("Cancel", "取消")}
                    </button>
                  )}
                  {job?.retryable && !uncertain && (
                    <button onClick={() => void outbox.retry(item.key)}>
                      {t("Retry", "重试")}
                    </button>
                  )}
                  {(item.stage === "failed" ||
                    item.stage === "cancelled" ||
                    (job && ended.has(job.state))) && (
                    <button onClick={() => outbox.dismiss(item.key)}>
                      {t("Dismiss", "关闭")}
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
  useLocale();
  const { element, visible } = useVisible<HTMLDivElement>();
  const opening = useSyncExternalStore(
    connection.session.subscribe,
    connection.session.snapshot,
  ).opening;
  const model = connection.incomingMedia.get(message.accountId, message.id);
  const state = useSyncExternalStore(model.subscribe, model.snapshot);
  const gallery = useMediaViewer();
  const image = message.type === 3;
  const video = message.type === 43;
  const sticker =
    message.type === 47 ||
    (message.type === 49 && message.appMessageType === 8);
  const voice = message.type === 34;
  const visual = image || sticker || video;
  const standard = standardImage(state.assets);
  const original = viewer ? cachedOriginal(state.assets) : undefined;
  const stickerOriginal = sticker
    ? state.assets.find((asset) => asset.variant === "original")
    : undefined;
  const asset = image
    ? viewer && (!standard || !cachedVariant(standard)) && original
      ? original
      : imagePreview(state.assets)
    : sticker
    ? cachedOriginal(state.assets) ??
      state.assets.find((asset) => asset.variant === "thumbnail") ??
      stickerOriginal
    : video
    ? videoAsset(state.assets, viewer)
    : state.assets.find(
        (asset) => asset.variant === (voice ? "normal" : "standard"),
      ) ?? state.assets[0];
  const working = !!asset && model.working(asset);
  useEffect(() => {
    if (visible || viewer) void model.load();
  }, [model, visible, viewer]);
  useEffect(() => {
    if (
      (visual || voice) &&
      !opening &&
      (visible || viewer) &&
      asset &&
      (!video || asset.variant !== "original")
    ) {
      model.ensure(asset);
      if (image && standard && standard.id !== asset.id) model.ensure(standard);
      if (stickerOriginal && stickerOriginal.id !== asset.id)
        model.ensure(stickerOriginal);
    }
  }, [
    model,
    asset,
    standard,
    stickerOriginal,
    image,
    opening,
    visible,
    viewer,
    visual,
    voice,
    video,
  ]);
  const available = asset && cachedVariant(asset);
  return (
    <div
      ref={element}
      className={`bubble media-card${visual ? " visual-media" : ""}${
        video ? " video-card" : ""
      }${sticker ? " sticker-card" : ""}${voice ? " voice-card" : ""}${
        viewer ? " viewer-media" : ""
      }`}
    >
      {!visual && !voice && (
        <div className="attachment-file">
          <Icon name="file" />
          <strong>{asset?.filename || t("Attachment", "附件")}</strong>
        </div>
      )}
      {video && !viewer ? (
        <VideoPoster
          asset={available ? asset : undefined}
          connection={connection}
          open={
            !previewOnly && gallery ? () => gallery.open(message) : undefined
          }
        />
      ) : available && (visual || voice) ? (
        <AssetContent
          key={`${asset.id}:${asset.sha256}`}
          asset={asset}
          upgrade={original?.id !== asset.id ? original : undefined}
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
                ? t("Loading voice message", "正在加载语音消息")
                : video
                ? t("Loading video", "正在加载视频")
                : t("Loading image", "正在加载图片")
              : voice
              ? t("Voice message unavailable", "语音消息不可用")
              : video
              ? t("Video unavailable", "视频不可用")
              : t("Image unavailable", "图片不可用")
          }
        >
          <Icon name={voice || video ? "play" : "image-off"} />
          {!state.loaded || working ? (
            <span className="media-spinner" />
          ) : !previewOnly && asset && canRetrieve(asset) ? (
            <button onClick={() => void model.retrieve(asset)}>
              {asset.state === "pending"
                ? t("Load", "加载")
                : t("Retry", "重试")}
            </button>
          ) : null}
        </div>
      ) : null}
      {!previewOnly && !voice && !image && !video && state.loaded && (
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
            {t("Retry voice message", "重试语音消息")}
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

function VideoPoster({
  asset,
  connection,
  open,
}: {
  asset?: MediaAsset;
  connection: Connection;
  open?: () => void;
}) {
  const content = (
    <>
      {asset && <AssetContent asset={asset} connection={connection} />}
      <span className="video-play">
        <Icon name="play" />
      </span>
    </>
  );
  return open ? (
    <button
      className="video-poster"
      aria-label={t("Open video", "打开视频")}
      onClick={open}
    >
      {content}
    </button>
  ) : (
    <div className="video-poster">{content}</div>
  );
}

export function AssetContent({
  asset,
  connection,
  open,
  upgrade,
}: {
  asset: MediaAsset;
  connection: Connection;
  open?: () => void;
  upgrade?: MediaAsset;
}) {
  useLocale();
  const cache = connection.incomingMedia.previews;
  const cached = cache.peek(asset);
  const [url, setUrl] = useState(cached?.url ?? "");
  const [error, setError] = useState("");
  const [baseReady, setBaseReady] = useState(!!cached);
  const [shownVariant, setShownVariant] = useState(asset.variant);
  const baseUrl = useRef(cached?.url ?? "");
  const type = asset.preview?.contentType ?? asset.contentType ?? "";
  const identity = mediaPreviewIdentity(asset);
  const upgradeIdentity = upgrade ? mediaPreviewIdentity(upgrade) : "";
  const inline =
    /^image\/(jpeg|png|gif|webp)$/.test(type) ||
    /^video\/mp4$/.test(type) ||
    /^audio\/(wav|mpeg|ogg)$/.test(type);
  useEffect(() => {
    if (!inline) return;
    let active = true;
    const lease = cache.acquire(asset);
    void lease.ready
      .then((content) => {
        if (!active) return;
        baseUrl.current = content.url;
        setUrl(content.url);
        setBaseReady(true);
        setError("");
      })
      .catch((error) => {
        if (active) setError(readableError(error));
      });
    return () => {
      active = false;
      lease.release();
    };
  }, [cache, identity, inline]);
  useEffect(() => {
    if (!baseReady || !upgrade || !cachedVariant(upgrade)) return;
    let active = true;
    const lease = cache.acquire(upgrade);
    void lease.ready
      .then(async (content) => {
        if (!active) return;
        const decoded = new Image();
        decoded.src = content.url;
        await decoded.decode();
        if (!active) return;
        setUrl(content.url);
        setShownVariant(upgrade.variant);
      })
      .catch(() => {
        /* A failed original preview keeps the visible standard image. */
      });
    return () => {
      active = false;
      lease.release();
      setUrl(baseUrl.current);
      setShownVariant(asset.variant);
    };
  }, [cache, baseReady, upgradeIdentity]);
  const image = (
    <img
      className="media-preview"
      src={url}
      data-media-variant={shownVariant}
      alt={
        asset.kind === "sticker"
          ? t("Sticker", "表情")
          : asset.filename || t("Shared image", "分享的图片")
      }
    />
  );
  return (
    <>
      {!inline && (
        <div
          className="media-placeholder"
          role="status"
          aria-label={t("Preview unavailable", "预览不可用")}
        >
          <Icon name="image-off" />
        </div>
      )}
      {inline && !url && !error && (
        <div
          className="media-placeholder"
          role="status"
          aria-label={t("Loading media", "正在加载媒体")}
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
              aria-label={t("Open photo", "打开图片")}
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
            aria-label={t("Voice message", "语音消息")}
          />
        ))}
      {error && (
        <div
          className="media-placeholder"
          role="status"
          aria-label={t("Media unavailable", "媒体不可用")}
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
  if (!asset) return null;
  const standard = state.assets.find(
    (item) =>
      item.kind === asset.kind &&
      item.variant === (asset.kind === "video" ? "normal" : "standard") &&
      (cachedVariant(item) || canRetrieve(item)),
  );
  const fallback =
    asset.variant === "original" && !cachedVariant(asset)
      ? standard
      : undefined;
  return (
    <>
      {fallback && (
        <MediaDownloadAction
          key={fallback.id}
          asset={fallback}
          model={model}
          connection={connection}
        />
      )}
      {(!fallback || canRetrieve(asset) || model.working(asset)) && (
        <MediaDownloadAction
          key={asset.id}
          asset={asset}
          model={model}
          connection={connection}
        />
      )}
    </>
  );
}

function MediaDownloadAction({
  asset,
  model,
  connection,
}: {
  asset: MediaAsset;
  model: IncomingMedia;
  connection: Connection;
}) {
  useLocale();
  const state = useSyncExternalStore(model.subscribe, model.snapshot);
  const id = useId();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const ready = cachedVariant(asset);
  const working = model.working(asset);
  const job = state.jobs[asset.id];
  const original = asset.variant === "original";
  const downloadBytes =
    (ready && !original && asset.kind === "image"
      ? asset.preview?.byteLength
      : null) ??
    asset.byteLength ??
    asset.advertisedByteLength ??
    job?.totalBytes;
  const quality = original
    ? "original"
    : asset.kind === "image" || asset.kind === "video"
      ? "standard"
      : "";
  const label = saving
    ? t("Saving…", "正在保存…")
    : working
      ? original
        ? t("Getting original…", "正在获取原图/原视频…")
        : t("Downloading…", "正在下载…")
      : ready
        ? t("Download", "下载")
        : original && canRetrieve(asset)
          ? t("Get original", "获取原文件")
          : canRetrieve(asset)
            ? t("Download", "下载")
            : original
              ? t("Original unavailable", "原文件不可用")
              : t("Download unavailable", "下载不可用");
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
      // Original downloads keep source bytes even when the viewer uses a preview.
      const representation =
        !original && asset.preview && asset.kind === "image"
          ? "preview"
          : "original";
      const content = await connection.session.client.mediaContent(
        asset.accountId,
        asset.id,
        { representation },
      );
      const blob = new Blob([new Uint8Array(content.bytes).buffer], { type: content.contentType });
      const filename = (representation === "preview" ? asset.preview?.filename : asset.filename) || "attachment";
      if (desktop) await saveDesktopFile(blob, filename);
      else {
        const href = URL.createObjectURL(blob);
        const link = document.createElement("a");
        link.href = href;
        link.download = filename;
        link.click();
        setTimeout(() => URL.revokeObjectURL(href), 30000);
      }
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
        {working ? (
          <span className="media-spinner" aria-hidden="true" />
        ) : (
          <Icon name={original && !ready ? "cloud-download" : "download"} />
        )}
        <span>{label}</span>
      </button>
      <span className="download-disclosure" id={id} role="tooltip">
        {quality
          ? quality === "original"
            ? t("Original", "原文件")
            : t("Compressed", "压缩版")
          : asset.filename || t("File", "文件")}
        {downloadBytes
          ? ` · ${size(downloadBytes)}`
          : t(" · Size unknown", " · 大小未知")}
        {quality === "standard" &&
        !state.assets.some((item) => item.variant === "original")
          ? t(" · Original unavailable", " · 原文件不可用")
          : ""}
        {!ready && canRetrieve(asset)
          ? original
            ? t(
                " · Make original available for download",
                " · 获取原文件以供下载",
              )
            : t(" · Make available for download", " · 获取文件以供下载")
          : ""}
      </span>
      {working && (
        <>
          <progress
            aria-label={
              original
                ? t("Original download progress", "原文件下载进度")
                : t("Download progress", "下载进度")
            }
            value={progress}
            max={100}
          />
          {progress !== undefined && (
            <span className="download-percent">{progress}%</span>
          )}
          <button
            className="media-transfer-cancel"
            aria-label={
              original
                ? t("Cancel original download", "取消原文件下载")
                : t("Cancel download", "取消下载")
            }
            disabled={!job || job.cancelRequested}
            onClick={() => {
              void model
                .cancel(asset)
                .catch((error) => setError(readableError(error)));
            }}
          >
            {job?.cancelRequested
              ? t("Cancelling…", "正在取消…")
              : t("Cancel", "取消")}
          </button>
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
