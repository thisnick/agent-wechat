import { Channel, invoke } from "@tauri-apps/api/core";
import {
  NativeCallError,
  type CallEvent,
  type CallSnapshot,
  type CallTrack,
  type CallType,
  type NativeCallAnswerOptions,
  type NativeCallDialOptions,
  type NativeCallJoinOptions,
  type NativeCallVideoRotationDegrees,
  type NativeWeChatClient,
} from "@agent-wechat/shared";
import type {
  CallDeviceSelection,
  CallDevicesSnapshot,
  CallSessionTransport,
  CallTransportCallbacks,
  LocalPreviewSurface,
} from "./media-backend.ts";

export type DesktopCallPacket =
  | { type: "event"; event: CallEvent }
  | { type: "tracksSynced"; tracks: CallTrack[] }
  | {
      type: "remoteFrame";
      trackId: number;
      participantId?: string;
      rotationDegrees: number;
      jpeg: string;
    }
  | { type: "localJpeg"; jpeg: string }
  | { type: "trackCleared"; trackId: number }
  | {
      type: "levels";
      micLevel: number;
      speakerLevel: number;
      trackLevels: Record<number, number>;
    }
  | { type: "mediaError"; code: string; message?: string }
  | { type: "osCallAlert"; callId: string | null }
  | { type: "disconnected"; code: string };

export type DesktopPreviewPacket =
  | { type: "frame"; jpeg: string }
  | { type: "ended" };

export function decodeBase64Bytes(base64: string): Uint8Array {
  const fromBase64 = (Uint8Array as unknown as { fromBase64?: (input: string) => Uint8Array })
    .fromBase64;
  if (typeof fromBase64 === "function") {
    return fromBase64(base64);
  }
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function normalizeRotation(degrees: number): NativeCallVideoRotationDegrees {
  if (degrees === 90 || degrees === 180 || degrees === 270) return degrees;
  return 0;
}

function toCallError(error: unknown): NativeCallError {
  if (error instanceof NativeCallError) return error;
  const raw = typeof error === "string" ? error : error instanceof Error ? error.message : String(error);
  const code = raw.trim() || "request_failed";
  return new NativeCallError(code, code === "outcome_unknown");
}

export type DesktopInvokeFn = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;
export type DesktopChannelFactory<T> = () => Channel<T>;

/**
 * Tauri IPC transport implementing `CallSessionTransport` over the Rust
 * `wechat-call-client` + `wechat-audio` + bundled FFmpeg backend.
 */
export class DesktopCallTransport implements CallSessionTransport {
  readonly kind = "desktop" as const;
  private readonly connectionId: string;
  private readonly account: string;
  private readonly client: Pick<NativeWeChatClient, "callSnapshot">;
  private readonly invokeFn: DesktopInvokeFn;
  private readonly createCallChannel: DesktopChannelFactory<DesktopCallPacket>;
  private readonly createPreviewChannel: DesktopChannelFactory<DesktopPreviewPacket>;
  private sessionSeq = 0;
  private activeSession = 0;
  private nativeSessionId: number | null = null;
  private callbacks: CallTransportCallbacks | null = null;
  private hostFullscreenActive = false;
  private videoAckScheduled = false;
  private videoAckRafId: number | null = null;
  private videoAckGen = 0;

  constructor(
    connectionId: string,
    account: string,
    client: Pick<NativeWeChatClient, "callSnapshot">,
    options?: {
      invoke?: DesktopInvokeFn;
      createCallChannel?: DesktopChannelFactory<DesktopCallPacket>;
      createPreviewChannel?: DesktopChannelFactory<DesktopPreviewPacket>;
    },
  ) {
    this.connectionId = connectionId;
    this.account = account;
    this.client = client;
    this.invokeFn = options?.invoke ?? invoke;
    this.createCallChannel = options?.createCallChannel ?? (() => new Channel<DesktopCallPacket>());
    this.createPreviewChannel =
      options?.createPreviewChannel ?? (() => new Channel<DesktopPreviewPacket>());
  }

  probe(signal?: AbortSignal): Promise<CallSnapshot> {
    return this.client.callSnapshot(this.account, signal);
  }

  async connect(callbacks: CallTransportCallbacks): Promise<Extract<CallEvent, { type: "hello" }>> {
    this.disconnect();
    const session = ++this.sessionSeq;
    this.activeSession = session;
    this.callbacks = callbacks;

    const channel = this.createCallChannel();
    channel.onmessage = (packet) => {
      if (this.activeSession !== session || this.callbacks !== callbacks) return;
      switch (packet.type) {
        case "event":
          callbacks.onEvent(packet.event);
          break;
        case "tracksSynced":
          callbacks.onTracksSynced(packet.tracks);
          break;
        case "remoteFrame":
          callbacks.onRemoteFrame({
            trackId: packet.trackId,
            ...(packet.participantId ? { participantId: packet.participantId } : {}),
            rotationDegrees: normalizeRotation(packet.rotationDegrees),
            jpegBytes: decodeBase64Bytes(packet.jpeg),
          });
          this.scheduleVideoAck(session);
          break;
        case "localJpeg":
          callbacks.onLocalJpeg(decodeBase64Bytes(packet.jpeg));
          this.scheduleVideoAck(session);
          break;
        case "trackCleared":
          callbacks.onTrackCleared(packet.trackId);
          break;
        case "levels":
          callbacks.onLevels({
            micLevel: packet.micLevel,
            speakerLevel: packet.speakerLevel,
            trackLevels: packet.trackLevels ?? {},
          });
          break;
        case "mediaError":
          callbacks.onMediaError(packet.code, packet.message);
          break;
        case "osCallAlert":
          callbacks.onOsCallAlert?.(packet.callId);
          break;
        case "disconnected":
          this.cancelVideoAck();
          this.activeSession = 0;
          this.nativeSessionId = null;
          this.callbacks = null;
          this.exitHostFullscreenIfOwned();
          callbacks.onDisconnected(packet.code);
          break;
      }
    };

    try {
      const response = await this.invokeFn<{
        sessionId: number;
        hello: Extract<CallEvent, { type: "hello" }>;
      }>("call_connect", {
        connectionId: this.connectionId,
        accountId: this.account,
        channel,
      });
      if (this.activeSession !== session) {
        void this.invokeFn("call_disconnect", {
          connectionId: this.connectionId,
          accountId: this.account,
          sessionId: response.sessionId,
        }).catch(() => {});
        throw new NativeCallError("closed");
      }
      this.nativeSessionId = response.sessionId;
      return response.hello;
    } catch (error) {
      if (this.activeSession === session) {
        this.activeSession = 0;
        this.nativeSessionId = null;
        this.callbacks = null;
      }
      throw toCallError(error);
    }
  }

  disconnect(): void {
    this.cancelVideoAck();
    const wasActive = this.activeSession !== 0;
    const sessionId = this.nativeSessionId;
    this.activeSession = 0;
    this.nativeSessionId = null;
    this.callbacks = null;
    this.exitHostFullscreenIfOwned();
    if (wasActive && sessionId !== null) {
      void this.invokeFn("call_disconnect", {
        connectionId: this.connectionId,
        accountId: this.account,
        sessionId,
      }).catch(() => {});
    }
  }

  async listDevices(): Promise<CallDevicesSnapshot> {
    try {
      return await this.invokeFn<CallDevicesSnapshot>("call_devices");
    } catch {
      return {
        audioInputs: [],
        audioOutputs: [],
        videoInputs: [],
        permissionState: "unknown",
        supportsOutputSelection: true,
      };
    }
  }

  async startLocalPreview(cameraId?: string): Promise<LocalPreviewSurface> {
    const listeners = new Set<(jpegBytes: Uint8Array) => void>();
    let stopped = false;
    const channel = this.createPreviewChannel();
    channel.onmessage = (packet) => {
      if (stopped || packet.type !== "frame") return;
      const bytes = decodeBase64Bytes(packet.jpeg);
      for (const listener of listeners) {
        listener(bytes);
      }
    };
    let generation: number;
    try {
      generation = await this.invokeFn<number>("call_preview_start", {
        cameraId: cameraId ?? null,
        channel,
      });
    } catch (error) {
      stopped = true;
      throw toCallError(error);
    }
    return {
      subscribeJpeg: (listener) => {
        listeners.add(listener);
        return () => {
          listeners.delete(listener);
        };
      },
      stop: () => {
        if (stopped) return;
        stopped = true;
        listeners.clear();
        void this.invokeFn("call_preview_stop", { generation }).catch(() => {});
      },
    };
  }

  async dialDirect(
    requestId: string,
    contactId: string,
    options: NativeCallDialOptions,
  ): Promise<{ callId: string }> {
    try {
      return await this.invokeFn<{ callId: string }>("call_dial", {
        connectionId: this.connectionId,
        accountId: this.account,
        requestId,
        contactId,
        options,
      });
    } catch (error) {
      throw toCallError(error);
    }
  }

  async startConference(
    requestId: string,
    groupId: string,
    invitees: string[],
    options: NativeCallDialOptions,
  ): Promise<{ callId: string }> {
    try {
      return await this.invokeFn<{ callId: string }>("call_conference_start", {
        connectionId: this.connectionId,
        accountId: this.account,
        requestId,
        groupId,
        invitees,
        options,
      });
    } catch (error) {
      throw toCallError(error);
    }
  }

  async joinConference(
    requestId: string,
    invitationId: string,
    invitationRevision: number,
    options: NativeCallJoinOptions,
  ): Promise<{ callId: string }> {
    try {
      return await this.invokeFn<{ callId: string }>("call_conference_join", {
        connectionId: this.connectionId,
        accountId: this.account,
        requestId,
        invitationId,
        invitationRevision,
        options,
      });
    } catch (error) {
      throw toCallError(error);
    }
  }

  async answer(
    requestId: string,
    callId: string,
    expectedRevision: number,
    _callType: CallType,
    options: NativeCallAnswerOptions,
  ): Promise<void> {
    try {
      await this.invokeFn("call_answer", {
        connectionId: this.connectionId,
        accountId: this.account,
        requestId,
        callId,
        expectedRevision,
        options,
      });
    } catch (error) {
      throw toCallError(error);
    }
  }

  async attachConnectedMedia(
    callId: string,
    callType: CallType,
    initialMuted: boolean,
    initialCameraEnabled: boolean,
    devices: CallDeviceSelection,
    speakerMuted: boolean,
  ): Promise<void> {
    try {
      await this.invokeFn("call_attach_media", {
        connectionId: this.connectionId,
        accountId: this.account,
        callId,
        callType,
        initialMuted,
        initialCameraEnabled,
        devices,
        speakerMuted,
      });
    } catch (error) {
      // Rust emits a structured `mediaError` packet before rejecting AttachMedia; only
      // emit a fallback here for IPC/pre-actor rejections so we don't clobber the code.
      const callError = toCallError(error);
      if (
        callError.code !== "media_attach_failed" &&
        callError.code !== "missing_owner_token" &&
        callError.code !== "unsupported"
      ) {
        this.callbacks?.onMediaError(callError.code);
      }
    }
  }

  async reject(requestId: string, callId: string, expectedRevision: number): Promise<void> {
    try {
      await this.invokeFn("call_reject", {
        connectionId: this.connectionId,
        accountId: this.account,
        requestId,
        callId,
        expectedRevision,
      });
    } catch (error) {
      throw toCallError(error);
    }
  }

  async invite(requestId: string, callId: string, contactIds: string[]): Promise<void> {
    try {
      await this.invokeFn("call_invite", {
        connectionId: this.connectionId,
        accountId: this.account,
        requestId,
        callId,
        contactIds,
      });
    } catch (error) {
      throw toCallError(error);
    }
  }

  async setMute(requestId: string, callId: string, muted: boolean): Promise<void> {
    try {
      await this.invokeFn("call_set_mute", {
        connectionId: this.connectionId,
        accountId: this.account,
        requestId,
        callId,
        muted,
      });
    } catch (error) {
      throw toCallError(error);
    }
  }

  async setCamera(requestId: string, callId: string, enabled: boolean): Promise<void> {
    try {
      await this.invokeFn("call_set_camera", {
        connectionId: this.connectionId,
        accountId: this.account,
        requestId,
        callId,
        enabled,
      });
    } catch (error) {
      throw toCallError(error);
    }
  }

  setSpeakerMuted(muted: boolean): void {
    void this.invokeFn("call_set_speaker_muted", {
      connectionId: this.connectionId,
      accountId: this.account,
      muted,
    }).catch(() => {});
  }

  async switchDevices(selection: CallDeviceSelection): Promise<void> {
    try {
      await this.invokeFn("call_switch_devices", {
        connectionId: this.connectionId,
        accountId: this.account,
        selection,
      });
    } catch (error) {
      throw toCallError(error);
    }
  }

  async end(
    type: "call.cancel" | "call.hangup",
    requestId: string,
    callId: string,
    expectedRevision: number,
  ): Promise<void> {
    try {
      await this.invokeFn("call_end", {
        connectionId: this.connectionId,
        accountId: this.account,
        kind: type,
        requestId,
        callId,
        expectedRevision,
      });
    } catch (error) {
      throw toCallError(error);
    }
  }

  stopLocalMedia(): void {
    void this.invokeFn("call_stop_local_media", {
      connectionId: this.connectionId,
      accountId: this.account,
    }).catch(() => {});
  }

  stopMedia(): void {
    this.cancelVideoAck();
    this.exitHostFullscreenIfOwned();
    void this.invokeFn("call_stop_media", {
      connectionId: this.connectionId,
      accountId: this.account,
    }).catch(() => {});
  }

  private scheduleVideoAck(session: number): void {
    if (this.videoAckScheduled) return;
    this.videoAckScheduled = true;
    const gen = this.videoAckGen;
    const sendAck = () => {
      if (this.videoAckGen !== gen) return;
      this.videoAckScheduled = false;
      this.videoAckRafId = null;
      if (this.activeSession !== session) return;
      void this.invokeFn("call_video_ack", {
        connectionId: this.connectionId,
        accountId: this.account,
      }).catch(() => {});
    };
    if (typeof requestAnimationFrame === "function") {
      this.videoAckRafId = requestAnimationFrame(sendAck);
    } else {
      queueMicrotask(sendAck);
    }
  }

  private cancelVideoAck(): void {
    this.videoAckGen += 1;
    if (this.videoAckRafId !== null && typeof cancelAnimationFrame === "function") {
      cancelAnimationFrame(this.videoAckRafId);
    }
    this.videoAckRafId = null;
    this.videoAckScheduled = false;
  }

  async setFullscreen(enabled: boolean): Promise<boolean> {
    if (!enabled && !this.hostFullscreenActive) {
      return false;
    }
    try {
      const actual = await this.invokeFn<boolean>("call_set_fullscreen", { enabled });
      this.hostFullscreenActive = enabled ? actual : false;
      return actual;
    } catch {
      return false;
    }
  }

  private exitHostFullscreenIfOwned(): void {
    if (!this.hostFullscreenActive) return;
    this.hostFullscreenActive = false;
    void this.invokeFn("call_set_fullscreen", { enabled: false }).catch(() => {});
  }
}
