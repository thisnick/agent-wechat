import type { CallAudioFormat, NativeCallMediaClient } from "@agent-wechat/shared";
import type { CallAudioTrace } from "./calls-audio-trace.ts";
import type { PreparedCallVoice, StreamingPcmResampler } from "./calls-voice.ts";

const MAX_BACKLOG_MS = 8_000;
// A continuous stream plays as it arrives; a longer backlog only adds delay.
const MAX_CONTINUOUS_BACKLOG_MS = 3_000;
const LOOK_AHEAD_FRAMES = 2;
const MAX_QUEUED_SPANS = 256;
type AudioSpan = { itemId: string; bytes: number };
type PlayoutSpan = AudioSpan & { startsAt: number; endsAt: number };

/** Clocked call output with silence in the slots the provider leaves empty.
 * Response mode tracks provider reply items so a clear can reject their late
 * chunks. Continuous mode plays one unlabeled stream, keeps its newest audio
 * when the backlog grows, and accepts new audio after each clear. */
export class CallPcmOutput {
  private readonly queue: Buffer;
  private readOffset = 0;
  private pendingBytes = 0;
  private readonly bytesPerMs: number;
  private readonly frameBytes: number;
  private started = false;
  private timer?: ReturnType<typeof setTimeout>;
  private submittedUntilMs = 0;
  private generation?: number;
  private resampler: StreamingPcmResampler;
  private resamplerNeedsReset = false;
  private readonly items = new Set<string>();
  private readonly revokedItems = new Set<string>();
  private readonly truncatedItems = new Set<string>();
  private readonly queuedSpans: AudioSpan[] = [];
  private readonly scheduledSpans: PlayoutSpan[] = [];
  private readonly playedBytes = new Map<string, number>();
  private closed = false;
  private paused = false;
  private clearEpoch = 0;
  private readonly media: Pick<NativeCallMediaClient, "sendPcm" | "tracks">;
  private readonly format: CallAudioFormat;
  private readonly voice: PreparedCallVoice;
  private readonly clearRemote: () => Promise<unknown>;
  private readonly failed: () => void;
  private readonly trace?: CallAudioTrace;
  private readonly continuous: boolean;
  constructor(media: Pick<NativeCallMediaClient, "sendPcm" | "tracks">, format: CallAudioFormat,
    voice: PreparedCallVoice, clearRemote: () => Promise<unknown>, failed: () => void, trace?: CallAudioTrace,
    options?: { continuous?: boolean }) {
    if (format.channels !== 1 || format.frameMs !== 20 || ![8000, 12000, 16000, 24000, 48000].includes(format.sampleRate)) throw Error("Calling PCM format unsupported");
    this.media = media;
    this.format = format;
    this.voice = voice;
    this.clearRemote = clearRemote;
    this.failed = failed;
    this.trace = trace;
    this.continuous = options?.continuous === true;
    this.bytesPerMs = format.sampleRate * 2 / 1000;
    this.frameBytes = this.bytesPerMs * format.frameMs;
    this.queue = Buffer.alloc(this.bytesPerMs * MAX_BACKLOG_MS);
    this.resampler = voice.resampler(voice.sampleRate, format.sampleRate);
  }
  start(): void {
    if (this.started || this.closed) return;
    this.started = true;
    this.flush();
  }
  playbackState(): Array<{ itemId: string; audioEndMs: number }> {
    const now = performance.now();
    this.settlePlayback(now);
    const items = new Map<string, number>();
    for (const { itemId } of this.queuedSpans) items.set(itemId, this.playedBytes.get(itemId) ?? 0);
    for (const span of this.scheduledSpans) {
      const elapsed = Math.max(0, now - span.startsAt);
      items.set(span.itemId, (items.get(span.itemId) ?? this.playedBytes.get(span.itemId) ?? 0)
        + Math.min(span.bytes, elapsed * this.bytesPerMs));
    }
    return [...items].map(([itemId, bytes]) => ({ itemId, audioEndMs: Math.floor(bytes / this.bytesPerMs) }));
  }
  enqueue(audio: Buffer, metadata?: { itemId: string }): void {
    if (this.closed) return;
    const epoch = this.clearEpoch;
    const item = this.continuous ? `stream-${epoch}` : metadata?.itemId;
    const started = this.trace ? performance.now() : 0;
    this.trace?.add("providerOutputChunks");
    this.trace?.add("providerOutputSamples", audio.length / 2);
    this.trace?.gap("providerOutputGapMs", started);
    this.trace?.measure("providerOutputChunkMs", audio.length * 500 / this.voice.sampleRate);
    // Item identity lets clear reject late chunks from an interrupted response.
    if (!item || item.length > 200 || audio.length % 2 !== 0 || audio.length > this.voice.sampleRate * 4) return this.fail();
    if (this.revokedItems.has(item)) { this.trace?.add("outputStaleChunks"); return; }
    if (this.truncatedItems.has(item)) { this.trace?.add("outputOverflowProviderSamples", audio.length / 2); return; }
    if (!this.items.has(item) && this.items.size >= 256) return this.fail();
    this.items.add(item);
    let converted: Buffer;
    try {
      if (this.resamplerNeedsReset && !this.resetResampler(epoch)) return;
      converted = this.resampler.process(audio);
    } catch { if (epoch === this.clearEpoch) this.fail(); return; }
    this.trace?.measure("outputResampleMs", performance.now() - started);
    // A synchronous provider/resampler callback may interrupt this conversion.
    // Its output still belongs to the old resampler, even after clear returns.
    if (this.closed || epoch !== this.clearEpoch || this.revokedItems.has(item)) {
      this.trace?.add("outputStaleChunks");
      return;
    }
    if (converted.length % 2 !== 0) return this.fail();
    if (this.continuous) {
      const limit = this.bytesPerMs * MAX_CONTINUOUS_BACKLOG_MS;
      if (converted.length > limit) {
        this.trace?.add("outputOverflowSamples", (converted.length - limit) / 2);
        converted = converted.subarray(converted.length - limit);
      }
      const excess = this.pendingBytes + converted.length - limit;
      if (excess > 0) this.dropOldest(excess);
    }
    const lastSpan = this.queuedSpans.at(-1);
    const spanAvailable = lastSpan?.itemId === item || this.queuedSpans.length < MAX_QUEUED_SPANS;
    const accepted = spanAvailable ? Math.min(converted.length, this.queue.length - this.pendingBytes) : 0;
    if (accepted < converted.length) {
      // Keep the coherent prefix. Future chunks from this item cannot follow a cut.
      this.truncatedItems.add(item);
      this.trace?.add("outputTruncatedItems");
      this.trace?.add("outputOverflowSamples", (converted.length - accepted) / 2);
      converted = converted.subarray(0, accepted);
      this.resamplerNeedsReset = true;
    }
    const tail = (this.readOffset + this.pendingBytes) % this.queue.length;
    const first = Math.min(converted.length, this.queue.length - tail);
    converted.copy(this.queue, tail, 0, first);
    if (first < converted.length) converted.copy(this.queue, 0, first);
    this.pendingBytes += converted.length;
    if (converted.length) {
      if (lastSpan?.itemId === item) lastSpan.bytes += converted.length;
      else this.queuedSpans.push({ itemId: item, bytes: converted.length });
    }
    this.trace?.measure("outputQueuedAudioMs", this.pendingBytes / this.bytesPerMs);
    // New response items remain bounded in pending while the daemon clear is
    // outstanding. Only its current acknowledgement can resume playout.
    if (!this.paused && !this.timer) this.flush();
  }
  async clear(): Promise<void> {
    if (this.closed) return;
    const epoch = ++this.clearEpoch;
    this.paused = true;
    this.discard();
    for (const item of this.items) this.revokedItems.add(item);
    this.truncatedItems.clear();
    // Continuous stream labels change with each clear, so the epoch check
    // already rejects audio from before it.
    if (this.continuous) { this.items.clear(); this.revokedItems.clear(); }
    try {
      if (!this.resetResampler(epoch)) return;
      await this.clearRemote();
      if (!this.closed && epoch === this.clearEpoch) {
        this.paused = false;
        this.flush();
      }
    } catch { if (epoch === this.clearEpoch) this.fail(); }
  }
  private resetResampler(epoch: number): boolean {
    const resampler = this.voice.resampler(this.voice.sampleRate, this.format.sampleRate);
    if (this.closed || epoch !== this.clearEpoch) return false;
    this.resampler = resampler;
    this.resamplerNeedsReset = false;
    return true;
  }
  private settlePlayback(now: number): void {
    while (this.scheduledSpans.length && this.scheduledSpans[0].endsAt <= now) {
      const span = this.scheduledSpans.shift()!;
      this.playedBytes.set(span.itemId, (this.playedBytes.get(span.itemId) ?? 0) + span.bytes);
    }
  }
  private schedulePlayback(bytes: number, startsAt: number): void {
    while (bytes > 0) {
      const span = this.queuedSpans[0];
      const used = Math.min(bytes, span.bytes);
      const endsAt = startsAt + used / this.bytesPerMs;
      this.scheduledSpans.push({ itemId: span.itemId, bytes: used, startsAt, endsAt });
      span.bytes -= used;
      if (!span.bytes) this.queuedSpans.shift();
      bytes -= used;
      startsAt = endsAt;
    }
  }
  private dropOldest(bytes: number): void {
    this.readOffset = (this.readOffset + bytes) % this.queue.length;
    this.pendingBytes -= bytes;
    this.trace?.add("outputOverflowSamples", bytes / 2);
    while (bytes > 0 && this.queuedSpans.length) {
      const span = this.queuedSpans[0];
      const used = Math.min(bytes, span.bytes);
      span.bytes -= used;
      bytes -= used;
      if (!span.bytes) this.queuedSpans.shift();
    }
  }
  private flush(): void {
    if (!this.started || this.closed || this.paused) return;
    const now = performance.now();
    const lead = Math.max(0, this.submittedUntilMs - now);
    // Limit our lead even after a timer stall. The daemon owns the packet clock.
    const available = Math.floor((LOOK_AHEAD_FRAMES * this.format.frameMs - lead) / this.format.frameMs);
    if (available <= 0) { this.schedule(); return; }
    const epoch = this.clearEpoch;
    try {
      const generation = this.generation ?? this.media.tracks().sendTrack.generation;
      if (this.closed || this.clearEpoch !== epoch) return;
      this.generation = generation;
      const slotBytes = available * this.frameBytes;
      const bytes = Math.min(this.pendingBytes, slotBytes);
      const samples = new Int16Array(slotBytes / 2);
      for (let i = 0; i < bytes / 2; i++) samples[i] = this.queue.readInt16LE((this.readOffset + i * 2) % this.queue.length);
      this.readOffset = (this.readOffset + bytes) % this.queue.length;
      this.pendingBytes -= bytes;
      const started = performance.now();
      const playoutStart = Math.max(started, this.submittedUntilMs);
      this.settlePlayback(started);
      this.schedulePlayback(bytes, playoutStart);
      this.submittedUntilMs = playoutStart + slotBytes / this.bytesPerMs;
      const leadAfterSubmit = this.submittedUntilMs - started;
      this.media.sendPcm(samples, generation);
      this.trace?.add("daemonOutputChunks");
      this.trace?.add("daemonOutputSamples", samples.length);
      this.trace?.add("outputSilenceSamples", (slotBytes - bytes) / 2);
      this.trace?.gap("daemonOutputGapMs", started);
      this.trace?.measure("daemonForwardMs", performance.now() - started);
      this.trace?.measure("outputLeadMs", leadAfterSubmit);
      if (!this.closed && this.clearEpoch === epoch && !this.timer) this.schedule();
    } catch { if (this.clearEpoch === epoch) this.fail(); }
  }
  private schedule(): void {
    const delay = this.submittedUntilMs - performance.now() - (LOOK_AHEAD_FRAMES - 1) * this.format.frameMs;
    this.timer = setTimeout(() => { this.timer = undefined; this.flush(); }, Math.max(1, Math.ceil(delay)));
    this.timer.unref();
  }
  private discard(): void {
    clearTimeout(this.timer);
    this.timer = undefined;
    if (this.pendingBytes) this.trace?.add("outputDiscardedSamples", this.pendingBytes / 2);
    this.readOffset = 0;
    this.pendingBytes = 0;
    this.queuedSpans.length = 0;
    this.scheduledSpans.length = 0;
    this.playedBytes.clear();
    this.submittedUntilMs = 0;
    this.generation = undefined;
  }
  private fail(): void {
    if (this.closed) return;
    this.trace?.add("outputFailures");
    this.close();
    this.failed();
  }
  close(): void { this.closed = true; this.discard(); this.items.clear(); this.revokedItems.clear(); this.truncatedItems.clear(); }
}
