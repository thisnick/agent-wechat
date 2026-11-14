import { useEffect, useState, useSyncExternalStore } from "react";
import type { MediaAsset, NativeMessage } from "@agent-wechat/shared";
import { Link2, ArrowUpRight, Play } from "lucide-react";
import { sharedLinkTarget } from "./shared-link-target.ts";
import type { Connection } from "./connection.ts";
import { t } from "./i18n.ts";
import { cachedVariant, isChannelsCard } from "./media-variants.ts";
import { useMediaViewer } from "./media-viewer-context.ts";
import { mediaPreviewIdentity } from "./media-preview-cache.ts";
import { useVisible } from "./visible.tsx";
import "./shared-link.css";
import { emojiText } from "./emoji-text.ts";

export function SharedLink({
  message,
  connection,
  compact = false,
}: {
  message: NativeMessage;
  connection: Connection;
  compact?: boolean;
}) {
  const link = message.link!;
  const { element, visible } = useVisible<HTMLSpanElement>();
  const opening = useSyncExternalStore(
    connection.session.subscribe,
    connection.session.snapshot
  ).opening;
  const model = connection.incomingMedia.get(message.accountId, message.id);
  const media = useSyncExternalStore(model.subscribe, model.snapshot);
  const thumbnail = media.assets.find(
    (asset) => asset.kind === "image" && asset.variant === "thumbnail"
  );
  useEffect(() => {
    if (visible) void model.load();
  }, [model, visible]);
  useEffect(() => {
    if (visible && !opening && thumbnail) model.ensure(thumbnail);
  }, [model, visible, opening, thumbnail]);
  const gallery = useMediaViewer();
  const channels = message.appMessageType === 51;
  // A Channels card's own address is WeChat's upgrade notice, never the post.
  const target = channels ? null : sharedLinkTarget(link.url);
  const video =
    isChannelsCard(message) &&
    media.assets.some((asset) => asset.kind === "video");
  const title =
    (channels
      ? link.description?.trim() || link.sourceName?.trim()
      : link.title?.trim()) ||
    target?.host ||
    (channels ? t("sharedLink.wechatChannels") : t("sharedLink.sharedLink"));
  const duration =
    channels && link.durationSeconds
      ? `${Math.floor(link.durationSeconds / 60)}:${String(
          link.durationSeconds % 60,
        ).padStart(2, "0")}`
      : undefined;
  const content = (
    <>
      <span className="shared-link-art" ref={element} aria-hidden="true">
        <LinkThumbnail asset={thumbnail} connection={connection} />
        {video && (
          <span className="shared-link-play">
            <Play size={18} fill="currentColor" />
          </span>
        )}
        {video && duration && (
          <span className="shared-link-duration">{duration}</span>
        )}
      </span>
      <span className="shared-link-copy">
        <strong>{emojiText(title)}</strong>
        {!compact && !channels && link.description?.trim() && (
          <span className="shared-link-description">{emojiText(link.description!)}</span>
        )}
        <span className="shared-link-source">
          <span>
            {channels
              ? [
                  t("sharedLink.wechatChannels"),
                  link.sourceName,
                  video ? undefined : duration,
                ]
                  .filter(Boolean)
                  .join(" · ")
              : target?.host ||
                link.sourceName ||
                t("sharedLink.sharedLink")}
          </span>
          {!compact && target && <ArrowUpRight aria-hidden="true" size={14} />}
        </span>
      </span>
    </>
  );
  // The quote itself handles navigation. Keep its card presentational rather
  // than putting an external-link target inside the jump-to-message control.
  return compact ? (
    <div className="shared-link-card compact-link-card">{content}</div>
  ) : video && gallery ? (
    <button
      type="button"
      className="bubble shared-link-card channels-video-card"
      aria-label={t("sharedLink.playChannelsVideo", { title })}
      onClick={() => gallery.open(message)}
    >
      {content}
    </button>
  ) : target ? (
    <a
      className="bubble shared-link-card"
      href={target.href}
      target="_blank"
      rel="noopener noreferrer"
      aria-label={t("sharedLink.open", { title, host: target.host })}
    >
      {content}
    </a>
  ) : (
    <div className="bubble shared-link-card">{content}</div>
  );
}

function LinkThumbnail({
  asset,
  connection,
}: {
  asset?: MediaAsset;
  connection: Connection;
}) {
  const [url, setUrl] = useState("");
  const cache = connection.incomingMedia.previews;
  const identity = asset ? mediaPreviewIdentity(asset) : "";
  useEffect(() => {
    setUrl("");
    if (!asset || !cachedVariant(asset)) return;
    let active = true;
    const lease = cache.acquire(asset);
    void lease.ready
      .then((content) => {
        if (active) setUrl(content.url);
      })
      .catch(() => {});
    return () => {
      active = false;
      lease.release();
    };
  }, [cache, identity]);
  return url ? (
    <img src={url} alt="" onError={() => setUrl("")} />
  ) : (
    <Link2 strokeWidth={1.5} />
  );
}
