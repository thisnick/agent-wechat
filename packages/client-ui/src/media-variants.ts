import type { MediaAsset } from "@agent-wechat/shared";

// Display quality is stable even when the original becomes available later.
export const standardImage = (assets: MediaAsset[]): MediaAsset | undefined =>
  assets.find((asset) => asset.variant === "standard");
export const cachedVariant = (asset: MediaAsset): boolean =>
  asset.state === "available" && asset.actualVariant === asset.variant;
export const canRetrieve = (asset: MediaAsset): boolean =>
  asset.retryable &&
  !["available", "expired", "unsupported"].includes(asset.state);
