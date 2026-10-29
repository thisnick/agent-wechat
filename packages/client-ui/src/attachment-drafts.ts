import type { VideoSendQuality } from "@agent-wechat/shared";
import type { MediaOutbox } from "./media-outbox.ts";
import {
  MAX_COMPOSER_ATTACHMENTS,
  reviewAttachments,
  type ReviewedAttachment,
  type AttachmentLimits,
} from "./composer-attachments.ts";

export type StagedAttachment = ReviewedAttachment & { id: string };
export type AttachmentDraft = {
  files: readonly StagedAttachment[];
  videoQuality: VideoSendQuality;
  submission?: string;
};
export type AttachmentBatch = AttachmentDraft & {
  account: string;
  thread: string;
  submission: string;
};
const empty: AttachmentDraft = { files: [], videoQuality: "normal" };
const key = (account: string, thread: string) =>
  JSON.stringify([account, thread]);

/** Unsent File handles stay in memory on this connection, never in receipt storage. */
export class AttachmentDrafts {
  private value: Record<string, AttachmentDraft> = {};
  private listeners = new Set<() => void>();
  snapshot = () => this.value;
  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };
  get(account: string, thread: string): AttachmentDraft {
    return this.value[key(account, thread)] ?? empty;
  }
  private put(account: string, thread: string, draft?: AttachmentDraft) {
    this.value = { ...this.value };
    if (draft) this.value[key(account, thread)] = draft;
    else delete this.value[key(account, thread)];
    for (const listener of this.listeners) listener();
  }
  add(
    account: string,
    thread: string,
    files: readonly File[],
    supported: readonly string[],
    limits?: AttachmentLimits,
  ) {
    const draft = this.get(account, thread);
    if (draft.submission)
      throw new Error("Wait for the current message to be queued.");
    const reviewed = reviewAttachments(files, supported, limits);
    if (draft.files.length + reviewed.length > MAX_COMPOSER_ATTACHMENTS)
      throw new Error("Stage up to 10 files in one message draft.");
    this.put(account, thread, {
      ...draft,
      files: [
        ...draft.files,
        ...reviewed.map((item) => ({ ...item, id: crypto.randomUUID() })),
      ],
    });
  }
  remove(account: string, thread: string, id: string) {
    const draft = this.get(account, thread);
    if (draft.submission) return;
    const files = draft.files.filter((item) => item.id !== id);
    this.put(account, thread, files.length ? { ...draft, files } : undefined);
  }
  quality(account: string, thread: string, videoQuality: VideoSendQuality) {
    const draft = this.get(account, thread);
    if (!draft.submission && draft.files.length)
      this.put(account, thread, { ...draft, videoQuality });
  }
  claim(account: string, thread: string): AttachmentBatch | null {
    const draft = this.get(account, thread);
    if (!draft.files.length || draft.submission) return null;
    const batch = {
      ...draft,
      account,
      thread,
      submission: crypto.randomUUID(),
    };
    this.put(account, thread, batch);
    return batch;
  }
  finish(batch: AttachmentBatch, consumed: boolean): boolean {
    if (this.get(batch.account, batch.thread).submission !== batch.submission)
      return false; // Sign-out/connection cleanup must not resurrect a draft.
    this.put(
      batch.account,
      batch.thread,
      consumed || !batch.files.length
        ? undefined
        : {
            files: batch.files,
            videoQuality: batch.videoQuality,
          },
    );
    return true;
  }
  clear(account?: string) {
    this.value =
      account === undefined
        ? {}
        : Object.fromEntries(
            Object.entries(this.value).filter(
              ([target]) => JSON.parse(target)[0] !== account,
            ),
          );
    for (const listener of this.listeners) listener();
  }
}

/** Each selected file gets its own existing outbox operation and receipt. */
export async function sendAttachmentBatch(
  outbox: Pick<MediaOutbox, "send">,
  batch: AttachmentBatch,
) {
  for (const item of batch.files)
    await outbox.send(batch.thread, item.file, item.kind, batch.videoQuality);
}
