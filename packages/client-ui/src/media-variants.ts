import type { MediaAsset } from "@agent-wechat/shared";

// Display quality is stable even when the original becomes available later.
export const standardImage = (assets: MediaAsset[]): MediaAsset | undefined =>
  assets.find((asset) => asset.variant === "standard");
export const cachedVariant = (asset: MediaAsset): boolean =>
  asset.state === "available" && asset.actualVariant === asset.variant;
export const cachedOriginal = (assets: MediaAsset[]): MediaAsset | undefined =>
  assets.find((asset) => asset.variant === "original" && cachedVariant(asset));
export const canRetrieve = (asset: MediaAsset): boolean =>
  asset.retryable &&
  !["available", "expired", "unsupported"].includes(asset.state);

// Policy comes from the hub; older hubs may only auto-fetch thumbnails and voice.
export const canAutoRetrieve = (asset: MediaAsset): boolean =>
  asset.autoDownloadAllowed === true ||
  (asset.autoDownloadAllowed === undefined &&
    (asset.variant === "thumbnail" ||
      (asset.kind === "voice" && asset.variant === "normal")));

export const imagePreview = (assets: MediaAsset[]): MediaAsset | undefined => {
  const standard = standardImage(assets);
  return standard && cachedVariant(standard)
    ? standard
    : assets.find((asset) => asset.variant === "thumbnail") ?? standard;
};

// Never silently save standard bytes when an original is advertised.
export const downloadAsset = (
  assets: MediaAsset[],
  displayed?: MediaAsset,
): MediaAsset | undefined =>
  assets.find((asset) => asset.variant === "original") ?? displayed;

// Thumbnail browsing never starts a full video transfer. Playback is admitted
// only for the selected viewer item, using normal quality when offered.
export const videoAsset = (
  assets: MediaAsset[],
  viewer: boolean,
): MediaAsset | undefined =>
  viewer
    ? cachedOriginal(assets) ??
      assets.find((asset) => asset.variant === "normal")
    : assets.find((asset) => asset.variant === "thumbnail");

// A received Channels card offers its post's video through the same viewer as a
// video message; its cover is the card's thumbnail.
export const isChannelsCard = (
  message: import("@agent-wechat/shared").NativeMessage,
): boolean =>
  message.type === 49 && message.appMessageType === 51 && !!message.link;
export const isVideoMessage = (
  message: import("@agent-wechat/shared").NativeMessage,
): boolean => message.type === 43 || isChannelsCard(message);

export const conversationMedia = (
  messages: import("@agent-wechat/shared").NativeMessage[],
  account: string,
  thread: string,
) =>
  messages.filter(
    (message) =>
      message.accountId === account &&
      message.threadId === thread &&
      (message.type === 3 || isVideoMessage(message)) &&
      !message.recall,
  );
