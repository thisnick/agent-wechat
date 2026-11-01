import type { CallAudioFormat, NativeCallMediaClient } from "@agent-wechat/shared";
import type { PreparedCallVoice, StreamingPcmResampler } from "./calls-voice.ts";

/** Generated/provider PCM only; this module opens no devices and records no media. */
export class CallPcmOutput {
  private pending = Buffer.alloc(0);
  private resampler: StreamingPcmResampler;
  private readonly items = new Set<string>();
  private readonly revokedItems = new Set<string>();
  private timer?: ReturnType<typeof setTimeout>;
  private due?: number;
  private closed = false;
  private paused = false;
  private clearEpoch = 0;
  private sequence = 0n;
  private sampleTime = 0n;
  private readonly frameBytes: number;
  private readonly maxBytes: number;
  private media: Pick<NativeCallMediaClient, "sendPcm" | "tracks">;
  private format: CallAudioFormat;
  private voice: PreparedCallVoice;
  private clearRemote: () => Promise<unknown>;
  private failed: () => void;
  constructor(media: Pick<NativeCallMediaClient, "sendPcm" | "tracks">, format: CallAudioFormat, voice: PreparedCallVoice,
    clearRemote: () => Promise<unknown>, failed: () => void) {
    this.media = media; this.format = format; this.voice = voice; this.clearRemote = clearRemote; this.failed = failed;
    if (format.channels !== 1 || format.frameMs !== 20 || ![8000, 12000, 16000, 24000, 48000].includes(format.sampleRate)) throw Error("Calling PCM format unsupported");
    this.frameBytes = format.sampleRate * format.frameMs / 1000 * 2;
    // Provider output can arrive faster than playout. Keep at most eight seconds
    // while the WebSocket itself receives one negotiated frame per clock tick.
    this.maxBytes = format.sampleRate * 2 * 8;
    this.resampler = voice.resampler(voice.sampleRate, format.sampleRate);
  }
  enqueue(audio: Buffer, metadata?: { itemId: string }): void {
    if (this.closed) return;
    const epoch = this.clearEpoch;
    const item = metadata?.itemId;
    // Item identity lets clear reject late chunks from an interrupted response.
    if (!item || item.length > 200 || audio.length % 2 !== 0 || audio.length > this.voice.sampleRate * 4) return this.fail();
    if (this.revokedItems.has(item)) return;
    if (!this.items.has(item) && this.items.size >= 256) return this.fail();
    this.items.add(item);
    let converted: Buffer;
    try { converted = this.resampler.process(audio); } catch { if (epoch === this.clearEpoch) this.fail(); return; }
    // A synchronous provider/resampler callback may interrupt this conversion.
    // Its output still belongs to the old resampler, even after clear returns.
    if (this.closed || epoch !== this.clearEpoch || this.revokedItems.has(item)) return;
    if (converted.length % 2 !== 0 || this.pending.length + converted.length > this.maxBytes) return this.fail();
    this.pending = Buffer.concat([this.pending, converted]);
    // New response items remain bounded in pending while the daemon clear is
    // outstanding. Only its current acknowledgement can resume playout.
    if (!this.paused && !this.timer && this.pending.length >= this.frameBytes) this.schedule();
  }
  async clear(): Promise<void> {
    if (this.closed) return;
    const epoch = ++this.clearEpoch;
    this.paused = true; this.pending = Buffer.alloc(0); clearTimeout(this.timer); this.timer = undefined; this.due = undefined;
    for (const item of this.items) this.revokedItems.add(item);
    try {
      const resampler = this.voice.resampler(this.voice.sampleRate, this.format.sampleRate);
      if (this.closed || epoch !== this.clearEpoch) return;
      this.resampler = resampler;
      await this.clearRemote();
      if (!this.closed && epoch === this.clearEpoch) {
        this.paused = false;
        if (!this.timer && this.pending.length >= this.frameBytes) this.schedule();
      }
    } catch { if (epoch === this.clearEpoch) this.fail(); }
  }
  private schedule(): void {
    if (this.closed || this.paused || this.timer || this.pending.length < this.frameBytes) return;
    this.due ??= performance.now();
    this.timer = setTimeout(() => {
      this.timer = undefined;
      if (this.closed || this.paused) return;
      if (performance.now() - this.due! > 200) return this.fail();
      try {
        const track = this.media.tracks().sendTrack;
        const samples = new Int16Array(this.frameBytes / 2);
        for (let i = 0; i < samples.length; i++) samples[i] = this.pending.readInt16LE(i * 2);
        this.pending = this.pending.subarray(this.frameBytes);
        this.media.sendPcm({ trackId: track.id, generation: track.generation, sequence: this.sequence++, timestampMicros: this.sampleTime }, samples);
        this.sampleTime += BigInt(this.format.frameMs * 1000);
      } catch { return this.fail(); }
      if (this.pending.length >= this.frameBytes) { this.due = Math.max(this.due! + this.format.frameMs, performance.now()); this.schedule(); }
      else this.due = undefined;
    }, Math.max(0, this.due - performance.now()));
    this.timer.unref?.();
  }
  private fail(): void { if (!this.closed) { this.close(); this.failed(); } }
  close(): void { this.closed = true; clearTimeout(this.timer); this.timer = undefined; this.pending = Buffer.alloc(0); this.items.clear(); this.revokedItems.clear(); }
}
