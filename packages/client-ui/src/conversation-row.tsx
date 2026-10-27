import {
  useId,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import { createPortal } from "react-dom";
import type { NativeThread } from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { ConversationAvatar } from "./avatars.tsx";
import { PinFeedback } from "./thread-pinning.tsx";
import { threadPreview } from "./thread-preview.ts";
import { Icon } from "./ui.tsx";

export function ConversationRow({
  connection,
  account,
  thread,
  name,
  selected,
  select,
  heading,
  divider,
}: {
  connection: Connection;
  account: string;
  thread: NativeThread;
  name: string;
  selected: boolean;
  select: () => void;
  heading?: boolean;
  divider?: boolean;
}) {
  const model = connection.threadPinning(account, thread.id);
  const pin = useSyncExternalStore(model.subscribe, model.snapshot);
  const [open, setOpen] = useState(false);
  const [focusLast, setFocusLast] = useState(false);
  const frame = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const menuId = useId();
  const focused = frame.current?.contains(document.activeElement)
    ? (document.activeElement as HTMLElement)
    : null;
  useLayoutEffect(() => {
    if (focused?.isConnected && document.activeElement === document.body)
      focused.focus({ preventScroll: true });
  });
  const close = (restore = false) => {
    setOpen(false);
    if (restore) trigger.current?.focus({ preventScroll: true });
  };
  useLayoutEffect(() => {
    if (!open) return;
    const items = menu.current?.querySelectorAll<HTMLElement>(
      '[role="menuitem"]:not(:disabled)',
    );
    (items?.[focusLast ? items.length - 1 : 0] ?? menu.current)?.focus();
    const dismiss = (event: PointerEvent) => {
      if (
        !menu.current?.contains(event.target as Node) &&
        !trigger.current?.contains(event.target as Node)
      )
        close();
    };
    const scrolled = (event: Event) => {
      const target = event.target;
      if (
        event.type === "resize" ||
        target === document ||
        (target instanceof Element &&
          frame.current &&
          target.contains(frame.current))
      )
        close(true);
    };
    document.addEventListener("pointerdown", dismiss);
    window.addEventListener("resize", scrolled);
    document.addEventListener("scroll", scrolled, true);
    return () => {
      document.removeEventListener("pointerdown", dismiss);
      window.removeEventListener("resize", scrolled);
      document.removeEventListener("scroll", scrolled, true);
    };
  }, [open, focusLast]);
  const anchor = trigger.current?.getBoundingClientRect();
  const label = pin.pinned ? "Unpin chat" : "Pin chat";
  return (
    <div className="thread-list-item" data-thread-id={thread.id}>
      {heading && <div className="pinned-heading">Pinned</div>}
      {divider && <div className="pinned-divider" aria-hidden="true" />}
      <div
        ref={frame}
        className="chat-row-frame"
        onContextMenu={(event) => {
          event.preventDefault();
          setFocusLast(false);
          setOpen(true);
        }}
        onKeyDown={(event) => {
          if (
            event.key === "ContextMenu" ||
            (event.shiftKey && event.key === "F10")
          ) {
            event.preventDefault();
            setFocusLast(false);
            setOpen(true);
          }
        }}
      >
        <button
          className={`chat-row ${selected ? "selected" : ""}`}
          onClick={select}
          aria-current={selected ? "true" : undefined}
        >
          <ConversationAvatar
            connection={connection}
            account={account}
            username={thread.id}
            name={name}
            members={thread.avatarMembers}
          />
          <div className="chat-copy">
            <div className="chat-top">
              <span className="chat-name">{name}</span>
              {thread.pinned === true && (
                <span className="thread-pin-icon" title="Pinned">
                  <Icon name="pin" />
                  <span className="sr-only">Pinned</span>
                </span>
              )}
            </div>
            <div className="preview-row">
              <span className="preview" title={threadPreview(thread)}>
                {threadPreview(thread)}
              </span>
              {!!thread.unreadCount && (
                <span className="count">{thread.unreadCount}</span>
              )}
            </div>
          </div>
        </button>
        <button
          ref={trigger}
          className="thread-row-more icon-btn"
          aria-label={`More actions for ${name}`}
          title="More actions"
          aria-haspopup="menu"
          aria-expanded={open}
          aria-controls={open ? menuId : undefined}
          onClick={() => {
            setFocusLast(false);
            setOpen(!open);
          }}
          onKeyDown={(event) => {
            if (["ArrowDown", "ArrowUp"].includes(event.key)) {
              event.preventDefault();
              setFocusLast(event.key === "ArrowUp");
              setOpen(true);
            }
          }}
        >
          {pin.busy ? (
            <span className="pin-spinner" aria-label="Saving pin" />
          ) : (
            <Icon name="more" />
          )}
        </button>
      </div>
      <PinFeedback model={model} />
      {open &&
        createPortal(
          <div
            ref={menu}
            id={menuId}
            role="menu"
            tabIndex={-1}
            aria-label={`Actions for ${name}`}
            className="thread-row-menu"
            style={{
              top: Math.max(
                8,
                Math.min((anchor?.bottom ?? 8) + 4, innerHeight - 155),
              ),
              left: Math.max(
                8,
                Math.min((anchor?.right ?? 8) - 180, innerWidth - 188),
              ),
            }}
            onKeyDown={(event) => {
              const items = [
                ...event.currentTarget.querySelectorAll<HTMLButtonElement>(
                  '[role="menuitem"]:not(:disabled)',
                ),
              ];
              const index = items.indexOf(
                document.activeElement as HTMLButtonElement,
              );
              if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
                event.preventDefault();
                const next =
                  event.key === "Home"
                    ? 0
                    : event.key === "End"
                    ? items.length - 1
                    : (index +
                        (event.key === "ArrowDown" ? 1 : -1) +
                        items.length) %
                      items.length;
                items[next]?.focus();
              } else if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                close(true);
              } else if (event.key === "Tab") close(true);
              else if (
                event.key.length === 1 &&
                event.key.trim() &&
                !event.ctrlKey &&
                !event.altKey &&
                !event.metaKey
              ) {
                const next = [
                  ...items.slice(index + 1),
                  ...items.slice(0, index + 1),
                ].find((item) =>
                  item.textContent
                    ?.trim()
                    .toLowerCase()
                    .startsWith(event.key.toLowerCase()),
                );
                if (next) {
                  event.preventDefault();
                  next.focus();
                }
              }
            }}
          >
            <button
              role="menuitem"
              disabled={pin.disabled}
              title={
                !pin.supported
                  ? "Pinning isn’t supported for this chat"
                  : pin.pinned === null
                  ? "Pin state unavailable"
                  : undefined
              }
              onClick={() => {
                close(true);
                void model.toggle();
              }}
            >
              <Icon name="pin" />
              {label}
            </button>
            {(pin.uncertain || pin.error) && (
              <button
                role="menuitem"
                disabled={pin.busy}
                onClick={() => {
                  close(true);
                  void model.check();
                }}
              >
                <Icon name="refresh" />
                {pin.uncertain ? "Check status" : "Refresh"}
              </button>
            )}
            {pin.pinned === null && !pin.error && !pin.uncertain && (
              <button
                role="menuitem"
                disabled={pin.busy}
                onClick={() => {
                  close(true);
                  void model.check();
                }}
              >
                <Icon name="refresh" />
                Refresh pin state
              </button>
            )}
          </div>,
          document.body,
        )}
    </div>
  );
}
