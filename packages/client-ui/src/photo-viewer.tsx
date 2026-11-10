import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { createPortal } from "react-dom";
import {
  TransformComponent,
  TransformWrapper,
  useControls,
  useTransformComponent,
  type ReactZoomPanPinchContextState,
  type ReactZoomPanPinchRef,
} from "react-zoom-pan-pinch";
import type { NativeMessage } from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { dateLocale, t, useLocale } from "./i18n.ts";
import { conversationMedia, videoAsset } from "./media-variants.ts";
import { mergeMessage } from "./session.ts";
import { MediaCard, MediaDownload } from "./media.tsx";
import { downloadAsset, standardImage } from "./media-variants.ts";
import { useConversationNavigation } from "./conversation-navigation.tsx";
import { Button } from "./ui.tsx";
import { mediaSwipeDirection } from "./media-swipe.ts";

import { MediaViewerContext } from "./media-viewer-context.ts";

/**
 * The viewer is a view of the address: `id` names the open message, and every
 * way of opening, moving or closing goes through the address.
 */
export type MediaRoute = {
  id: string | null;
  /** Opens the viewer on a message; adds a history entry. */
  open: (id: string) => void;
  /** Shows another item while the viewer stays open. */
  move: (id: string) => void;
  close: () => void;
  /** Leaves the viewer for the message in its conversation. */
  showMessage: (id: string) => void;
  /** The named message is not a viewable item of this conversation. */
  missing: () => void;
};

export function PhotoViewer({
  connection,
  route,
  children,
}: {
  connection: Connection;
  route: MediaRoute;
  children: ReactNode;
}) {
  useLocale();
  const state = useSyncExternalStore(
    connection.session.subscribe,
    connection.session.snapshot,
  );
  // A message outside the loaded pages (a search result or a link) is fetched once.
  const [fetched, setFetched] = useState<NativeMessage | null>(null);
  const loaded = route.id
    ? state.messages.find((message) => message.id === route.id)
    : undefined;
  const selected = loaded ?? (fetched?.id === route.id ? fetched : null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const reducedMotion = useReducedMotion();
  const animationTime = reducedMotion ? 0 : 220;
  const navigation = useConversationNavigation();
  const dialog = useRef<HTMLDialogElement>(null);
  const transform = useRef<ReactZoomPanPinchRef>(null);
  const thumbnails = useRef<HTMLDivElement>(null);
  const request = useRef(0);
  const scoped =
    selected?.accountId === state.account &&
    selected?.threadId === state.selected
      ? state.messages.find((message) => message.id === selected.id) ?? selected
      : null;
  const active = scoped && !scoped.recall ? scoped : null;
  const photos = active
    ? conversationMedia(
        mergeMessage(state.messages, active),
        active.accountId,
        active.threadId,
      )
    : [];
  const index = photos.findIndex((photo) => photo.id === active?.id);
  const current = photos[index];
  useEffect(() => {
    thumbnails.current
      ?.querySelector('[aria-current="true"]')
      ?.scrollIntoView({ block: "nearest", inline: "center" });
  }, [current?.id]);
  const previous = index > 0 || !!state.nextMessages;
  const next = index >= 0 && index < photos.length - 1;
  function reset() {
    request.current++;
    setLoading(false);
    setError("");
  }
  const unavailable = !!route.id && !!scoped && !current;
  const missing = useRef(route.missing);
  missing.current = route.missing;
  useEffect(() => {
    if (unavailable) missing.current();
  }, [unavailable]);
  useEffect(() => {
    if (
      !route.id ||
      loaded ||
      fetched?.id === route.id ||
      state.loading ||
      !state.account ||
      !state.selected
    )
      return;
    const { account, selected: thread } = state;
    const id = route.id;
    let cancelled = false;
    void (async () => {
      try {
        const message = await connection.session.client.message(account, id);
        if (message.accountId !== account || message.threadId !== thread)
          throw new Error("Not in this conversation");
        if (!cancelled) setFetched(message);
      } catch {
        if (!cancelled) missing.current();
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [route.id, !!loaded, state.account, state.selected, state.loading]);
  useEffect(() => {
    if (!active) {
      reset();
      return;
    }
    const target = dialog.current;
    const focused = document.activeElement;
    target?.showModal();
    return () => {
      target?.close();
      if (focused instanceof HTMLElement && focused.isConnected)
        focused.focus();
    };
  }, [!!active]);
  async function move(direction: -1 | 1) {
    if (loading || !current) return;
    setError("");
    if (photos[index + direction]) {
      route.move(photos[index + direction].id);
      return;
    }
    if (direction !== -1 || !state.nextMessages) return;
    await earlier(current);
  }
  async function earlier(anchor: NativeMessage) {
    if (loading) return;
    const generation = ++request.current;
    setLoading(true);
    try {
      // Older photos are discovered through the normal history cursor, without gaps in the chat.
      for (;;) {
        const before = connection.session.snapshot();
        if (!before.nextMessages || before.loading) break;
        await connection.session.older();
        if (generation !== request.current) return;
        const after = connection.session.snapshot();
        if (
          after.account !== anchor.accountId ||
          after.selected !== anchor.threadId
        )
          return;
        const updated = conversationMedia(
          mergeMessage(after.messages, anchor),
          anchor.accountId,
          anchor.threadId,
        );
        const position = updated.findIndex((photo) => photo.id === anchor.id);
        if (position > 0) {
          route.move(updated[position - 1].id);
          return;
        }
        if (after.nextMessages === before.nextMessages) {
          setError(
            after.error ||
              t("Couldn’t load earlier media.", "无法加载更早的媒体。"),
          );
          break;
        }
      }
    } finally {
      if (generation === request.current) setLoading(false);
    }
  }
  return (
    <MediaViewerContext.Provider
      value={{
        open: (message) => {
          setError("");
          setFetched(message);
          route.open(message.id);
        },
      }}
    >
      {children}
      {active &&
        current &&
        createPortal(
          <dialog
            ref={dialog}
            className="photo-viewer"
            aria-label={t("Media viewer", "媒体查看器")}
            onCancel={(event) => {
              event.preventDefault();
              route.close();
            }}
            onClick={(event) => {
              if (event.target === event.currentTarget) route.close();
            }}
            onKeyDown={(event) => {
              // Native video controls use arrow keys for seeking.
              if (event.target instanceof HTMLVideoElement) return;
              if (event.ctrlKey || event.metaKey || event.altKey) return;
              if (
                current.type === 3 &&
                ["+", "=", "-", "0"].includes(event.key)
              ) {
                event.preventDefault();
                if (event.key === "0")
                  void transform.current?.resetTransform(animationTime);
                else if (event.key === "-")
                  void transform.current?.zoomOut(0.5, animationTime);
                else void transform.current?.zoomIn(0.5, animationTime);
              }
              if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
                event.preventDefault();
                void move(event.key === "ArrowLeft" ? -1 : 1);
              }
            }}
          >
            <TransformWrapper
              ref={transform}
              disabled={current.type !== 3}
              minScale={1}
              maxScale={4}
              centerOnInit
              centerZoomedOut
              smooth={!reducedMotion}
              doubleClick={{ disabled: true }}
              zoomAnimation={{ disabled: reducedMotion, animationTime }}
              autoAlignment={{
                animationTime,
                velocityAlignmentTime: animationTime,
              }}
              velocityAnimation={{ disabled: reducedMotion }}
              panning={{ allowRightClickPan: false }}
            >
              <ResetPhotoZoom messageId={current.id} />
              <div className="photo-viewer-head">
                <div className="photo-viewer-info">
                  <strong>
                    {current.direction === "outgoing"
                      ? t("You", "我")
                      : state.contacts.find(
                          (contact) => contact.id === current.senderId,
                        )?.displayName ||
                        current.senderId ||
                        (current.type === 43
                          ? t("Video", "视频")
                          : t("Photo", "图片"))}
                  </strong>
                  <span aria-live="polite">
                    {loading
                      ? t("Finding earlier media…", "正在查找更早的媒体…")
                      : `${index + 1} / ${photos.length}`}
                    {current.createdAt &&
                      ` · ${new Date(
                        /^\d+$/.test(current.createdAt)
                          ? Number(current.createdAt) * 1000
                          : current.createdAt,
                      ).toLocaleString(dateLocale(), {
                        month: "short",
                        day: "numeric",
                        hour: "numeric",
                        minute: "2-digit",
                      })}`}
                    {navigation && (
                      <button onClick={() => route.showMessage(current.id)}>
                        {t("Show message", "定位到消息")}
                      </button>
                    )}
                  </span>
                </div>
                <div className="photo-viewer-tools">
                  {current.type === 3 && (
                    <PhotoZoomControls animationTime={animationTime} />
                  )}
                  <PhotoDownload message={current} connection={connection} />
                  <Button icon="close" label="Close viewer" onClick={route.close} />
                </div>
              </div>
              <div className="photo-viewer-body">
                <PhotoCanvas
                  image={current.type === 3}
                  animationTime={animationTime}
                  navigate={(direction) => void move(direction)}
                >
                  <MediaCard
                    key={current.id}
                    message={current}
                    connection={connection}
                    viewer
                  />
                </PhotoCanvas>
                <Button
                  icon="back"
                  label={t("Previous item", "上一项")}
                  disabled={!previous || loading}
                  onClick={() => void move(-1)}
                  className="photo-previous"
                />
                <Button
                  icon="chevron"
                  label={t("Next item", "下一项")}
                  disabled={!next || loading}
                  onClick={() => void move(1)}
                  className="photo-next"
                />
              </div>
              <div
                ref={thumbnails}
                className="photo-thumbnails"
                aria-label={t("Media thumbnails", "媒体缩略图")}
              >
                {state.nextMessages && (
                  <button
                    className="earlier-photos"
                    disabled={loading}
                    onClick={() => {
                      void earlier(photos[0]);
                    }}
                  >
                    {t("Earlier media", "更早的媒体")}
                  </button>
                )}
                {photos.map((photo, position) => (
                  <button
                    key={photo.id}
                    className="photo-thumbnail"
                    aria-label={`${
                      photo.type === 43
                        ? t("Video", "视频")
                        : t("Photo", "图片")
                    } ${position + 1}`}
                    aria-current={photo.id === current.id ? "true" : undefined}
                    onClick={() => route.move(photo.id)}
                  >
                    <MediaCard
                      message={photo}
                      connection={connection}
                      previewOnly
                    />
                  </button>
                ))}
              </div>
              {error && (
                <p className="photo-viewer-error" role="status">
                  {error}
                </p>
              )}
            </TransformWrapper>
          </dialog>,
          document.body,
        )}
    </MediaViewerContext.Provider>
  );
}

const zoomScale = ({ state }: ReactZoomPanPinchContextState) => state.scale;
const isZoomed = ({ state }: ReactZoomPanPinchContextState) =>
  state.scale > 1.001;

function PhotoZoomControls({ animationTime }: { animationTime: number }) {
  useLocale();
  const scale = useTransformComponent(zoomScale);
  const { zoomIn, zoomOut, resetTransform } = useControls();
  return (
    <>
      <Button
        icon="reset-zoom"
        label={t("Reset zoom", "重置缩放")}
        onClick={() => void resetTransform(animationTime)}
      />
      <Button
        icon="zoom-out"
        label={t("Zoom out", "缩小")}
        disabled={scale <= 1.001}
        onClick={() => void zoomOut(0.5, animationTime)}
      />
      <span
        className="photo-zoom-level"
        aria-label={t("Zoom level", "缩放比例")}
      >
        {Math.round(scale * 100)}%
      </span>
      <Button
        icon="zoom-in"
        label={t("Zoom in", "放大")}
        disabled={scale >= 3.999}
        onClick={() => void zoomIn(0.5, animationTime)}
      />
    </>
  );
}

function ResetPhotoZoom({ messageId }: { messageId: string }) {
  // The controls belong to this wrapper for its lifetime. Reset only when the
  // selected message changes, not when cached media or history is refreshed.
  const reset = useRef(useControls().resetTransform);
  useLayoutEffect(() => {
    void reset.current(0);
  }, [messageId]);
  return null;
}

function PhotoCanvas({
  image,
  animationTime,
  navigate,
  children,
}: {
  image: boolean;
  animationTime: number;
  navigate: (direction: -1 | 1) => void;
  children: ReactNode;
}) {
  const zoomed = useTransformComponent(isZoomed);
  const { state, resetTransform, zoomToPoint } = useControls();
  const gesture = useRef<{
    id: number;
    x: number;
    y: number;
    moved: boolean;
    cancelled: boolean;
    pointerType: string;
    startedAt: number;
    scale: number;
  } | null>(null);
  function moved(event: ReactPointerEvent<HTMLDivElement>) {
    const start = gesture.current;
    if (
      start &&
      (event.pointerId !== start.id ||
        Math.hypot(event.clientX - start.x, event.clientY - start.y) > 5)
    )
      start.moved = true;
  }
  function toggle(clientX: number, clientY: number) {
    if (state.scale > 1.001) void resetTransform(animationTime);
    else void zoomToPoint(2, clientX, clientY, animationTime);
  }
  return (
    <TransformComponent
      wrapperClass={`photo-stage${image ? " photo-image-stage" : ""}${
        zoomed ? " is-zoomed" : ""
      }`}
      contentClass="photo-transform"
      wrapperProps={{
        tabIndex: image ? 0 : undefined,
        "aria-label": image ? t("Image canvas", "图片画布") : undefined,
        onPointerDown: (event) => {
          // Leave native video controls, buttons and links to their own gestures.
          if (
            (event.target as Element).closest("video, button, a, input, select")
          ) {
            gesture.current = null;
            return;
          }
          if (!event.isPrimary) {
            if (gesture.current) {
              gesture.current.moved = true;
              gesture.current.cancelled = true;
            }
          } else
            gesture.current = {
              id: event.pointerId,
              x: event.clientX,
              y: event.clientY,
              moved: false,
              cancelled: false,
              pointerType: event.pointerType,
              startedAt: event.timeStamp,
              scale: state.scale,
            };
        },
        onPointerMove: moved,
        onPointerUp: (event) => {
          moved(event);
          const start = gesture.current;
          if (!start || start.id !== event.pointerId) return;
          const direction = mediaSwipeDirection({
            pointerType: start.pointerType,
            startX: start.x,
            startY: start.y,
            endX: event.clientX,
            endY: event.clientY,
            elapsed: event.timeStamp - start.startedAt,
            startScale: start.scale,
            endScale: state.scale,
            cancelled: start.cancelled,
          });
          if (direction) {
            start.moved = true;
            navigate(direction);
          }
        },
        onPointerCancel: () => {
          if (gesture.current) {
            gesture.current.moved = true;
            gesture.current.cancelled = true;
          }
        },
        onDragStart: (event) => {
          if (image) event.preventDefault();
        },
        onClick: (event) => {
          if (
            image &&
            event.button === 0 &&
            !gesture.current?.moved &&
            event.target instanceof HTMLImageElement
          )
            toggle(event.clientX, event.clientY);
        },
        onKeyDown: (event) => {
          if (
            !image ||
            event.target !== event.currentTarget ||
            !["Enter", " "].includes(event.key)
          )
            return;
          event.preventDefault();
          const bounds = event.currentTarget.getBoundingClientRect();
          toggle(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
        },
      }}
    >
      {children}
    </TransformComponent>
  );
}

function useReducedMotion() {
  const [reduced, setReduced] = useState(
    () => matchMedia("(prefers-reduced-motion: reduce)").matches,
  );
  useEffect(() => {
    const query = matchMedia("(prefers-reduced-motion: reduce)");
    const update = () => setReduced(query.matches);
    query.addEventListener("change", update);
    return () => query.removeEventListener("change", update);
  }, []);
  return reduced;
}

function PhotoDownload({
  message,
  connection,
}: {
  message: NativeMessage;
  connection: Connection;
}) {
  const model = connection.incomingMedia.get(message.accountId, message.id);
  const state = useSyncExternalStore(model.subscribe, model.snapshot);
  useEffect(() => {
    void model.load();
  }, [model]);
  return (
    <MediaDownload
      model={model}
      connection={connection}
      asset={downloadAsset(
        state.assets,
        message.type === 43
          ? videoAsset(state.assets, true)
          : standardImage(state.assets),
      )}
    />
  );
}
