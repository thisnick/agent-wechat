/**
 * Speech provider contract (DESIGN.md §6.2). The calls layer never talks to a
 * vendor SDK: it consumes PCM16 frames and transcript/text events through
 * this interface, so tests inject fakes and alternative providers stay
 * possible. v1 ships OpenAI (`speech-openai.ts`).
 *
 * Direction of the pipeline: daemon audio → transcribe (agent turns) and
 * provider audio ← synthesize (agent replies). Realtime bidirectional mode is
 * optional; M5 wires the agent-mode pipeline.
 */
export type TranscribeOptions = { language?: string; signal?: AbortSignal };

/** Speech-to-text over raw PCM16 mono frames. */
export interface SpeechTranscriber {
  readonly provider: string;
  transcribe(pcm: Int16Array, sampleRate: number, options?: TranscribeOptions): Promise<string>;
}

export type SynthesizeOptions = { signal?: AbortSignal; voice?: string };

/** Text-to-speech; returns encoded audio bytes (mp3 for OpenAI v1). */
export interface SpeechSynthesizer {
  readonly provider: string;
  synthesize(text: string, options?: SynthesizeOptions): Promise<{ audio: Uint8Array; mimeType: string }>;
}

export type RealtimeEvent =
  | { type: 'transcript'; role: 'user' | 'assistant'; text: string; final: boolean }
  | { type: 'audio'; pcm: Int16Array }
  | { type: 'speech_started' }
  | { type: 'error'; message: string }
  | { type: 'closed' };

export type RealtimeSessionOptions = { instructions?: string; voice?: string; signal?: AbortSignal };

/**
 * One live bidirectional conversation. Audio flows in as PCM16 mono 24 kHz
 * (the daemon call format); transcripts and synthesized PCM16 come back as
 * events. `close` is idempotent.
 */
export interface RealtimeSession {
  readonly provider: string;
  events(): AsyncIterable<RealtimeEvent>;
  sendAudio(pcm: Int16Array): void;
  /** Commit the input buffer and ask for a response (turn-taking control). */
  commit(): void;
  /** Stop the in-flight response (barge-in). */
  interrupt(): void;
  close(): void;
}

export type SpeechProvider = {
  readonly provider: string;
  transcriber?: SpeechTranscriber;
  synthesizer?: SpeechSynthesizer;
  realtime?(options: RealtimeSessionOptions): Promise<RealtimeSession>;
};

export class SpeechProviderError extends Error {
  readonly provider: string;
  readonly status?: number | undefined;

  constructor(message: string, provider: string, status?: number) {
    super(message);
    this.name = 'SpeechProviderError';
    this.provider = provider;
    this.status = status;
  }
}
