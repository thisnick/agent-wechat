import {
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import { isNativeTombstone, type NativeGroupMember } from "@agent-wechat/shared";
import type { Connection } from "../connection.ts";
import { HubAvatar } from "../avatars.tsx";
import { t } from "../i18n.ts";
import { inventory, readableError } from "../session.ts";
import { Button, Icon } from "../ui.tsx";
import {
  offerKey,
  type CallController,
} from "./call-controller.ts";
import type {
  LocalPreviewSurface,
  RemoteVideoFrame,
} from "./media-backend.ts";

/** Draws a decoded remote video frame onto a canvas while honoring WXCM clockwise rotation. */
export function drawRotatedFrame(
  canvas: HTMLCanvasElement,
  source: CanvasImageSource,
  srcWidth: number,
  srcHeight: number,
  rotationDegrees: 0 | 90 | 180 | 270,
): void {
  if (srcWidth <= 0 || srcHeight <= 0) return;
  const swapped = rotationDegrees === 90 || rotationDegrees === 270;
  const dstW = swapped ? srcHeight : srcWidth;
  const dstH = swapped ? srcWidth : srcHeight;
  if (canvas.width !== dstW || canvas.height !== dstH) {
    canvas.width = dstW;
    canvas.height = dstH;
  }
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  ctx.save();
  ctx.clearRect(0, 0, dstW, dstH);
  if (rotationDegrees === 90) {
    ctx.translate(dstW, 0);
    ctx.rotate(Math.PI / 2);
  } else if (rotationDegrees === 180) {
    ctx.translate(dstW, dstH);
    ctx.rotate(Math.PI);
  } else if (rotationDegrees === 270) {
    ctx.translate(0, dstH);
    ctx.rotate(-Math.PI / 2);
  }
  ctx.drawImage(source, 0, 0, srcWidth, srcHeight);
  ctx.restore();
}

/** Subscribes directly to a track's video frames so 15-30 fps rendering bypasses React re-renders. */
export function RemoteVideoCanvas({
  controller,
  trackId,
  label,
  className = "",
}: {
  controller: CallController;
  trackId: number;
  label: string;
  className?: string;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    let disposed = false;
    let loadSeq = 0;
    const renderFrame = (frame: RemoteVideoFrame | null) => {
      const canvas = canvasRef.current;
      if (!canvas || disposed) return;
      if (!frame) {
        loadSeq += 1;
        const ctx = canvas.getContext("2d");
        ctx?.clearRect(0, 0, canvas.width, canvas.height);
        return;
      }
      if (frame.bitmap) {
        loadSeq += 1;
        drawRotatedFrame(
          canvas,
          frame.bitmap,
          frame.bitmap.width,
          frame.bitmap.height,
          frame.rotationDegrees,
        );
      } else if (frame.jpegBytes) {
        const seq = ++loadSeq;
        const bytes = new Uint8Array(frame.jpegBytes.byteLength);
        bytes.set(frame.jpegBytes);
        const blob = new Blob([bytes], { type: "image/jpeg" });
        const url = URL.createObjectURL(blob);
        const img = new Image();
        img.onload = () => {
          URL.revokeObjectURL(url);
          if (!disposed && seq === loadSeq && canvasRef.current) {
            drawRotatedFrame(
              canvasRef.current,
              img,
              img.naturalWidth,
              img.naturalHeight,
              frame.rotationDegrees,
            );
          }
        };
        img.onerror = () => URL.revokeObjectURL(url);
        img.src = url;
      }
    };

    renderFrame(controller.getRemoteFrame(trackId));
    const unsubscribe = controller.subscribeRemoteFrame(trackId, renderFrame);
    return () => {
      disposed = true;
      loadSeq += 1;
      unsubscribe();
    };
  }, [controller, trackId]);

  return (
    <canvas
      ref={canvasRef}
      className={`call-video-canvas ${className}`}
      aria-label={label}
    />
  );
}

/** Renders local camera video from either a browser MediaStream or desktop JPEG frames. */
export function LocalVideoSurface({
  stream,
  subscribeJpeg,
  label,
  className = "",
}: {
  stream?: MediaStream | null;
  subscribeJpeg?: (listener: (bytes: Uint8Array) => void) => () => void;
  label: string;
  className?: string;
}) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;
    video.srcObject = stream ?? null;
    if (stream) {
      void video.play().catch(() => {});
    }
    return () => {
      video.srcObject = null;
    };
  }, [stream]);

  useEffect(() => {
    if (!subscribeJpeg || stream) return;
    let disposed = false;
    let loadSeq = 0;
    const unsubscribe = subscribeJpeg((jpegBytes) => {
      const canvas = canvasRef.current;
      if (!canvas || disposed) return;
      const seq = ++loadSeq;
      const bytes = new Uint8Array(jpegBytes.byteLength);
      bytes.set(jpegBytes);
      const blob = new Blob([bytes], { type: "image/jpeg" });
      const url = URL.createObjectURL(blob);
      const img = new Image();
      img.onload = () => {
        URL.revokeObjectURL(url);
        if (!disposed && seq === loadSeq && canvasRef.current) {
          drawRotatedFrame(canvasRef.current, img, img.naturalWidth, img.naturalHeight, 0);
        }
      };
      img.onerror = () => URL.revokeObjectURL(url);
      img.src = url;
    });
    return () => {
      disposed = true;
      loadSeq += 1;
      unsubscribe();
    };
  }, [subscribeJpeg, stream]);

  if (stream) {
    return (
      <video
        ref={videoRef}
        className={`call-local-video ${className}`}
        aria-label={label}
        muted
        playsInline
        autoPlay
      />
    );
  }
  return (
    <canvas
      ref={canvasRef}
      className={`call-local-video ${className}`}
      aria-label={label}
    />
  );
}

/** Anchored direct-video pre-call preview popover with device & mute/camera controls. */
export function CallPreviewPopover({
  controller,
  peerName,
  triggerRef,
  onStart,
  onClose,
}: {
  controller: CallController;
  peerName: string;
  triggerRef?: RefObject<HTMLElement | null>;
  onStart: (options: { muted: boolean; cameraEnabled: boolean }) => void;
  onClose: () => void;
}) {
  const callState = useSyncExternalStore(controller.subscribe, controller.snapshot);
  const [cameraEnabled, setCameraEnabled] = useState(true);
  const [muted, setMuted] = useState(false);
  const [preview, setPreview] = useState<LocalPreviewSurface | null>(null);
  const previewRef = useRef<LocalPreviewSurface | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const popoverRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    void controller.refreshDevices();
  }, [controller]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
      }
    };
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node | null;
      if (!target) return;
      if (popoverRef.current?.contains(target)) return;
      if (triggerRef?.current?.contains(target)) return;
      onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPointerDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPointerDown);
    };
  }, [onClose, triggerRef]);

  useEffect(() => {
    if (!cameraEnabled) {
      previewRef.current?.stop();
      previewRef.current = null;
      setPreview(null);
      setPreviewError(null);
      return;
    }
    let cancelled = false;
    let activeSurface: LocalPreviewSurface | null = null;
    setPreviewError(null);
    void controller
      .startLocalPreview(callState.selectedDevices.cameraDeviceId)
      .then(
        (surface) => {
          if (cancelled) {
            surface.stop();
            return;
          }
          activeSurface = surface;
          previewRef.current = surface;
          setPreview(surface);
          void controller.refreshDevices();
        },
        () => {
          if (!cancelled) {
            previewRef.current = null;
            setPreview(null);
            setPreviewError(t("calls.cameraPreviewUnavailable"));
          }
        },
      );
    return () => {
      cancelled = true;
      activeSurface?.stop();
      if (previewRef.current === activeSurface) previewRef.current = null;
      setPreview(null);
    };
  }, [controller, cameraEnabled, callState.selectedDevices.cameraDeviceId]);

  return (
    <div
      ref={popoverRef}
      className="call-preview-popover"
      role="dialog"
      aria-label={t("calls.videoCallPreviewTitle", { name: peerName })}
    >
      <div className="call-preview-head">
        <strong>{t("calls.videoCallPreviewTitle", { name: peerName })}</strong>
        <Button icon="close" label={t("calls.close")} onClick={onClose} />
      </div>
      <div className="call-preview-stage">
        {cameraEnabled && preview ? (
          <LocalVideoSurface
            stream={preview.stream}
            subscribeJpeg={preview.subscribeJpeg}
            label={t("calls.cameraPreview")}
          />
        ) : (
          <div className="call-preview-placeholder">
            <Icon name="video-off" />
            <span>{previewError || t("calls.cameraOff")}</span>
          </div>
        )}
      </div>
      <div className="call-preview-toggles">
        <button
          type="button"
          className={`call-toggle-pill ${muted ? "is-off" : ""}`}
          aria-pressed={muted}
          onClick={() => setMuted((v) => !v)}
        >
          <Icon name={muted ? "mic-off" : "mic"} />
          {muted ? t("calls.muted") : t("calls.microphoneOn")}
        </button>
        <button
          type="button"
          className={`call-toggle-pill ${!cameraEnabled ? "is-off" : ""}`}
          aria-pressed={cameraEnabled}
          onClick={() => setCameraEnabled((v) => !v)}
        >
          <Icon name={cameraEnabled ? "video" : "video-off"} />
          {cameraEnabled ? t("calls.cameraOn") : t("calls.cameraOff")}
        </button>
      </div>
      <div className="call-preview-devices">
        {callState.devices.videoInputs.length > 0 && (
          <label className="call-device-field">
            <span>{t("calls.camera")}</span>
            <select
              aria-label={t("calls.camera")}
              value={callState.selectedDevices.cameraDeviceId ?? ""}
              onChange={(e) =>
                void controller.selectDevices({ cameraDeviceId: e.target.value || undefined })
              }
            >
              <option value="">{t("calls.systemDefault")}</option>
              {callState.devices.videoInputs.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.label}
                </option>
              ))}
            </select>
          </label>
        )}
        {callState.devices.audioInputs.length > 0 && (
          <label className="call-device-field">
            <span>{t("calls.microphone")}</span>
            <select
              aria-label={t("calls.microphone")}
              value={callState.selectedDevices.inputDeviceId ?? ""}
              onChange={(e) =>
                void controller.selectDevices({ inputDeviceId: e.target.value || undefined })
              }
            >
              <option value="">{t("calls.systemDefault")}</option>
              {callState.devices.audioInputs.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.label}
                </option>
              ))}
            </select>
          </label>
        )}
        {callState.devices.supportsOutputSelection && callState.devices.audioOutputs.length > 0 && (
          <label className="call-device-field">
            <span>{t("calls.speaker")}</span>
            <select
              aria-label={t("calls.speaker")}
              value={callState.selectedDevices.outputDeviceId ?? ""}
              onChange={(e) =>
                void controller.selectDevices({ outputDeviceId: e.target.value || undefined })
              }
            >
              <option value="">{t("calls.systemDefault")}</option>
              {callState.devices.audioOutputs.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.label}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>
      <label className="call-skip-preview">
        <input
          type="checkbox"
          checked={callState.skipVideoPreview}
          onChange={(e) => controller.setSkipVideoPreview(e.target.checked)}
        />
        <span>{t("calls.skipPreviewNextTime")}</span>
      </label>
      <div className="call-preview-footer">
        <button type="button" className="secondary" onClick={onClose}>
          {t("calls.cancel")}
        </button>
        <button
          type="button"
          className="primary call-start-btn"
          disabled={callState.availability.busy || callState.actionPending}
          onClick={() => {
            // Release single-consumer hardware cameras (V4L2/Windows) before dialing.
            previewRef.current?.stop();
            previewRef.current = null;
            setPreview(null);
            onStart({ muted, cameraEnabled });
          }}
        >
          <Icon name="video" />
          {t("calls.startVideoCall")}
        </button>
      </div>
    </div>
  );
}

/** NewChat-style 1-100 member picker modal (native top-layer <dialog> so it stacks above Theater). */
export function GroupCallPickerModal({
  connection,
  account,
  groupId,
  groupName,
  mode,
  initialCallType = "voice",
  allowVideo,
  excludeIds = [],
  busy = false,
  onSubmit,
  onClose,
}: {
  connection: Connection;
  account: string;
  groupId: string;
  groupName: string;
  mode: "start" | "invite";
  initialCallType?: "voice" | "video";
  allowVideo: boolean;
  excludeIds?: string[];
  busy?: boolean;
  onSubmit: (payload: {
    invitees: string[];
    callType: "voice" | "video";
    muted: boolean;
    cameraEnabled: boolean;
  }) => void;
  onClose: () => void;
}) {
  const session = connection.session;
  const sessionState = useSyncExternalStore(session.subscribe, session.snapshot);
  const dialogRef = useRef<HTMLDialogElement>(null);
  const [members, setMembers] = useState<NativeGroupMember[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [callType, setCallType] = useState<"voice" | "video">(
    initialCallType === "video" && allowVideo ? "video" : "voice",
  );
  const [muted, setMuted] = useState(false);
  const [cameraEnabled, setCameraEnabled] = useState(
    initialCallType === "video" && allowVideo,
  );

  const ownUsername = sessionState.accounts.find((a) => a.id === account)?.ownUsername;
  const excludeKey = excludeIds.join("\0");
  const excluded = useMemo(() => {
    const set = new Set(excludeIds);
    if (ownUsername) set.add(ownUsername);
    return set;
  }, [excludeKey, ownUsername]);

  // Prune any selected members that join the call while the invite picker is open.
  useEffect(() => {
    setSelected((prev) => {
      const next = prev.filter((id) => !excluded.has(id));
      return next.length === prev.length ? prev : next;
    });
  }, [excluded]);

  useEffect(() => {
    const el = dialogRef.current;
    if (!el) return;
    if (!el.open) {
      try {
        el.showModal();
      } catch {}
    }
    return () => {
      if (el.open) el.close();
    };
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    setError("");
    void inventory((cursor) =>
      session.client.groupMembers(account, groupId, { limit: 200, cursor }, controller.signal),
    ).then(
      (page) => {
        if (controller.signal.aborted) return;
        setMembers(
          page.items.filter((m) => !isNativeTombstone(m)) as NativeGroupMember[],
        );
        setLoading(false);
      },
      (err) => {
        if (controller.signal.aborted) return;
        setError(readableError(err));
        setLoading(false);
      },
    );
    return () => controller.abort();
  }, [session.client, account, groupId]);

  const candidates = useMemo(() => {
    const contactNames = new Map(
      sessionState.contacts.map((c) => [c.id, c.displayName || c.username || c.id]),
    );
    return members
      .filter((m) => !excluded.has(m.username))
      .map((m) => ({
        id: m.username,
        label: m.displayName || contactNames.get(m.username) || m.username,
      }))
      .filter((item) =>
        `${item.label} ${item.id}`.toLowerCase().includes(query.trim().toLowerCase()),
      );
  }, [members, excluded, sessionState.contacts, query]);

  const toggle = (id: string) => {
    setSelected((prev) =>
      prev.includes(id)
        ? prev.filter((item) => item !== id)
        : prev.length >= 100
          ? prev
          : [...prev, id],
    );
  };

  return createPortal(
    <dialog
      ref={dialogRef}
      className="call-picker-dialog"
      aria-label={
        mode === "start"
          ? t("calls.startGroupCallTitle", { name: groupName })
          : t("calls.inviteMembersTitle")
      }
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <section
        className="modal new-chat call-picker-modal"
        onClick={(event) => event.stopPropagation()}
      >
        <div className="modal-head">
          <h2>
            {mode === "start"
              ? t("calls.startGroupCallTitle", { name: groupName })
              : t("calls.inviteMembersTitle")}
          </h2>
          <Button icon="close" label={t("calls.close")} onClick={onClose} />
        </div>

        {mode === "start" && (
          <div className="new-chat-tabs" role="tablist" aria-label={t("calls.callMode")}>
            <button
              type="button"
              role="tab"
              aria-selected={callType === "voice"}
              onClick={() => {
                setCallType("voice");
                setCameraEnabled(false);
              }}
            >
              {t("calls.voiceCall")}
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={callType === "video"}
              disabled={!allowVideo}
              onClick={() => {
                setCallType("video");
                setCameraEnabled(true);
              }}
            >
              {t("calls.videoCall")}
            </button>
          </div>
        )}

        <label className="search">
          <input
            autoFocus
            aria-label={t("calls.searchMembers")}
            placeholder={t("calls.searchMembers")}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </label>

        {error && (
          <p className="attachment-error" role="alert">
            {error}
          </p>
        )}

        <div className="new-chat-contacts">
          {loading ? (
            <p className="no-results">{t("calls.loadingMembers")}</p>
          ) : candidates.length === 0 ? (
            <p className="no-results">{t("calls.noMembersAvailable")}</p>
          ) : (
            candidates.map((member) => {
              const checked = selected.includes(member.id);
              return (
                <label className="new-chat-contact" key={member.id}>
                  <HubAvatar
                    connection={connection}
                    account={account}
                    username={member.id}
                    name={member.label}
                  />
                  <span>{member.label}</span>
                  <input
                    type="checkbox"
                    aria-label={t("calls.selectMember", { name: member.label })}
                    checked={checked}
                    disabled={!checked && selected.length >= 100}
                    onChange={() => toggle(member.id)}
                  />
                </label>
              );
            })
          )}
        </div>

        {mode === "start" && (
          <div className="call-picker-options">
            <button
              type="button"
              className={`call-toggle-pill ${muted ? "is-off" : ""}`}
              aria-pressed={muted}
              onClick={() => setMuted((v) => !v)}
            >
              <Icon name={muted ? "mic-off" : "mic"} />
              {muted ? t("calls.startMuted") : t("calls.microphoneOn")}
            </button>
            {callType === "video" && (
              <button
                type="button"
                className={`call-toggle-pill ${!cameraEnabled ? "is-off" : ""}`}
                aria-pressed={cameraEnabled}
                onClick={() => setCameraEnabled((v) => !v)}
              >
                <Icon name={cameraEnabled ? "video" : "video-off"} />
                {cameraEnabled ? t("calls.cameraOn") : t("calls.cameraOff")}
              </button>
            )}
          </div>
        )}

        <div className="new-chat-footer">
          <small>{t("calls.membersSelected", { count: selected.length })}</small>
          <button
            type="button"
            className="primary call-start-btn"
            disabled={selected.length === 0 || busy}
            onClick={() =>
              onSubmit({
                invitees: selected,
                callType,
                muted,
                cameraEnabled: callType === "video" ? cameraEnabled : false,
              })
            }
          >
            <Icon name={mode === "invite" ? "user-plus" : callType === "video" ? "video" : "phone"} />
            {mode === "invite"
              ? t("calls.inviteSelected", { count: selected.length })
              : callType === "video"
                ? t("calls.startGroupVideoCall")
                : t("calls.startGroupVoiceCall")}
          </button>
        </div>
      </section>
    </dialog>,
    document.body,
  );
}

/** Non-blocking top-right incoming call toast; never steals composer focus. */
export function CallIncomingToast({
  connection,
  account,
  controller,
  resolveName,
}: {
  connection: Connection;
  account: string;
  controller: CallController;
  resolveName: (id: string) => string;
}) {
  const callState = useSyncExternalStore(controller.subscribe, controller.snapshot);
  const offer = callState.currentOffer;
  const pending = !offer ? callState.currentPendingOffer : null;
  const activeOffer = offer ?? pending;
  const key = offerKey(activeOffer);

  if (
    !activeOffer ||
    key === callState.dismissedOfferKey ||
    callState.currentCall?.ownedByThisClient
  ) {
    return null;
  }

  const callerName = resolveName(activeOffer.callerId);
  const isVideo = activeOffer.callType === "video";
  const isPreparing = !offer;

  return (
    <aside
      className="call-incoming-toast"
      role="region"
      aria-label={t("calls.incomingCallFrom", { name: callerName })}
    >
      <div className="call-incoming-top">
        <div className="call-incoming-avatar-wrap">
          <HubAvatar
            connection={connection}
            account={account}
            username={activeOffer.callerId}
            name={callerName}
          />
          <span className="call-incoming-badge" aria-hidden="true">
            <Icon name={isVideo ? "video" : "phone-incoming"} />
          </span>
        </div>
        <div className="call-incoming-copy">
          <strong>{callerName}</strong>
          <span>
            {isPreparing
              ? t("calls.incomingPreparing")
              : isVideo
                ? t("calls.incomingVideoCall")
                : t("calls.incomingVoiceCall")}
          </span>
          {!isPreparing && callState.ringtoneAutoplayBlocked && !callState.ringtoneMuted && (
            <small className="call-incoming-autoplay">{t("calls.ringtoneMutedByBrowser")}</small>
          )}
        </div>
        <div className="call-incoming-mini">
          {!isPreparing && (
            <Button
              icon={callState.ringtoneMuted ? "volume-off" : "volume"}
              label={
                callState.ringtoneMuted
                  ? t("calls.unmuteRingtone")
                  : t("calls.muteRingtone")
              }
              onClick={() => controller.setRingtoneMuted(!callState.ringtoneMuted)}
            />
          )}
          <Button
            icon="close"
            label={t("calls.dismissLocally")}
            onClick={() => controller.dismissOfferLocally()}
          />
        </div>
      </div>

      {!isPreparing && (
        <div className="call-incoming-actions">
          <button
            type="button"
            className="call-btn-decline"
            disabled={callState.actionPending}
            onClick={() => void controller.rejectOffer()}
          >
            <Icon name="phone-off" />
            {t("calls.decline")}
          </button>
          {isVideo && (
            <button
              type="button"
              className="call-btn-audio-only"
              disabled={callState.actionPending}
              onClick={() => void controller.answerOffer({ cameraEnabled: false })}
            >
              <Icon name="phone" />
              {t("calls.answerAudioOnly")}
            </button>
          )}
          <button
            type="button"
            className="call-btn-answer"
            disabled={callState.actionPending}
            onClick={() => void controller.answerOffer({ cameraEnabled: isVideo })}
          >
            <Icon name={isVideo ? "video" : "phone"} />
            {isVideo ? t("calls.answerVideo") : t("calls.answer")}
          </button>
        </div>
      )}
    </aside>
  );
}
