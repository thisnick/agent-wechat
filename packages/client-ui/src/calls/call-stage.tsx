import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { createPortal } from "react-dom";
import type {
  CallConferenceInvitation,
  CallParticipant,
  CallVideoTrack,
} from "@agent-wechat/shared";
import type { Connection } from "../connection.ts";
import { ConversationAvatar, HubAvatar } from "../avatars.tsx";
import { t } from "../i18n.ts";
import {
  Button,
  Icon,
  Notice,
  NoticeAction,
  NoticeDescription,
  NoticeTitle,
} from "../ui.tsx";
import {
  canCallThread,
  formatCallDuration,
  isCancellableCall,
  type CallController,
  type CallView,
} from "./call-controller.ts";
import {
  CallIncomingToast,
  CallPreviewPopover,
  GroupCallPickerModal,
  LocalVideoSurface,
  RemoteVideoCanvas,
} from "./call-modals.tsx";

function callErrorText(code: string): string {
  switch (code) {
    case "busy":
      return t("calls.errorBusy");
    case "already_claimed":
      return t("calls.errorAlreadyClaimed");
    case "offer_expired":
      return t("calls.errorOfferExpired");
    case "unsupported":
      return t("calls.errorUnsupported");
    case "microphone_unavailable":
      return t("calls.errorMicrophoneUnavailable");
    case "camera_unavailable":
      return t("calls.errorCameraUnavailable");
    case "output_device_failed":
      return t("calls.errorOutputDeviceFailed");
    case "outcome_unknown":
      return t("calls.noticeUncertain");
    default:
      return t("calls.errorGeneric", { code });
  }
}

function callStateLabel(call: CallView, elapsedSeconds: number): string {
  switch (call.state) {
    case "dialing":
      return t("calls.stateDialing");
    case "ringing":
      return t("calls.stateRinging");
    case "connecting":
      return t("calls.stateConnecting");
    case "connected":
      return formatCallDuration(elapsedSeconds);
    case "ending":
      return t("calls.stateEnding");
    case "uncertain":
      return t("calls.stateUncertain");
    default:
      return t("calls.stateEnded");
  }
}

/** Voice & Video buttons placed in `.chat-header` (left of Search and Details). */
export function CallHeaderActions({
  controller,
  threadId,
  threadName,
  ownUsername,
  onOpenGroupPicker,
}: {
  controller: CallController;
  threadId: string | null;
  threadName: string;
  ownUsername: string | null | undefined;
  onOpenGroupPicker: (callType: "voice" | "video") => void;
}) {
  const callState = useSyncExternalStore(controller.subscribe, controller.snapshot);
  const [previewOpen, setPreviewOpen] = useState(false);
  const videoTriggerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setPreviewOpen(false);
  }, [threadId]);

  const eligibility = canCallThread(threadId, ownUsername, callState.capabilities);
  if (!threadId || (!eligibility.voice && !eligibility.video)) {
    return null;
  }

  const busy = callState.availability.busy || callState.actionPending;
  const busyTitle = callState.availability.busy ? t("calls.busyTooltip") : undefined;

  return (
    <div className="call-header-actions">
      {eligibility.voice && (
        <Button
          icon="phone"
          label={eligibility.isGroup ? t("calls.startGroupVoiceCall") : t("calls.voiceCall")}
          title={busyTitle ?? (eligibility.isGroup ? t("calls.startGroupVoiceCall") : t("calls.voiceCall"))}
          disabled={busy}
          onClick={() => {
            if (eligibility.isGroup) {
              onOpenGroupPicker("voice");
            } else {
              void controller.dialDirect(threadId, { callType: "voice" });
            }
          }}
        />
      )}
      {eligibility.video && (
        <div ref={videoTriggerRef} className="call-video-trigger-wrap">
          <Button
            icon="video"
            label={eligibility.isGroup ? t("calls.startGroupVideoCall") : t("calls.videoCall")}
            title={busyTitle ?? (eligibility.isGroup ? t("calls.startGroupVideoCall") : t("calls.videoCall"))}
            disabled={busy}
            onClick={() => {
              if (eligibility.isGroup) {
                onOpenGroupPicker("video");
              } else if (callState.skipVideoPreview) {
                void controller.dialDirect(threadId, { callType: "video", cameraEnabled: true });
              } else {
                setPreviewOpen((v) => !v);
              }
            }}
          />
          {previewOpen && !eligibility.isGroup && (
            <CallPreviewPopover
              controller={controller}
              peerName={threadName}
              triggerRef={videoTriggerRef}
              onClose={() => setPreviewOpen(false)}
              onStart={({ muted, cameraEnabled }) => {
                setPreviewOpen(false);
                void controller.dialDirect(threadId, {
                  callType: "video",
                  muted,
                  cameraEnabled,
                });
              }}
            />
          )}
        </div>
      )}
    </div>
  );
}

/** Mirrored voice/video call actions inside the Conversation Details sidebar. */
export function CallDetailActions({
  controller,
  threadId,
  threadName,
  ownUsername,
  onOpenGroupPicker,
}: {
  controller: CallController;
  threadId: string;
  threadName?: string;
  ownUsername: string | null | undefined;
  onOpenGroupPicker: (callType: "voice" | "video") => void;
}) {
  const callState = useSyncExternalStore(controller.subscribe, controller.snapshot);
  const [previewOpen, setPreviewOpen] = useState(false);
  const triggerRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    setPreviewOpen(false);
  }, [threadId]);
  const eligibility = canCallThread(threadId, ownUsername, callState.capabilities);
  if (!eligibility.voice && !eligibility.video) return null;
  const busy = callState.availability.busy || callState.actionPending;

  return (
    <div className="detail-section call-detail-actions">
      <div className="section-head">{t("calls.sectionTitle")}</div>
      <div ref={triggerRef} className="call-detail-buttons call-video-trigger-wrap">
        {eligibility.voice && (
          <button
            type="button"
            className="secondary call-detail-btn"
            disabled={busy}
            title={busy ? t("calls.busyTooltip") : undefined}
            onClick={() =>
              eligibility.isGroup
                ? onOpenGroupPicker("voice")
                : void controller.dialDirect(threadId, { callType: "voice" })
            }
          >
            <Icon name="phone" />
            {t("calls.voiceCall")}
          </button>
        )}
        {eligibility.video && (
          <button
            type="button"
            className="secondary call-detail-btn"
            disabled={busy}
            title={busy ? t("calls.busyTooltip") : undefined}
            onClick={() => {
              if (eligibility.isGroup) {
                onOpenGroupPicker("video");
              } else if (callState.skipVideoPreview) {
                void controller.dialDirect(threadId, { callType: "video", cameraEnabled: true });
              } else {
                setPreviewOpen((v) => !v);
              }
            }}
          >
            <Icon name="video" />
            {t("calls.videoCall")}
          </button>
        )}
        {previewOpen && !eligibility.isGroup && (
          <CallPreviewPopover
            controller={controller}
            peerName={threadName ?? threadId}
            triggerRef={triggerRef}
            onClose={() => setPreviewOpen(false)}
            onStart={({ muted, cameraEnabled }) => {
              setPreviewOpen(false);
              void controller.dialDirect(threadId, {
                callType: "video",
                muted,
                cameraEnabled,
              });
            }}
          />
        )}
      </div>
    </div>
  );
}

/** Green join bar below `.chat-header` when a group has an active conference invitation. */
export function ConferenceJoinBar({
  controller,
  invitation,
  callerName,
}: {
  controller: CallController;
  invitation: CallConferenceInvitation;
  callerName: string;
}) {
  const callState = useSyncExternalStore(controller.subscribe, controller.snapshot);
  if (!callState.capabilities.conferenceJoin) return null;
  const busy = callState.availability.busy || callState.actionPending;
  return (
    <div className="call-conference-join-bar" role="region" aria-label={t("calls.activeGroupCall")}>
      <div className="call-conference-join-copy">
        <span className="call-live-dot" aria-hidden="true" />
        <strong>{t("calls.activeGroupCall")}</strong>
        <span>{t("calls.startedBy", { name: callerName })}</span>
      </div>
      <div className="call-conference-join-actions">
        <button
          type="button"
          className="call-join-btn"
          disabled={busy}
          onClick={() => void controller.joinConference(invitation, { cameraEnabled: false })}
        >
          <Icon name="phone" />
          {t("calls.joinAudio")}
        </button>
        {callState.capabilities.conferenceVideo && (
          <button
            type="button"
            className="call-join-btn call-join-video"
            disabled={busy}
            onClick={() => void controller.joinConference(invitation, { cameraEnabled: true })}
          >
            <Icon name="video" />
            {t("calls.joinVideo")}
          </button>
        )}
      </div>
    </div>
  );
}

/** Lightweight level meter bar used in the voice strip & device popover. */
function AudioLevelMeter({ level, label }: { level: number; label: string }) {
  const clamped = Math.max(0, Math.min(1, level));
  return (
    <span className="call-meter" role="meter" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(clamped * 100)}>
      <span className="call-meter-fill" style={{ width: `${Math.round(clamped * 100)}%` }} />
    </span>
  );
}

/** Compact docked voice strip shown above chat for voice-only calls. */
function VoiceCallStrip({
  connection,
  account,
  controller,
  call,
  peerName,
  peerId,
  elapsedSeconds,
}: {
  connection: Connection;
  account: string;
  controller: CallController;
  call: CallView;
  peerName: string;
  peerId: string | null;
  elapsedSeconds: number;
}) {
  const callState = useSyncExternalStore(controller.subscribe, controller.snapshot);
  return (
    <div className="call-voice-strip" role="region" aria-label={t("calls.activeVoiceCall")}>
      <div className="call-voice-strip-info">
        {peerId ? (
          <ConversationAvatar
            connection={connection}
            account={account}
            username={peerId}
            name={peerName}
          />
        ) : (
          <span className="call-voice-strip-icon">
            <Icon name="phone" />
          </span>
        )}
        <div className="call-voice-strip-copy">
          <strong>{peerName}</strong>
          <span className="call-voice-strip-status">
            <span className="call-live-dot" aria-hidden="true" />
            {callStateLabel(call, elapsedSeconds)}
          </span>
        </div>
        <div className="call-voice-strip-meters" aria-hidden="true">
          <AudioLevelMeter level={callState.levels.micLevel} label={t("calls.microphoneLevel")} />
          <AudioLevelMeter level={callState.levels.speakerLevel} label={t("calls.speakerLevel")} />
        </div>
      </div>
      <div className="call-voice-strip-controls">
        <Button
          icon={callState.localMuted ? "mic-off" : "mic"}
          label={callState.localMuted ? t("calls.unmute") : t("calls.mute")}
          className={callState.localMuted ? "call-ctrl-off" : ""}
          onClick={() => void controller.toggleMute()}
        />
        <Button
          icon={callState.speakerMuted ? "volume-off" : "volume"}
          label={callState.speakerMuted ? t("calls.unmuteSpeaker") : t("calls.muteSpeaker")}
          className={callState.speakerMuted ? "call-ctrl-off" : ""}
          onClick={() => controller.toggleSpeakerMuted()}
        />
        <Button
          icon="maximize"
          label={t("calls.expandCall")}
          onClick={() => controller.setPresentationMode("theater")}
        />
        <button
          type="button"
          className="call-strip-hangup"
          aria-label={isCancellableCall(call) ? t("calls.cancelCall") : t("calls.hangUp")}
          disabled={callState.actionPending}
          onClick={() => void controller.hangupOrCancel()}
        >
          <Icon name="phone-off" />
        </button>
      </div>
    </div>
  );
}

/** Floating draggable Picture-in-Picture card when minimized from Theater. */
function CallPipCard({
  controller,
  call,
  peerName,
  primaryVideoTrack,
  elapsedSeconds,
}: {
  controller: CallController;
  call: CallView;
  peerName: string;
  primaryVideoTrack: CallVideoTrack | null;
  elapsedSeconds: number;
}) {
  const callState = useSyncExternalStore(controller.subscribe, controller.snapshot);
  const [pos, setPos] = useState<{ right: number; bottom: number }>({ right: 20, bottom: 20 });
  const dragRef = useRef<{
    id: number;
    startX: number;
    startY: number;
    startRight: number;
    startBottom: number;
  } | null>(null);

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if ((event.target as Element).closest("button")) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    dragRef.current = {
      id: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      startRight: pos.right,
      startBottom: pos.bottom,
    };
  };

  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const drag = dragRef.current;
    if (!drag || drag.id !== event.pointerId) return;
    const dx = event.clientX - drag.startX;
    const dy = event.clientY - drag.startY;
    const nextRight = Math.max(12, Math.min(window.innerWidth - 260, drag.startRight - dx));
    const nextBottom = Math.max(12, Math.min(window.innerHeight - 180, drag.startBottom - dy));
    setPos({ right: nextRight, bottom: nextBottom });
  };

  const endDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (dragRef.current?.id === event.pointerId) {
      dragRef.current = null;
    }
  };

  return createPortal(
    <aside
      className="call-pip-card"
      role="region"
      aria-label={t("calls.pipLabel", { name: peerName })}
      style={{ right: `${pos.right}px`, bottom: `${pos.bottom}px` }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onLostPointerCapture={() => {
        dragRef.current = null;
      }}
    >
      <div className="call-pip-stage" onDoubleClick={() => controller.setPresentationMode("theater")}>
        {primaryVideoTrack && callState.remoteTrackFrames[primaryVideoTrack.id] ? (
          <RemoteVideoCanvas
            controller={controller}
            trackId={primaryVideoTrack.id}
            label={peerName}
          />
        ) : (
          <div className="call-pip-fallback">
            <Icon name={call.callType === "video" ? "video" : "phone"} />
            <span>{peerName}</span>
          </div>
        )}
        <div className="call-pip-badge">
          <span className="call-live-dot" aria-hidden="true" />
          <span>{callStateLabel(call, elapsedSeconds)}</span>
        </div>
      </div>
      <div className="call-pip-controls">
        <Button
          icon={callState.localMuted ? "mic-off" : "mic"}
          label={callState.localMuted ? t("calls.unmute") : t("calls.mute")}
          className={callState.localMuted ? "call-ctrl-off" : ""}
          onClick={() => void controller.toggleMute()}
        />
        <Button
          icon="maximize"
          label={t("calls.expandCall")}
          onClick={() => controller.setPresentationMode("theater")}
        />
        <button
          type="button"
          className="call-strip-hangup"
          aria-label={isCancellableCall(call) ? t("calls.cancelCall") : t("calls.hangUp")}
          disabled={callState.actionPending}
          onClick={() => void controller.hangupOrCancel()}
        >
          <Icon name="phone-off" />
        </button>
      </div>
    </aside>,
    document.body,
  );
}

/** Hybrid Theater <dialog> + True Fullscreen call stage with slide-over chat/roster drawer. */
function CallTheaterDialog({
  connection,
  account,
  controller,
  call,
  peerName,
  peerId,
  resolveName,
  onOpenInvite,
}: {
  connection: Connection;
  account: string;
  controller: CallController;
  call: CallView;
  peerName: string;
  peerId: string | null;
  resolveName: (id: string) => string;
  onOpenInvite: () => void;
}) {
  const callState = useSyncExternalStore(controller.subscribe, controller.snapshot);
  const sessionState = useSyncExternalStore(
    connection.session.subscribe,
    connection.session.snapshot,
  );
  const dialogRef = useRef<HTMLDialogElement>(null);
  const [devicesOpen, setDevicesOpen] = useState(false);
  const [drawerTab, setDrawerTab] = useState<"chat" | "roster" | null>(null);
  const [chromeHidden, setChromeHidden] = useState(false);
  const [quickDraft, setQuickDraft] = useState("");
  const [quickSending, setQuickSending] = useState(false);
  const [quickError, setQuickError] = useState(false);
  const idleTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const isFullscreen = callState.presentationMode === "fullscreen";

  // Open/close native <dialog> top-layer modal cleanly.
  useEffect(() => {
    const el = dialogRef.current;
    if (!el) return;
    if (!el.open) {
      try {
        el.showModal();
      } catch {
        // Already open or detached.
      }
    }
    return () => {
      if (document.fullscreenElement === el) {
        void document.exitFullscreen?.().catch(() => {});
      }
      if (el.open) el.close();
    };
  }, []);

  // Sync browser Fullscreen API + native host fallback.
  useEffect(() => {
    const el = dialogRef.current;
    if (!el) return;
    if (isFullscreen) {
      if (document.fullscreenElement !== el) {
        if (typeof el.requestFullscreen === "function") {
          void el.requestFullscreen().catch(() => {
            void controller.requestHostFullscreen(true);
          });
        } else {
          void controller.requestHostFullscreen(true);
        }
      }
    } else {
      if (document.fullscreenElement === el) {
        void document.exitFullscreen?.().catch(() => {});
      }
      void controller.requestHostFullscreen(false);
      setChromeHidden(false);
    }
  }, [isFullscreen, controller]);

  useEffect(() => {
    const onFsChange = () => {
      if (!document.fullscreenElement && controller.snapshot().presentationMode === "fullscreen") {
        controller.setPresentationMode("theater");
      }
    };
    document.addEventListener("fullscreenchange", onFsChange);
    return () => document.removeEventListener("fullscreenchange", onFsChange);
  }, [controller]);

  // 3 s idle auto-hide chrome in True Fullscreen.
  const wakeChrome = useCallback(() => {
    setChromeHidden(false);
    if (idleTimerRef.current) clearTimeout(idleTimerRef.current);
    if (isFullscreen && !devicesOpen && !drawerTab) {
      idleTimerRef.current = setTimeout(() => setChromeHidden(true), 3000);
    }
  }, [isFullscreen, devicesOpen, drawerTab]);

  useEffect(() => {
    wakeChrome();
    return () => {
      if (idleTimerRef.current) clearTimeout(idleTimerRef.current);
    };
  }, [wakeChrome]);

  const toggleFullscreen = useCallback(() => {
    controller.setPresentationMode(isFullscreen ? "theater" : "fullscreen");
  }, [controller, isFullscreen]);

  const videoReceiveTracks = useMemo(
    () =>
      callState.tracks.filter(
        (tr): tr is CallVideoTrack => tr.media === "video" && tr.direction === "receive",
      ),
    [callState.tracks],
  );

  const audioTrackByParticipant = useMemo(() => {
    const map = new Map<string, number>();
    for (const tr of callState.tracks) {
      if (tr.media === "audio" && tr.direction === "receive" && tr.participantId) {
        map.set(tr.participantId, tr.id);
      }
    }
    return map;
  }, [callState.tracks]);

  const videoTrackByParticipant = useMemo(() => {
    const map = new Map<string, CallVideoTrack>();
    for (const tr of videoReceiveTracks) {
      if (tr.participantId) map.set(tr.participantId, tr);
    }
    return map;
  }, [videoReceiveTracks]);

  const joinedParticipants: CallParticipant[] = useMemo(() => {
    const list = (call.participants ?? []).filter((p) => p.membership !== "left");
    return list;
  }, [call.participants]);

  const isConference = call.kind === "conference";
  const singleRemoteVideo = !isConference ? videoReceiveTracks[0] ?? null : null;
  const directRemoteState = useMemo(() => {
    if (isConference) return { muted: undefined as boolean | undefined, cameraEnabled: undefined as boolean | undefined };
    let muted: boolean | undefined;
    let cameraEnabled: boolean | undefined;
    for (const item of call.controls ?? []) {
      if (item.control.kind === "microphone") muted = item.control.muted;
      else if (item.control.kind === "camera") cameraEnabled = item.control.enabled;
    }
    return { muted, cameraEnabled };
  }, [isConference, call.controls]);

  const chatTargetId = call.groupId ?? peerId;
  const chatMatchesSelected = !!chatTargetId && sessionState.selected === chatTargetId;
  const recentMessages = useMemo(() => {
    if (!chatMatchesSelected) return [];
    return sessionState.messages.slice(-20);
  }, [chatMatchesSelected, sessionState.messages]);

  const sendQuickReply = async () => {
    const trimmed = quickDraft.trim();
    if (!trimmed || !chatTargetId || quickSending) return;
    if (connection.session.snapshot().selected !== chatTargetId) {
      setQuickError(true);
      return;
    }
    setQuickSending(true);
    setQuickError(false);
    try {
      const ok = await connection.session.send(trimmed);
      if (ok) {
        setQuickDraft("");
      } else {
        setQuickError(true);
      }
    } catch {
      setQuickError(true);
    } finally {
      setQuickSending(false);
    }
  };

  const subscribeLocalJpeg = useCallback(
    (cb: (bytes: Uint8Array) => void) => controller.subscribeLocalJpeg(cb),
    [controller],
  );

  return createPortal(
    <dialog
      ref={dialogRef}
      className={`call-theater-dialog ${isFullscreen ? "is-fullscreen" : ""} ${chromeHidden ? "chrome-hidden" : ""}`}
      aria-label={t("calls.callDialogLabel", { name: peerName })}
      onCancel={(event) => {
        // Esc ONLY steps down Fullscreen -> Theater and NEVER hangs up or closes an active call!
        event.preventDefault();
        if (isFullscreen) {
          controller.setPresentationMode("theater");
        } else if (devicesOpen) {
          setDevicesOpen(false);
        } else if (drawerTab) {
          setDrawerTab(null);
        } else {
          controller.setPresentationMode(call.callType === "voice" && !isConference ? "strip" : "pip");
        }
      }}
      onClose={() => {
        // Recover gracefully if the browser's CloseWatcher force-closes <dialog> on rapid Esc presses.
        const currentMode = controller.snapshot().presentationMode;
        if (currentMode === "theater" || currentMode === "fullscreen") {
          controller.setPresentationMode(call.callType === "voice" && !isConference ? "strip" : "pip");
        }
      }}
      onPointerMove={wakeChrome}
      onPointerDown={wakeChrome}
      onKeyDown={(event) => {
        wakeChrome();
        const target = event.target as HTMLElement | null;
        const tag = target?.tagName;
        if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target?.isContentEditable) return;
        if (event.key.toLowerCase() === "f" && !event.ctrlKey && !event.metaKey && !event.altKey) {
          event.preventDefault();
          toggleFullscreen();
        }
      }}
    >
      <div className="call-theater-shell">
        <header className="call-theater-header">
          <div className="call-theater-title">
            <span className="call-live-dot" aria-hidden="true" />
            <div>
              <h2>{peerName}</h2>
              <p>
                {call.callType === "video" ? t("calls.videoCall") : t("calls.voiceCall")}
                {" · "}
                {callStateLabel(call, callState.elapsedSeconds)}
                {isConference && joinedParticipants.length > 0
                  ? ` · ${t("calls.participantCount", { count: joinedParticipants.length })}`
                  : ""}
              </p>
            </div>
          </div>
          <div className="call-theater-top-actions">
            {isConference && callState.capabilities.conferenceInvite && (
              <Button
                icon="user-plus"
                label={t("calls.invitePeople")}
                disabled={call.state !== "connected"}
                onClick={onOpenInvite}
              />
            )}
            <Button
              icon="people"
              label={t("calls.participants")}
              className={drawerTab === "roster" ? "is-active" : ""}
              onClick={() => setDrawerTab((tab) => (tab === "roster" ? null : "roster"))}
            />
            {chatTargetId && (
              <Button
                icon="message-square"
                label={t("calls.inCallChat")}
                className={drawerTab === "chat" ? "is-active" : ""}
                onClick={() => setDrawerTab((tab) => (tab === "chat" ? null : "chat"))}
              />
            )}
            <Button
              icon="pip"
              label={t("calls.minimizePip")}
              onClick={() =>
                controller.setPresentationMode(
                  call.callType === "voice" && !isConference ? "strip" : "pip",
                )
              }
            />
            <Button
              icon={isFullscreen ? "minimize" : "maximize"}
              label={isFullscreen ? t("calls.exitFullscreen") : t("calls.enterFullscreen")}
              onClick={toggleFullscreen}
            />
          </div>
        </header>

        {/* Honest banners for 18:30 cap warning, media warnings, and invite results */}
        <div className="call-theater-notices">
          {callState.capWarning && (
            <Notice tone="neutral">
              <NoticeTitle>{t("calls.capWarningTitle")}</NoticeTitle>
              <NoticeDescription>{t("calls.capWarningDescription")}</NoticeDescription>
            </Notice>
          )}
          {callState.mediaWarning && (
            <Notice tone="error">
              <NoticeDescription>{callErrorText(callState.mediaWarning)}</NoticeDescription>
              <NoticeAction>
                <button type="button" onClick={() => controller.clearNotice()}>
                  {t("calls.dismiss")}
                </button>
              </NoticeAction>
            </Notice>
          )}
          {callState.lastError && (
            <Notice tone="error">
              <NoticeDescription>{callErrorText(callState.lastError)}</NoticeDescription>
              <NoticeAction>
                <button type="button" onClick={() => controller.clearNotice()}>
                  {t("calls.dismiss")}
                </button>
              </NoticeAction>
            </Notice>
          )}
          {callState.invitationResults && (
            <Notice tone="neutral">
              <NoticeTitle>{t("calls.invitationResultsTitle")}</NoticeTitle>
              <NoticeDescription>
                {callState.invitationResults.results
                  .map(
                    (item) =>
                      `${resolveName(item.contactId)}: ${item.status}${item.verificationRequired ? ` (${t("calls.verificationRequired")})` : ""}`,
                  )
                  .join(" · ")}
              </NoticeDescription>
              <NoticeAction>
                <button type="button" onClick={() => controller.clearNotice()}>
                  {t("calls.dismiss")}
                </button>
              </NoticeAction>
            </Notice>
          )}
        </div>

        <div className="call-theater-body">
          <div className={`call-stage ${isConference ? "is-conference" : "is-direct"}`}>
            {isConference ? (
              <div
                className="call-conference-grid"
                data-count={Math.min(9, Math.max(1, joinedParticipants.length || 1))}
              >
                {joinedParticipants.length === 0 ? (
                  <div className="call-tile call-tile-empty">
                    <ConversationAvatar
                      connection={connection}
                      account={account}
                      username={call.groupId ?? peerId ?? ""}
                      name={peerName}
                    />
                    <strong>{peerName}</strong>
                    <span>{callStateLabel(call, callState.elapsedSeconds)}</span>
                  </div>
                ) : (
                  joinedParticipants.slice(0, 9).map((participant) => {
                    const label = resolveName(participant.contactId);
                    const vTrack = videoTrackByParticipant.get(participant.participantId);
                    const hasFrame = !!vTrack && !!callState.remoteTrackFrames[vTrack.id];
                    const aTrackId = audioTrackByParticipant.get(participant.participantId);
                    const level = aTrackId ? (callState.levels.trackLevels[aTrackId] ?? 0) : 0;
                    const speaking = level > 0.08;
                    return (
                      <div
                        key={participant.participantId}
                        className={`call-tile ${speaking ? "is-speaking" : ""}`}
                      >
                        {vTrack && hasFrame ? (
                          <RemoteVideoCanvas
                            controller={controller}
                            trackId={vTrack.id}
                            label={label}
                          />
                        ) : (
                          <div className="call-tile-avatar">
                            <HubAvatar
                              connection={connection}
                              account={account}
                              username={participant.contactId}
                              name={label}
                            />
                          </div>
                        )}
                        <div className="call-tile-footer">
                          <span className="call-tile-name">{label}</span>
                          <span className="call-tile-badges">
                            {participant.membership === "invited" && (
                              <span className="call-chip">{t("calls.invited")}</span>
                            )}
                            {participant.microphoneEnabled === false ? (
                              <span className="call-badge is-muted" title={t("calls.muted")}>
                                <Icon name="mic-off" />
                              </span>
                            ) : participant.microphoneEnabled === true ? (
                              <span className="call-badge" title={t("calls.microphoneOn")}>
                                <Icon name="mic" />
                              </span>
                            ) : (
                              <span
                                className="call-badge is-neutral"
                                title={t("calls.remoteStateUnknown")}
                              >
                                ·
                              </span>
                            )}
                            {call.callType === "video" && participant.cameraEnabled === false && (
                              <span className="call-badge is-muted" title={t("calls.cameraOff")}>
                                <Icon name="video-off" />
                              </span>
                            )}
                          </span>
                        </div>
                      </div>
                    );
                  })
                )}
              </div>
            ) : (
              <div className="call-direct-stage">
                {singleRemoteVideo && callState.remoteTrackFrames[singleRemoteVideo.id] ? (
                  <RemoteVideoCanvas
                    controller={controller}
                    trackId={singleRemoteVideo.id}
                    label={peerName}
                    className="call-remote-primary"
                  />
                ) : (
                  <div className="call-direct-placeholder">
                    {peerId ? (
                      <HubAvatar
                        connection={connection}
                        account={account}
                        username={peerId}
                        name={peerName}
                      />
                    ) : (
                      <Icon name="phone" />
                    )}
                    <strong>{peerName}</strong>
                    <span>{callStateLabel(call, callState.elapsedSeconds)}</span>
                  </div>
                )}
                {(directRemoteState.muted === true || directRemoteState.cameraEnabled === false) && (
                  <div className="call-direct-status-badges" aria-live="polite">
                    {directRemoteState.muted === true && (
                      <span className="call-badge is-muted" title={t("calls.muted")}>
                        <Icon name="mic-off" />
                      </span>
                    )}
                    {call.callType === "video" && directRemoteState.cameraEnabled === false && (
                      <span className="call-badge is-muted" title={t("calls.cameraOff")}>
                        <Icon name="video-off" />
                      </span>
                    )}
                    <span>{peerName}</span>
                  </div>
                )}
              </div>
            )}

            {/* Self-view corner tile for active connected video calls */}
            {call.callType === "video" && callState.localCamera && call.state === "connected" && (
              <div className="call-self-view" aria-label={t("calls.selfView")}>
                <LocalVideoSurface
                  stream={callState.localStream}
                  subscribeJpeg={subscribeLocalJpeg}
                  label={t("calls.selfView")}
                />
                <span className="call-self-label">{t("calls.you")}</span>
              </div>
            )}
          </div>

          {/* Translucent slide-over Chat or Roster drawer */}
          {drawerTab && (
            <aside
              className="call-side-drawer"
              aria-label={
                drawerTab === "roster" ? t("calls.participants") : t("calls.inCallChat")
              }
            >
              <div className="call-drawer-head">
                <strong>
                  {drawerTab === "roster" ? t("calls.participants") : t("calls.inCallChat")}
                </strong>
                <Button
                  icon="close"
                  label={t("calls.close")}
                  onClick={() => setDrawerTab(null)}
                />
              </div>
              {drawerTab === "roster" ? (
                <ul className="call-roster-list">
                  <li className="call-roster-row">
                    <span>{t("calls.you")}</span>
                    <span className="call-roster-status">
                      {callState.localMuted ? t("calls.muted") : t("calls.microphoneOn")}
                    </span>
                  </li>
                  {!isConference && joinedParticipants.length === 0 && (
                    <li className="call-roster-row">
                      <span>{peerName}</span>
                      <span className="call-roster-status">
                        {directRemoteState.muted === true
                          ? t("calls.muted")
                          : directRemoteState.muted === false
                            ? t("calls.microphoneOn")
                            : t("calls.joined")}
                      </span>
                    </li>
                  )}
                  {joinedParticipants.map((p) => (
                    <li key={p.participantId} className="call-roster-row">
                      <span>{resolveName(p.contactId)}</span>
                      <span className="call-roster-status">
                        {p.membership === "invited"
                          ? t("calls.invited")
                          : p.microphoneEnabled === false
                            ? t("calls.muted")
                            : p.microphoneEnabled === true
                              ? t("calls.microphoneOn")
                              : t("calls.joined")}
                      </span>
                    </li>
                  ))}
                </ul>
              ) : (
                <div className="call-drawer-chat">
                  <div className="call-drawer-messages">
                    {!chatMatchesSelected ? (
                      <p className="call-drawer-empty">{t("calls.openChatToMessage")}</p>
                    ) : recentMessages.length === 0 ? (
                      <p className="call-drawer-empty">{t("calls.noRecentMessages")}</p>
                    ) : (
                      recentMessages.map((msg) => (
                        <div
                          key={msg.id}
                          className={`call-drawer-msg ${msg.direction === "outgoing" ? "is-own" : ""}`}
                        >
                          <small>
                            {msg.direction === "outgoing"
                              ? t("calls.you")
                              : resolveName(msg.senderId ?? "")}
                          </small>
                          <p>{msg.body || t("calls.mediaMessage")}</p>
                        </div>
                      ))
                    )}
                  </div>
                  {quickError && (
                    <p className="call-drawer-error" role="alert">
                      {t("calls.inCallMessageFailed")}
                    </p>
                  )}
                  {chatMatchesSelected && (
                    <form
                      className="call-drawer-composer"
                      onSubmit={(e) => {
                        e.preventDefault();
                        void sendQuickReply();
                      }}
                    >
                      <input
                        aria-label={t("calls.sendInCallMessage")}
                        placeholder={t("calls.sendInCallMessage")}
                        disabled={quickSending}
                        value={quickDraft}
                        onChange={(e) => {
                          setQuickError(false);
                          setQuickDraft(e.target.value);
                        }}
                      />
                      <button type="submit" disabled={!quickDraft.trim() || quickSending}>
                        <Icon name="send" />
                      </button>
                    </form>
                  )}
                </div>
              )}
            </aside>
          )}
        </div>

        {/* Bottom control bar */}
        <footer className="call-theater-controls">
          <div className="call-controls-group">
            <button
              type="button"
              className={`call-control-btn ${callState.localMuted ? "is-off" : ""}`}
              aria-pressed={callState.localMuted}
              onClick={() => void controller.toggleMute()}
            >
              <Icon name={callState.localMuted ? "mic-off" : "mic"} />
              <span>{callState.localMuted ? t("calls.unmute") : t("calls.mute")}</span>
            </button>

            {call.callType === "video" && (
              <button
                type="button"
                className={`call-control-btn ${!callState.localCamera ? "is-off" : ""}`}
                aria-pressed={callState.localCamera}
                disabled={call.state !== "connected"}
                title={call.state !== "connected" ? t("calls.cameraToggleWaitConnected") : undefined}
                onClick={() => void controller.toggleCamera()}
              >
                <Icon name={callState.localCamera ? "video" : "video-off"} />
                <span>{callState.localCamera ? t("calls.cameraOn") : t("calls.cameraOff")}</span>
              </button>
            )}

            <button
              type="button"
              className={`call-control-btn ${callState.speakerMuted ? "is-off" : ""}`}
              aria-pressed={callState.speakerMuted}
              onClick={() => controller.toggleSpeakerMuted()}
            >
              <Icon name={callState.speakerMuted ? "volume-off" : "volume"} />
              <span>{callState.speakerMuted ? t("calls.speakerOff") : t("calls.speaker")}</span>
            </button>

            <div className="call-devices-anchor">
              <button
                type="button"
                className={`call-control-btn ${devicesOpen ? "is-active" : ""}`}
                aria-expanded={devicesOpen}
                onClick={() => {
                  const next = !devicesOpen;
                  setDevicesOpen(next);
                  if (next) void controller.refreshDevices();
                }}
              >
                <Icon name="sliders" />
                <span>{t("calls.devices")}</span>
              </button>

              {devicesOpen && (
                <div
                  className="call-devices-popover"
                  role="dialog"
                  aria-label={t("calls.audioVideoDevices")}
                >
                  <div className="call-device-meters">
                    <div>
                      <span>{t("calls.microphone")}</span>
                      <AudioLevelMeter
                        level={callState.levels.micLevel}
                        label={t("calls.microphoneLevel")}
                      />
                    </div>
                    <div>
                      <span>{t("calls.speaker")}</span>
                      <AudioLevelMeter
                        level={callState.levels.speakerLevel}
                        label={t("calls.speakerLevel")}
                      />
                    </div>
                  </div>
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
                  {callState.devices.supportsOutputSelection && (
                    <label className="call-device-field">
                      <span>{t("calls.speaker")}</span>
                      <select
                        aria-label={t("calls.speaker")}
                        value={callState.selectedDevices.outputDeviceId ?? ""}
                        onChange={(e) =>
                          void controller.selectDevices({
                            outputDeviceId: e.target.value || undefined,
                          })
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
                  {call.callType === "video" && (
                    <label className="call-device-field">
                      <span>{t("calls.camera")}</span>
                      <select
                        aria-label={t("calls.camera")}
                        value={callState.selectedDevices.cameraDeviceId ?? ""}
                        onChange={(e) =>
                          void controller.selectDevices({
                            cameraDeviceId: e.target.value || undefined,
                          })
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
                </div>
              )}
            </div>

            <button
              type="button"
              className="call-control-btn call-control-hangup"
              disabled={callState.actionPending}
              onClick={() => void controller.hangupOrCancel()}
            >
              <Icon name="phone-off" />
              <span>{isCancellableCall(call) ? t("calls.cancelCall") : t("calls.hangUp")}</span>
            </button>
          </div>
        </footer>
      </div>
    </dialog>,
    document.body,
  );
}

/**
 * Inline workspace banners placed directly below `.chat-header`: active group
 * conference join bar, passive observer banner, post-call uncertain notice,
 * and the docked voice strip for voice-only calls.
 */
export function CallInlineBanners({
  connection,
  account,
  controller,
  selectedThreadId,
  resolveName,
}: {
  connection: Connection;
  account: string;
  controller: CallController;
  selectedThreadId: string | null;
  resolveName: (id: string) => string;
}) {
  const callState = useSyncExternalStore(controller.subscribe, controller.snapshot);
  const ownedCall = callState.currentCall?.ownedByThisClient ? callState.currentCall : null;
  const passiveBusy =
    callState.availability.busy &&
    !callState.availability.ownedByThisClient &&
    !ownedCall &&
    !callState.currentOffer;
  const matchingInvitation =
    selectedThreadId && (!ownedCall || ownedCall.groupId !== selectedThreadId)
      ? callState.conferenceInvitations.find((inv) => inv.groupId === selectedThreadId) ?? null
      : null;

  const peerId = ownedCall?.groupId ?? callState.activePeerId;
  const peerName = peerId ? resolveName(peerId) : t("calls.activeCallFallback");

  const isTheaterOpen =
    !!ownedCall &&
    (callState.presentationMode === "theater" || callState.presentationMode === "fullscreen");

  return (
    <>
      {matchingInvitation && (
        <ConferenceJoinBar
          controller={controller}
          invitation={matchingInvitation}
          callerName={resolveName(matchingInvitation.callerId)}
        />
      )}

      {passiveBusy && (
        <div className="call-passive-banner">
          <Notice tone="neutral">
            <NoticeTitle>{t("calls.passiveBusyTitle")}</NoticeTitle>
            <NoticeDescription>{t("calls.passiveBusyDescription")}</NoticeDescription>
          </Notice>
        </div>
      )}

      {!isTheaterOpen && callState.capWarning && ownedCall && (
        <div className="call-passive-banner">
          <Notice tone="neutral">
            <NoticeTitle>{t("calls.capWarningTitle")}</NoticeTitle>
            <NoticeDescription>{t("calls.capWarningDescription")}</NoticeDescription>
          </Notice>
        </div>
      )}

      {!isTheaterOpen && (callState.mediaWarning || callState.lastError) && (
        <div className="call-passive-banner">
          <Notice tone="error">
            <NoticeDescription>
              {callErrorText(callState.mediaWarning ?? callState.lastError!)}
            </NoticeDescription>
            <NoticeAction>
              <button type="button" onClick={() => controller.clearNotice()}>
                {t("calls.dismiss")}
              </button>
            </NoticeAction>
          </Notice>
        </div>
      )}

      {callState.notice && !ownedCall && (
        <div className="call-passive-banner">
          <Notice tone={callState.notice.kind === "ended" ? "neutral" : "error"}>
            <NoticeTitle>
              {callState.notice.message === "disconnected"
                ? t("calls.noticeDisconnectedTitle")
                : t("calls.noticeUncertainTitle")}
            </NoticeTitle>
            <NoticeDescription>
              {callState.notice.message === "disconnected"
                ? t("calls.noticeDisconnectedDescription")
                : t("calls.noticeUncertain")}
            </NoticeDescription>
            <NoticeAction>
              <button type="button" onClick={() => controller.clearNotice()}>
                {t("calls.dismiss")}
              </button>
            </NoticeAction>
          </Notice>
        </div>
      )}

      {ownedCall && callState.presentationMode === "strip" && (
        <VoiceCallStrip
          connection={connection}
          account={account}
          controller={controller}
          call={ownedCall}
          peerName={peerName}
          peerId={peerId}
          elapsedSeconds={callState.elapsedSeconds}
        />
      )}
    </>
  );
}

/**
 * Portal/overlay call chrome mounted at the shell level so incoming offers,
 * floating PiP, Theater/Fullscreen dialogs, and group pickers stay available
 * across mobile chat lists, contacts, and conversations.
 */
export function CallWorkspaceChrome({
  connection,
  account,
  controller,
  resolveName,
  groupPicker,
  onCloseGroupPicker,
}: {
  connection: Connection;
  account: string;
  controller: CallController;
  resolveName: (id: string) => string;
  groupPicker: { groupId: string; groupName: string; callType: "voice" | "video" } | null;
  onCloseGroupPicker: () => void;
}) {
  const callState = useSyncExternalStore(controller.subscribe, controller.snapshot);
  const [inviteOpen, setInviteOpen] = useState(false);

  const ownedCall = callState.currentCall?.ownedByThisClient ? callState.currentCall : null;

  useEffect(() => {
    if (!ownedCall || ownedCall.kind !== "conference") {
      setInviteOpen(false);
    }
  }, [ownedCall?.callId, ownedCall?.kind]);

  const peerId = ownedCall?.groupId ?? callState.activePeerId;
  const peerName = peerId ? resolveName(peerId) : t("calls.activeCallFallback");

  const primaryVideoTrack = useMemo(
    () =>
      callState.tracks.find(
        (tr): tr is CallVideoTrack => tr.media === "video" && tr.direction === "receive",
      ) ?? null,
    [callState.tracks],
  );

  return (
    <>
      {ownedCall && callState.presentationMode === "pip" && (
        <CallPipCard
          controller={controller}
          call={ownedCall}
          peerName={peerName}
          primaryVideoTrack={primaryVideoTrack}
          elapsedSeconds={callState.elapsedSeconds}
        />
      )}

      {ownedCall &&
        (callState.presentationMode === "theater" ||
          callState.presentationMode === "fullscreen") && (
          <CallTheaterDialog
            connection={connection}
            account={account}
            controller={controller}
            call={ownedCall}
            peerName={peerName}
            peerId={peerId}
            resolveName={resolveName}
            onOpenInvite={() => setInviteOpen(true)}
          />
        )}

      <CallIncomingToast
        connection={connection}
        account={account}
        controller={controller}
        resolveName={resolveName}
      />

      {groupPicker && (
        <GroupCallPickerModal
          key={`start:${account}:${groupPicker.groupId}:${groupPicker.callType}`}
          connection={connection}
          account={account}
          groupId={groupPicker.groupId}
          groupName={groupPicker.groupName}
          mode="start"
          initialCallType={groupPicker.callType}
          allowVideo={callState.capabilities.conferenceVideo}
          busy={callState.availability.busy || callState.actionPending}
          onClose={onCloseGroupPicker}
          onSubmit={({ invitees, callType, muted, cameraEnabled }) => {
            onCloseGroupPicker();
            void controller.startConference(groupPicker.groupId, invitees, {
              callType,
              muted,
              cameraEnabled,
            });
          }}
        />
      )}

      {inviteOpen && ownedCall?.groupId && (
        <GroupCallPickerModal
          key={`invite:${account}:${ownedCall.callId}`}
          connection={connection}
          account={account}
          groupId={ownedCall.groupId}
          groupName={peerName}
          mode="invite"
          allowVideo={false}
          busy={callState.actionPending || ownedCall.state !== "connected"}
          excludeIds={(ownedCall.participants ?? [])
            .filter((p) => p.membership !== "left")
            .map((p) => p.contactId)}
          onClose={() => setInviteOpen(false)}
          onSubmit={({ invitees }) => {
            setInviteOpen(false);
            void controller.inviteMembers(invitees);
          }}
        />
      )}
    </>
  );
}
