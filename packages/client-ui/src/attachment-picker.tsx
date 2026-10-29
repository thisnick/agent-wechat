import { useEffect, useRef, useState, type RefObject } from "react";
import { createPortal } from "react-dom";
import type { Connection } from "./connection.ts";
import { Button, Icon } from "./ui.tsx";
import {
  bindComposerAttachments,
  reviewAttachments,
  type ReviewedAttachment,
} from "./composer-attachments.ts";
import { readableError } from "./session.ts";

type Review = {
  files: ReviewedAttachment[];
  threadId: string;
  recipient: string;
};
export function AttachmentPicker({
  connection,
  account,
  threadId,
  recipient,
  enabled,
  mediaKinds,
  composerRef,
}: {
  connection: Connection;
  account: string;
  threadId: string | null;
  recipient: string;
  enabled: boolean;
  mediaKinds: readonly string[];
  composerRef: RefObject<HTMLDivElement | null>;
}) {
  const input = useRef<HTMLInputElement>(null),
    dialog = useRef<HTMLElement>(null),
    sendButton = useRef<HTMLButtonElement>(null);
  const [review, setReview] = useState<Review | null>(null),
    [error, setError] = useState(""),
    [dragging, setDragging] = useState(false);
  const pending = useRef(review);
  pending.current = review;
  const outbox = connection.mediaOutbox(account);
  function choose(files: File[]) {
    if (!enabled || !threadId) {
      setError("Attachments aren't available in this conversation right now.");
      return;
    }
    try {
      const filesToSend = reviewAttachments(files, mediaKinds);
      setError("");
      setReview({ files: filesToSend, threadId, recipient });
    } catch (error) {
      setError(readableError(error));
    }
  }
  useEffect(() => {
    const root = composerRef.current;
    if (root)
      return bindComposerAttachments(root, choose, (value) =>
        setDragging(value && enabled && !!threadId)
      );
  }, [
    connection,
    account,
    threadId,
    recipient,
    enabled,
    mediaKinds,
    composerRef,
  ]);
  useEffect(() => {
    if (!enabled) {
      setReview(null);
      setDragging(false);
    }
  }, [enabled]);
  useEffect(() => {
    if (!review) return;
    const previous = document.activeElement;
    sendButton.current?.focus();
    const keyboard = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        setReview(null);
      }
      if (event.key !== "Tab") return;
      const buttons = Array.from(
        dialog.current?.querySelectorAll<HTMLButtonElement>(
          "button:not(:disabled)"
        ) ?? []
      );
      const first = buttons[0],
        last = buttons.at(-1);
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last?.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first?.focus();
      }
    };
    document.addEventListener("keydown", keyboard, true);
    return () => {
      document.removeEventListener("keydown", keyboard, true);
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus({ preventScroll: true });
    };
  }, [review]);
  function send() {
    const target = pending.current;
    if (!enabled || !target) return;
    pending.current = null;
    setReview(null);
    void connection.session.readOnInteraction(account, target.threadId);
    // Keep a confirmed batch on its original account and conversation, even
    // if navigation changes while an earlier file is uploading.
    void (async () => {
      for (const item of target.files)
        await outbox.send(target.threadId, item.file, item.kind);
    })().catch((error) => setError(readableError(error)));
  }
  return (
    <>
      <Button
        icon="attach"
        label="Attach file"
        disabled={!enabled || !threadId}
        onClick={() => input.current?.click()}
      />
      <input
        ref={input}
        className="attachment-input"
        type="file"
        multiple
        aria-label="Choose attachment"
        onChange={(event) => {
          const files = Array.from(event.target.files ?? []);
          event.target.value = "";
          if (files.length) choose(files);
        }}
      />
      {dragging && (
        <span className="composer-drop-hint" role="status">
          <Icon name="attach" />
          Drop to attach
        </span>
      )}
      {error && (
        <span className="attachment-error" role="alert">
          {error}
        </span>
      )}
      {review &&
        createPortal(
          <div className="overlay-backdrop" onClick={() => setReview(null)}>
            <section
              ref={dialog}
              className="modal attachment-review"
              role="dialog"
              aria-modal="true"
              aria-label="Send attachment"
              onClick={(event) => event.stopPropagation()}
            >
              <div className="modal-head">
                <h2>
                  {review.files.length === 1
                    ? "Send attachment"
                    : "Send attachments"}
                </h2>
                <Button
                  icon="close"
                  label="Close attachment"
                  onClick={() => setReview(null)}
                />
              </div>
              <div className="modal-body">
                <p>
                  To <strong>{review.recipient}</strong>
                </p>
                <ul className="attachment-review-list">
                  {review.files.map((item, index) => (
                    <li key={index}>
                      <AttachmentPreview item={item} />
                    </li>
                  ))}
                </ul>
                <div className="attachment-actions">
                  <button className="secondary" onClick={() => setReview(null)}>
                    Cancel
                  </button>
                  <button
                    ref={sendButton}
                    className="primary"
                    disabled={!enabled}
                    onClick={send}
                  >
                    Send
                  </button>
                </div>
              </div>
            </section>
          </div>,
          document.body
        )}
    </>
  );
}
function AttachmentPreview({
  item: { file, kind },
}: {
  item: ReviewedAttachment;
}) {
  const [url, setUrl] = useState("");
  useEffect(() => {
    if (kind === "file") return;
    const object = URL.createObjectURL(file);
    setUrl(object);
    return () => URL.revokeObjectURL(object);
  }, [file, kind]);
  const size =
    file.size < 1024 * 1024
      ? `${Math.ceil(file.size / 1024)} KB`
      : `${(file.size / 1024 / 1024).toFixed(1)} MB`;
  return (
    <>
      {url && kind === "image" && (
        <img
          className="attachment-review-preview"
          src={url}
          alt="Image to send"
        />
      )}
      {url && kind === "video" && (
        <video
          className="attachment-review-preview"
          src={url}
          controls
          preload="metadata"
          aria-label="Video to send"
        />
      )}
      <div className="attachment-file">
        <Icon
          name={kind === "image" ? "image" : kind === "video" ? "play" : "file"}
        />
        <span>
          <strong>{file.name}</strong>
          <small>
            {kind === "file" ? "File" : kind === "image" ? "Image" : "Video"} ·{" "}
            {size}
          </small>
        </span>
      </div>
    </>
  );
}
