import {
  type MediaJob,
  type MediaKind,
  type NativeWeChatClient,
  type VideoSendQuality,
} from "@agent-wechat/shared";
import { pause, readableError } from "./session.ts";
import { validateAttachmentSource } from "./composer-attachments.ts";

export type PendingMedia = {
  key: string;
  threadId: string;
  name: string;
  kind: MediaKind;
  size: number;
  videoQuality?: VideoSendQuality;
  stage:
    | "uploading"
    | "submitting"
    | "checking"
    | "failed"
    | "cancelled"
    | "job";
  sourceId?: string;
  job?: MediaJob;
  uncertain?: boolean;
  error?: string;
};
type FileSource = {
  name: string;
  type: string;
  size: number;
  arrayBuffer(): Promise<ArrayBuffer>;
};
type Storage = { read(): PendingMedia[]; write(value: PendingMedia[]): void };
const ended = new Set([
  "accepted",
  "failed",
  "cancelled",
  "needs_review",
  "partial",
]);

/** One account owns each queue. Checking an uncertain submission never sends it again. */
export class MediaOutbox {
  private client: NativeWeChatClient;
  private account: string;
  private storage: Storage;
  private value: PendingMedia[];
  private listeners = new Set<() => void>();
  private controller = new AbortController();
  private uploads = new Map<string, AbortController>();
  private watching = new Set<string>();
  constructor(client: NativeWeChatClient, account: string, storage: Storage) {
    this.client = client;
    this.account = account;
    this.storage = storage;
    this.value = storage.read().map((item) =>
      item.stage === "uploading"
        ? {
            ...item,
            stage: "failed",
            error: "Upload interrupted. Choose the file again.",
          }
        : item,
    );
    queueMicrotask(() => {
      for (const item of this.value)
        if (item.sourceId || item.job) void this.check(item.key);
    });
  }
  snapshot = (): PendingMedia[] => this.value;
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  private update(key: string, patch: Partial<PendingMedia>): void {
    if (this.controller.signal.aborted) return;
    this.value = this.value.map((item) =>
      item.key === key ? { ...item, ...patch } : item,
    );
    this.storage.write(this.value.slice(-50));
    for (const listener of this.listeners) listener();
  }
  close(): void {
    this.controller.abort();
    this.listeners.clear();
  }
  dismiss(key: string): void {
    this.value = this.value.filter((item) => item.key !== key);
    this.storage.write(this.value);
    for (const listener of this.listeners) listener();
  }
  async send(
    threadId: string,
    file: FileSource,
    kind: MediaKind,
    videoQuality: VideoSendQuality = "normal",
  ): Promise<void> {
    if (this.controller.signal.aborted) return;
    if (!threadId || !["file", "image", "video", "voice"].includes(kind))
      throw new Error("Choose a conversation and attachment type.");
    validateAttachmentSource(file, kind === "voice" ? "image" : kind as "image" | "video" | "file");
    const key = crypto.randomUUID();
    this.value = [
      ...this.value,
      {
        key,
        threadId,
        name: file.name,
        kind,
        size: file.size,
        ...(kind === "video" ? { videoQuality } : {}),
        stage: "uploading",
      },
    ];
    this.update(key, {});
    const upload = new AbortController();
    this.uploads.set(key, upload);
    let submitted = false;
    try {
      const source = await this.client.upload(
        this.account,
        file instanceof Blob ? file : new Uint8Array(await file.arrayBuffer()),
        file.name,
        file.type || "application/octet-stream",
        AbortSignal.any([upload.signal, this.controller.signal]),
      );
      if (upload.signal.aborted || this.controller.signal.aborted) return;
      this.update(key, { sourceId: source.id, stage: "submitting" });
      submitted = true;
      const job = await this.client.sendMedia(
        this.account,
        {
          threadId,
          kind,
          sourceId: source.id,
          standardSourceId: null,
          thumbnailSourceId: null,
          playSeconds: null,
          durationMs: null,
          ...(kind === "video" ? { videoQuality } : {}),
        },
        key,
      );
      this.accept(key, job);
      void this.watch(key);
    } catch (error) {
      if (!upload.signal.aborted)
        this.update(key, {
          stage: submitted ? "checking" : "failed",
          uncertain: submitted,
          error: readableError(error),
        });
    } finally {
      this.uploads.delete(key);
    }
  }
  private accept(key: string, job: MediaJob): void {
    if (
      !job ||
      job.accountId !== this.account ||
      job.kind !== "send" ||
      !job.id
    )
      throw new Error("Attachment receipt belongs to another account.");
    this.update(key, {
      job,
      stage: "job",
      uncertain: job.state === "needs_review",
      error: job.state === "failed"
        ? job.reason === "image_decode_limit"
          ? "Image is too large to process. Resize it or send it as a file."
          : job.reason === "image_decode_failed"
            ? "Couldn't read this image. Try exporting it as JPEG or PNG."
            : undefined
        : undefined,
    });
  }
  async check(key: string): Promise<void> {
    const item = this.value.find((item) => item.key === key);
    if (!item || this.controller.signal.aborted) return;
    try {
      const job = item.job
        ? await this.client.job(
            this.account,
            item.job.id,
            this.controller.signal,
          )
        : await this.client.sendByKey(this.account, key);
      if (!("phase" in job)) throw new Error("Expected an attachment receipt.");
      this.accept(key, job);
      void this.watch(key);
    } catch (error) {
      this.update(key, { error: readableError(error) });
    }
  }
  private async watch(key: string): Promise<void> {
    if (this.watching.has(key)) return;
    this.watching.add(key);
    try {
      while (!this.controller.signal.aborted) {
        const item = this.value.find((item) => item.key === key);
        if (!item?.job || ended.has(item.job.state)) return;
        await pause(800, this.controller.signal);
        if (this.controller.signal.aborted) return;
        const job = await this.client.job(
          this.account,
          item.job.id,
          this.controller.signal,
        );
        this.accept(key, job);
      }
    } catch (error) {
      this.update(key, { error: readableError(error) });
    } finally {
      this.watching.delete(key);
    }
  }
  async cancel(key: string): Promise<void> {
    const item = this.value.find((item) => item.key === key);
    if (!item) return;
    const upload = this.uploads.get(key);
    if (upload && !item.sourceId) {
      upload.abort();
      this.update(key, { stage: "cancelled" });
      return;
    }
    if (!item.job) return;
    try {
      this.accept(key, await this.client.cancelJob(this.account, item.job.id));
    } catch (error) {
      this.update(key, { error: readableError(error) });
    }
  }
  async retry(key: string): Promise<void> {
    const item = this.value.find((item) => item.key === key);
    if (!item?.job?.retryable || item.uncertain) return;
    try {
      this.accept(key, await this.client.retryJob(this.account, item.job.id));
      void this.watch(key);
    } catch (error) {
      this.update(key, { error: readableError(error) });
    }
  }
}
