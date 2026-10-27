import { useEffect, useState, useSyncExternalStore } from "react";
import type { MediaAsset, NativeMessage } from "@agent-wechat/shared";
import { Link2, ArrowUpRight } from "lucide-react";
import { sharedLinkTarget } from "./shared-link-target.ts";
import type { Connection } from "./connection.ts";
import { cachedVariant } from "./media-variants.ts";
import { mediaPreviewIdentity } from "./media-preview-cache.ts";
import { useVisible } from "./visible.tsx";
import "./shared-link.css";

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
  const target = sharedLinkTarget(link.url);
  const title = link.title?.trim() || target?.host || "Shared link";
  const content = (
    <>
      <span className="shared-link-art" ref={element} aria-hidden="true">
        <LinkThumbnail asset={thumbnail} connection={connection} />
      </span>
      <span className="shared-link-copy">
        <strong>{title}</strong>
        {!compact && link.description?.trim() && (
          <span className="shared-link-description">{link.description}</span>
        )}
        <span className="shared-link-source">
          <span>
            {message.appMessageType === 51
              ? `WeChat Channels${
                  link.sourceName ? ` · ${link.sourceName}` : ""
                }`
              : target?.host || link.sourceName || "Shared link"}
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
  ) : target ? (
    <a
      className="bubble shared-link-card"
      href={target.href}
      target="_blank"
      rel="noopener noreferrer"
      aria-label={`Open ${title} (${target.host})`}
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
