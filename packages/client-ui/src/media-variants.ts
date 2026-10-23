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

// Never silently save standard bytes when an original is advertised.
export const downloadAsset = (
  assets: MediaAsset[],
  displayed?: MediaAsset,
): MediaAsset | undefined =>
  assets.find((asset) => asset.variant === "original") ?? displayed;

export const conversationPhotos = (
  messages: import("@agent-wechat/shared").NativeMessage[],
  account: string,
  thread: string,
) =>
  messages.filter(
    (message) =>
      message.accountId === account &&
      message.threadId === thread &&
      message.type === 3 &&
      !message.recall,
  );
