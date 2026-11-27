/** Native call control and binary WXCM v3 PCM/video media. Hosts supply a
 * WebSocket implementation that supports authenticated request headers, or
 * browsers can supply a ticketProvider that requests a single-use ticket from
 * POST /v1/accounts/{a}/calls/tickets. Call routes require the account's
 * `calls` API scope. Connecting alone stays passive; readiness and ownership
 * are separate protocol actions. There are no device or codec dependencies. */
import { validateNativeBaseUrl } from "./native-client.ts";
import type {
  CallAudioFormat,
  CallCommand,
  CallEvent,
  CallMediaCommand,
  CallReadyScope,
  CallSnapshot,
  CallTicketSocket,
  CallTrack,
  CallType,
  CallVideoCapabilities,
  CallVideoCodec,
} from "./native.generated.js";
export type CallAudioTrack = Extract<CallTrack, { media: "audio" }>;
export type CallVideoTrack = Extract<CallTrack, { media: "video" }>;
export type {
  CallAudioFormat,
  CallTrack,
  CallCommand,
  CallControl,
  CallParticipantControl,
  CallEvent,
  CallSnapshot,
  CallPendingOffer,
  CallReadyScope,
  CallConferenceInvitation,
  CallParticipant,
  CallParticipantStatus,
  CallParticipantStatusKind,
  CallInvitationResult,
  CallType,
  CallVideoCapabilities,
  CallVideoCodec,
  CallVideoFormat,
  CallVideoMode,
  CallTicketSocket,
  CallTicketRequest,
  CallTicketResponse,
} from "./native.generated.js";

export const NATIVE_CALL_MAX_FRAME_BYTES = 64 * 1024;
export const NATIVE_CALL_MAX_VIDEO_FRAME_BYTES = 512 * 1024;
export const NATIVE_CALL_PROTOCOL_VERSION = 3;
export const NATIVE_CALL_VIDEO_FLAG_KEYFRAME = 0x0001;
export const NATIVE_CALL_VIDEO_FLAG_CODEC_CONFIG = 0x0002;
export const NATIVE_CALL_VIDEO_FLAG_ROTATE_90 = 0x0004;
export const NATIVE_CALL_VIDEO_FLAG_ROTATE_180 = 0x0008;
export const NATIVE_CALL_VIDEO_FLAG_ROTATE_270 = 0x000c;
export const NATIVE_CALL_VIDEO_FLAG_ROTATION_MASK = 0x000c;
const KNOWN_VIDEO_FLAGS =
  NATIVE_CALL_VIDEO_FLAG_KEYFRAME |
  NATIVE_CALL_VIDEO_FLAG_CODEC_CONFIG |
  NATIVE_CALL_VIDEO_FLAG_ROTATION_MASK;
export type NativeCallVideoRotationDegrees = 0 | 90 | 180 | 270;

function videoRotationFromFlags(flags: number): NativeCallVideoRotationDegrees {
  switch (flags & NATIVE_CALL_VIDEO_FLAG_ROTATION_MASK) {
    case NATIVE_CALL_VIDEO_FLAG_ROTATE_90: return 90;
    case NATIVE_CALL_VIDEO_FLAG_ROTATE_180: return 180;
    case NATIVE_CALL_VIDEO_FLAG_ROTATE_270: return 270;
    default: return 0;
  }
}

function videoRotationFlags(degrees: NativeCallVideoRotationDegrees | undefined): number {
  switch (degrees) {
    case 90: return NATIVE_CALL_VIDEO_FLAG_ROTATE_90;
    case 180: return NATIVE_CALL_VIDEO_FLAG_ROTATE_180;
    case 270: return NATIVE_CALL_VIDEO_FLAG_ROTATE_270;
    case 0:
    case undefined: return 0;
    default: throw new NativeCallError("invalid_media");
  }
}
const WXCM_HEADER_BYTES = 36;
const WXCM_MAGIC = [0x57, 0x58, 0x43, 0x4d]; // "WXCM"
const WXCM_PAYLOAD_PCM_S16LE = 1;
const WXCM_PAYLOAD_H265_ANNEXB = 2;
const WXCM_PAYLOAD_H264_ANNEXB = 3;
const WXCM_PAYLOAD_JPEG = 4;
const MEDIA_BINARY_BYTES = WXCM_HEADER_BYTES + NATIVE_CALL_MAX_VIDEO_FRAME_BYTES;

export type NativeCallPcmFormat = CallAudioFormat;
export type NativeCallMediaFrame = {
  trackId: number;
  generation: number;
  payload: Uint8Array;
  /** Per-track sequence, independent of transport and application generations. */
  sequence?: number;
  /** Per-channel sample position on the shared call timeline, including queue drops. */
  sampleOffset?: number;
  /** Monotonic arrival time when a PCM observer is attached. */
  receivedAtMs?: number;
};
export type NativeCallVideoFrame = {
  trackId: number;
  generation: number;
  codec: CallVideoCodec;
  flags: number;
  keyframe: boolean;
  codecConfig: boolean;
  rotationDegrees: NativeCallVideoRotationDegrees;
  sequence: number;
  timestampMicros: number;
  payload: Uint8Array;
};
export type NativeCallKeyframeRequest = {
  callId: string;
  trackId: number;
  generation: number;
};
export type NativeCallDialOptions = {
  callType?: CallType;
  cameraEnabled?: boolean;
  muted?: boolean;
};
export type NativeCallAnswerOptions = {
  cameraEnabled?: boolean;
  muted?: boolean;
};
export type NativeCallJoinOptions = {
  cameraEnabled?: boolean;
  muted?: boolean;
};
export type NativeCallVideoSendOptions = {
  codec?: CallVideoCodec;
  keyframe?: boolean;
  codecConfig?: boolean;
  rotationDegrees?: NativeCallVideoRotationDegrees;
  timestampMicros?: number;
};
export type NativeCallTicketProvider = (socket: CallTicketSocket, callId?: string) => Promise<string>;
export type NativeCallPcmObserver = (arrival: {
  receivedAtMs: number; samples: number; queueDepth: number; droppedSamples: number;
}) => void;

function pcmBytes(format: NativeCallPcmFormat): number {
  if (![8000, 12000, 16000, 24000, 48000].includes(format.sampleRate)
    || ![1, 2].includes(format.channels) || ![10, 20, 40, 60].includes(format.frameMs)) throw new Error("Invalid call PCM format");
  return format.sampleRate * format.frameMs / 1000 * format.channels * 2;
}

function videoPayloadKind(codec: CallVideoCodec): number {
  switch (codec) {
    case "h265": return WXCM_PAYLOAD_H265_ANNEXB;
    case "h264": return WXCM_PAYLOAD_H264_ANNEXB;
    case "jpeg": return WXCM_PAYLOAD_JPEG;
  }
}

function payloadKindToVideoCodec(kind: number): CallVideoCodec | undefined {
  switch (kind) {
    case WXCM_PAYLOAD_H265_ANNEXB: return "h265";
    case WXCM_PAYLOAD_H264_ANNEXB: return "h264";
    case WXCM_PAYLOAD_JPEG: return "jpeg";
    default: return undefined;
  }
}

export interface NativeCallSocket {
  readonly readyState: number;
  readonly bufferedAmount: number;
  binaryType: string;
  onopen: ((event: unknown) => void) | null;
  onmessage: ((event: { data: unknown }) => void) | null;
  onerror: ((event: unknown) => void) | null;
  onclose: ((event: unknown) => void) | null;
  send(data: string | Uint8Array): void;
  close(code?: number, reason?: string): void;
}
export type NativeCallWebSocketFactory = (url: string, options: { headers: Record<string, string>; maxPayload: number }) => NativeCallSocket;
export class NativeCallError extends Error {
  readonly code: string;
  readonly outcomeUnknown: boolean;
  constructor(code: string, outcomeUnknown = false) {
    super(`Native call operation failed (${code})`); this.name = "NativeCallError"; this.code = code; this.outcomeUnknown = outcomeUnknown;
  }
}
function resourceId(value: string): string {
  if (!value || value === "." || value === ".." || value.length > 200 || /[\u0000-\u001f\u007f]/.test(value)) throw new Error("Invalid call resource ID");
  return encodeURIComponent(value);
}
function callSocketUrl(baseUrl: string, accountId: string, suffix: string, ticket?: string): string {
  const url = new URL(`v1/accounts/${resourceId(accountId)}/calls/${suffix}`, validateNativeBaseUrl(baseUrl));
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  if (ticket !== undefined) {
    if (!/^[0-9a-f]{64}$/i.test(ticket)) throw new NativeCallError("invalid_ticket");
    url.searchParams.set("ticket", ticket);
  }
  return url.toString();
}

class EventQueue<T> implements AsyncIterable<T> {
  private values: T[] = [];
  private waiter?: { resolve: (value: IteratorResult<T>) => void; reject: (reason: unknown) => void };
  private ended = false;
  private failure?: Error;
  private consumed = false;
  private readonly limit: number;
  constructor(limit: number) { this.limit = limit; }
  push(value: T): boolean {
    if (this.ended) return false;
    if (this.waiter) { const waiter = this.waiter; this.waiter = undefined; waiter.resolve({ value, done: false }); return true; }
    if (this.values.length >= this.limit) return false;
    this.values.push(value); return true;
  }
  get depth(): number { return this.values.length; }
  pushLatest(value: T): T | undefined {
    const dropped = this.values.length === this.limit ? this.values.shift() : undefined;
    this.push(value);
    return dropped;
  }
  retain(predicate: (value: T) => boolean): void { this.values = this.values.filter(predicate); }
  clear(): void { this.values = []; }
  close(failure?: Error): void {
    this.ended = true; this.failure = failure; this.values = [];
    if (this.waiter) {
      const waiter = this.waiter; this.waiter = undefined;
      if (failure) waiter.reject(failure); else waiter.resolve({ value: undefined, done: true });
    }
  }
  [Symbol.asyncIterator](): AsyncIterator<T> {
    if (this.consumed) throw new Error("Call event stream already has a consumer");
    this.consumed = true;
    return { next: () => {
      const value = this.values.shift();
      if (value !== undefined) return Promise.resolve({ value, done: false });
      if (this.failure) return Promise.reject(this.failure);
      if (this.ended) return Promise.resolve({ value: undefined, done: true });
      if (this.waiter) return Promise.reject(new Error("Concurrent call event reads are unsupported"));
      return new Promise((resolve, reject) => { this.waiter = { resolve, reject }; });
    } };
  }
}

function encodeWxcm(kind: number, flags: number, trackId: number, generation: number, sequence: number, timestampMicros: number, payload: Uint8Array): Uint8Array {
  const out = new Uint8Array(WXCM_HEADER_BYTES + payload.byteLength);
  out.set(WXCM_MAGIC, 0);
  out[4] = NATIVE_CALL_PROTOCOL_VERSION;
  out[5] = kind;
  const view = new DataView(out.buffer);
  view.setUint16(6, flags, false);
  view.setUint32(8, trackId, false);
  view.setUint32(12, generation, false);
  view.setBigUint64(16, BigInt(sequence), false);
  view.setBigUint64(24, BigInt(timestampMicros), false);
  view.setUint32(32, payload.byteLength, false);
  out.set(payload, WXCM_HEADER_BYTES);
  return out;
}

function toBytes(data: unknown): Uint8Array | undefined {
  if (data instanceof Uint8Array) return data;
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  return undefined;
}

function decodeWxcm(raw: Uint8Array): { kind: number; flags: number; trackId: number; generation: number; sequence: number; timestampMicros: number; payload: Uint8Array } {
  if (raw.byteLength < WXCM_HEADER_BYTES || raw.byteLength > MEDIA_BINARY_BYTES) throw new NativeCallError("invalid_media");
  if (raw[0] !== WXCM_MAGIC[0] || raw[1] !== WXCM_MAGIC[1] || raw[2] !== WXCM_MAGIC[2] || raw[3] !== WXCM_MAGIC[3] || raw[4] !== NATIVE_CALL_PROTOCOL_VERSION) {
    throw new NativeCallError("invalid_media");
  }
  const kind = raw[5]!;
  if (![WXCM_PAYLOAD_PCM_S16LE, WXCM_PAYLOAD_H265_ANNEXB, WXCM_PAYLOAD_H264_ANNEXB, WXCM_PAYLOAD_JPEG].includes(kind)) {
    throw new NativeCallError("invalid_media");
  }
  const view = new DataView(raw.buffer, raw.byteOffset, raw.byteLength);
  const flags = view.getUint16(6, false);
  const trackId = view.getUint32(8, false);
  const generation = view.getUint32(12, false);
  const sequenceBig = view.getBigUint64(16, false);
  const timestampBig = view.getBigUint64(24, false);
  const payloadLen = view.getUint32(32, false);
  if (trackId === 0 || generation === 0 || payloadLen === 0 || payloadLen !== raw.byteLength - WXCM_HEADER_BYTES) {
    throw new NativeCallError("invalid_media");
  }
  if (kind === WXCM_PAYLOAD_PCM_S16LE) {
    if (flags !== 0 || payloadLen > NATIVE_CALL_MAX_FRAME_BYTES) throw new NativeCallError("invalid_media");
  } else if ((flags & ~KNOWN_VIDEO_FLAGS) !== 0 || payloadLen > NATIVE_CALL_MAX_VIDEO_FRAME_BYTES) {
    throw new NativeCallError("invalid_media");
  }
  const sequence = Number(sequenceBig);
  const timestampMicros = Number(timestampBig);
  if (!Number.isSafeInteger(sequence) || !Number.isSafeInteger(timestampMicros)) throw new NativeCallError("invalid_media");
  return { kind, flags, trackId, generation, sequence, timestampMicros, payload: raw.slice(WXCM_HEADER_BYTES) };
}

type CallHello = Extract<CallEvent, { type: "hello" }>;
type CallResult = Extract<CallEvent, { type: "command.result" }>;
type MediaReady = Extract<CallEvent, { type: "media.ready" }>;
const CONTROL_BYTES = 16 * 1024;
function object(value: unknown): value is Record<string, unknown> { return !!value && typeof value === "object" && !Array.isArray(value); }
function idValue(value: unknown): value is string { return typeof value === "string" && value.length > 0 && value.length <= 200 && !/[\u0000-\u001f\u007f]/.test(value); }
function uint32(value: unknown): value is number { return typeof value === "number" && Number.isInteger(value) && value >= 0 && value <= 0xffffffff; }
function formatValue(value: unknown): value is CallAudioFormat {
  if (!object(value)) return false;
  try { pcmBytes(value as CallAudioFormat); return true; } catch { return false; }
}
function videoCapabilitiesValue(value: unknown): boolean {
  if (!object(value) || !Array.isArray(value.sendCodecs) || !Array.isArray(value.receiveCodecs)) return false;
  const codecs = ["h265", "h264", "jpeg"];
  return value.sendCodecs.length <= 8 && value.receiveCodecs.length <= 8
    && value.sendCodecs.every(c => codecs.includes(String(c)))
    && value.receiveCodecs.every(c => codecs.includes(String(c)))
    && (value.maxWidth === undefined || uint32(value.maxWidth))
    && (value.maxHeight === undefined || uint32(value.maxHeight))
    && (value.maxFps === undefined || uint32(value.maxFps));
}
function videoFormatValue(value: unknown): boolean {
  return object(value) && ["h265", "h264", "jpeg"].includes(String(value.codec))
    && ["passthrough", "transcoded"].includes(String(value.mode))
    && uint32(value.maxWidth) && uint32(value.maxHeight) && uint32(value.maxFps)
    && (value.bitrateKbps === undefined || uint32(value.bitrateKbps));
}
function trackValue(value: unknown): value is CallTrack {
  if (!object(value) || !uint32(value.id) || value.id <= 0 || !uint32(value.generation) || value.generation <= 0) return false;
  if (!["send", "receive"].includes(String(value.direction))) return false;
  if (value.participantId !== undefined && !idValue(value.participantId)) return false;
  if (value.media === "audio") return formatValue(value.format);
  if (value.media === "video") return videoFormatValue(value.format) && (value.active === undefined || typeof value.active === "boolean");
  return false;
}
function availabilityValue(value: unknown): boolean { return object(value) && typeof value.busy === "boolean" && typeof value.ownedByThisClient === "boolean"; }
function participantControlValue(value: unknown): boolean {
  if (!object(value) || !idValue(value.participantId) || !object(value.control)) return false;
  if (value.control.kind === "microphone") return typeof value.control.muted === "boolean";
  if (value.control.kind === "camera") return typeof value.control.enabled === "boolean";
  return false;
}
function invitationValue(value: unknown): boolean {
  return object(value) && idValue(value.invitationId) && uint32(value.revision) && value.revision > 0 && idValue(value.groupId) && idValue(value.callerId);
}
function participantStatusValue(value: unknown): boolean {
  if (!object(value) || !uint32(value.code)) return false;
  const kinds = ["undefined", "joined", "invited", "acknowledged", "left", "cancelled", "busy", "rejected", "no_response"];
  return value.kind === (kinds[Number(value.code)] ?? "unknown");
}
function participantValue(value: unknown): boolean {
  return object(value) && idValue(value.participantId) && idValue(value.contactId)
    && ["joined", "invited", "left", "unknown"].includes(String(value.membership))
    && (value.microphoneEnabled === undefined || typeof value.microphoneEnabled === "boolean")
    && (value.cameraEnabled === undefined || typeof value.cameraEnabled === "boolean")
    && (value.status === undefined || participantStatusValue(value.status));
}
function callValue(value: unknown): boolean {
  return object(value) && idValue(value.callId) && uint32(value.revision)
    && ["direct", "conference"].includes(String(value.kind)) && ["voice", "video"].includes(String(value.callType)) && ["incoming", "outgoing"].includes(String(value.direction))
    && ["offered", "dialing", "ringing", "connecting", "connected", "ending", "ended", "uncertain"].includes(String(value.state)) && typeof value.ownedByThisClient === "boolean"
    && (value.groupId === undefined || idValue(value.groupId))
    && (value.participants === undefined || (Array.isArray(value.participants) && value.participants.length <= 256 && value.participants.every(participantValue)
      && new Set(value.participants.map(item => (item as Record<string, unknown>).participantId)).size === value.participants.length))
    && (value.controls === undefined || (Array.isArray(value.controls) && value.controls.length <= 256 && value.controls.every(participantControlValue)));
}
function offerValue(value: unknown): boolean {
  return object(value) && idValue(value.callId) && uint32(value.revision) && idValue(value.callerId)
    && ["direct", "conference"].includes(String(value.kind)) && ["voice", "video"].includes(String(value.callType));
}
function pendingOfferValue(value: unknown): boolean {
  return offerValue(value) && object(value) && ["waiting_for_ready", "preparing"].includes(String(value.phase));
}
function errorCode(value: unknown): boolean {
  return typeof value === "string" && ["invalid_request", "busy", "already_claimed", "offer_expired", "not_owner", "unsupported", "media_not_ready", "outcome_unknown", "capacity", "stale", "forbidden", "closed"].includes(value);
}
function parseCallEvent(data: unknown, byteLimit = CONTROL_BYTES): CallEvent | null {
  if (typeof data !== "string" || new TextEncoder().encode(data).byteLength > byteLimit) throw new NativeCallError("invalid_event");
  let value: unknown;
  try { value = JSON.parse(data); } catch { throw new NativeCallError("invalid_event"); }
  if (!object(value) || typeof value.type !== "string" || value.type.length === 0 || value.type.length > 128 || /[\u0000-\u001f\u007f]/.test(value.type)) throw new NativeCallError("invalid_event");
  if (value.type === "protocol.error") throw new NativeCallError(["unsupported_protocol_version", "invalid_protocol_version"].includes(String(value.code)) ? String(value.code) : "invalid_event");
  if (["hello", "media.ready"].includes(String(value.type)) && (value.protocolVersion ?? 1) !== NATIVE_CALL_PROTOCOL_VERSION) throw new NativeCallError("unsupported_protocol_version");
  let valid = false;
  switch (value.type) {
    case "hello":
      if (value.currentPendingOffer === undefined) value.currentPendingOffer = null;
      if (value.conferenceInvitations === undefined) value.conferenceInvitations = [];
      valid = value.protocolVersion === NATIVE_CALL_PROTOCOL_VERSION && idValue(value.clientId) && object(value.capabilities)
      && Object.entries(value.capabilities).every(([key, enabled]) => idValue(key) && typeof enabled === "boolean")
      && availabilityValue(value.availability) && (value.currentOffer === null || offerValue(value.currentOffer))
      && (value.currentPendingOffer === null || pendingOfferValue(value.currentPendingOffer))
      && (value.currentCall === null || callValue(value.currentCall)) && formatValue(value.audioFormat)
      && (value.videoCapabilities === undefined || videoCapabilitiesValue(value.videoCapabilities))
      && Array.isArray(value.conferenceInvitations) && value.conferenceInvitations.length <= 16 && value.conferenceInvitations.every(invitationValue); break;
    case "command.result": valid = idValue(value.requestId) && ["accepted", "replayed", "error"].includes(String(value.status))
      && (value.callId === null || idValue(value.callId)) && (value.ownerToken === null || idValue(value.ownerToken))
      && (value.code === null || errorCode(value.code)); break;
    case "call.offered": valid = offerValue(value.offer); break;
    case "call.pending": valid = value.offer === null || pendingOfferValue(value.offer); break;
    case "call.state": valid = callValue(value.call); break;
    case "conference.invitations": valid = Array.isArray(value.invitations) && value.invitations.length <= 16 && value.invitations.every(invitationValue); break;
    case "participant.updated": valid = idValue(value.callId) && uint32(value.revision) && participantValue(value.participant); break;
    case "call.invitation_result": valid = idValue(value.callId) && idValue(value.requestId) && Array.isArray(value.results) && value.results.length <= 100
      && value.results.every(item => object(item) && idValue(item.contactId) && ["submitted", "rejected", "unknown", "uncertain"].includes(String(item.status)) && typeof item.verificationRequired === "boolean"); break;
    case "track.removed": valid = idValue(value.callId) && uint32(value.trackId) && value.trackId > 0 && uint32(value.generation) && value.generation > 0 && idValue(value.reason); break;
    case "call.control": valid = idValue(value.callId) && participantControlValue(value); break;
    case "call.peer_status": valid = idValue(value.callId) && idValue(value.participantId) && participantStatusValue(value.status); break;
    case "calls.availability": valid = availabilityValue(value.availability); break;
    case "track.added": case "track.updated": valid = idValue(value.callId) && trackValue(value.track); break;
    case "video.keyframe_requested": valid = idValue(value.callId) && uint32(value.trackId) && value.trackId > 0 && uint32(value.generation) && value.generation > 0; break;
    case "media.ready": {
      const sendTracks = value.sendTracks;
      const receiveTracks = value.receiveTracks;
      if (value.protocolVersion === NATIVE_CALL_PROTOCOL_VERSION && Array.isArray(sendTracks) && Array.isArray(receiveTracks)
        && sendTracks.length >= 1 && sendTracks.length <= 16 && receiveTracks.length <= 256
        && sendTracks.every(track => trackValue(track) && track.direction === "send")
        && receiveTracks.every(track => trackValue(track) && track.direction === "receive")) {
        const sendIds = new Set(sendTracks.map(track => (track as CallTrack).id));
        const recvIds = new Set(receiveTracks.map(track => (track as CallTrack).id));
        const hasAudioSend = sendTracks.some(track => (track as CallTrack).media === "audio");
        valid = hasAudioSend && sendIds.size === sendTracks.length && recvIds.size === receiveTracks.length
          && [...recvIds].every(id => !sendIds.has(id));
      }
      break;
    }
    case "call.error": valid = errorCode(value.code); break;
    default: return null;
  }
  if (!valid) throw new NativeCallError("invalid_event");
  return value as CallEvent;
}

export type NativeCallsClientOptions = {
  baseUrl: string;
  token?: string;
  ticketProvider?: NativeCallTicketProvider;
  accountId: string;
  webSocketFactory: NativeCallWebSocketFactory;
  handshakeTimeoutMs?: number;
  commandTimeoutMs?: number;
  maxEvents?: number;
  maxBufferedBytes?: number;
};

function validateSocketAuthOptions(options: NativeCallsClientOptions): void {
  if (!options.ticketProvider) {
    if (typeof options.token !== "string" || !options.token || /[\r\n]/.test(options.token)) {
      throw new Error("Call API bearer token is required");
    }
  } else if (options.token !== undefined && (typeof options.token !== "string" || /[\r\n]/.test(options.token))) {
    throw new Error("Invalid call API bearer token");
  }
}

async function resolveSocketTarget(
  options: NativeCallsClientOptions,
  socketKind: CallTicketSocket,
  suffix: string,
  callId?: string,
): Promise<{ url: string; headers: Record<string, string> }> {
  if (options.ticketProvider) {
    const ticket = await options.ticketProvider(socketKind, callId);
    return {
      url: callSocketUrl(options.baseUrl, options.accountId, suffix, ticket),
      headers: {},
    };
  }
  return {
    url: callSocketUrl(options.baseUrl, options.accountId, suffix),
    headers: { Authorization: `Bearer ${options.token}` },
  };
}

type PendingCall = { command: string; promise: Promise<CallResult>; resolve: (value: CallResult) => void; reject: (reason: Error) => void; timer: ReturnType<typeof setTimeout> };
/** Explicitly connected headless call controller. No command is automatically
 * retried, no readiness is implied by connection, and no microphone is opened. */
export class NativeCallsClient {
  private readonly options: NativeCallsClientOptions;
  private readonly queue: EventQueue<CallEvent>;
  private readonly pending = new Map<string, PendingCall>();
  private socket?: NativeCallSocket;
  private opening?: Promise<CallHello>;
  private openResolve?: (hello: CallHello) => void;
  private openReject?: (error: Error) => void;
  private handshakeTimer?: ReturnType<typeof setTimeout>;
  private hello?: CallHello;
  private closed = false;
  private owner?: { callId: string; token: string };
  private readonly media = new Map<string, NativeCallMediaClient>();
  constructor(options: NativeCallsClientOptions) {
    const url = validateNativeBaseUrl(options.baseUrl);
    const env = (globalThis as typeof globalThis & { process?: { env?: Record<string, string | undefined> } }).process?.env;
    if (url.protocol === "https:" && env?.NODE_TLS_REJECT_UNAUTHORIZED === "0") throw new Error("Call API requires TLS verification");
    validateSocketAuthOptions(options);
    resourceId(options.accountId);
    for (const value of [options.handshakeTimeoutMs ?? 10_000, options.commandTimeoutMs ?? 30_000]) if (!Number.isInteger(value) || value < 1 || value > 300_000) throw new Error("Invalid call timeout");
    const maxEvents = options.maxEvents ?? 64; const maxBufferedBytes = options.maxBufferedBytes ?? 64 * 1024;
    if (!Number.isInteger(maxEvents) || maxEvents < 1 || maxEvents > 256 || !Number.isInteger(maxBufferedBytes) || maxBufferedBytes < 1024 || maxBufferedBytes > 1024 * 1024) throw new Error("Invalid call queue bound");
    this.options = { ...options, maxEvents, maxBufferedBytes }; this.queue = new EventQueue(maxEvents);
  }
  connect(): Promise<CallHello> {
    if (this.closed) return Promise.reject(new NativeCallError("closed"));
    if (this.opening) return this.opening;
    this.opening = new Promise((resolve, reject) => { this.openResolve = resolve; this.openReject = reject; });
    this.handshakeTimer = setTimeout(() => this.fail(new NativeCallError("handshake_timeout")), this.options.handshakeTimeoutMs ?? 10_000);
    const openSocket = (target: { url: string; headers: Record<string, string> }) => {
      if (this.closed) return;
      try {
        const socket = this.options.webSocketFactory(target.url, { headers: target.headers, maxPayload: CONTROL_BYTES });
        this.socket = socket; socket.binaryType = "arraybuffer";
        socket.onopen = () => { try { socket.send(JSON.stringify({ type: "call.hello", supportedProtocolVersions: [NATIVE_CALL_PROTOCOL_VERSION] })); } catch { this.fail(new NativeCallError("send_failed")); } };
        socket.onmessage = event => { try { this.receive(parseCallEvent(event.data)); } catch (error) { this.fail(error instanceof NativeCallError ? error : new NativeCallError("invalid_event")); } };
        socket.onerror = () => this.fail(new NativeCallError("connection_failed", this.pending.size > 0));
        socket.onclose = () => this.fail(new NativeCallError("closed", this.pending.size > 0));
      } catch { this.fail(new NativeCallError("connection_failed")); }
    };
    if (this.options.ticketProvider) {
      void resolveSocketTarget(this.options, "control", "socket")
        .then(openSocket, error => this.fail(error instanceof NativeCallError ? error : new NativeCallError("connection_failed")));
    } else {
      openSocket({
        url: callSocketUrl(this.options.baseUrl, this.options.accountId, "socket"),
        headers: { Authorization: `Bearer ${this.options.token}` },
      });
    }
    return this.opening;
  }
  events(): AsyncIterable<CallEvent> { return this.queue; }
  private receive(event: CallEvent | null): void {
    if (this.closed) return;
    if (!event) {
      if (!this.hello) throw new NativeCallError("missing_hello");
      return;
    }
    if (!this.hello && event.type !== "hello") throw new NativeCallError("missing_hello");
    if (event.type === "hello") {
      if (this.hello) throw new NativeCallError("duplicate_hello");
      this.hello = event; clearTimeout(this.handshakeTimer);
    }
    // Keep snapshot and subsequent events in one queue from the first message.
    if (!this.queue.push(event)) { this.fail(new NativeCallError("event_backpressure")); return; }
    if (event.type === "hello") { this.openResolve?.(event); this.openResolve = undefined; this.openReject = undefined; }
    if (event.type === "command.result") {
      const pending = this.pending.get(event.requestId);
      if (!pending) return;
      clearTimeout(pending.timer); this.pending.delete(event.requestId);
      if (event.status === "error") pending.reject(new NativeCallError(event.code ?? "invalid_request", event.code === "outcome_unknown"));
      else {
        if (event.callId && event.ownerToken) this.owner = { callId: event.callId, token: event.ownerToken };
        pending.resolve(event);
      }
    }
    // Receive lifecycle is ordered on the media socket itself. A faster control
    // socket must not remove a source before its preceding frames are delivered.
    if ((event.type === "track.updated" || event.type === "track.added") && event.track.direction === "send") this.media.get(event.callId)?.updateTrack(event.track);
    if (event.type === "call.state") {
      if (!event.call.ownedByThisClient || ["ended", "uncertain"].includes(event.call.state)) this.dropMedia(event.call.callId);
      else if (event.call.state === "ending") this.media.get(event.call.callId)?.stopAudio();
    }
    if (event.type === "calls.availability" && (!event.availability.busy || !event.availability.ownedByThisClient)) { for (const callId of this.media.keys()) this.dropMedia(callId); this.owner = undefined; }
    if (event.type === "call.error" && ["forbidden", "closed"].includes(event.code)) this.fail(new NativeCallError(event.code));
  }
  command(command: CallCommand): Promise<CallResult> {
    if (!this.hello || this.closed || this.socket?.readyState !== 1) return Promise.reject(new NativeCallError("not_connected"));
    if (!idValue(command.requestId)) return Promise.reject(new NativeCallError("invalid_request"));
    const encoded = JSON.stringify(command);
    const encodedBytes = new TextEncoder().encode(encoded).byteLength;
    if (encodedBytes > CONTROL_BYTES) return Promise.reject(new NativeCallError("invalid_request"));
    const prior = this.pending.get(command.requestId);
    if (prior) return prior.command === encoded ? prior.promise : Promise.reject(new NativeCallError("idempotency_conflict"));
    if (this.pending.size >= 64 || this.socket.bufferedAmount + encodedBytes > this.options.maxBufferedBytes!) return Promise.reject(new NativeCallError("control_backpressure"));
    let resolve!: PendingCall["resolve"]; let reject!: PendingCall["reject"];
    const promise = new Promise<CallResult>((yes, no) => { resolve = yes; reject = no; });
    const timer = setTimeout(() => { this.pending.delete(command.requestId); reject(new NativeCallError("command_timeout", true)); }, this.options.commandTimeoutMs ?? 30_000);
    this.pending.set(command.requestId, { command: encoded, promise, resolve, reject, timer });
    try { this.socket.send(encoded); } catch { this.fail(new NativeCallError("send_failed", true)); }
    return promise;
  }
  ready(requestId: string, ready: boolean, format = this.hello?.audioFormat, scope?: CallReadyScope, video?: CallVideoCapabilities): Promise<CallResult> {
    if (!format) return Promise.reject(new NativeCallError("not_connected"));
    pcmBytes(format);
    return this.command({
      type: "call.ready",
      requestId,
      ready,
      format,
      ...(scope ? { scope } : {}),
      ...(video ? { video } : {}),
    });
  }
  dial(requestId: string, contactId: string, options: NativeCallDialOptions = {}): Promise<CallResult> {
    return this.command({
      type: "call.dial",
      requestId,
      target: { kind: "direct", contactId },
      callType: options.callType ?? "voice",
      ...(options.cameraEnabled !== undefined ? { cameraEnabled: options.cameraEnabled } : {}),
      ...(options.muted !== undefined ? { muted: options.muted } : {}),
    });
  }
  startConference(requestId: string, groupId: string, invitees: string[], options: NativeCallDialOptions = {}): Promise<CallResult> {
    return this.command({
      type: "call.dial",
      requestId,
      target: { kind: "conference", groupId, invitees },
      callType: options.callType ?? "voice",
      ...(options.cameraEnabled !== undefined ? { cameraEnabled: options.cameraEnabled } : {}),
      ...(options.muted !== undefined ? { muted: options.muted } : {}),
    });
  }
  joinConference(requestId: string, invitationId: string, invitationRevision: number, options: NativeCallJoinOptions = {}): Promise<CallResult> {
    return this.command({
      type: "call.join",
      requestId,
      invitationId,
      invitationRevision,
      ...(options.cameraEnabled !== undefined ? { cameraEnabled: options.cameraEnabled } : {}),
      ...(options.muted !== undefined ? { muted: options.muted } : {}),
    });
  }
  invite(requestId: string, callId: string, contactIds: string[]): Promise<CallResult> { return this.command({ type: "call.invite", requestId, callId, contactIds }); }
  answer(requestId: string, callId: string, expectedRevision: number, options: NativeCallAnswerOptions = {}): Promise<CallResult> {
    return this.command({
      type: "call.answer",
      requestId,
      callId,
      expectedRevision,
      ...(options.cameraEnabled !== undefined ? { cameraEnabled: options.cameraEnabled } : {}),
      ...(options.muted !== undefined ? { muted: options.muted } : {}),
    });
  }
  setMute(requestId: string, callId: string, muted: boolean): Promise<CallResult> {
    return this.command({ type: "call.mute", requestId, callId, muted });
  }
  setCamera(requestId: string, callId: string, enabled: boolean): Promise<CallResult> {
    return this.command({ type: "call.camera", requestId, callId, enabled });
  }
  /** Acceptance starts termination. Retain control and media transports until
   * the terminal call.state event; a lost confirmation leaves an unknown outcome. */
  end(type: "call.reject" | "call.cancel" | "call.hangup", requestId: string, callId: string, expectedRevision: number): Promise<CallResult> { return this.command({ type, requestId, callId, expectedRevision }); }
  clearAudio(requestId: string, callId: string): Promise<CallResult> { return this.command({ type: "call.clear_audio", requestId, callId }); }
  async attachMedia(callId: string, observePcm?: NativeCallPcmObserver): Promise<NativeCallMediaClient> {
    if (!this.hello || this.closed || this.owner?.callId !== callId) throw new NativeCallError("not_owner");
    if (this.media.has(callId)) throw new NativeCallError("media_already_attached");
    const attachment: CallMediaCommand = { type: "media.attach", supportedProtocolVersions: [NATIVE_CALL_PROTOCOL_VERSION], clientId: this.hello.clientId, ownerToken: this.owner.token };
    const media = new NativeCallMediaClient(this.options, callId, attachment, observePcm);
    this.media.set(callId, media);
    try { await media.connect(); return media; } catch (error) { this.media.delete(callId); throw error; }
  }
  private dropMedia(callId: string): void { this.media.get(callId)?.close(); this.media.delete(callId); if (this.owner?.callId === callId) this.owner = undefined; }
  private fail(error: NativeCallError): void {
    if (this.closed) return;
    this.closed = true; clearTimeout(this.handshakeTimer); this.openReject?.(error); this.openReject = undefined;
    for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(new NativeCallError(error.code, true)); }
    this.pending.clear(); for (const media of this.media.values()) media.close(); this.media.clear(); this.owner = undefined;
    this.queue.close(error);
    try { this.socket?.close(1000, "call client stopped"); } catch { /* Local cleanup is independent of a remote close acknowledgement. */ }
  }
  close(): void { this.fail(new NativeCallError("closed")); }
}

/** Owner-only binary WXCM v3 media socket. connect resolves only after the server's
 * media.ready acknowledgement; opening TCP/WebSocket alone does not establish attachment. */
export class NativeCallMediaClient {
  private readonly options: NativeCallsClientOptions;
  private readonly callId: string;
  private readonly attachment: CallMediaCommand;
  private readonly observePcm?: NativeCallPcmObserver;
  private readonly queue = new EventQueue<NativeCallMediaFrame>(16);
  private readonly videoQueue = new EventQueue<NativeCallVideoFrame>(8);
  private readonly keyframeQueue = new EventQueue<NativeCallKeyframeRequest>(8);
  private readonly sampleOffsets = new Map<number, number>();
  private socket?: NativeCallSocket;
  private opening?: Promise<MediaReady>;
  private openResolve?: (ready: MediaReady) => void;
  private openReject?: (error: Error) => void;
  private timer?: ReturnType<typeof setTimeout>;
  private readyEvent?: MediaReady;
  private sendGeneration?: number;
  private videoSendGeneration?: number;
  private sendSequence = 0;
  private videoSendSequence = 0;
  private ending = false;
  private closed = false;
  constructor(options: NativeCallsClientOptions, callId: string, attachment: CallMediaCommand, observePcm?: NativeCallPcmObserver) {
    const url = validateNativeBaseUrl(options.baseUrl); resourceId(callId);
    const env = (globalThis as typeof globalThis & { process?: { env?: Record<string, string | undefined> } }).process?.env;
    if (url.protocol === "https:" && env?.NODE_TLS_REJECT_UNAUTHORIZED === "0") throw new Error("Call API requires TLS verification");
    validateSocketAuthOptions(options);
    this.options = options; this.callId = callId; this.attachment = attachment; this.observePcm = observePcm;
  }
  connect(): Promise<MediaReady> {
    if (this.closed) return Promise.reject(new NativeCallError("closed"));
    if (this.opening) return this.opening;
    this.opening = new Promise((resolve, reject) => { this.openResolve = resolve; this.openReject = reject; });
    this.timer = setTimeout(() => this.fail(new NativeCallError("media_handshake_timeout")), this.options.handshakeTimeoutMs ?? 10_000);
    const suffix = `${resourceId(this.callId)}/media`;
    const openSocket = (target: { url: string; headers: Record<string, string> }) => {
      if (this.closed) return;
      try {
        const socket = this.options.webSocketFactory(target.url, { headers: target.headers, maxPayload: MEDIA_BINARY_BYTES });
        this.socket = socket; socket.binaryType = "arraybuffer";
        socket.onopen = () => { try { socket.send(JSON.stringify(this.attachment)); } catch { this.fail(new NativeCallError("media_attach_failed")); } };
        socket.onmessage = event => { try { this.receive(event.data); } catch (error) { this.fail(error instanceof NativeCallError ? error : new NativeCallError("invalid_media")); } };
        socket.onerror = () => this.fail(new NativeCallError("media_connection_failed"));
        socket.onclose = () => this.fail(new NativeCallError("media_closed"));
      } catch { this.fail(new NativeCallError("media_connection_failed")); }
    };
    if (this.options.ticketProvider) {
      void resolveSocketTarget(this.options, "media", suffix, this.callId)
        .then(openSocket, error => this.fail(error instanceof NativeCallError ? error : new NativeCallError("media_connection_failed")));
    } else {
      openSocket({
        url: callSocketUrl(this.options.baseUrl, this.options.accountId, suffix),
        headers: { Authorization: `Bearer ${this.options.token}` },
      });
    }
    return this.opening;
  }
  frames(): AsyncIterable<NativeCallMediaFrame> { return this.queue; }
  videoFrames(): AsyncIterable<NativeCallVideoFrame> { return this.videoQueue; }
  keyframeRequests(): AsyncIterable<NativeCallKeyframeRequest> { return this.keyframeQueue; }
  private audioSendTrack(): CallAudioTrack {
    const sendTrack = this.readyEvent?.sendTracks.find((track): track is CallAudioTrack => track.media === "audio");
    if (!sendTrack) throw new NativeCallError("media_not_ready");
    return sendTrack;
  }
  private videoSendTrack(): CallVideoTrack | null {
    return this.readyEvent?.sendTracks.find((track): track is CallVideoTrack => track.media === "video") ?? null;
  }
  tracks(): { sendTrack: CallAudioTrack; receiveTracks: CallAudioTrack[] } {
    if (!this.readyEvent) throw new NativeCallError("media_not_ready");
    const sendTrack = this.audioSendTrack();
    const receiveTracks = this.readyEvent.receiveTracks.filter((track): track is CallAudioTrack => track.media === "audio");
    return { sendTrack: structuredClone(sendTrack), receiveTracks: structuredClone(receiveTracks) };
  }
  videoTracks(): { sendTrack: CallVideoTrack | null; receiveTracks: CallVideoTrack[] } {
    if (!this.readyEvent) throw new NativeCallError("media_not_ready");
    const sendTrack = this.videoSendTrack();
    const receiveTracks = this.readyEvent.receiveTracks.filter((track): track is CallVideoTrack => track.media === "video");
    return { sendTrack: sendTrack ? structuredClone(sendTrack) : null, receiveTracks: structuredClone(receiveTracks) };
  }
  private receive(data: unknown): void {
    if (this.closed) return;
    if (typeof data === "string") {
      const event = parseCallEvent(data, CONTROL_BYTES);
      if (!event) {
        if (!this.readyEvent) throw new Error("Unexpected media event");
        return;
      }
      if (event.type === "call.error") { this.fail(new NativeCallError(event.code)); return; }
      if (event.type === "track.removed") {
        if (event.callId !== this.callId || !this.readyEvent) throw new Error("Wrong media call");
        const track = this.readyEvent.receiveTracks.find(track => track.id === event.trackId);
        if (!track || track.generation !== event.generation) throw new Error("Invalid removed track");
        this.readyEvent.receiveTracks = this.readyEvent.receiveTracks.filter(track => track.id !== event.trackId);
        this.queue.retain(frame => frame.trackId !== event.trackId);
        this.videoQueue.retain(frame => frame.trackId !== event.trackId);
        this.sampleOffsets.delete(event.trackId);
        return;
      }
      if (event.type === "track.updated" || event.type === "track.added") { if (event.callId !== this.callId) throw new Error("Wrong media call"); this.updateTrack(event.track); return; }
      if (event.type === "video.keyframe_requested") {
        if (event.callId !== this.callId || !this.readyEvent) throw new Error("Wrong media call");
        this.keyframeQueue.pushLatest({ callId: event.callId, trackId: event.trackId, generation: event.generation });
        return;
      }
      if (event.type !== "media.ready" || this.readyEvent) throw new Error("Unexpected media event");
      this.readyEvent = structuredClone(event);
      clearTimeout(this.timer); this.openResolve?.(structuredClone(event)); this.openResolve = undefined; this.openReject = undefined; return;
    }
    const raw = toBytes(data);
    if (!raw || !this.readyEvent) throw new Error("Invalid binary media");
    const decoded = decodeWxcm(raw);
    if (decoded.kind !== WXCM_PAYLOAD_PCM_S16LE) {
      const codec = payloadKindToVideoCodec(decoded.kind);
      if (!codec) throw new NativeCallError("invalid_media");
      const track = this.readyEvent.receiveTracks.find((item): item is CallVideoTrack => item.media === "video" && item.id === decoded.trackId);
      if (!track || decoded.generation === 0 || decoded.generation > track.generation || track.format.codec !== codec) throw new NativeCallError("invalid_media");
      if (this.ending || decoded.generation < track.generation) return;
      this.videoQueue.pushLatest({
        trackId: track.id,
        generation: track.generation,
        codec,
        flags: decoded.flags,
        keyframe: (decoded.flags & NATIVE_CALL_VIDEO_FLAG_KEYFRAME) !== 0,
        codecConfig: (decoded.flags & NATIVE_CALL_VIDEO_FLAG_CODEC_CONFIG) !== 0,
        rotationDegrees: videoRotationFromFlags(decoded.flags),
        sequence: decoded.sequence,
        timestampMicros: decoded.timestampMicros,
        payload: decoded.payload,
      });
      return;
    }
    const track = this.readyEvent.receiveTracks.find((item): item is CallAudioTrack => item.media === "audio" && item.id === decoded.trackId);
    if (!track || decoded.generation === 0 || decoded.generation > track.generation) throw new Error("Invalid media track");
    const bytes = decoded.payload;
    const sampleBytes = track.format.channels * 2;
    if (bytes.byteLength === 0 || bytes.byteLength % sampleBytes !== 0) throw new Error("Invalid PCM samples");
    if (this.ending || decoded.generation < track.generation) return;
    const samples = bytes.byteLength / sampleBytes;
    const offset = Math.round(decoded.timestampMicros * track.format.sampleRate / 1_000_000);
    const frame: NativeCallMediaFrame = { trackId: track.id, generation: track.generation, payload: bytes, sampleOffset: offset, sequence: decoded.sequence };
    this.sampleOffsets.set(track.id, offset + samples);
    if (this.observePcm) frame.receivedAtMs = performance.now();
    // A late consumer needs current PCM. Control events still fail on overflow.
    const dropped = this.queue.pushLatest(frame);
    if (this.observePcm && frame.receivedAtMs !== undefined) {
      try {
        const droppedTrack = dropped ? this.readyEvent.receiveTracks.find((item): item is CallAudioTrack => item.media === "audio" && item.id === dropped.trackId) : undefined;
        this.observePcm({ receivedAtMs: frame.receivedAtMs, samples, queueDepth: this.queue.depth,
          droppedSamples: dropped ? dropped.payload.byteLength / ((droppedTrack?.format.channels ?? track.format.channels) * 2) : 0 });
      } catch { /* Diagnostics must not detach the media owner. */ }
    }
  }
  /** Called from authenticated control track updates as well as this socket. */
  updateTrack(track: CallTrack): void {
    if (!this.readyEvent || this.closed) return;
    if (!trackValue(track)) { this.fail(new NativeCallError("invalid_track_update")); return; }
    const list = track.direction === "send" ? this.readyEvent.sendTracks : this.readyEvent.receiveTracks;
    const prior = list.find(item => item.id === track.id);
    if (!prior) {
      const other = track.direction === "send" ? this.readyEvent.receiveTracks : this.readyEvent.sendTracks;
      if (list.length >= 256 || other.some(item => item.id === track.id)) { this.fail(new NativeCallError("invalid_track_update")); return; }
      list.push(structuredClone(track));
      return;
    }
    if (track.media !== prior.media || track.id !== prior.id || track.generation < prior.generation) { this.fail(new NativeCallError("invalid_track_update")); return; }
    if (track.media === "audio" && prior.media === "audio" && JSON.stringify(track.format) !== JSON.stringify(prior.format)) { this.fail(new NativeCallError("invalid_track_update")); return; }
    if (track.generation === prior.generation) {
      if (track.media === "video" && prior.media === "video" && track.active !== prior.active) {
        Object.assign(prior, structuredClone(track));
      }
      return;
    }
    if (track.direction === "receive") {
      this.queue.retain(frame => frame.trackId !== track.id);
      this.videoQueue.retain(frame => frame.trackId !== track.id);
    }
    Object.assign(prior, structuredClone(track));
  }

  /** Stop audio and video immediately without detaching the owner before termination. */
  stopAudio(): void { this.ending = true; this.queue.clear(); this.videoQueue.clear(); }
  /** Send PCM chunks as binary WXCM v3 frames, with the generation captured for
   * this producer. The daemon buffers, frames and clocks the samples. An old
   * producer cannot silently adopt the new generation after clearAudio. */
  sendPcm(samples: Int16Array, generation: number): void {
    if (this.closed || !this.readyEvent || this.socket?.readyState !== 1) throw new NativeCallError("media_not_ready");
    if (this.ending) throw new NativeCallError("call_ending");
    const sendTrack = this.audioSendTrack();
    if (generation !== sendTrack.generation) throw new NativeCallError("stale");
    if (!(samples instanceof Int16Array) || samples.length === 0 || samples.length % sendTrack.format.channels !== 0 || samples.byteLength > NATIVE_CALL_MAX_FRAME_BYTES) throw new NativeCallError("invalid_pcm");
    const start = this.sendGeneration === generation ? undefined : JSON.stringify({ type: "track.start", trackId: sendTrack.id, generation } satisfies CallMediaCommand);
    const bytes = new Uint8Array(samples.byteLength); const view = new DataView(bytes.buffer);
    for (let i = 0; i < samples.length; i++) view.setInt16(i * 2, samples[i], true);
    const packet = encodeWxcm(WXCM_PAYLOAD_PCM_S16LE, 0, sendTrack.id, generation, this.sendSequence, 0, bytes);
    const limit = Math.max(this.options.maxBufferedBytes ?? 65536, WXCM_HEADER_BYTES + NATIVE_CALL_MAX_FRAME_BYTES + 128);
    if (this.socket.bufferedAmount + packet.byteLength + (start?.length ?? 0) > limit) throw new NativeCallError("media_backpressure");
    try {
      if (start) { this.socket.send(start); this.sendGeneration = generation; }
      this.socket.send(packet);
      this.sendSequence += 1;
    } catch { this.fail(new NativeCallError("media_send_failed", true)); throw new NativeCallError("media_send_failed", true); }
  }
  /** Send one encoded video frame (Annex-B H.265/H.264 or JPEG) on the negotiated
   * video send track for the captured generation. */
  sendVideo(payload: Uint8Array, generation: number, options: NativeCallVideoSendOptions = {}): void {
    if (this.closed || !this.readyEvent || this.socket?.readyState !== 1) throw new NativeCallError("media_not_ready");
    if (this.ending) throw new NativeCallError("call_ending");
    const sendTrack = this.videoSendTrack();
    if (!sendTrack) throw new NativeCallError("unsupported");
    if (generation !== sendTrack.generation) throw new NativeCallError("stale");
    if (!(payload instanceof Uint8Array) || payload.byteLength === 0 || payload.byteLength > NATIVE_CALL_MAX_VIDEO_FRAME_BYTES) throw new NativeCallError("invalid_media");
    const codec = options.codec ?? sendTrack.format.codec;
    if (codec !== sendTrack.format.codec) throw new NativeCallError("invalid_media");
    const flags =
      (options.keyframe ? NATIVE_CALL_VIDEO_FLAG_KEYFRAME : 0) |
      (options.codecConfig ? NATIVE_CALL_VIDEO_FLAG_CODEC_CONFIG : 0) |
      videoRotationFlags(options.rotationDegrees);
    const timestampMicros = options.timestampMicros ?? 0;
    if (!Number.isSafeInteger(timestampMicros) || timestampMicros < 0) throw new NativeCallError("invalid_media");
    const start = this.videoSendGeneration === generation ? undefined : JSON.stringify({ type: "track.start", trackId: sendTrack.id, generation } satisfies CallMediaCommand);
    const packet = encodeWxcm(videoPayloadKind(codec), flags, sendTrack.id, generation, this.videoSendSequence, timestampMicros, payload);
    const limit = Math.max(this.options.maxBufferedBytes ?? 65536, MEDIA_BINARY_BYTES + 128);
    if (this.socket.bufferedAmount + packet.byteLength + (start?.length ?? 0) > limit) throw new NativeCallError("media_backpressure");
    try {
      if (start) { this.socket.send(start); this.videoSendGeneration = generation; }
      this.socket.send(packet);
      this.videoSendSequence += 1;
    } catch { this.fail(new NativeCallError("media_send_failed", true)); throw new NativeCallError("media_send_failed", true); }
  }
  /** Ask the remote sender of a receive video track to emit a fresh keyframe. */
  requestKeyframe(trackId: number): void {
    if (this.closed || !this.readyEvent || this.socket?.readyState !== 1) throw new NativeCallError("media_not_ready");
    if (!uint32(trackId) || trackId === 0) throw new NativeCallError("invalid_request");
    const message = JSON.stringify({ type: "video.request_keyframe", trackId } satisfies CallMediaCommand);
    try { this.socket.send(message); } catch { this.fail(new NativeCallError("media_send_failed", true)); throw new NativeCallError("media_send_failed", true); }
  }
  private fail(error: NativeCallError): void {
    if (this.closed) return;
    this.closed = true; clearTimeout(this.timer); this.openReject?.(error); this.openReject = undefined;
    this.queue.close(error); this.videoQueue.close(error); this.keyframeQueue.close(error);
    try { this.socket?.close(1000, "call media stopped"); } catch { /* Stop locally even if the transport has failed. */ }
  }
  close(): void { this.fail(new NativeCallError("media_closed")); }
}
