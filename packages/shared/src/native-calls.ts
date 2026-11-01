/** Native call control and PCM media. Hosts supply a WebSocket implementation
 * that supports authenticated request headers; browser WebSocket alone cannot
 * provide this authentication flow. There are no device or codec dependencies. */
import { validateNativeBaseUrl } from "./native-client.ts";
import type { CallAudioFormat, CallAudioTrack, CallCommand, CallEvent, CallMediaCommand, CallSnapshot, CallReadyScope } from "./native.generated.js";
export type { CallAudioFormat, CallAudioTrack, CallCommand, CallEvent, CallSnapshot, CallPendingOffer, CallReadyScope } from "./native.generated.js";

export const NATIVE_CALL_MEDIA_HEADER_BYTES = 36;
export const NATIVE_CALL_MAX_FRAME_BYTES = 64 * 1024;
export type NativeCallPcmFormat = CallAudioFormat;
export type NativeCallMediaHeader = { trackId: number; generation: number; sequence: bigint; timestampMicros: bigint };
export type NativeCallMediaFrame = NativeCallMediaHeader & { payload: Uint8Array };

function pcmBytes(format: NativeCallPcmFormat): number {
  if (![8000, 12000, 16000, 24000, 48000].includes(format.sampleRate)
    || ![1, 2].includes(format.channels) || ![10, 20, 40, 60].includes(format.frameMs)) throw new Error("Invalid call PCM format");
  return format.sampleRate * format.frameMs / 1000 * format.channels * 2;
}
function validMediaHeader(header: NativeCallMediaHeader): void {
  for (const value of [header.trackId, header.generation]) if (!Number.isInteger(value) || value < 1 || value > 0xffffffff) throw new Error("Invalid call media identity");
  for (const value of [header.sequence, header.timestampMicros]) if (typeof value !== "bigint" || value < 0n || value > 0xffffffffffffffffn) throw new Error("Invalid call media counter");
}
/** Encode one interleaved PCM16 frame. Header numbers are BE; samples are LE. */
export function encodeNativeCallPcm(header: NativeCallMediaHeader, format: NativeCallPcmFormat, samples: Int16Array): Uint8Array {
  validMediaHeader(header);
  const length = pcmBytes(format);
  if (!(samples instanceof Int16Array) || samples.length * 2 !== length || length + 36 > NATIVE_CALL_MAX_FRAME_BYTES) throw new Error("Invalid call PCM frame");
  const bytes = new Uint8Array(36 + length); const view = new DataView(bytes.buffer);
  bytes.set([0x57, 0x58, 0x43, 0x4d, 1, 1, 0, 0]);
  view.setUint32(8, header.trackId); view.setUint32(12, header.generation);
  view.setBigUint64(16, header.sequence); view.setBigUint64(24, header.timestampMicros); view.setUint32(32, length);
  for (let i = 0; i < samples.length; i++) view.setInt16(36 + i * 2, samples[i], true);
  return bytes;
}
/** Decode without allocating or copying the payload. Retain/copy it only as long
 * as needed; the caller owns the backing input buffer. */
export function decodeNativeCallPcm(bytes: Uint8Array, format: NativeCallPcmFormat): NativeCallMediaFrame {
  if (!(bytes instanceof Uint8Array) || bytes.length < 36 || bytes.length > NATIVE_CALL_MAX_FRAME_BYTES) throw new Error("Invalid call media frame");
  if (bytes[0] !== 0x57 || bytes[1] !== 0x58 || bytes[2] !== 0x43 || bytes[3] !== 0x4d || bytes[4] !== 1 || bytes[5] !== 1 || bytes[6] !== 0 || bytes[7] !== 0) throw new Error("Invalid call media header");
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const length = view.getUint32(32);
  if (length !== bytes.length - 36 || length !== pcmBytes(format)) throw new Error("Invalid call PCM frame extent");
  const header = { trackId: view.getUint32(8), generation: view.getUint32(12), sequence: view.getBigUint64(16), timestampMicros: view.getBigUint64(24) };
  validMediaHeader(header);
  return { ...header, payload: bytes.subarray(36) };
}
export class NativeCallMediaStream {
  private last?: NativeCallMediaHeader;
  readonly trackId: number;
  private generation: number;
  constructor(trackId: number, generation: number) { this.trackId = trackId; this.generation = generation; validMediaHeader({ trackId, generation, sequence: 0n, timestampMicros: 0n }); }
  admit(frame: NativeCallMediaFrame): void {
    validMediaHeader(frame);
    if (frame.trackId !== this.trackId) throw new Error("Wrong call media track");
    if (frame.generation !== this.generation) throw new Error("Stale call media generation");
    if (this.last && (frame.sequence <= this.last.sequence || frame.timestampMicros <= this.last.timestampMicros)) throw new Error("Out-of-order call media frame");
    this.last = { trackId: frame.trackId, generation: frame.generation, sequence: frame.sequence, timestampMicros: frame.timestampMicros };
  }
  advanceGeneration(generation: number): void {
    validMediaHeader({ trackId: this.trackId, generation, sequence: 0n, timestampMicros: 0n });
    if (generation <= this.generation) throw new Error("Stale call media generation");
    this.generation = generation; this.last = undefined;
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
function callSocketUrl(baseUrl: string, accountId: string, suffix: string): string {
  const url = new URL(`v1/accounts/${resourceId(accountId)}/calls/${suffix}`, validateNativeBaseUrl(baseUrl));
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
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

function binaryBytes(value: unknown): Uint8Array | undefined {
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  return undefined;
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
function trackValue(value: unknown): value is CallAudioTrack {
  return object(value) && uint32(value.id) && value.id > 0 && uint32(value.generation) && value.generation > 0
    && ["send", "receive"].includes(String(value.direction)) && formatValue(value.format);
}
function availabilityValue(value: unknown): boolean { return object(value) && typeof value.busy === "boolean" && typeof value.ownedByThisClient === "boolean"; }
function callValue(value: unknown): boolean {
  return object(value) && idValue(value.callId) && uint32(value.revision)
    && ["direct", "conference"].includes(String(value.kind)) && ["incoming", "outgoing"].includes(String(value.direction))
    && ["offered", "dialing", "ringing", "connecting", "connected", "ending", "ended", "uncertain"].includes(String(value.state)) && typeof value.ownedByThisClient === "boolean";
}
function offerValue(value: unknown): boolean {
  return object(value) && idValue(value.callId) && uint32(value.revision) && idValue(value.callerId)
    && ["direct", "conference"].includes(String(value.kind)) && object(value.offeredMedia)
    && typeof value.offeredMedia.audio === "boolean" && typeof value.offeredMedia.video === "boolean";
}
function pendingOfferValue(value: unknown): boolean {
  return offerValue(value) && object(value) && ["waiting_for_ready", "preparing"].includes(String(value.phase));
}
function errorCode(value: unknown): boolean {
  return typeof value === "string" && ["invalid_request", "busy", "already_claimed", "offer_expired", "not_owner", "unsupported", "media_not_ready", "outcome_unknown", "capacity", "stale", "forbidden", "closed"].includes(value);
}
function parseCallEvent(data: unknown): CallEvent {
  if (typeof data !== "string" || new TextEncoder().encode(data).byteLength > CONTROL_BYTES) throw new NativeCallError("invalid_event");
  let value: unknown;
  try { value = JSON.parse(data); } catch { throw new NativeCallError("invalid_event"); }
  if (!object(value)) throw new NativeCallError("invalid_event");
  let valid = false;
  switch (value.type) {
    case "hello":
      if (value.currentPendingOffer === undefined) value.currentPendingOffer = null;
      valid = value.protocolVersion === 1 && idValue(value.clientId) && object(value.capabilities)
      && ["directAudio", "directVideo", "conferenceAudio", "conferenceVideo"].every(key => typeof (value.capabilities as Record<string, unknown>)[key] === "boolean")
      && availabilityValue(value.availability) && (value.currentOffer === null || offerValue(value.currentOffer))
      && (value.currentPendingOffer === null || pendingOfferValue(value.currentPendingOffer))
      && (value.currentCall === null || callValue(value.currentCall)) && formatValue(value.audioFormat); break;
    case "command.result": valid = idValue(value.requestId) && ["accepted", "replayed", "error"].includes(String(value.status))
      && (value.callId === null || idValue(value.callId)) && (value.ownerToken === null || idValue(value.ownerToken))
      && (value.code === null || errorCode(value.code)); break;
    case "call.offered": valid = offerValue(value.offer); break;
    case "call.pending": valid = value.offer === null || pendingOfferValue(value.offer); break;
    case "call.state": valid = callValue(value.call); break;
    case "calls.availability": valid = availabilityValue(value.availability); break;
    case "track.added": case "track.updated": valid = idValue(value.callId) && trackValue(value.track); break;
    case "media.ready": valid = trackValue(value.sendTrack) && trackValue(value.receiveTrack)
      && value.sendTrack.direction === "send" && value.receiveTrack.direction === "receive" && value.sendTrack.id !== value.receiveTrack.id; break;
    case "call.error": valid = errorCode(value.code); break;
  }
  if (!valid) throw new NativeCallError("invalid_event");
  return value as CallEvent;
}

export type NativeCallsClientOptions = {
  baseUrl: string; token: string; accountId: string; webSocketFactory: NativeCallWebSocketFactory;
  handshakeTimeoutMs?: number; commandTimeoutMs?: number; maxEvents?: number; maxBufferedBytes?: number;
};
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
    if (typeof options.token !== "string" || !options.token || /[\r\n]/.test(options.token)) throw new Error("Call API bearer token is required");
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
    try {
      const socket = this.options.webSocketFactory(callSocketUrl(this.options.baseUrl, this.options.accountId, "socket"), { headers: { Authorization: `Bearer ${this.options.token}` }, maxPayload: CONTROL_BYTES });
      this.socket = socket; socket.binaryType = "arraybuffer";
      socket.onmessage = event => { try { this.receive(parseCallEvent(event.data)); } catch { this.fail(new NativeCallError("invalid_event")); } };
      socket.onerror = () => this.fail(new NativeCallError("connection_failed", this.pending.size > 0));
      socket.onclose = () => this.fail(new NativeCallError("closed", this.pending.size > 0));
    } catch { this.fail(new NativeCallError("connection_failed")); }
    return this.opening;
  }
  events(): AsyncIterable<CallEvent> { return this.queue; }
  private receive(event: CallEvent): void {
    if (this.closed) return;
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
    if (event.type === "track.updated" || event.type === "track.added") this.media.get(event.callId)?.updateTrack(event.track);
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
  ready(requestId: string, ready: boolean, format = this.hello?.audioFormat, scope?: CallReadyScope): Promise<CallResult> {
    if (!format) return Promise.reject(new NativeCallError("not_connected"));
    pcmBytes(format); return this.command({ type: "call.ready", requestId, ready, format, ...(scope ? {scope} : {}) });
  }
  dial(requestId: string, contactId: string): Promise<CallResult> { return this.command({ type: "call.dial", requestId, target: { kind: "direct", contactId }, media: { audio: true, video: false } }); }
  answer(requestId: string, callId: string, expectedRevision: number): Promise<CallResult> { return this.command({ type: "call.answer", requestId, callId, expectedRevision, media: { audio: true, video: false } }); }
  /** Acceptance starts termination. Retain control and media transports until
   * the terminal call.state event; a lost confirmation leaves an unknown outcome. */
  end(type: "call.reject" | "call.cancel" | "call.hangup", requestId: string, callId: string, expectedRevision: number): Promise<CallResult> { return this.command({ type, requestId, callId, expectedRevision }); }
  clearAudio(requestId: string, callId: string): Promise<CallResult> { return this.command({ type: "call.clear_audio", requestId, callId }); }
  async attachMedia(callId: string): Promise<NativeCallMediaClient> {
    if (!this.hello || this.closed || this.owner?.callId !== callId) throw new NativeCallError("not_owner");
    if (this.media.has(callId)) throw new NativeCallError("media_already_attached");
    const attachment: CallMediaCommand = { type: "media.attach", clientId: this.hello.clientId, ownerToken: this.owner.token };
    const media = new NativeCallMediaClient(this.options, callId, attachment);
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

/** Owner-only PCM socket. connect resolves only after the server's media.ready
 * acknowledgement; opening TCP/WebSocket alone does not establish attachment. */
export class NativeCallMediaClient {
  private readonly options: NativeCallsClientOptions;
  private readonly callId: string;
  private readonly attachment: CallMediaCommand;
  private readonly queue = new EventQueue<NativeCallMediaFrame>(16);
  private socket?: NativeCallSocket;
  private opening?: Promise<MediaReady>;
  private openResolve?: (ready: MediaReady) => void;
  private openReject?: (error: Error) => void;
  private timer?: ReturnType<typeof setTimeout>;
  private readyEvent?: MediaReady;
  private sendGuard?: NativeCallMediaStream;
  private receiveGuard?: NativeCallMediaStream;
  private ending = false;
  private closed = false;
  constructor(options: NativeCallsClientOptions, callId: string, attachment: CallMediaCommand) {
    const url = validateNativeBaseUrl(options.baseUrl); resourceId(callId);
    const env = (globalThis as typeof globalThis & { process?: { env?: Record<string, string | undefined> } }).process?.env;
    if (url.protocol === "https:" && env?.NODE_TLS_REJECT_UNAUTHORIZED === "0") throw new Error("Call API requires TLS verification");
    if (typeof options.token !== "string" || !options.token || /[\r\n]/.test(options.token)) throw new Error("Call API bearer token is required");
    this.options = options; this.callId = callId; this.attachment = attachment;
  }
  connect(): Promise<MediaReady> {
    if (this.closed) return Promise.reject(new NativeCallError("closed"));
    if (this.opening) return this.opening;
    this.opening = new Promise((resolve, reject) => { this.openResolve = resolve; this.openReject = reject; });
    this.timer = setTimeout(() => this.fail(new NativeCallError("media_handshake_timeout")), this.options.handshakeTimeoutMs ?? 10_000);
    try {
      const socket = this.options.webSocketFactory(callSocketUrl(this.options.baseUrl, this.options.accountId, `${resourceId(this.callId)}/media`), { headers: { Authorization: `Bearer ${this.options.token}` }, maxPayload: NATIVE_CALL_MAX_FRAME_BYTES });
      this.socket = socket; socket.binaryType = "arraybuffer";
      socket.onopen = () => { try { socket.send(JSON.stringify(this.attachment)); } catch { this.fail(new NativeCallError("media_attach_failed")); } };
      socket.onmessage = event => { try { this.receive(event.data); } catch { this.fail(new NativeCallError("invalid_media")); } };
      socket.onerror = () => this.fail(new NativeCallError("media_connection_failed"));
      socket.onclose = () => this.fail(new NativeCallError("media_closed"));
    } catch { this.fail(new NativeCallError("media_connection_failed")); }
    return this.opening;
  }
  frames(): AsyncIterable<NativeCallMediaFrame> { return this.queue; }
  tracks(): { sendTrack: CallAudioTrack; receiveTrack: CallAudioTrack } {
    if (!this.readyEvent) throw new NativeCallError("media_not_ready");
    return { sendTrack: structuredClone(this.readyEvent.sendTrack), receiveTrack: structuredClone(this.readyEvent.receiveTrack) };
  }
  private receive(data: unknown): void {
    if (this.closed) return;
    if (typeof data === "string") {
      const event = parseCallEvent(data);
      if (event.type === "call.error") { this.fail(new NativeCallError(event.code)); return; }
      if (event.type === "track.updated" || event.type === "track.added") { if (event.callId !== this.callId) throw new Error("Wrong media call"); this.updateTrack(event.track); return; }
      if (event.type !== "media.ready" || this.readyEvent) throw new Error("Unexpected media event");
      this.readyEvent = structuredClone(event); this.sendGuard = new NativeCallMediaStream(event.sendTrack.id, event.sendTrack.generation); this.receiveGuard = new NativeCallMediaStream(event.receiveTrack.id, event.receiveTrack.generation);
      clearTimeout(this.timer); this.openResolve?.(structuredClone(event)); this.openResolve = undefined; this.openReject = undefined; return;
    }
    const bytes = binaryBytes(data);
    if (!bytes || !this.readyEvent || !this.receiveGuard) throw new Error("Unattached media");
    if (this.ending) return;
    const frame = decodeNativeCallPcm(bytes, this.readyEvent.receiveTrack.format);
    this.receiveGuard.admit(frame);
    if (!this.queue.push(frame)) this.fail(new NativeCallError("media_backpressure"));
  }
  /** Called from authenticated control track updates as well as this socket. */
  updateTrack(track: CallAudioTrack): void {
    if (!this.readyEvent || this.closed) return;
    const key = track.direction === "send" ? "sendTrack" : "receiveTrack";
    const prior = this.readyEvent[key];
    if (track.id !== prior.id || track.generation < prior.generation || JSON.stringify(track.format) !== JSON.stringify(prior.format)) { this.fail(new NativeCallError("invalid_track_update")); return; }
    if (track.generation === prior.generation) return;
    const guard = track.direction === "send" ? this.sendGuard : this.receiveGuard;
    guard?.advanceGeneration(track.generation);
    if (track.direction === "receive") this.queue.clear();
    this.readyEvent[key] = structuredClone(track);
  }
  /** Stop audio immediately without detaching the owner before termination. */
  stopAudio(): void { this.ending = true; this.queue.clear(); }
  sendPcm(header: NativeCallMediaHeader, samples: Int16Array): void {
    if (this.closed || !this.readyEvent || !this.sendGuard || this.socket?.readyState !== 1) throw new NativeCallError("media_not_ready");
    if (this.ending) throw new NativeCallError("call_ending");
    const bytes = encodeNativeCallPcm(header, this.readyEvent.sendTrack.format, samples);
    if (this.socket.bufferedAmount + bytes.byteLength > (this.options.maxBufferedBytes ?? 65536)) throw new NativeCallError("media_backpressure");
    const frame = decodeNativeCallPcm(bytes, this.readyEvent.sendTrack.format);
    this.sendGuard.admit(frame);
    try { this.socket.send(bytes); } catch { this.fail(new NativeCallError("media_send_failed", true)); throw new NativeCallError("media_send_failed", true); }
  }
  private fail(error: NativeCallError): void {
    if (this.closed) return;
    this.closed = true; clearTimeout(this.timer); this.openReject?.(error); this.openReject = undefined; this.queue.close(error);
    try { this.socket?.close(1000, "call media stopped"); } catch { /* Stop locally even if the transport has failed. */ }
  }
  close(): void { this.fail(new NativeCallError("media_closed")); }
}
