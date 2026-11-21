import {
  NativeApiError,
  NativeCallError,
  type CallAudioFormat,
  type CallConferenceInvitation,
  type CallEvent,
  type CallInvitationResult,
  type CallPendingOffer,
  type CallSnapshot,
  type CallTrack,
  type CallType,
  type CallVideoCapabilities,
  type NativeCallAnswerOptions,
  type NativeCallDialOptions,
  type NativeCallJoinOptions,
  type NativeCallMediaClient,
  type NativeCallSocket,
  type NativeCallWebSocketFactory,
  type NativeCallsClient,
  type NativeWeChatClient,
} from "@agent-wechat/shared";
import { BrowserCallMediaBackend } from "./browser-media.ts";
import {
  EMPTY_CALL_DEVICES,
  EMPTY_CALL_LEVELS,
  type CallAudioLevels,
  type CallDeviceSelection,
  type CallDevicesSnapshot,
  type CallMediaBackend,
  type CallMediaHandle,
  type CallSessionTransport,
  type CallTransportCallbacks,
  type LocalPreviewSurface,
  type RemoteVideoFrame,
} from "./media-backend.ts";
import { CallRingtone } from "./ringtone.ts";

/** Hard WeChat call cap is 20 minutes; warn at 18:30 (1110 seconds). */
export const CALL_CAP_WARNING_SECONDS = 18 * 60 + 30;
const SKIP_PREVIEW_STORAGE_KEY = "agentwechat-call-skip-preview";
const DEVICES_STORAGE_KEY = "agentwechat-call-devices";

export type CallOffer = NonNullable<CallSnapshot["currentOffer"]>;
export type CallView = NonNullable<CallSnapshot["currentCall"]>;
export type CallPresentationMode = "strip" | "theater" | "pip" | "fullscreen";

export type CallCapabilities = {
  directAudio: boolean;
  directVideo: boolean;
  conferenceCreate: boolean;
  conferenceJoin: boolean;
  conferenceInvite: boolean;
  conferenceVideo: boolean;
};

export type CallSummaryNotice = {
  kind: "ended" | "uncertain" | "error";
  message: string;
};

export type CallControllerState = {
  status: "idle" | "checking" | "disabled" | "unavailable" | "connecting" | "ready" | "error";
  capabilities: CallCapabilities;
  availability: { busy: boolean; ownedByThisClient: boolean };
  currentOffer: CallOffer | null;
  currentPendingOffer: CallPendingOffer | null;
  currentCall: CallView | null;
  conferenceInvitations: CallConferenceInvitation[];
  tracks: CallTrack[];
  invitationResults: { requestId: string; results: CallInvitationResult[] } | null;
  /** Remembered direct peer ID (for outgoing calls where CallView carries no direct peer field). */
  activePeerId: string | null;
  localMuted: boolean;
  localCamera: boolean;
  speakerMuted: boolean;
  ringtoneMuted: boolean;
  ringtoneAutoplayBlocked: boolean;
  dismissedOfferKey: string | null;
  presentationMode: CallPresentationMode;
  connectedAtMs: number | null;
  elapsedSeconds: number;
  capWarning: boolean;
  actionPending: boolean;
  lastError: string | null;
  mediaWarning: string | null;
  notice: CallSummaryNotice | null;
  devices: CallDevicesSnapshot;
  selectedDevices: CallDeviceSelection;
  skipVideoPreview: boolean;
  levels: CallAudioLevels;
  localStream: MediaStream | null;
  /** Incremented whenever a remote track receives a new frame or clears. */
  remoteTrackFrames: Record<number, number>;
};

export const EMPTY_CAPABILITIES: CallCapabilities = {
  directAudio: false,
  directVideo: false,
  conferenceCreate: false,
  conferenceJoin: false,
  conferenceInvite: false,
  conferenceVideo: false,
};

export function normalizeCapabilities(raw: Record<string, boolean> | undefined): CallCapabilities {
  return {
    directAudio: raw?.directAudio === true,
    directVideo: raw?.directVideo === true,
    conferenceCreate: raw?.conferenceCreate === true,
    conferenceJoin: raw?.conferenceJoin === true,
    conferenceInvite: raw?.conferenceInvite === true,
    conferenceVideo: raw?.conferenceVideo === true,
  };
}

export function hasAnyCallCapability(caps: CallCapabilities): boolean {
  return (
    caps.directAudio ||
    caps.directVideo ||
    caps.conferenceCreate ||
    caps.conferenceJoin ||
    caps.conferenceInvite ||
    caps.conferenceVideo
  );
}

export function offerKey(offer: { callId: string; revision: number } | null | undefined): string | null {
  return offer ? `${offer.callId}:${offer.revision}` : null;
}

export function isTerminalCallState(state: CallView["state"] | undefined): boolean {
  return state === "ended" || state === "uncertain";
}

/** Outgoing calls remain in admission Phase::Dialing across dialing/ringing/connecting (including conferences) and accept only `call.cancel`. */
export function isCancellableCall(call: Pick<CallView, "direction" | "state">): boolean {
  return (
    call.direction === "outgoing" &&
    (call.state === "dialing" || call.state === "ringing" || call.state === "connecting")
  );
}

export function formatCallDuration(seconds: number): string {
  const safe = Math.max(0, Math.floor(seconds));
  const mins = Math.floor(safe / 60);
  const secs = safe % 60;
  return `${String(mins).padStart(2, "0")}:${String(secs).padStart(2, "0")}`;
}

export function canCallThread(
  threadId: string | null,
  ownUsername: string | null | undefined,
  capabilities: CallCapabilities,
): { voice: boolean; video: boolean; isGroup: boolean } {
  if (!threadId || threadId === "filehelper" || (ownUsername && threadId === ownUsername)) {
    return { voice: false, video: false, isGroup: false };
  }
  const isGroup = threadId.endsWith("@chatroom");
  if (isGroup) {
    return {
      voice: capabilities.conferenceCreate,
      video: capabilities.conferenceCreate && capabilities.conferenceVideo,
      isGroup: true,
    };
  }
  return {
    voice: capabilities.directAudio,
    video: capabilities.directVideo,
    isGroup: false,
  };
}

function mergeTrackList(existing: CallTrack[], incoming: CallTrack[]): CallTrack[] {
  const byId = new Map<number, CallTrack>();
  for (const track of existing) byId.set(track.id, track);
  for (const track of incoming) {
    const prior = byId.get(track.id);
    if (!prior || track.generation >= prior.generation) {
      byId.set(track.id, track);
    }
  }
  return [...byId.values()];
}

function readSkipPreview(): boolean {
  try {
    return globalThis.localStorage?.getItem(SKIP_PREVIEW_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

function readSavedDevices(): CallDeviceSelection {
  try {
    const parsed = JSON.parse(globalThis.localStorage?.getItem(DEVICES_STORAGE_KEY) ?? "null");
    if (parsed && typeof parsed === "object") {
      return {
        ...(typeof parsed.inputDeviceId === "string" ? { inputDeviceId: parsed.inputDeviceId } : {}),
        ...(typeof parsed.outputDeviceId === "string" ? { outputDeviceId: parsed.outputDeviceId } : {}),
        ...(typeof parsed.cameraDeviceId === "string" ? { cameraDeviceId: parsed.cameraDeviceId } : {}),
      };
    }
  } catch {}
  return {};
}

function defaultWebSocketFactory(): NativeCallWebSocketFactory {
  return (url) => new WebSocket(url) as unknown as NativeCallSocket;
}

/** Browser transport wrapping NativeCallsClient + BrowserCallMediaBackend. */
export class BrowserCallTransport implements CallSessionTransport {
  readonly kind = "browser" as const;
  private readonly client: NativeWeChatClient;
  private readonly account: string;
  private readonly webSocketFactory: NativeCallWebSocketFactory;
  private readonly backend: CallMediaBackend;
  private callsClient: NativeCallsClient | null = null;
  private mediaClient: NativeCallMediaClient | null = null;
  private mediaHandle: CallMediaHandle | null = null;
  private attachedMediaCallId: string | null = null;
  private attachingCallId: string | null = null;
  private audioFormat: CallAudioFormat = { sampleRate: 16000, channels: 1, frameMs: 20 };
  private callbacks: CallTransportCallbacks | null = null;
  private tracks: CallTrack[] = [];

  constructor(
    client: NativeWeChatClient,
    account: string,
    options?: {
      webSocketFactory?: NativeCallWebSocketFactory;
      backend?: CallMediaBackend;
    },
  ) {
    this.client = client;
    this.account = account;
    this.webSocketFactory = options?.webSocketFactory ?? defaultWebSocketFactory();
    this.backend = options?.backend ?? new BrowserCallMediaBackend();
  }

  probe(signal?: AbortSignal): Promise<CallSnapshot> {
    return this.client.callSnapshot(this.account, signal);
  }

  async connect(callbacks: CallTransportCallbacks): Promise<Extract<CallEvent, { type: "hello" }>> {
    this.disconnect();
    this.callbacks = callbacks;
    if (this.backend instanceof BrowserCallMediaBackend) {
      await this.backend.ensureProbed();
    }
    const calls = this.client.calls(this.account, this.webSocketFactory, {
      ticketProvider: async (socket, callId) => {
        try {
          const ticket = await this.client.issueCallTicket(this.account, {
            socket,
            ...(callId ? { callId } : {}),
          });
          return ticket.ticket;
        } catch (error) {
          if (error instanceof NativeApiError && [401, 403].includes(error.status)) {
            throw new NativeCallError("forbidden");
          }
          throw error;
        }
      },
    });
    this.callsClient = calls;
    const hello = await calls.connect();
    this.audioFormat = hello.audioFormat;

    // Drain events in the background; notify controller on events or disconnect.
    void (async () => {
      try {
        for await (const event of calls.events()) {
          if (this.callsClient !== calls) break;
          if (event.type === "hello") continue;
          if (event.type === "track.added" || event.type === "track.updated") {
            this.upsertTrack(event.track);
          } else if (event.type === "track.removed") {
            const existing = this.tracks.find((item) => item.id === event.trackId);
            if (existing && existing.generation === event.generation) {
              this.tracks = this.tracks.filter((item) => item.id !== event.trackId);
              this.syncMediaTracks();
            }
          }
          callbacks.onEvent(event);
        }
      } catch (error) {
        if (this.callsClient === calls) {
          this.stopMedia();
          this.callsClient = null;
          callbacks.onDisconnected(error instanceof NativeCallError ? error.code : "closed");
        }
      }
    })();

    // Advertise incoming readiness + client video capabilities without opening any device.
    const videoCaps: CallVideoCapabilities | undefined = this.backend.videoCapabilities();
    try {
      await calls.ready(crypto.randomUUID(), true, hello.audioFormat, { kind: "all" }, videoCaps);
    } catch {
      if (this.callsClient !== calls) {
        throw new NativeCallError("closed");
      }
      // If ready fails while the socket stays open (e.g. older daemon), stay connected for outgoing calls.
    }
    if (this.callsClient !== calls) {
      throw new NativeCallError("closed");
    }
    return hello;
  }

  disconnect(): void {
    this.stopMedia();
    const existing = this.callsClient;
    this.callsClient = null;
    existing?.close();
    this.callbacks = null;
  }

  listDevices(): Promise<CallDevicesSnapshot> {
    return this.backend.listDevices();
  }

  startLocalPreview(cameraId?: string): Promise<LocalPreviewSurface> {
    return this.backend.startLocalPreview(cameraId);
  }

  async dialDirect(
    requestId: string,
    contactId: string,
    options: NativeCallDialOptions,
  ): Promise<{ callId: string }> {
    const calls = this.requireConnected();
    const res = await calls.dial(requestId, contactId, options);
    const callId = res.callId;
    if (!callId) throw new NativeCallError("invalid_event");
    return { callId };
  }

  async startConference(
    requestId: string,
    groupId: string,
    invitees: string[],
    options: NativeCallDialOptions,
  ): Promise<{ callId: string }> {
    const calls = this.requireConnected();
    const res = await calls.startConference(requestId, groupId, invitees, options);
    const callId = res.callId;
    if (!callId) throw new NativeCallError("invalid_event");
    return { callId };
  }

  async joinConference(
    requestId: string,
    invitationId: string,
    invitationRevision: number,
    options: NativeCallJoinOptions,
  ): Promise<{ callId: string }> {
    const calls = this.requireConnected();
    const res = await calls.joinConference(requestId, invitationId, invitationRevision, options);
    const callId = res.callId;
    if (!callId) throw new NativeCallError("invalid_event");
    return { callId };
  }

  async answer(
    requestId: string,
    callId: string,
    expectedRevision: number,
    _callType: CallType,
    options: NativeCallAnswerOptions,
  ): Promise<void> {
    const calls = this.requireConnected();
    await calls.answer(requestId, callId, expectedRevision, options);
  }

  async attachConnectedMedia(
    callId: string,
    callType: CallType,
    initialMuted: boolean,
    initialCameraEnabled: boolean,
    devices: CallDeviceSelection,
    speakerMuted: boolean,
  ): Promise<void> {
    const calls = this.callsClient;
    const callbacks = this.callbacks;
    if (!calls || !callbacks) return;
    if (this.attachedMediaCallId === callId || this.attachingCallId === callId) return;

    this.stopMedia();
    this.attachingCallId = callId;
    try {
      const media = await calls.attachMedia(callId);
      if (this.callsClient !== calls || this.attachingCallId !== callId) {
        media.close();
        return;
      }
      this.mediaClient = media;
      this.attachedMediaCallId = callId;
      const audioTracks = media.tracks();
      const videoTracks = media.videoTracks();
      this.tracks = mergeTrackList(this.tracks, [
        audioTracks.sendTrack,
        ...audioTracks.receiveTracks,
        ...(videoTracks.sendTrack ? [videoTracks.sendTrack] : []),
        ...videoTracks.receiveTracks,
      ]);
      callbacks.onTracksSynced([...this.tracks]);
      const handle = await this.backend.attach({
        media,
        audioFormat: this.audioFormat,
        callType,
        initialMuted,
        initialCameraEnabled,
        devices,
        speakerMuted,
        onLocalStream: callbacks.onLocalStream,
        onLocalJpeg: callbacks.onLocalJpeg,
        onRemoteFrame: callbacks.onRemoteFrame,
        onTrackCleared: callbacks.onTrackCleared,
        onLevels: callbacks.onLevels,
        onError: callbacks.onMediaError,
      });
      if (this.mediaClient !== media || this.attachingCallId !== callId) {
        handle.stop();
        return;
      }
      this.mediaHandle = handle;
      this.attachingCallId = null;
      this.syncMediaTracks();
    } catch (error) {
      if (this.attachingCallId === callId) {
        this.attachingCallId = null;
      }
      callbacks.onMediaError(error instanceof NativeCallError ? error.code : "media_attach_failed");
    }
  }

  async reject(requestId: string, callId: string, expectedRevision: number): Promise<void> {
    const calls = this.requireConnected();
    await calls.end("call.reject", requestId, callId, expectedRevision);
  }

  async invite(requestId: string, callId: string, contactIds: string[]): Promise<void> {
    const calls = this.requireConnected();
    await calls.invite(requestId, callId, contactIds);
  }

  async setMute(requestId: string, callId: string, muted: boolean): Promise<void> {
    this.mediaHandle?.setMuted(muted);
    try {
      const calls = this.requireConnected();
      await calls.setMute(requestId, callId, muted);
    } catch (error) {
      this.mediaHandle?.setMuted(!muted);
      throw error;
    }
  }

  async setCamera(requestId: string, callId: string, enabled: boolean): Promise<void> {
    const calls = this.requireConnected();
    await calls.setCamera(requestId, callId, enabled);
    await this.mediaHandle?.setCameraEnabled(enabled);
  }

  setSpeakerMuted(muted: boolean): void {
    this.mediaHandle?.setSpeakerMuted(muted);
  }

  async switchDevices(selection: CallDeviceSelection): Promise<void> {
    await this.mediaHandle?.switchDevices(selection);
  }

  async end(
    type: "call.cancel" | "call.hangup",
    requestId: string,
    callId: string,
    expectedRevision: number,
  ): Promise<void> {
    const calls = this.requireConnected();
    await calls.end(type, requestId, callId, expectedRevision);
  }

  stopLocalMedia(): void {
    this.attachingCallId = null;
    this.mediaHandle?.stop();
    this.mediaHandle = null;
    this.mediaClient?.stopAudio();
  }

  stopMedia(): void {
    this.stopLocalMedia();
    this.tracks = [];
    this.attachedMediaCallId = null;
    this.mediaClient?.close();
    this.mediaClient = null;
  }

  private requireConnected(): NativeCallsClient {
    if (!this.callsClient) throw new NativeCallError("not_connected");
    return this.callsClient;
  }

  private upsertTrack(track: CallTrack): void {
    const prior = this.tracks.find((item) => item.id === track.id);
    if (prior && track.generation < prior.generation) return;
    this.tracks = mergeTrackList(this.tracks, [track]);
    this.syncMediaTracks();
  }

  private syncMediaTracks(): void {
    if (!this.mediaHandle) return;
    const send = this.tracks.filter((t) => t.direction === "send");
    const receive = this.tracks.filter((t) => t.direction === "receive");
    this.mediaHandle.updateTracks(send, receive);
  }
}

export type CallControllerOptions = {
  transport?: CallSessionTransport | null;
  disabled?: boolean;
  now?: () => number;
  ringtone?: CallRingtone;
  isWindowFocused?: () => boolean;
};

function defaultWindowFocused(): boolean {
  if (typeof document === "undefined") return true;
  if (document.hidden) return false;
  return typeof document.hasFocus === "function" ? document.hasFocus() : true;
}

/**
 * Per-account call store (`useSyncExternalStore` compatible) owning signaling
 * state, ringtone lifecycle, timers, device preferences, and remote frame routing.
 */
export class CallController {
  private readonly transport: CallSessionTransport | null;
  private readonly now: () => number;
  private readonly ringtone: CallRingtone;
  private readonly isWindowFocused: () => boolean;
  private readonly removeFocusListeners: (() => void) | null;
  private readonly listeners = new Set<() => void>();
  private readonly remoteFrames = new Map<number, RemoteVideoFrame>();
  private readonly frameListeners = new Map<number, Set<(frame: RemoteVideoFrame | null) => void>>();
  private readonly localJpegListeners = new Set<(bytes: Uint8Array) => void>();

  private state: CallControllerState;
  private started = false;
  private probedOnce = false;
  private disposed = false;
  private probeAbort: AbortController | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelayMs = 1000;
  private durationTimer: ReturnType<typeof setInterval> | null = null;

  constructor(options: CallControllerOptions) {
    this.transport = options.disabled ? null : (options.transport ?? null);
    this.now = options.now ?? Date.now;
    this.isWindowFocused = options.isWindowFocused ?? defaultWindowFocused;
    this.ringtone =
      options.ringtone ??
      new CallRingtone((blocked) => {
        this.update({ ringtoneAutoplayBlocked: blocked });
      });
    this.state = {
      status: options.disabled || !this.transport ? "disabled" : "idle",
      capabilities: { ...EMPTY_CAPABILITIES },
      availability: { busy: false, ownedByThisClient: false },
      currentOffer: null,
      currentPendingOffer: null,
      currentCall: null,
      conferenceInvitations: [],
      tracks: [],
      invitationResults: null,
      activePeerId: null,
      localMuted: false,
      localCamera: false,
      speakerMuted: false,
      ringtoneMuted: false,
      ringtoneAutoplayBlocked: false,
      dismissedOfferKey: null,
      presentationMode: "strip",
      connectedAtMs: null,
      elapsedSeconds: 0,
      capWarning: false,
      actionPending: false,
      lastError: null,
      mediaWarning: null,
      notice: null,
      devices: { ...EMPTY_CALL_DEVICES },
      selectedDevices: readSavedDevices(),
      skipVideoPreview: readSkipPreview(),
      levels: { ...EMPTY_CALL_LEVELS },
      localStream: null,
      remoteTrackFrames: {},
    };
    if (this.transport?.kind === "desktop" && typeof window !== "undefined") {
      const onFocusChange = () => {
        if (!this.disposed) this.syncRingtone();
      };
      window.addEventListener("focus", onFocusChange);
      window.addEventListener("blur", onFocusChange);
      if (typeof document !== "undefined") {
        document.addEventListener("visibilitychange", onFocusChange);
      }
      this.removeFocusListeners = () => {
        window.removeEventListener("focus", onFocusChange);
        window.removeEventListener("blur", onFocusChange);
        if (typeof document !== "undefined") {
          document.removeEventListener("visibilitychange", onFocusChange);
        }
      };
    } else {
      this.removeFocusListeners = null;
    }
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  };

  snapshot = (): CallControllerState => this.state;

  getRemoteFrame(trackId: number): RemoteVideoFrame | null {
    return this.remoteFrames.get(trackId) ?? null;
  }

  subscribeRemoteFrame(
    trackId: number,
    listener: (frame: RemoteVideoFrame | null) => void,
  ): () => void {
    let set = this.frameListeners.get(trackId);
    if (!set) {
      set = new Set();
      this.frameListeners.set(trackId, set);
    }
    set.add(listener);
    return () => {
      set?.delete(listener);
      if (set?.size === 0) this.frameListeners.delete(trackId);
    };
  }

  subscribeLocalJpeg(listener: (bytes: Uint8Array) => void): () => void {
    this.localJpegListeners.add(listener);
    return () => {
      this.localJpegListeners.delete(listener);
    };
  }

  start(): void {
    if (this.started || this.disposed || !this.transport) return;
    this.started = true;
    void this.probeAndConnect();
  }

  /**
   * Pause a background account's call signaling when switching accounts so an
   * unseen account never keeps `call.ready` armed or rings invisibly.
   */
  pause(): void {
    if (this.disposed || !this.started) return;
    this.ringtone.stop();
    if (this.state.currentCall?.ownedByThisClient) return;
    this.started = false;
    this.probeAbort?.abort();
    this.probeAbort = null;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.transport?.disconnect();
    this.update({
      status: "idle",
      currentOffer: null,
      currentPendingOffer: null,
    });
  }

  close(): void {
    this.disposed = true;
    this.started = false;
    this.removeFocusListeners?.();
    this.probeAbort?.abort();
    this.probeAbort = null;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.stopDurationTimer();
    this.ringtone.dispose();
    this.transport?.disconnect();
    this.clearAllRemoteFrames();
  }

  async refreshDevices(): Promise<CallDevicesSnapshot> {
    if (!this.transport) return { ...EMPTY_CALL_DEVICES };
    const devices = await this.transport.listDevices();
    if (!this.disposed) this.update({ devices });
    return devices;
  }

  startLocalPreview(cameraId?: string): Promise<LocalPreviewSurface> {
    if (!this.transport) return Promise.reject(new Error("calls_disabled"));
    return this.transport.startLocalPreview(cameraId ?? this.state.selectedDevices.cameraDeviceId);
  }

  setSkipVideoPreview(skip: boolean): void {
    try {
      if (skip) globalThis.localStorage?.setItem(SKIP_PREVIEW_STORAGE_KEY, "1");
      else globalThis.localStorage?.removeItem(SKIP_PREVIEW_STORAGE_KEY);
    } catch {}
    this.update({ skipVideoPreview: skip });
  }

  async selectDevices(selection: CallDeviceSelection): Promise<void> {
    const merged: CallDeviceSelection = { ...this.state.selectedDevices, ...selection };
    try {
      globalThis.localStorage?.setItem(DEVICES_STORAGE_KEY, JSON.stringify(merged));
    } catch {}
    this.update({ selectedDevices: merged });
    if (this.transport && this.state.currentCall?.ownedByThisClient) {
      await this.transport.switchDevices(merged);
    }
  }

  setPresentationMode(mode: CallPresentationMode): void {
    this.update({ presentationMode: mode });
  }

  setRingtoneMuted(muted: boolean): void {
    this.ringtone.setMuted(muted);
    this.update({ ringtoneMuted: muted });
  }

  dismissOfferLocally(): void {
    const key = offerKey(this.state.currentOffer) ?? offerKey(this.state.currentPendingOffer);
    this.ringtone.stop();
    this.update({ dismissedOfferKey: key });
  }

  clearNotice(): void {
    this.update({ notice: null, lastError: null, mediaWarning: null, invitationResults: null });
  }

  async dialDirect(
    contactId: string,
    options: NativeCallDialOptions = {},
  ): Promise<void> {
    if (!this.transport || this.state.actionPending) return;
    const callType: CallType = options.callType ?? "voice";
    const initialCamera = callType === "video" && options.cameraEnabled !== false;
    const initialMuted = options.muted === true;
    this.ringtone.stop();
    this.update({
      actionPending: true,
      lastError: null,
      mediaWarning: null,
      notice: null,
      activePeerId: contactId,
      localMuted: initialMuted,
      localCamera: initialCamera,
      presentationMode: callType === "video" ? "theater" : "strip",
    });
    try {
      await this.transport.dialDirect(
        crypto.randomUUID(),
        contactId,
        { callType, cameraEnabled: initialCamera, muted: initialMuted },
        this.state.selectedDevices,
        this.state.speakerMuted,
      );
      this.update({ actionPending: false });
    } catch (error) {
      this.handleActionFailure(error);
    }
  }

  async startConference(
    groupId: string,
    invitees: string[],
    options: NativeCallDialOptions = {},
  ): Promise<void> {
    if (!this.transport || this.state.actionPending) return;
    const callType: CallType = options.callType ?? "voice";
    const initialCamera = callType === "video" && options.cameraEnabled !== false;
    const initialMuted = options.muted === true;
    this.ringtone.stop();
    this.update({
      actionPending: true,
      lastError: null,
      mediaWarning: null,
      notice: null,
      activePeerId: groupId,
      localMuted: initialMuted,
      localCamera: initialCamera,
      presentationMode: "theater",
    });
    try {
      await this.transport.startConference(
        crypto.randomUUID(),
        groupId,
        invitees,
        { callType, cameraEnabled: initialCamera, muted: initialMuted },
        this.state.selectedDevices,
        this.state.speakerMuted,
      );
      this.update({ actionPending: false });
    } catch (error) {
      this.handleActionFailure(error);
    }
  }

  async joinConference(
    invitation: CallConferenceInvitation,
    options: NativeCallJoinOptions = {},
  ): Promise<void> {
    if (!this.transport || this.state.actionPending) return;
    const initialCamera = options.cameraEnabled === true;
    const initialMuted = options.muted === true;
    this.ringtone.stop();
    this.update({
      actionPending: true,
      lastError: null,
      mediaWarning: null,
      notice: null,
      activePeerId: invitation.groupId,
      localMuted: initialMuted,
      localCamera: initialCamera,
      presentationMode: "theater",
    });
    try {
      await this.transport.joinConference(
        crypto.randomUUID(),
        invitation.invitationId,
        invitation.revision,
        { cameraEnabled: initialCamera, muted: initialMuted },
        this.state.selectedDevices,
        this.state.speakerMuted,
      );
      this.update({ actionPending: false });
    } catch (error) {
      this.handleActionFailure(error);
    }
  }

  async answerOffer(options: NativeCallAnswerOptions = {}): Promise<void> {
    const offer = this.state.currentOffer;
    if (!this.transport || !offer || this.state.actionPending) return;
    const initialCamera = offer.callType === "video" && options.cameraEnabled !== false;
    const initialMuted = options.muted === true;
    this.ringtone.stop();
    this.update({
      actionPending: true,
      lastError: null,
      mediaWarning: null,
      notice: null,
      activePeerId: offer.callerId,
      localMuted: initialMuted,
      localCamera: initialCamera,
      presentationMode: offer.callType === "video" ? "theater" : "strip",
    });
    try {
      await this.transport.answer(
        crypto.randomUUID(),
        offer.callId,
        offer.revision,
        offer.callType,
        { cameraEnabled: initialCamera, muted: initialMuted },
        this.state.selectedDevices,
        this.state.speakerMuted,
      );
      this.update({ actionPending: false });
    } catch (error) {
      this.handleActionFailure(error);
    }
  }

  async rejectOffer(): Promise<void> {
    const offer = this.state.currentOffer;
    if (!this.transport || !offer || this.state.actionPending) return;
    this.ringtone.stop();
    this.update({ actionPending: true, lastError: null });
    try {
      await this.transport.reject(crypto.randomUUID(), offer.callId, offer.revision);
      const stillSame =
        this.state.currentOffer?.callId === offer.callId &&
        this.state.currentOffer.revision === offer.revision;
      this.update({
        actionPending: false,
        ...(stillSame ? { currentOffer: null } : {}),
      });
    } catch (error) {
      this.handleActionFailure(error);
    }
  }

  async inviteMembers(contactIds: string[]): Promise<void> {
    const call = this.state.currentCall;
    if (
      !this.transport ||
      !call ||
      !call.ownedByThisClient ||
      call.kind !== "conference" ||
      call.state !== "connected" ||
      !this.state.capabilities.conferenceInvite
    ) {
      return;
    }
    this.update({ actionPending: true, lastError: null });
    try {
      await this.transport.invite(crypto.randomUUID(), call.callId, contactIds);
      this.update({ actionPending: false });
    } catch (error) {
      this.handleActionFailure(error);
    }
  }

  async toggleMute(): Promise<void> {
    const call = this.state.currentCall;
    if (!this.transport || !call || !call.ownedByThisClient) return;
    const next = !this.state.localMuted;
    this.update({ localMuted: next, lastError: null });
    try {
      await this.transport.setMute(crypto.randomUUID(), call.callId, next);
    } catch (error) {
      this.update({ localMuted: !next });
      this.handleActionFailure(error);
    }
  }

  async toggleCamera(): Promise<void> {
    const call = this.state.currentCall;
    if (
      !this.transport ||
      !call ||
      !call.ownedByThisClient ||
      call.callType !== "video" ||
      call.state !== "connected"
    ) {
      return;
    }
    const next = !this.state.localCamera;
    this.update({ localCamera: next, lastError: null, mediaWarning: null });
    try {
      await this.transport.setCamera(crypto.randomUUID(), call.callId, next);
    } catch (error) {
      this.update({ localCamera: !next });
      this.handleActionFailure(error);
    }
  }

  toggleSpeakerMuted(): void {
    const next = !this.state.speakerMuted;
    this.transport?.setSpeakerMuted(next);
    this.update({ speakerMuted: next });
  }

  async hangupOrCancel(): Promise<void> {
    const call = this.state.currentCall;
    if (!this.transport || !call || !call.ownedByThisClient) return;
    const action: "call.cancel" | "call.hangup" = isCancellableCall(call)
      ? "call.cancel"
      : "call.hangup";
    this.update({ actionPending: true, lastError: null });
    try {
      await this.transport.end(action, crypto.randomUUID(), call.callId, call.revision);
      this.update({ actionPending: false });
    } catch (error) {
      this.handleActionFailure(error);
    }
  }

  async requestHostFullscreen(enabled: boolean): Promise<boolean> {
    if (this.transport?.setFullscreen) {
      try {
        return await this.transport.setFullscreen(enabled);
      } catch {
        return false;
      }
    }
    return false;
  }

  private async probeAndConnect(): Promise<void> {
    if (!this.transport || this.disposed || !this.started) return;
    this.probeAbort?.abort();
    const abort = new AbortController();
    this.probeAbort = abort;
    this.update({ status: "checking" });
    try {
      const snap = await this.transport.probe(abort.signal);
      if (this.disposed || !this.started || abort.signal.aborted) return;
      this.probedOnce = true;
      const caps = normalizeCapabilities(snap.capabilities);
      if (!hasAnyCallCapability(caps)) {
        this.update({
          status: "disabled",
          capabilities: caps,
          availability: snap.availability,
          conferenceInvitations: snap.conferenceInvitations ?? [],
        });
        return;
      }
      this.update({
        status: "connecting",
        capabilities: caps,
        availability: snap.availability,
        currentOffer: snap.currentOffer,
        currentPendingOffer: snap.currentPendingOffer,
        currentCall: snap.currentCall,
        conferenceInvitations: snap.conferenceInvitations ?? [],
      });
      await this.connectSocket();
    } catch (error) {
      if (this.disposed || !this.started || abort.signal.aborted) return;
      if (error instanceof NativeApiError && [401, 403, 404, 501].includes(error.status)) {
        // Missing `calls` scope or unsupported backend: stay silent with no error banners.
        this.update({ status: "unavailable", capabilities: { ...EMPTY_CAPABILITIES } });
        return;
      }
      this.update({ status: "error" });
      this.scheduleReconnect();
    }
  }

  private async connectSocket(): Promise<void> {
    if (!this.transport || this.disposed || !this.started) return;
    try {
      const hello = await this.transport.connect({
        onEvent: (event) => this.applyEvent(event),
        onDisconnected: (code) => this.handleDisconnect(code),
        onTracksSynced: (tracks) => {
          if (!this.disposed) {
            this.update({ tracks: mergeTrackList(this.state.tracks, tracks) });
          }
        },
        onLocalStream: (stream) => {
          if (!this.disposed) this.update({ localStream: stream });
        },
        onLocalJpeg: (bytes) => {
          for (const listener of this.localJpegListeners) listener(bytes);
        },
        onRemoteFrame: (frame) => this.handleRemoteFrame(frame),
        onTrackCleared: (trackId) => this.clearRemoteFrame(trackId),
        onLevels: (levels) => {
          if (!this.disposed) this.update({ levels });
        },
        onMediaError: (code) => {
          if (!this.disposed) this.update({ mediaWarning: code });
        },
      });
      if (this.disposed || !this.started) {
        this.transport.disconnect();
        return;
      }
      this.reconnectDelayMs = 1000;
      const caps = normalizeCapabilities(hello.capabilities);
      const nextCall = hello.currentCall;
      const peerId =
        nextCall?.groupId ??
        hello.currentOffer?.callerId ??
        hello.currentPendingOffer?.callerId ??
        this.state.activePeerId;
      this.update({
        status: hasAnyCallCapability(caps) ? "ready" : "disabled",
        capabilities: caps,
        availability: hello.availability,
        currentOffer: hello.currentOffer,
        currentPendingOffer: hello.currentPendingOffer,
        currentCall: nextCall,
        conferenceInvitations: hello.conferenceInvitations ?? [],
        activePeerId: peerId,
        presentationMode: this.initialPresentationForCall(nextCall, this.state.presentationMode),
      });
      this.syncDurationTimer(nextCall);
      this.syncRingtone();
    } catch (error) {
      if (this.disposed || !this.started) return;
      if (error instanceof NativeApiError && [401, 403, 404, 501].includes(error.status)) {
        this.update({ status: "unavailable", capabilities: { ...EMPTY_CAPABILITIES } });
        return;
      }
      if (error instanceof NativeCallError && error.code === "forbidden") {
        this.update({ status: "unavailable", capabilities: { ...EMPTY_CAPABILITIES } });
        return;
      }
      this.update({ status: "error" });
      this.scheduleReconnect();
    }
  }

  private applyEvent(event: CallEvent): void {
    if (this.disposed) return;
    switch (event.type) {
      case "calls.availability": {
        const lostOwnership =
          this.state.currentCall?.ownedByThisClient &&
          (!event.availability.busy || !event.availability.ownedByThisClient);
        if (lostOwnership) {
          this.transport?.stopMedia();
          this.clearAllRemoteFrames();
          this.stopDurationTimer();
        }
        const clearActiveCall = !event.availability.busy || lostOwnership;
        this.update({
          availability: event.availability,
          ...(clearActiveCall
            ? {
                currentCall: null,
                tracks: [],
                localStream: null,
                levels: { ...EMPTY_CALL_LEVELS },
                connectedAtMs: null,
                elapsedSeconds: 0,
                capWarning: false,
                invitationResults: null,
                mediaWarning: null,
              }
            : {}),
          ...(!event.availability.busy ? { currentOffer: null } : {}),
        });
        this.syncRingtone();
        break;
      }
      case "call.pending": {
        this.update({
          currentPendingOffer: event.offer,
          ...(event.offer && !this.state.currentCall ? { activePeerId: event.offer.callerId } : {}),
        });
        this.syncRingtone();
        break;
      }
      case "call.offered": {
        const key = offerKey(event.offer);
        const resetDismiss = key !== this.state.dismissedOfferKey;
        this.update({
          currentOffer: event.offer,
          currentPendingOffer: null,
          ...(resetDismiss ? { dismissedOfferKey: null, ringtoneMuted: false } : {}),
          ...(!this.state.currentCall ? { activePeerId: event.offer.callerId } : {}),
        });
        if (resetDismiss) this.ringtone.setMuted(false);
        this.syncRingtone();
        break;
      }
      case "conference.invitations": {
        this.update({ conferenceInvitations: event.invitations });
        break;
      }
      case "call.state": {
        const call = event.call;
        if (
          this.state.currentCall?.callId === call.callId &&
          call.revision < this.state.currentCall.revision
        ) {
          break;
        }
        const terminal = isTerminalCallState(call.state);
        if (terminal || !call.ownedByThisClient) {
          this.transport?.stopMedia();
          this.clearAllRemoteFrames();
        } else if (call.state === "ending") {
          // Stop local capture/playout immediately while retaining the media socket until terminal state.
          this.transport?.stopLocalMedia();
          this.clearAllRemoteFrames();
        }
        let notice = this.state.notice;
        if (call.state === "uncertain") {
          notice = { kind: "uncertain", message: "uncertain" };
        }
        const nextCall = terminal ? null : call;
        const nextTracks = terminal ? [] : this.state.tracks;
        const peerId =
          call.groupId ??
          (call.participants?.find((p) => p.contactId)?.contactId ?? this.state.activePeerId);
        this.update({
          currentCall: nextCall,
          currentOffer:
            this.state.currentOffer?.callId === call.callId ? null : this.state.currentOffer,
          tracks: nextTracks,
          activePeerId: nextCall ? peerId : null,
          ...(call.state === "ending"
            ? {
                localStream: null,
                levels: { ...EMPTY_CALL_LEVELS },
              }
            : {}),
          ...(terminal
            ? {
                localStream: null,
                levels: { ...EMPTY_CALL_LEVELS },
                connectedAtMs: null,
                elapsedSeconds: 0,
                capWarning: false,
                invitationResults: null,
                mediaWarning: null,
              }
            : {
                presentationMode: this.initialPresentationForCall(call, this.state.presentationMode),
              }),
          notice,
        });
        if (call.state === "connected" && call.ownedByThisClient) {
          void this.transport?.attachConnectedMedia(
            call.callId,
            call.callType,
            this.state.localMuted,
            this.state.localCamera,
            this.state.selectedDevices,
            this.state.speakerMuted,
          );
        }
        this.syncDurationTimer(nextCall);
        this.syncRingtone();
        break;
      }
      case "participant.updated": {
        const current = this.state.currentCall;
        if (!current || current.callId !== event.callId || event.revision < current.revision) break;
        const participants = [...(current.participants ?? [])];
        const idx = participants.findIndex(
          (item) => item.participantId === event.participant.participantId,
        );
        if (idx >= 0) participants[idx] = event.participant;
        else participants.push(event.participant);
        this.update({
          currentCall: { ...current, revision: event.revision, participants },
        });
        break;
      }
      case "call.control": {
        const current = this.state.currentCall;
        if (!current || current.callId !== event.callId) break;
        const controls = (current.controls ?? []).filter(
          (item) =>
            !(
              item.participantId === event.participantId &&
              item.control.kind === event.control.kind
            ),
        );
        controls.push({ participantId: event.participantId, control: event.control });
        const participants = (current.participants ?? []).map((member) => {
          if (member.participantId !== event.participantId) return member;
          if (event.control.kind === "microphone") {
            return { ...member, microphoneEnabled: !event.control.muted };
          }
          if (event.control.kind === "camera") {
            return { ...member, cameraEnabled: event.control.enabled };
          }
          return member;
        });
        this.update({
          currentCall: { ...current, controls, participants },
        });
        break;
      }
      case "call.invitation_result": {
        this.update({
          invitationResults: { requestId: event.requestId, results: event.results },
        });
        break;
      }
      case "track.added":
      case "track.updated": {
        if (this.state.currentCall && event.callId !== this.state.currentCall.callId) break;
        const prior = this.state.tracks.find((t) => t.id === event.track.id);
        if (prior && event.track.generation < prior.generation) break;
        this.update({ tracks: mergeTrackList(this.state.tracks, [event.track]) });
        break;
      }
      case "track.removed": {
        if (this.state.currentCall && event.callId !== this.state.currentCall.callId) break;
        const existing = this.state.tracks.find((t) => t.id === event.trackId);
        if (!existing || existing.generation !== event.generation) break;
        const next = this.state.tracks.filter((t) => t.id !== event.trackId);
        this.clearRemoteFrame(event.trackId);
        this.update({ tracks: next });
        break;
      }
      case "call.error": {
        if (event.code === "outcome_unknown") {
          this.update({ notice: { kind: "uncertain", message: "uncertain" } });
        } else {
          this.update({ lastError: event.code });
        }
        break;
      }
    }
  }

  private initialPresentationForCall(
    call: CallView | null,
    current: CallPresentationMode,
  ): CallPresentationMode {
    if (!call) return "strip";
    if (this.state.currentCall?.callId === call.callId) return current;
    if (call.kind === "conference" || call.callType === "video") return "theater";
    return "strip";
  }

  private handleDisconnect(code: string): void {
    if (this.disposed || !this.started) return;
    this.ringtone.stop();
    this.stopDurationTimer();
    const hadOwnedCall = this.state.currentCall?.ownedByThisClient === true;
    this.clearAllRemoteFrames();
    this.update({
      status: code === "forbidden" ? "unavailable" : "error",
      currentCall: null,
      currentOffer: null,
      currentPendingOffer: null,
      tracks: [],
      localStream: null,
      levels: { ...EMPTY_CALL_LEVELS },
      connectedAtMs: null,
      elapsedSeconds: 0,
      capWarning: false,
      ...(hadOwnedCall ? { notice: { kind: "uncertain", message: "disconnected" } } : {}),
    });
    if (code !== "forbidden") this.scheduleReconnect();
  }

  private scheduleReconnect(): void {
    if (this.disposed || !this.started || this.reconnectTimer) return;
    const delay = this.reconnectDelayMs;
    this.reconnectDelayMs = Math.min(15000, this.reconnectDelayMs * 2);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      if (!this.disposed && this.started) {
        if (this.probedOnce) void this.connectSocket();
        else void this.probeAndConnect();
      }
    }, delay);
  }

  private handleActionFailure(error: unknown): void {
    const code =
      error instanceof NativeCallError
        ? error.code
        : error instanceof NativeApiError
          ? error.code
          : "request_failed";
    const unknown = error instanceof NativeCallError && error.outcomeUnknown;
    this.update({
      actionPending: false,
      lastError: code,
      ...(unknown ? { notice: { kind: "uncertain", message: "uncertain" } } : {}),
    });
  }

  private syncRingtone(): void {
    const offer = this.state.currentOffer;
    const key = offerKey(offer);
    // On Desktop, OS notifications play AgentWechatSoftRing.wav while the window is
    // hidden or unfocused; suppress Web Audio ringing then to avoid double chimes.
    const windowFocused = this.transport?.kind !== "desktop" || this.isWindowFocused();
    const shouldRing =
      this.started &&
      windowFocused &&
      !!offer &&
      key !== this.state.dismissedOfferKey &&
      !this.state.currentCall?.ownedByThisClient;
    if (shouldRing) {
      this.ringtone.start();
    } else {
      this.ringtone.stop();
    }
  }

  private syncDurationTimer(call: CallView | null): void {
    if (call?.state === "connected") {
      if (this.state.connectedAtMs === null) {
        const startedAt = this.now();
        this.update({ connectedAtMs: startedAt, elapsedSeconds: 0, capWarning: false });
      }
      if (!this.durationTimer) {
        this.durationTimer = setInterval(() => {
          if (this.disposed || this.state.connectedAtMs === null) {
            this.stopDurationTimer();
            return;
          }
          const elapsed = Math.max(0, Math.floor((this.now() - this.state.connectedAtMs) / 1000));
          if (elapsed !== this.state.elapsedSeconds) {
            this.update({
              elapsedSeconds: elapsed,
              capWarning: elapsed >= CALL_CAP_WARNING_SECONDS,
            });
          }
        }, 1000);
      }
    } else {
      this.stopDurationTimer();
      if (this.state.connectedAtMs !== null) {
        this.update({ connectedAtMs: null, elapsedSeconds: 0, capWarning: false });
      }
    }
  }

  private stopDurationTimer(): void {
    if (this.durationTimer) {
      clearInterval(this.durationTimer);
      this.durationTimer = null;
    }
  }

  private handleRemoteFrame(frame: RemoteVideoFrame): void {
    if (this.disposed) {
      frame.bitmap?.close();
      return;
    }
    const previous = this.remoteFrames.get(frame.trackId);
    if (previous?.bitmap && previous.bitmap !== frame.bitmap) {
      previous.bitmap.close();
    }
    this.remoteFrames.set(frame.trackId, frame);
    const listeners = this.frameListeners.get(frame.trackId);
    if (listeners) {
      for (const listener of listeners) listener(frame);
    }
    // Only bump React state when a track delivers its first frame so placeholders swap cleanly.
    if (!(frame.trackId in this.state.remoteTrackFrames)) {
      this.update({
        remoteTrackFrames: { ...this.state.remoteTrackFrames, [frame.trackId]: 1 },
      });
    }
  }

  private clearRemoteFrame(trackId: number): void {
    const previous = this.remoteFrames.get(trackId);
    if (previous?.bitmap) previous.bitmap.close();
    this.remoteFrames.delete(trackId);
    const listeners = this.frameListeners.get(trackId);
    if (listeners) {
      for (const listener of listeners) listener(null);
    }
    if (trackId in this.state.remoteTrackFrames) {
      const next = { ...this.state.remoteTrackFrames };
      delete next[trackId];
      this.update({ remoteTrackFrames: next });
    }
  }

  private clearAllRemoteFrames(): void {
    for (const [trackId, frame] of this.remoteFrames.entries()) {
      frame.bitmap?.close();
      const listeners = this.frameListeners.get(trackId);
      if (listeners) {
        for (const listener of listeners) listener(null);
      }
    }
    this.remoteFrames.clear();
    if (Object.keys(this.state.remoteTrackFrames).length > 0) {
      this.update({ remoteTrackFrames: {} });
    }
  }

  private update(patch: Partial<CallControllerState>): void {
    this.state = { ...this.state, ...patch };
    for (const listener of this.listeners) listener();
  }
}
