import {
  useEffect,
  useId,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { createPortal } from "react-dom";
import type { NativeMessage } from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { canRecall } from "./message-recall.ts";
import { messagePresentation } from "./message-presentation.ts";
import { useMediaViewer } from "./media-viewer-context.ts";
import { useReply } from "./reply-context.ts";
import { canReply } from "./reply.ts";
import { messageSummary } from "./thread-preview.ts";
import { Button, Icon } from "./ui.tsx";

export function MessageActions({
  message,
  connection,
}: {
  message: NativeMessage;
  connection: Connection;
}) {
  const gallery = useMediaViewer();
  const composer = useReply();
  const presentation = messagePresentation(message);
  const copyable =
    ["text", "reply", "forwarded", "link"].includes(presentation) &&
    !!message.body;
  const photo = [3, 43].includes(message.type) && !message.recall && gallery;
  const [status, setStatus] = useState("");
  const [open, setOpen] = useState(false);
  const menu = useRef<HTMLDetailsElement>(null);
  useEffect(() => {
    if (!open) return;
    const dismiss = (event: PointerEvent) => {
      if (menu.current && !menu.current.contains(event.target as Node))
        menu.current.open = false;
    };
    document.addEventListener("pointerdown", dismiss);
    return () => document.removeEventListener("pointerdown", dismiss);
  }, [open]);
  useEffect(() => {
    if (!status) return;
    const timer = setTimeout(() => setStatus(""), 2000);
    return () => clearTimeout(timer);
  }, [status]);
  return (
    <>
      <div
        className="message-actions"
        role="toolbar"
        aria-label="Message actions"
      >
        {composer?.enabled && canReply(message) && (
          <Button
            icon="reply"
            label="Reply"
            onClick={() => composer.reply(message)}
          />
        )}
        {canRecall(message) && (
          <RecallButton message={message} connection={connection} />
        )}
        {(copyable || photo) && (
          <details
            className="message-menu"
            ref={menu}
            onToggle={(event) => setOpen(event.currentTarget.open)}
            onKeyDown={(event) => {
              if (event.key === "Escape" && menu.current) {
                menu.current.open = false;
                menu.current.querySelector("summary")?.focus();
                event.stopPropagation();
              }
            }}
          >
            <summary aria-label="More message actions" title="More">
              <Icon name="more" />
            </summary>
            <div className="message-menu-items">
              {copyable && (
                <button
                  onClick={() => {
                    void navigator.clipboard
                      .writeText(message.body!)
                      .then(() => setStatus("Copied"))
                      .catch(() => setStatus("Couldn’t copy text."));
                    if (menu.current) menu.current.open = false;
                  }}
                >
                  Copy text
                </button>
              )}
              {photo && (
                <button
                  onClick={() => {
                    gallery.open(message);
                    if (menu.current) menu.current.open = false;
                  }}
                >
                  {message.type === 43 ? "Open video" : "Open photo"}
                </button>
              )}
            </div>
          </details>
        )}
      </div>
      {status && (
        <small className="message-action-status" role="status">
          {status}
        </small>
      )}
    </>
  );
}

function RecallButton({
  message,
  connection,
}: {
  message: NativeMessage;
  connection: Connection;
}) {
  const model = connection.messageRecall(message);
  const state = useSyncExternalStore(model.subscribe, model.snapshot);
  const [confirming, setConfirming] = useState(false);
  useEffect(() => {
    if (state.operation?.state === "succeeded")
      void connection.session
        .refreshMessage(message.accountId, message.id)
        .catch(() => {});
  }, [
    connection.session,
    message.accountId,
    message.id,
    state.operation?.state,
  ]);
  const label = state.busy
    ? "Recalling…"
    : state.uncertain
      ? "Check recall"
      : "Recall";
  return (
    <>
      <span className="recall-action">
        <Button
          icon="recall"
          label={label}
          disabled={state.busy || (!!state.operation && !state.uncertain)}
          onClick={() => {
            if (state.operation) void model.check();
            else setConfirming(true);
          }}
        />
        {state.error && (
          <span className="recall-error" role="status">
            {state.error}
          </span>
        )}
      </span>
      {confirming && (
        <RecallConfirmation
          message={message}
          close={() => setConfirming(false)}
          confirm={() => {
            setConfirming(false);
            const current = connection.session.snapshot();
            const target = current.messages.find(
              (item) => item.id === message.id,
            );
            if (
              current.account === message.accountId &&
              current.selected === message.threadId &&
              target &&
              target.serverId === message.serverId &&
              canRecall(target)
            )
              void model.recall();
          }}
        />
      )}
    </>
  );
}

function RecallConfirmation({
  message,
  close,
  confirm,
}: {
  message: NativeMessage;
  close: () => void;
  confirm: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const heading = useId();
  const description = useId();
  useEffect(() => {
    const target = dialog.current;
    const focused = document.activeElement;
    target?.showModal();
    return () => {
      target?.close();
      if (focused instanceof HTMLElement && focused.isConnected)
        focused.focus();
    };
  }, []);
  return createPortal(
    <dialog
      ref={dialog}
      className="modal recall-confirm"
      aria-labelledby={heading}
      aria-describedby={description}
      onCancel={(event) => {
        event.preventDefault();
        close();
      }}
    >
      <h2 id={heading}>Recall this message?</h2>
      <p id={description}>This can’t be undone.</p>
      <blockquote className="recall-preview">
        {messageSummary(message, "You").text.slice(0, 240)}
      </blockquote>
      <div className="recall-confirm-actions">
        <button className="secondary" autoFocus onClick={close}>
          Cancel
        </button>
        <button className="primary" onClick={confirm}>
          Recall
        </button>
      </div>
    </dialog>,
    document.body,
  );
}
