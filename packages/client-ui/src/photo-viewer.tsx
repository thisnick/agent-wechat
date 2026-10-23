import {
  useEffect,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import type { NativeMessage } from "@agent-wechat/shared";
import type { Connection } from "./connection.ts";
import { conversationMedia, videoAsset } from "./media-variants.ts";
import { mergeMessage } from "./session.ts";
import { MediaCard, MediaDownload } from "./media.tsx";
import { downloadAsset, standardImage } from "./media-variants.ts";
import { useConversationNavigation } from "./conversation-navigation.tsx";
import { Button } from "./ui.tsx";

import { MediaViewerContext } from "./media-viewer-context.ts";

export function PhotoViewer({
  connection,
  children,
}: {
  connection: Connection;
  children: ReactNode;
}) {
  const state = useSyncExternalStore(
    connection.session.subscribe,
    connection.session.snapshot,
  );
  const [selected, setSelected] = useState<NativeMessage | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [zoom, setZoom] = useState(1);
  const navigation = useConversationNavigation();
  const dialog = useRef<HTMLDialogElement>(null);
  const thumbnails = useRef<HTMLDivElement>(null);
  const request = useRef(0);
  const scoped =
    selected?.accountId === state.account &&
    selected?.threadId === state.selected
      ? (state.messages.find((message) => message.id === selected.id) ??
        selected)
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
  useEffect(() => {
    setZoom(1);
  }, [current?.id]);
  function close() {
    request.current++;
    setSelected(null);
    setLoading(false);
    setError("");
  }
  useEffect(() => {
    if (!active) {
      close();
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
      setSelected(photos[index + direction]);
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
          setSelected(updated[position - 1]);
          return;
        }
        if (after.nextMessages === before.nextMessages) {
          setError(after.error || "Couldn’t load earlier media.");
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
          setSelected(message);
        },
      }}
    >
      {children}
      {active && current && (
        <dialog
          ref={dialog}
          className="photo-viewer"
          aria-label="Media viewer"
          onCancel={(event) => {
            event.preventDefault();
            close();
          }}
          onClick={(event) => {
            if (event.target === event.currentTarget) close();
          }}
          onKeyDown={(event) => {
            // Native video controls use arrow keys for seeking.
            if (event.target instanceof HTMLVideoElement) return;
            if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
              event.preventDefault();
              void move(event.key === "ArrowLeft" ? -1 : 1);
            }
          }}
        >
          <div className="photo-viewer-head">
            <div className="photo-viewer-info">
              <strong>
                {current.direction === "outgoing"
                  ? "You"
                  : state.contacts.find(
                      (contact) => contact.id === current.senderId,
                    )?.displayName ||
                    current.senderId ||
                    (current.type === 43 ? "Video" : "Photo")}
              </strong>
              <span aria-live="polite">
                {loading
                  ? "Finding earlier media…"
                  : `${index + 1} / ${photos.length}`}
                {current.createdAt &&
                  ` · ${new Date(/^\d+$/.test(current.createdAt) ? Number(current.createdAt) * 1000 : current.createdAt).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" })}`}
                {navigation && (
                  <button
                    onClick={() => {
                      close();
                      void navigation.jump(current);
                    }}
                  >
                    Show message
                  </button>
                )}
              </span>
            </div>
            <div className="photo-viewer-tools">
              {current.type === 3 && (
                <>
                  <Button
                    icon="zoom-out"
                    label="Zoom out"
                    disabled={zoom <= 1}
                    onClick={() => setZoom((value) => Math.max(1, value - 0.5))}
                  />
                  <button
                    className="photo-zoom-reset"
                    aria-label="Fit photo"
                    onClick={() => setZoom(1)}
                  >
                    {zoom === 1 ? "Fit" : `${Math.round(zoom * 100)}%`}
                  </button>
                  <Button
                    icon="zoom-in"
                    label="Zoom in"
                    disabled={zoom >= 4}
                    onClick={() => setZoom((value) => Math.min(4, value + 0.5))}
                  />
                </>
              )}
              <PhotoDownload message={current} connection={connection} />
              <Button icon="close" label="Close viewer" onClick={close} />
            </div>
          </div>
          <div className="photo-viewer-body">
            <Button
              icon="back"
              label="Previous item"
              disabled={!previous || loading}
              onClick={() => void move(-1)}
            />
            <div
              className="photo-stage"
              style={{ "--photo-zoom": zoom } as import("react").CSSProperties}
            >
              <MediaCard
                key={current.id}
                message={current}
                connection={connection}
                viewer
              />
            </div>
            <Button
              icon="chevron"
              label="Next item"
              disabled={!next || loading}
              onClick={() => void move(1)}
            />
          </div>
          <div
            ref={thumbnails}
            className="photo-thumbnails"
            aria-label="Media thumbnails"
          >
            {state.nextMessages && (
              <button
                className="earlier-photos"
                disabled={loading}
                onClick={() => {
                  void earlier(photos[0]);
                }}
              >
                Earlier media
              </button>
            )}
            {photos.map((photo, position) => (
              <button
                key={photo.id}
                className="photo-thumbnail"
                aria-label={`${photo.type === 43 ? "Video" : "Photo"} ${position + 1}`}
                aria-current={photo.id === current.id ? "true" : undefined}
                onClick={() => setSelected(photo)}
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
        </dialog>
      )}
    </MediaViewerContext.Provider>
  );
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
