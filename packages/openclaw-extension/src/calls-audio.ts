import type { CallAudioFormat, NativeCallMediaClient } from "@agent-wechat/shared";
import type { CallAudioTrace } from "./calls-audio-trace.ts";
import type { PreparedCallVoice, StreamingPcmResampler } from "./calls-voice.ts";

const MAX_BACKLOG_MS = 8_000;
const LOOK_AHEAD_FRAMES = 2;

/** Generated/provider PCM only; this module opens no devices and records no media. */
export class CallPcmOutput {
  private readonly queue: Buffer;
  private readOffset = 0;
  private pendingBytes = 0;
  private readonly bytesPerMs: number;
  private readonly frameBytes: number;
  private timer?: ReturnType<typeof setTimeout>;
  private submittedUntilMs = 0;
  private generation?: number;
  private resampler: StreamingPcmResampler;
  private resamplerNeedsReset = false;
  private readonly items = new Set<string>();
  private readonly revokedItems = new Set<string>();
  private readonly truncatedItems = new Set<string>();
  private closed = false;
  private paused = false;
  private clearEpoch = 0;
  private readonly media: Pick<NativeCallMediaClient, "sendPcm" | "tracks">;
  private readonly format: CallAudioFormat;
  private readonly voice: PreparedCallVoice;
  private readonly clearRemote: () => Promise<unknown>;
  private readonly failed: () => void;
  private readonly trace?: CallAudioTrace;
  constructor(media: Pick<NativeCallMediaClient, "sendPcm" | "tracks">, format: CallAudioFormat,
    voice: PreparedCallVoice, clearRemote: () => Promise<unknown>, failed: () => void, trace?: CallAudioTrace) {
    if (format.channels !== 1 || format.frameMs !== 20 || ![8000, 12000, 16000, 24000, 48000].includes(format.sampleRate)) throw Error("Calling PCM format unsupported");
    this.media = media;
    this.format = format;
    this.voice = voice;
    this.clearRemote = clearRemote;
    this.failed = failed;
    this.trace = trace;
    this.bytesPerMs = format.sampleRate * 2 / 1000;
    this.frameBytes = this.bytesPerMs * format.frameMs;
    this.queue = Buffer.alloc(this.bytesPerMs * MAX_BACKLOG_MS);
    this.resampler = voice.resampler(voice.sampleRate, format.sampleRate);
  }
  enqueue(audio: Buffer, metadata?: { itemId: string }): void {
    if (this.closed) return;
    const epoch = this.clearEpoch;
    const item = metadata?.itemId;
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
    const accepted = Math.min(converted.length, this.queue.length - this.pendingBytes);
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
  private flush(): void {
    if (this.closed || this.paused || this.pendingBytes === 0) return;
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
      const bytes = Math.min(this.pendingBytes, available * this.frameBytes);
      const samples = new Int16Array(bytes / 2);
      for (let i = 0; i < samples.length; i++) samples[i] = this.queue.readInt16LE((this.readOffset + i * 2) % this.queue.length);
      this.readOffset = (this.readOffset + bytes) % this.queue.length;
      this.pendingBytes -= bytes;
      const started = performance.now();
      this.submittedUntilMs = Math.max(started, this.submittedUntilMs) + bytes / this.bytesPerMs;
      const leadAfterSubmit = this.submittedUntilMs - started;
      this.media.sendPcm(samples, generation);
      this.trace?.add("daemonOutputChunks");
      this.trace?.add("daemonOutputSamples", samples.length);
      this.trace?.gap("daemonOutputGapMs", started);
      this.trace?.measure("daemonForwardMs", performance.now() - started);
      this.trace?.measure("outputLeadMs", leadAfterSubmit);
      if (!this.closed && this.clearEpoch === epoch && this.pendingBytes && !this.timer) this.schedule();
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
