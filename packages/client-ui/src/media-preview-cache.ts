import {
  NATIVE_MAX_SOURCE_BYTES,
  type MediaAsset,
  type NativeWeChatClient,
} from "@agent-wechat/shared";
import type { BrowserCache } from "./browser-cache.ts";
import { cachedVariant } from "./media-variants.ts";

type Preview = { url: string; contentType: string; byteLength: number };
type Entry = {
  account: string;
  message: string | null;
  users: number;
  controller: AbortController;
  ready: Promise<Preview>;
  value?: Preview;
};

export function mediaPreviewIdentity(asset: MediaAsset): string {
  return JSON.stringify([
    asset.accountId,
    asset.messageId,
    asset.id,
    asset.variant,
    asset.actualVariant,
    asset.sha256,
    asset.preview ? "preview" : "original",
    asset.preview?.sha256,
    asset.preview?.sourceSha256,
    asset.preview?.contentType ?? asset.contentType,
    asset.preview?.byteLength ?? asset.byteLength,
  ]);
}

/** Connection-local blob URLs, shared by chat, viewer and thumbnail consumers.
 * Visible consumers pin their URLs; idle entries are evicted over budget.
 */
export class MediaPreviewCache {
  private entries = new Map<string, Entry>();
  private bytes = 0;
  private closed = false;
  private client: NativeWeChatClient;
  private maxBytes: number;
  private maxEntries: number;
  private persistent?: BrowserCache;
  constructor(
    client: NativeWeChatClient,
    maxBytes = 64 * 1024 * 1024,
    maxEntries = 128,
    persistent?: BrowserCache,
  ) {
    this.client = client;
    this.maxBytes = maxBytes;
    this.maxEntries = maxEntries;
    this.persistent = persistent;
  }

  peek(asset: MediaAsset): Preview | undefined {
    return cachedVariant(asset)
      ? this.entries.get(mediaPreviewIdentity(asset))?.value
      : undefined;
  }

  acquire(asset: MediaAsset): {
    ready: Promise<Preview>;
    release: () => void;
  } {
    if (this.closed || !cachedVariant(asset))
      throw new Error("Media preview is not available");
    const key = mediaPreviewIdentity(asset);
    let entry = this.entries.get(key);
    if (!entry) {
      const controller = new AbortController();
      const loading: Entry = {
        account: asset.accountId,
        message: asset.messageId,
        users: 0,
        controller,
        ready: Promise.resolve()
          .then(async () => {
            const verified = !!(asset.preview
              ? asset.preview.sha256
              : asset.sha256);
            const saved =
              verified && this.persistent
                ? await this.persistent.blob("media", asset.accountId, key)
                : undefined;
            if (this.closed || this.entries.get(key) !== loading)
              throw new DOMException("Preview was discarded", "AbortError");
            if (saved) return saved.blob;
            const content = await this.client.mediaContent(
              asset.accountId,
              asset.id,
              {
                representation: asset.preview ? "preview" : "original",
                maxBytes: NATIVE_MAX_SOURCE_BYTES,
                signal: controller.signal,
              },
            );
            if (this.closed || this.entries.get(key) !== loading)
              throw new DOMException("Preview was discarded", "AbortError");
            const blob = new Blob([new Uint8Array(content.bytes).buffer], {
              type: content.contentType,
            });
            // Signed/temporary content URLs are deliberately not persisted.
            if (verified)
              this.persistent?.saveBlob(
                "media",
                asset.accountId,
                key,
                { blob, checkedAt: Date.now() },
                asset.messageId ?? undefined,
              );
            return blob;
          })
          .then((blob) => {
            if (this.closed || this.entries.get(key) !== loading)
              throw new DOMException("Preview was discarded", "AbortError");
            loading.value = {
              url: URL.createObjectURL(blob),
              contentType: blob.type,
              byteLength: blob.size,
            };
            this.bytes += blob.size;
            this.trim();
            return loading.value;
          })
          .catch((error) => {
            if (this.entries.get(key) === loading) this.discard(key, loading);
            throw error;
          }),
      };
      entry = loading;
      this.entries.set(key, loading);
    }
    const held = entry;
    held.users++;
    this.entries.delete(key);
    this.entries.set(key, held);
    this.trim();
    let released = false;
    return {
      ready: held.ready,
      release: () => {
        if (released) return;
        released = true;
        held.users--;
        this.trim();
      },
    };
  }

  invalidateMessage(account: string, message: string): void {
    this.persistent?.invalidateMessage(account, message);
    for (const [key, entry] of this.entries)
      if (entry.account === account && entry.message === message)
        this.discard(key, entry);
  }
  clear(account?: string): void {
    for (const [key, entry] of this.entries)
      if (!account || entry.account === account) this.discard(key, entry);
  }
  close(): void {
    this.closed = true;
    this.clear();
  }
  private discard(key: string, entry: Entry): void {
    this.entries.delete(key);
    entry.controller.abort();
    if (entry.value) {
      this.bytes -= entry.value.byteLength;
      URL.revokeObjectURL(entry.value.url);
    }
  }
  private trim(): void {
    for (const [key, entry] of this.entries) {
      if (this.bytes <= this.maxBytes && this.entries.size <= this.maxEntries)
        break;
      if (!entry.users) this.discard(key, entry);
    }
  }
}
