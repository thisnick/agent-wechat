/**
 * Agent-mode call bridge (DESIGN.md §6.2) — the OpenClaw voice agent loop,
 * adapted to our SpeechProvider contract. The caller (daemon media track)
 * feeds PCM16 mono 24 kHz frames in; a hysteresis speech gate detects caller
 * turns; each finished turn is transcribed, handed to the agent, and the
 * reply is synthesized and paced onto the call at real-time speed. The
 * caller can barge in at any moment: playback generation bumps, in-flight
 * work is aborted and the daemon is asked to clear its audio. Late results
 * from an abandoned epoch are dropped — never spoken. The plugin records no
 * audio; only short-lived PCM buffers for the current turn exist.
 */
import { readPcm16Stats, SpeechGate } from './audio.ts';
import type { SpeechProvider } from './speech.ts';

const PCM_RATE = 24_000;
const CHUNK_MS = 40;
const CHUNK_SAMPLES = (PCM_RATE * CHUNK_MS) / 1000;
const TIMER_MS = 20;
const PACE_AHEAD_MS = 200;
const MAX_PLAYBACK_MS = 180_000;
const MAX_UTTERANCE_MS = 60_000;
const COALESCE_MS = 200;
const MAX_TURN_CHARS = 16_000;
const MAX_PENDING = 4;
const SAMPLES_PER_MS = PCM_RATE / 1000;

export type AgentCallBridgeDeps = {
  provider: SpeechProvider;
  /** Run one agent turn for the caller utterance (or greeting/host note). */
  onUserUtterance(text: string): Promise<string>;
  onTranscript?: (role: 'user' | 'assistant', text: string, final: boolean) => void;
  /** Barge-in: stop daemon-side playback (maps to calls.clearAudio). */
  onClearAudio?: () => void;
  /** Paced PCM sink — sendPcm(samples, generation) from the media client. */
  sendPcm: (samples: Int16Array, generation: number) => void;
  log?: (level: 'info' | 'warn', message: string) => void;
  now?: () => number;
};

export class AgentCallBridge {
  private readonly gate = new SpeechGate();
  private utterance: Int16Array[] = [];
  private utteranceSamples = 0;
  private speaking = false;
  private turnEpoch = 0;
  private generation = 0;
  private busy = false;
  private pending: string[] = [];
  private timer: ReturnType<typeof setInterval> | undefined;
  private playback:
    | { generation: number; pcm: Int16Array; offset: number; startedAt: number; done: () => void }
    | undefined;
  private abort: AbortController | undefined;
  private interruptedMs: number | undefined;
  private closed = false;
  private readonly now: () => number;

    private readonly deps: AgentCallBridgeDeps;

  constructor(deps: AgentCallBridgeDeps) {
    this.deps = deps;
    this.now = deps.now ?? Date.now;
  }

  /** Feed one received frame (PCM16 mono 24 kHz) from the daemon call. */
  acceptFrame(pcm: Int16Array, atMs: number = this.now()): void {
    if (this.closed || pcm.length === 0) return;
    const stats = readPcm16Stats(pcm);
    const open = this.gate.accept(stats, atMs, () => this.bargeIn());
    if (open && !this.speaking) {
      this.speaking = true;
      this.utterance = [];
      this.utteranceSamples = 0;
    }
    if (this.speaking) {
      this.utterance.push(pcm);
      this.utteranceSamples += pcm.length;
      if (this.utteranceSamples >= MAX_UTTERANCE_MS * SAMPLES_PER_MS) {
        this.finalizeTurn();
      } else if (!open) {
        // The gate closed after its silence run: end of utterance.
        this.finalizeTurn();
      }
      return;
    }
    if (this.utteranceSamples > 0) this.finalizeTurn();
  }

  private finalizeTurn(): void {
    this.speaking = false;
    if (this.utteranceSamples === 0) return;
    const epoch = ++this.turnEpoch;
    const pcm = mergePcm(this.utterance, this.utteranceSamples);
    this.utterance = [];
    this.utteranceSamples = 0;
    const interruptedMs = this.interruptedMs;
    this.interruptedMs = undefined;
    void this.handleUtterance(pcm, epoch, interruptedMs).catch(error => {
      // Failed turns are never retried; the input simply stays open.
      this.deps.log?.('warn', `wechat calls: agent turn failed: ${errorMessage(error)}`);
      this.busy = false;
      this.pump();
    });
  }

  private async handleUtterance(pcm: Int16Array, epoch: number, interruptedMs: number | undefined): Promise<void> {
    if (this.busy) {
      // A turn is already in flight; barge-in will have reset it. Serialize.
      await this.playback?.done();
      if (this.turnEpoch !== epoch) return;
    }
    this.busy = true;
    const abort = new AbortController();
    this.abort = abort;
    try {
      const transcriber = this.deps.provider.transcriber;
      if (!transcriber) return;
      const text = (await transcriber.transcribe(pcm, PCM_RATE, { signal: abort.signal })).trim();
      if (this.turnEpoch !== epoch) return;
      if (!text) return;
      this.deps.onTranscript?.('user', text, true);
      const input = interruptedMs !== undefined
        ? `${text}\n[The caller interrupted the previous reply after approximately ${interruptedMs} ms]`
        : text;
      const rawReply = await this.deps.onUserUtterance(input);
      if (this.turnEpoch !== epoch) return;
      const reply = rawReply.slice(0, MAX_TURN_CHARS);
      if (reply) this.deps.onTranscript?.('assistant', reply, true);
      const synthesizer = this.deps.provider.synthesizer;
      if (!reply || !synthesizer) return;
      const audio = await synthesizer.synthesize(reply, { signal: abort.signal });
      if (this.turnEpoch !== epoch) return;
      if (!audio.mimeType.startsWith('audio/pcm')) {
        this.deps.log?.('warn', `wechat calls: speech provider returned ${audio.mimeType}; only PCM can be played on a call`);
        return;
      }
      await this.playPcm(new Int16Array(audio.audio.buffer.slice(0)), epoch);
    } finally {
      if (this.abort === abort) this.abort = undefined;
      if (this.turnEpoch === epoch) this.busy = false;
      this.pump();
    }
  }

  /** Pace synthesized PCM onto the call in 40 ms chunks at real-time speed. */
  private playPcm(pcm: Int16Array, epoch: number): Promise<void> {
    const budget = Math.min(pcm.length, MAX_PLAYBACK_MS * SAMPLES_PER_MS);
    const trimmed = budget === pcm.length ? pcm : pcm.slice(0, budget);
    return new Promise<void>(resolve => {
      this.playback = { generation: this.generation, pcm: trimmed, offset: 0, startedAt: this.now(), done: resolve };
      this.timer = setInterval(() => this.tick(), TIMER_MS);
    });
  }

  private tick(): void {
    const batch = this.playback;
    if (!batch || batch.generation !== this.generation) return this.stopPlayback();
    const elapsedMs = this.now() - batch.startedAt;
    const targetSamples = Math.floor((elapsedMs + PACE_AHEAD_MS) * SAMPLES_PER_MS);
    while (batch.offset < batch.pcm.length && batch.offset < targetSamples) {
      const end = Math.min(batch.offset + CHUNK_SAMPLES, batch.pcm.length);
      const chunk = batch.pcm.subarray(batch.offset, end);
      try {
        this.deps.sendPcm(chunk, batch.generation);
      } catch (error) {
        this.deps.log?.('warn', `wechat calls: playback send failed: ${errorMessage(error)}`);
        batch.done();
        return this.stopPlayback();
      }
      batch.offset = end;
    }
    if (batch.offset >= batch.pcm.length) {
      batch.done();
      this.stopPlayback();
    }
  }

  private stopPlayback(): void {
    if (this.timer !== undefined) {
      clearInterval(this.timer);
      this.timer = undefined;
    }
    this.playback = undefined;
  }

  /** Barge-in: invalidate the current playback epoch and clear daemon audio. */
  private bargeIn(): boolean {
    this.speaking = false;
    this.utterance = [];
    this.utteranceSamples = 0;
    const batch = this.playback;
    if (batch !== undefined) {
      const playedMs = Math.round((this.now() - batch.startedAt));
      this.interruptedMs = Math.min(playedMs, MAX_PLAYBACK_MS);
      this.generation += 1;
      this.stopPlayback();
      batch.done();
      this.abort?.abort();
      this.deps.onClearAudio?.();
    }
    return true;
  }

  /** External interruption (e.g. the caller hangs up mid-reply). */
  interrupt(): void {
    this.bargeIn();
  }

  /** Queue a host-side message or greeting to run as the next agent turn. */
  enqueue(text: string): void {
    if (this.closed || !text.trim()) return;
    if (this.pending.length >= MAX_PENDING) this.pending.shift();
    this.pending.push(text);
    this.pump();
  }

  private pump(): void {
    if (this.busy || this.closed || this.pending.length === 0) return;
    const text = this.pending.shift()!;
    this.busy = true;
    void this.handleQueued(text, ++this.turnEpoch).catch(error => {
      this.deps.log?.('warn', `wechat calls: queued turn failed: ${errorMessage(error)}`);
      this.busy = false;
      this.pump();
    });
  }

  private async handleQueued(text: string, epoch: number): Promise<void> {
    const abort = new AbortController();
    try {
      this.deps.onTranscript?.('user', text, true);
      const reply = (await this.deps.onUserUtterance(text)).slice(0, MAX_TURN_CHARS);
      if (this.turnEpoch !== epoch) return;
      if (reply) this.deps.onTranscript?.('assistant', reply, true);
      const synthesizer = this.deps.provider.synthesizer;
      if (!reply || !synthesizer) return;
      const audio = await synthesizer.synthesize(reply, { signal: abort.signal });
      if (this.turnEpoch !== epoch) return;
      if (!audio.mimeType.startsWith('audio/pcm')) return;
      await this.playPcm(new Int16Array(audio.audio.buffer.slice(0)), epoch);
    } finally {
      if (this.turnEpoch === epoch) this.busy = false;
      this.pump();
    }
  }

  close(): void {
    this.closed = true;
    this.bargeIn();
    this.pending = [];
  }
}

function mergePcm(chunks: Int16Array[], total: number): Int16Array {
  const out = new Int16Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.length;
  }
  return out;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Convenience factory most call sites use. */
export function createAgentCallBridge(deps: AgentCallBridgeDeps): AgentCallBridge {
  return new AgentCallBridge(deps);
}

export { COALESCE_MS };
