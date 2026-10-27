import {
  useEffect,
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
  const muteModel = connection.threadMuting(account, thread.id);
  const mute = useSyncExternalStore(muteModel.subscribe, muteModel.snapshot);
  const [open, setOpen] = useState(false);
  const [info, setInfo] = useState(false);
  const frame = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const card = useRef<HTMLDivElement>(null);
  const infoTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const infoId = useId();
  const hideInfo = () => {
    clearTimeout(infoTimer.current);
    setInfo(false);
  };
  const showInfo = () => {
    clearTimeout(infoTimer.current);
    if (!open) infoTimer.current = setTimeout(() => setInfo(true), 450);
  };
  const leaveInfo = () => {
    clearTimeout(infoTimer.current);
    infoTimer.current = setTimeout(() => setInfo(false), 150);
  };
  useEffect(() => () => clearTimeout(infoTimer.current), []);
  useLayoutEffect(() => {
    if (!info || !frame.current || !card.current) return;
    const row = frame.current.getBoundingClientRect();
    const { width, height } = card.current.getBoundingClientRect();
    const right = row.right + 8;
    card.current.style.left = `${Math.max(
      8,
      Math.min(
        right + width <= innerWidth - 8 ? right : row.left - width - 8,
        innerWidth - width - 8,
      ),
    )}px`;
    card.current.style.top = `${Math.max(
      8,
      Math.min(row.top, innerHeight - height - 8),
    )}px`;
  }, [info, thread]);
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
    if (restore)
      (trigger.current?.disabled
        ? frame.current?.querySelector<HTMLButtonElement>(".chat-row")
        : trigger.current
      )?.focus({ preventScroll: true });
  };
  useLayoutEffect(() => {
    if (!open) return;
    const items = menu.current?.querySelectorAll<HTMLElement>(
      '[role="menuitem"]:not(:disabled)',
    );
    (items?.[0] ?? menu.current)?.focus();
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
  }, [open]);
  useEffect(() => {
    if (!info) return;
    const dismiss = (event: Event) => {
      const target = event.target;
      if (
        event.type === "resize" ||
        event.type === "pointerdown" ||
        target === document ||
        (target instanceof Element &&
          frame.current &&
          target.contains(frame.current))
      )
        hideInfo();
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") hideInfo();
    };
    document.addEventListener("pointerdown", dismiss);
    document.addEventListener("keydown", escape);
    document.addEventListener("scroll", dismiss, true);
    window.addEventListener("resize", dismiss);
    return () => {
      document.removeEventListener("pointerdown", dismiss);
      document.removeEventListener("keydown", escape);
      document.removeEventListener("scroll", dismiss, true);
      window.removeEventListener("resize", dismiss);
    };
  }, [info]);
  const anchor = trigger.current?.getBoundingClientRect();
  const displayedPin = pin.preview ?? pin.pinned;
  const label = displayedPin ? "Unpin chat" : "Pin chat";
  const muteLabel = mute.muted ? "Unmute notifications" : "Mute notifications";
  const accountName = connection.session
    .snapshot()
    .accounts.find((item) => item.id === account)?.alias;
  const value = thread.lastMessage?.createdAt;
  const date = value
    ? new Date(/^\d+$/.test(value) ? Number(value) * 1000 : value)
    : null;
  const lastActivity = date && Number.isFinite(date.getTime()) ? date : null;
  return (
    <div className="thread-list-item" data-thread-id={thread.id}>
      {heading && <div className="pinned-heading">Pinned</div>}
      {divider && <div className="pinned-divider" aria-hidden="true" />}
      <div
        ref={frame}
        className="chat-row-frame"
        data-menu-open={open}
        onPointerEnter={(event) => {
          if (event.pointerType === "mouse") showInfo();
        }}
        onPointerLeave={leaveInfo}
        onFocus={showInfo}
        onBlur={(event) => {
          if (!event.currentTarget.contains(event.relatedTarget)) hideInfo();
        }}
        onContextMenu={(event) => {
          event.preventDefault();
          hideInfo();
          setOpen(true);
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape" && info) {
            event.preventDefault();
            hideInfo();
          }
          if (
            event.key === "ContextMenu" ||
            (event.shiftKey && event.key === "F10")
          ) {
            event.preventDefault();
            hideInfo();
            setOpen(true);
          }
        }}
      >
        <button
          className={`chat-row ${selected ? "selected" : ""}`}
          onClick={select}
          aria-current={selected ? "true" : undefined}
          aria-describedby={info ? infoId : undefined}
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
            </div>
            <div className="preview-row">
              <span className="preview">{threadPreview(thread)}</span>
              {!!thread.unreadCount && (
                <span className="count">{thread.unreadCount}</span>
              )}
            </div>
          </div>
        </button>
        <span
          className="thread-row-status"
          aria-label={
            [
              thread.pinned === true ? "Pinned" : "",
              thread.muted === true ? "Notifications muted" : "",
            ]
              .filter(Boolean)
              .join(", ") || undefined
          }
        >
          {thread.pinned === true && (
            <span className="thread-pin-icon">
              <Icon name="pin" />
            </span>
          )}
          {thread.muted === true && <Icon name="bell-off" />}
        </span>
        <div className="thread-row-actions">
          <button
            ref={trigger}
            className="thread-row-pin icon-btn"
            aria-label={pin.pinned === null ? "Pin state unavailable" : label}
            aria-pressed={displayedPin ?? undefined}
            title={
              !pin.supported
                ? "Pinning isn’t supported for this chat"
                : pin.pinned === null
                ? "Pin state unavailable"
                : label
            }
            disabled={!pin.supported || pin.pinned === null}
            aria-disabled={pin.disabled}
            onClick={() => {
              hideInfo();
              void model.toggle();
            }}
          >
            {pin.saving ? (
              <span className="pin-spinner" aria-label="Saving pin" />
            ) : (
              <Icon name={displayedPin ? "unpin" : "pin"} />
            )}
          </button>
          <button
            className="thread-row-mute icon-btn"
            aria-label={
              mute.muted === null ? "Notification state unavailable" : muteLabel
            }
            aria-pressed={mute.muted ?? undefined}
            title={
              !mute.supported
                ? "Notifications aren’t supported for this chat"
                : mute.muted === null
                ? "Notification state unavailable"
                : muteLabel
            }
            disabled={!mute.supported || mute.muted === null}
            aria-disabled={mute.disabled}
            onClick={() => {
              hideInfo();
              void muteModel.toggle();
            }}
          >
            {mute.saving ? (
              <span className="pin-spinner" aria-label="Saving notifications" />
            ) : (
              <Icon name={mute.muted ? "bell" : "bell-off"} />
            )}
          </button>
        </div>
      </div>
      <PinFeedback model={model} />
      <PinFeedback model={muteModel} kind="notification" />
      {info &&
        !open &&
        createPortal(
          <div
            ref={card}
            id={infoId}
            role="tooltip"
            className="thread-info-card"
            onPointerEnter={() => clearTimeout(infoTimer.current)}
            onPointerLeave={leaveInfo}
          >
            <div className="thread-info-heading">
              <strong>{name}</strong>
              {lastActivity && (
                <time dateTime={lastActivity.toISOString()}>
                  {lastActivity.toLocaleString([], {
                    month: "short",
                    day: "numeric",
                    hour: "numeric",
                    minute: "2-digit",
                  })}
                </time>
              )}
            </div>
            <div className="thread-info-meta">
              <Icon name={thread.kind === "group" ? "people" : "chat"} />
              <span>
                {[
                  accountName,
                  thread.kind === "group" ? "Group chat" : "Direct chat",
                ]
                  .filter(Boolean)
                  .join(" · ")}
              </span>
            </div>
            {(!!thread.unreadCount || thread.muted === true) && (
              <div className="thread-info-meta">
                {[
                  thread.unreadCount ? `${thread.unreadCount} unread` : "",
                  thread.muted === true ? "Notifications muted" : "",
                ]
                  .filter(Boolean)
                  .join(" · ")}
              </div>
            )}
            <p>{threadPreview(thread)}</p>
          </div>,
          document.body,
        )}
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
            <button
              role="menuitem"
              disabled={mute.disabled}
              onClick={() => {
                close(true);
                void muteModel.toggle();
              }}
            >
              <Icon name={mute.muted ? "bell" : "bell-off"} />
              {muteLabel}
            </button>
            {(mute.uncertain || mute.error || mute.muted === null) && (
              <button
                role="menuitem"
                disabled={mute.busy}
                onClick={() => {
                  close(true);
                  void muteModel.check();
                }}
              >
                <Icon name="refresh" />
                {mute.uncertain
                  ? "Check notification status"
                  : "Refresh notification state"}
              </button>
            )}
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
