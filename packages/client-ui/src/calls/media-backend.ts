import type {
  CallAudioFormat,
  CallEvent,
  CallSnapshot,
  CallTrack,
  CallType,
  CallVideoCapabilities,
  NativeCallAnswerOptions,
  NativeCallDialOptions,
  NativeCallJoinOptions,
  NativeCallMediaClient,
  NativeCallVideoRotationDegrees,
} from "@agent-wechat/shared";

export type CallDeviceKind = "audioinput" | "audiooutput" | "videoinput";

export type CallDevice = {
  id: string;
  label: string;
  kind: CallDeviceKind;
  isDefault: boolean;
};

export type CallDevicesSnapshot = {
  audioInputs: CallDevice[];
  audioOutputs: CallDevice[];
  videoInputs: CallDevice[];
  permissionState: "unknown" | "granted" | "denied";
  supportsOutputSelection: boolean;
};

export const EMPTY_CALL_DEVICES: CallDevicesSnapshot = {
  audioInputs: [],
  audioOutputs: [],
  videoInputs: [],
  permissionState: "unknown",
  supportsOutputSelection: false,
};

export type CallAudioLevels = {
  /** Normalized 0..1 microphone RMS level. */
  micLevel: number;
  /** Normalized 0..1 speaker output RMS level. */
  speakerLevel: number;
  /** Normalized 0..1 per-track RMS level keyed by receive trackId. */
  trackLevels: Record<number, number>;
};

export const EMPTY_CALL_LEVELS: CallAudioLevels = {
  micLevel: 0,
  speakerLevel: 0,
  trackLevels: {},
};

export type RemoteVideoFrame = {
  trackId: number;
  participantId?: string;
  rotationDegrees: NativeCallVideoRotationDegrees;
  /** Decoded bitmap when available (browser path). */
  bitmap?: ImageBitmap;
  /** Encoded JPEG bytes when streamed across IPC (desktop path) or bitmap fallback. */
  jpegBytes?: Uint8Array;
  width?: number;
  height?: number;
};

export type LocalPreviewSurface = {
  /** Browser path attaches a MediaStream directly to a <video> element. */
  stream?: MediaStream;
  /** Desktop path streams preview JPEG frames over IPC. */
  subscribeJpeg?: (listener: (jpegBytes: Uint8Array) => void) => () => void;
  stop: () => void;
};

export type CallDeviceSelection = {
  inputDeviceId?: string;
  outputDeviceId?: string;
  cameraDeviceId?: string;
};

export type CallMediaAttachOptions = {
  media: NativeCallMediaClient;
  audioFormat: CallAudioFormat;
  callType: CallType;
  initialMuted: boolean;
  initialCameraEnabled: boolean;
  devices: CallDeviceSelection;
  speakerMuted: boolean;
  onLocalStream: (stream: MediaStream | null) => void;
  onLocalJpeg?: (jpegBytes: Uint8Array) => void;
  onRemoteFrame: (frame: RemoteVideoFrame) => void;
  onTrackCleared: (trackId: number) => void;
  onLevels: (levels: CallAudioLevels) => void;
  onError: (code: string, message?: string) => void;
};

export interface CallMediaHandle {
  setMuted(muted: boolean): void;
  setSpeakerMuted(muted: boolean): void;
  setCameraEnabled(enabled: boolean): Promise<void>;
  switchDevices(selection: CallDeviceSelection): Promise<void>;
  updateTracks(sendTracks: CallTrack[], receiveTracks: CallTrack[]): void;
  stop(): void;
}

/**
 * Pluggable local audio/video engine shared by the browser WebAudio/WebCodecs
 * implementation and the desktop native (Tauri IPC) implementation.
 */
export interface CallMediaBackend {
  readonly kind: "browser" | "desktop";
  videoCapabilities(): CallVideoCapabilities | undefined;
  listDevices(): Promise<CallDevicesSnapshot>;
  startLocalPreview(cameraId?: string): Promise<LocalPreviewSurface>;
  attach(options: CallMediaAttachOptions): Promise<CallMediaHandle>;
}

export type CallTransportCallbacks = {
  onEvent: (event: CallEvent) => void;
  onDisconnected: (code: string) => void;
  onTracksSynced: (tracks: CallTrack[]) => void;
  onLocalStream: (stream: MediaStream | null) => void;
  onLocalJpeg: (jpegBytes: Uint8Array) => void;
  onRemoteFrame: (frame: RemoteVideoFrame) => void;
  onTrackCleared: (trackId: number) => void;
  onLevels: (levels: CallAudioLevels) => void;
  onMediaError: (code: string, message?: string) => void;
  /** Desktop host reports the call ID whose OS notification is currently sounding, or null. */
  onOsCallAlert?: (callId: string | null) => void;
};

/**
 * Control + media session transport for one account. Browser mode uses
 * NativeCallsClient + BrowserCallMediaBackend; Desktop mode implements this
 * interface over Tauri IPC backed by the Rust wechat-call-client crate.
 */
export interface CallSessionTransport {
  readonly kind: "browser" | "desktop";
  probe(signal?: AbortSignal): Promise<CallSnapshot>;
  connect(callbacks: CallTransportCallbacks): Promise<Extract<CallEvent, { type: "hello" }>>;
  disconnect(): void;
  listDevices(): Promise<CallDevicesSnapshot>;
  startLocalPreview(cameraId?: string): Promise<LocalPreviewSurface>;
  dialDirect(requestId: string, contactId: string, options: NativeCallDialOptions, devices: CallDeviceSelection, speakerMuted: boolean): Promise<{ callId: string }>;
  startConference(requestId: string, groupId: string, invitees: string[], options: NativeCallDialOptions, devices: CallDeviceSelection, speakerMuted: boolean): Promise<{ callId: string }>;
  joinConference(requestId: string, invitationId: string, invitationRevision: number, options: NativeCallJoinOptions, devices: CallDeviceSelection, speakerMuted: boolean): Promise<{ callId: string }>;
  answer(requestId: string, callId: string, expectedRevision: number, callType: CallType, options: NativeCallAnswerOptions, devices: CallDeviceSelection, speakerMuted: boolean): Promise<void>;
  /** Attach owner media once the call reaches the `connected` state. */
  attachConnectedMedia(
    callId: string,
    callType: CallType,
    initialMuted: boolean,
    initialCameraEnabled: boolean,
    devices: CallDeviceSelection,
    speakerMuted: boolean,
  ): Promise<void>;
  reject(requestId: string, callId: string, expectedRevision: number): Promise<void>;
  invite(requestId: string, callId: string, contactIds: string[]): Promise<void>;
  setMute(requestId: string, callId: string, muted: boolean): Promise<void>;
  setCamera(requestId: string, callId: string, enabled: boolean): Promise<void>;
  setSpeakerMuted(muted: boolean): void;
  switchDevices(selection: CallDeviceSelection): Promise<void>;
  end(type: "call.cancel" | "call.hangup", requestId: string, callId: string, expectedRevision: number): Promise<void>;
  /** Stop local devices/audio on `ending` while retaining the media socket until terminal state. */
  stopLocalMedia(): void;
  /** Tear down local media and close the media socket on terminal state or ownership loss. */
  stopMedia(): void;
  /** Optional host fullscreen toggle (e.g. Tauri window fullscreen fallback). */
  setFullscreen?(enabled: boolean): Promise<boolean>;
}
