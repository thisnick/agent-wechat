import type {
  MediaAsset,
  MediaJob,
  NativeWeChatClient,
} from "@agent-wechat/shared";
import type { BrowserCache } from "./browser-cache.ts";
import { MediaMetadata } from "./media-metadata.ts";
import { canAutoRetrieve, canRetrieve } from "./media-variants.ts";
import { MediaPreviewCache } from "./media-preview-cache.ts";
import { pause, readableError } from "./session.ts";

export const mediaJobEnded = (job: MediaJob): boolean =>
  [
    "available",
    "accepted",
    "failed",
    "cancelled",
    "needs_review",
    "partial",
    "expired",
    "unsupported",
  ].includes(job.state);

type MediaState = {
  assets: MediaAsset[];
  jobs: Record<string, MediaJob>;
  starting: string[];
  loaded: boolean;
  error: string;
};

/** Shared by the conversation and photo viewer; a request survives closing either view. */
export class IncomingMedia {
  private client: NativeWeChatClient;
  readonly account: string;
  readonly messageId: string;
  private interval: number;
  private controller = new AbortController();
  private listeners = new Set<() => void>();
  private pending: Promise<void> | undefined;
  private refreshPending = false;
  private policyGeneration = 0;
  private pendingPolicyGeneration = 0;
  private requested = new Set<string>();
  private value: MediaState = {
    assets: [],
    jobs: {},
    starting: [],
    loaded: false,
    error: "",
  };
  constructor(
    client: NativeWeChatClient,
    account: string,
    messageId: string,
    interval = 700,
    lookup?: MediaMetadata["lookup"],
  ) {
    this.client = client;
    this.account = account;
    this.messageId = messageId;
    this.interval = interval;
    this.lookup =
      lookup ??
      ((account, message) => this.client.mediaAssets(account, message));
  }
  private lookup: MediaMetadata["lookup"];
  snapshot = (): MediaState => this.value;
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  private publish(change: Partial<MediaState>): void {
    if (this.controller.signal.aborted) return;
    this.value = { ...this.value, ...change };
    for (const listener of this.listeners) listener();
  }
  load(force = false): Promise<void> {
    if (this.pending) {
      this.refreshPending ||=
        force || this.pendingPolicyGeneration !== this.policyGeneration;
      return this.pending;
    }
    if (this.controller.signal.aborted || (this.value.loaded && !force))
      return Promise.resolve();
    const policyGeneration = this.policyGeneration;
    this.pendingPolicyGeneration = policyGeneration;
    this.pending = this.lookup(
      this.account,
      this.messageId,
      this.controller.signal,
    )
      .then((response) => {
        if (policyGeneration === this.policyGeneration)
          this.publish({ assets: response.items, loaded: true, error: "" });
      })
      .catch((error) => {
        if (policyGeneration === this.policyGeneration)
          this.publish({ loaded: true, error: readableError(error) });
      })
      .finally(() => {
        this.pending = undefined;
        if (this.refreshPending) {
          this.refreshPending = false;
          void this.load(true);
        }
      });
    return this.pending;
  }
  working(asset: MediaAsset): boolean {
    const job = this.value.jobs[asset.id];
    return (
      this.value.starting.includes(asset.id) || (!!job && !mediaJobEnded(job))
    );
  }
  refreshPolicy(): void {
    this.policyGeneration++;
    this.publish({
      loaded: false,
      assets: this.value.assets.map((asset) => ({
        ...asset,
        autoDownloadAllowed: false,
      })),
    });
    if (this.observed()) void this.load(true);
  }
  ensure(asset: MediaAsset): void {
    asset =
      this.value.assets.find((current) => current.id === asset.id) ?? asset;
    if (
      !canAutoRetrieve(asset) ||
      !canRetrieve(asset) ||
      this.requested.has(asset.id)
    )
      return;
    this.requested.add(asset.id);
    void this.retrieve(asset);
  }
  async retrieve(asset: MediaAsset): Promise<void> {
    if (this.working(asset) || this.controller.signal.aborted) return;
    this.publish({ starting: [...this.value.starting, asset.id], error: "" });
    try {
      const previous = this.value.jobs[asset.id];
      let job =
        previous?.retryable && mediaJobEnded(previous)
          ? await this.client.retryJob(this.account, previous.id)
          : await this.client.transferMedia(this.account, {
              messageId: this.messageId,
              assetId: asset.id,
              quality: asset.variant,
            });
      this.publish({ jobs: { ...this.value.jobs, [asset.id]: job } });
      while (!this.controller.signal.aborted && !mediaJobEnded(job)) {
        await pause(this.interval, this.controller.signal);
        if (this.controller.signal.aborted) return;
        job = await this.client.job(
          this.account,
          job.id,
          this.controller.signal,
        );
        this.publish({ jobs: { ...this.value.jobs, [asset.id]: job } });
      }
      if (!this.controller.signal.aborted) await this.load(true);
    } catch (error) {
      this.publish({ error: readableError(error) });
    } finally {
      this.publish({
        starting: this.value.starting.filter((id) => id !== asset.id),
      });
    }
  }
  async cancel(asset: MediaAsset): Promise<void> {
    const job = this.value.jobs[asset.id];
    if (!job) return;
    try {
      const current = await this.client.cancelJob(this.account, job.id);
      this.publish({ jobs: { ...this.value.jobs, [asset.id]: current } });
    } catch (error) {
      this.publish({ error: readableError(error) });
    }
  }
  observed(): boolean {
    return this.listeners.size > 0;
  }
  idle(): boolean {
    return (
      this.value.loaded &&
      !this.listeners.size &&
      !this.pending &&
      !this.value.starting.length
    );
  }
  close(): void {
    this.controller.abort();
    this.listeners.clear();
  }
}

export class MediaInbox {
  private entries = new Map<string, IncomingMedia>();
  private client: NativeWeChatClient;
  private metadata: MediaMetadata;
  readonly previews: MediaPreviewCache;
  constructor(client: NativeWeChatClient, persistent?: BrowserCache) {
    this.client = client;
    this.metadata = new MediaMetadata(client);
    this.previews = new MediaPreviewCache(
      client,
      undefined,
      undefined,
      persistent,
    );
  }
  get(account: string, messageId: string): IncomingMedia {
    const key = JSON.stringify([account, messageId]);
    let entry = this.entries.get(key);
    if (!entry)
      entry = new IncomingMedia(
        this.client,
        account,
        messageId,
        undefined,
        this.metadata.lookup,
      );
    this.entries.delete(key);
    this.entries.set(key, entry);
    for (const [oldKey, old] of this.entries) {
      if (this.entries.size <= 200) break;
      if (oldKey !== key && old.idle()) {
        old.close();
        this.entries.delete(oldKey);
      }
    }
    return entry;
  }
  refreshAccount(account: string): void {
    for (const entry of this.entries.values())
      if (entry.account === account) entry.refreshPolicy();
  }
  ready(assetId: string): void {
    for (const entry of this.entries.values())
      if (entry.snapshot().assets.some((asset) => asset.id === assetId))
        void entry.load(true);
  }
  invalidateMessage(account: string, messageId: string): void {
    this.previews.invalidateMessage(account, messageId);
    const key = JSON.stringify([account, messageId]);
    this.entries.get(key)?.close();
    this.entries.delete(key);
  }
  clear(account: string): void {
    for (const [key, entry] of this.entries)
      if (entry.account === account) {
        entry.close();
        this.entries.delete(key);
      }
    this.previews.clear(account);
  }
  close(): void {
    this.metadata.close();
    for (const entry of this.entries.values()) entry.close();
    this.entries.clear();
    this.previews.close();
  }
}
