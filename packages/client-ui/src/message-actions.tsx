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
import { t, useLocale } from "./i18n.ts";
import { canRecall } from "./message-recall.ts";
import { messagePresentation } from "./message-presentation.ts";
import { useMediaViewer } from "./media-viewer-context.ts";
import { useReply } from "./reply-context.ts";
import { canReply } from "./reply.ts";
import { messageSummary } from "./thread-preview.ts";
import { emojiText } from "./emoji-text.ts";
import { Button, Icon } from "./ui.tsx";

export function MessageActions({
  message,
  connection,
}: {
  message: NativeMessage;
  connection: Connection;
}) {
  useLocale();
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
        aria-label={t("Message actions", "消息操作")}
      >
        {composer?.enabled && canReply(message) && (
          <Button
            icon="reply"
            label={t("Reply", "回复")}
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
            <summary
              aria-label={t("More message actions", "更多消息操作")}
              title={t("More", "更多")}
            >
              <Icon name="more" />
            </summary>
            <div className="message-menu-items">
              {copyable && (
                <button
                  onClick={() => {
                    void navigator.clipboard
                      .writeText(message.body!)
                      .then(() => setStatus(t("Copied", "已复制")))
                      .catch(() =>
                        setStatus(t("Couldn’t copy text.", "无法复制文本。")),
                      );
                    if (menu.current) menu.current.open = false;
                  }}
                >
                  {t("Copy text", "复制文本")}
                </button>
              )}
              {photo && (
                <button
                  onClick={() => {
                    gallery.open(message);
                    if (menu.current) menu.current.open = false;
                  }}
                >
                  {message.type === 43
                    ? t("Open video", "打开视频")
                    : t("Open photo", "打开图片")}
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
  useLocale();
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
    ? t("Recalling…", "正在撤回…")
    : state.uncertain
      ? t("Check recall", "检查撤回状态")
      : t("Recall", "撤回");
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
  useLocale();
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
      <h2 id={heading}>{t("Recall this message?", "撤回这条消息？")}</h2>
      <p id={description}>{t("This can’t be undone.", "此操作无法撤销。")}</p>
      <blockquote className="recall-preview">
        {emojiText(messageSummary(message, t("You", "我")).text).slice(0, 240)}
      </blockquote>
      <div className="recall-confirm-actions">
        <button className="secondary" autoFocus onClick={close}>
          {t("Cancel", "取消")}
        </button>
        <button className="primary" onClick={confirm}>
          {t("Recall", "撤回")}
        </button>
      </div>
    </dialog>,
    document.body,
  );
}
