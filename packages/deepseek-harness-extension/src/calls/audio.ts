/**
 * Audio helpers for agent mode (DESIGN.md §6.2). Ported from the OpenClaw
 * voice SDK semantics: PCM16 level stats, a hysteresis speech gate
 * (rms 650 / peak 2500, 4 speech frames open, 10 silence frames close,
 * 300 ms cooldown between triggers) and naive downsampling for providers
 * that want a lower rate.
 */

export type PcmStats = { rms: number; peak: number };

export function readPcm16Stats(pcm: Int16Array): PcmStats {
  if (pcm.length === 0) return { rms: 0, peak: 0 };
  let sumSquares = 0;
  let peak = 0;
  for (let i = 0; i < pcm.length; i++) {
    const sample = pcm[i]!;
    sumSquares += sample * sample;
    const magnitude = Math.abs(sample);
    if (magnitude > peak) peak = magnitude;
  }
  return { rms: Math.sqrt(sumSquares / pcm.length), peak };
}

export type SpeechGateOptions = {
  rmsThreshold?: number;
  peakThreshold?: number;
  speechFrames?: number;
  silenceFrames?: number;
  cooldownMs?: number;
};

/**
 * Frames enter only while the gate is open (speech detected). The first frame
 * after a closed→open transition fires the trigger exactly once; the caller
 * typically uses it to barge in. The gate re-opens only after the cooldown.
 */
export class SpeechGate {
  private open = false;
  private speechRun = 0;
  private silenceRun = 0;
  private lastTriggerMs = Number.NEGATIVE_INFINITY;

    private readonly options: SpeechGateOptions;

  constructor(options: SpeechGateOptions = {}) {
    this.options = options;
  }

  accept(
    stats: PcmStats,
    atMs: number,
    onTrigger?: () => boolean,
  ): boolean {
    const rmsThreshold = this.options.rmsThreshold ?? 650;
    const peakThreshold = this.options.peakThreshold ?? 2500;
    const speechFrames = this.options.speechFrames ?? 4;
    const silenceFrames = this.options.silenceFrames ?? 10;
    const cooldownMs = this.options.cooldownMs ?? 300;
    const speech = stats.rms >= rmsThreshold && stats.peak >= peakThreshold;
    if (speech) {
      this.speechRun += 1;
      this.silenceRun = 0;
      if (!this.open && this.speechRun >= speechFrames && atMs - this.lastTriggerMs >= cooldownMs) {
        this.open = true;
        this.lastTriggerMs = atMs;
        if (onTrigger && !onTrigger()) return false;
      }
      return this.open;
    }
    this.silenceRun += 1;
    this.speechRun = 0;
    if (this.open && this.silenceRun >= silenceFrames) this.open = false;
    return this.open;
  }
}

/** Decimate PCM16 mono by an integer-ish ratio (nearest-sample picking). */
export function downsamplePcm16(pcm: Int16Array, fromRate: number, toRate: number): Int16Array {
  if (toRate >= fromRate || fromRate % toRate !== 0 || pcm.length === 0) return pcm;
  const ratio = fromRate / toRate;
  const out = new Int16Array(Math.floor(pcm.length / ratio));
  for (let i = 0; i < out.length; i++) out[i] = pcm[Math.floor(i * ratio)]!;
  return out;
}
