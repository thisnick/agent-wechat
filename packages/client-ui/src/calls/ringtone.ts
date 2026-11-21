/**
 * Synthesized looping in-app ringtone matching the bundled Desktop sound
 * (`AgentWechatSoftRing.wav` — MarimbaDouble motif: E5-A5, pause, E5-A5-C#6,
 * wooden marimba harmonics, 3.0 s seamless loop, peak -12.5 dBFS).
 */

export const RINGTONE_LOOP_SECONDS = 3.0;
const TARGET_PEAK = Math.pow(10, -12.5 / 20); // ~0.2371 linear (-12.5 dBFS)

const NOTES: ReadonlyArray<{
  freq: number;
  startSec: number;
  durationSec: number;
  amp: number;
}> = [
  { freq: 659.25, startSec: 0.0, durationSec: 0.24, amp: 0.9 }, // E5
  { freq: 880.0, startSec: 0.15, durationSec: 0.28, amp: 1.0 }, // A5
  { freq: 659.25, startSec: 0.62, durationSec: 0.24, amp: 0.9 }, // E5
  { freq: 880.0, startSec: 0.77, durationSec: 0.24, amp: 0.95 }, // A5
  { freq: 1108.73, startSec: 0.92, durationSec: 0.42, amp: 1.0 }, // C#6
];

const HARMONICS: ReadonlyArray<[number, number]> = [
  [1, 1.0],
  [2, 0.14],
  [4, 0.04],
];

/** Pure deterministic sample generator used both by WebAudio and Node tests. */
export function renderMarimbaDoubleSamples(sampleRate = 48000): Float32Array {
  const totalSamples = Math.round(sampleRate * RINGTONE_LOOP_SECONDS);
  const samples = new Float32Array(totalSamples);
  const attackSec = 0.005;
  const twoPi = Math.PI * 2;

  for (const note of NOTES) {
    const startIdx = Math.floor(note.startSec * sampleRate);
    const len = Math.floor(note.durationSec * sampleRate);
    const endIdx = Math.min(totalSamples, startIdx + len);
    for (let i = startIdx; i < endIdx; i++) {
      const t = (i - startIdx) / sampleRate;
      const attack = t < attackSec ? t / attackSec : 1.0;
      const decay = Math.exp(-8.5 * t);
      // Smooth tail fade over last 15 ms to avoid clicks.
      const remaining = note.durationSec - t;
      const release = remaining < 0.015 ? Math.max(0, remaining / 0.015) : 1.0;
      let wave = 0;
      for (const [multiple, weight] of HARMONICS) {
        wave += weight * Math.sin(twoPi * note.freq * multiple * t);
      }
      samples[i] += note.amp * attack * decay * release * wave;
    }
  }

  let peak = 0;
  for (let i = 0; i < totalSamples; i++) {
    const abs = Math.abs(samples[i]);
    if (abs > peak) peak = abs;
  }
  if (peak > 0) {
    const scale = TARGET_PEAK / peak;
    for (let i = 0; i < totalSamples; i++) {
      samples[i] *= scale;
    }
  }
  return samples;
}

export class CallRingtone {
  private ctx: AudioContext | null = null;
  private buffer: AudioBuffer | null = null;
  private source: AudioBufferSourceNode | null = null;
  private gain: GainNode | null = null;
  private active = false;
  private muted = false;
  private blocked = false;
  private onBlockedChange?: (blocked: boolean) => void;
  private unlockListener?: () => void;

  constructor(onBlockedChange?: (blocked: boolean) => void) {
    this.onBlockedChange = onBlockedChange;
  }

  isPlaying(): boolean {
    return this.active;
  }

  isMuted(): boolean {
    return this.muted;
  }

  isAutoplayBlocked(): boolean {
    return this.blocked;
  }

  setMuted(muted: boolean): void {
    this.muted = muted;
    if (this.gain && this.ctx) {
      this.gain.gain.setValueAtTime(muted ? 0 : 1, this.ctx.currentTime);
    }
  }

  start(): void {
    if (this.active) return;
    this.active = true;
    this.startPlayback();
  }

  stop(): void {
    this.active = false;
    this.clearUnlock();
    this.setBlocked(false);
    if (this.source) {
      try {
        this.source.stop();
        this.source.disconnect();
      } catch {
        // Source may already have stopped.
      }
      this.source = null;
    }
  }

  dispose(): void {
    this.stop();
    if (this.gain) {
      try {
        this.gain.disconnect();
      } catch {}
      this.gain = null;
    }
    if (this.ctx) {
      void this.ctx.close().catch(() => {});
      this.ctx = null;
    }
    this.buffer = null;
  }

  private setBlocked(value: boolean): void {
    if (this.blocked !== value) {
      this.blocked = value;
      this.onBlockedChange?.(value);
    }
  }

  private clearUnlock(): void {
    if (!this.unlockListener || typeof window === "undefined") return;
    window.removeEventListener("pointerdown", this.unlockListener, true);
    window.removeEventListener("keydown", this.unlockListener, true);
    this.unlockListener = undefined;
  }

  private installUnlock(): void {
    if (this.unlockListener || typeof window === "undefined") return;
    const unlock = () => {
      this.clearUnlock();
      if (!this.active || !this.ctx) return;
      void this.ctx.resume().then(
        () => {
          if (this.active) this.setBlocked(false);
        },
        () => {},
      );
    };
    this.unlockListener = unlock;
    window.addEventListener("pointerdown", unlock, { capture: true, once: true });
    window.addEventListener("keydown", unlock, { capture: true, once: true });
  }

  private startPlayback(): void {
    const AudioCtx =
      typeof globalThis !== "undefined"
        ? (globalThis.AudioContext ??
          (globalThis as unknown as { webkitAudioContext?: typeof AudioContext })
            .webkitAudioContext)
        : undefined;
    if (!AudioCtx) return;

    try {
      if (!this.ctx) {
        this.ctx = new AudioCtx();
      }
      const ctx = this.ctx;
      if (!this.buffer) {
        const samples = renderMarimbaDoubleSamples(ctx.sampleRate);
        const buf = ctx.createBuffer(1, samples.length, ctx.sampleRate);
        buf.copyToChannel(samples as Float32Array<ArrayBuffer>, 0);
        this.buffer = buf;
      }
      if (!this.gain) {
        this.gain = ctx.createGain();
        this.gain.connect(ctx.destination);
      }
      this.gain.gain.setValueAtTime(this.muted ? 0 : 1, ctx.currentTime);
      const source = ctx.createBufferSource();
      source.buffer = this.buffer;
      source.loop = true;
      source.connect(this.gain);
      source.start();
      this.source = source;

      if (ctx.state === "suspended") {
        void ctx.resume().then(
          () => {
            this.clearUnlock();
            if (this.active) this.setBlocked(false);
          },
          () => {
            if (this.active) {
              this.setBlocked(true);
              this.installUnlock();
            }
          },
        );
        // Check again synchronously in case resume stays pending until user gesture.
        if (ctx.state === "suspended") {
          this.setBlocked(true);
          this.installUnlock();
        }
      }
    } catch {
      this.setBlocked(true);
    }
  }
}
