/**
 * OpenAI speech provider (DESIGN.md §6.2): Realtime transcription STT,
 * `/v1/audio/speech` TTS and the Realtime bidirectional bridge. Plain HTTPS +
 * WebSocket against the public API — no vendor SDK. The WebSocket factory is
 * injectable so tests run the bridge against a fake server. Call audio is
 * PCM16 mono 24 kHz end to end; no mulaw conversion (that is the OpenClaw
 * telephony pipeline, not ours).
 */
import type { RealtimeEvent, RealtimeSession, RealtimeSessionOptions, SpeechProvider, SpeechTranscriber, SpeechSynthesizer, SynthesizeOptions, TranscribeOptions } from './speech.ts';
import { SpeechProviderError } from './speech.ts';

export const OPENAI_TRANSCRIBE_URL = 'https://api.openai.com/v1/audio/transcriptions';
export const OPENAI_SPEECH_URL = 'https://api.openai.com/v1/audio/speech';
export const OPENAI_REALTIME_URL = 'wss://api.openai.com/v1/realtime';

export type OpenAISocketLike = {
  readyState: number;
  send(data: string): void;
  close(): void;
};
export type OpenAISocketFactory = (url: string, protocols: string[]) => OpenAISocketLike;

const CONNECT_TIMEOUT_MS = 10_000;

function wrapWav(pcm: Int16Array, sampleRate: number): Uint8Array<ArrayBuffer> {
  const bytes = new Uint8Array(pcm.buffer, pcm.byteOffset, pcm.byteLength);
  const out = new Uint8Array(44 + bytes.length);
  const view = new DataView(out.buffer);
  const ascii = (offset: number, text: string) => {
    for (let i = 0; i < text.length; i++) view.setUint8(offset + i, text.charCodeAt(i));
  };
  ascii(0, 'RIFF');
  view.setUint32(4, 36 + bytes.length, true);
  ascii(8, 'WAVE');
  ascii(12, 'fmt ');
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  ascii(36, 'data');
  view.setUint32(40, bytes.length, true);
  out.set(bytes, 44);
  return out;
}

export class OpenAITranscriber implements SpeechTranscriber {
  readonly provider = 'openai';
  private readonly apiKey: string;
  private readonly model: string;
  private readonly fetchImpl: typeof fetch;

  constructor(
    apiKey: string,
    model: string = 'gpt-4o-mini-transcribe',
    fetchImpl: typeof fetch = fetch,
  ) {
    this.apiKey = apiKey;
    this.model = model;
    this.fetchImpl = fetchImpl;
  }

  async transcribe(pcm: Int16Array, sampleRate: number, options?: TranscribeOptions): Promise<string> {
    if (pcm.length === 0) return '';
    const form = new FormData();
    form.append('file', new Blob([wrapWav(pcm, sampleRate)], { type: 'audio/wav' }), 'audio.wav');
    form.append('model', this.model);
    if (options?.language) form.append('language', options.language);
    const response = await this.fetchImpl(OPENAI_TRANSCRIBE_URL, {
      method: 'POST',
      headers: { Authorization: `Bearer ${this.apiKey}` },
      body: form,
      ...(options?.signal !== undefined ? { signal: options.signal } : {}),
    });
    if (!response.ok) throw new SpeechProviderError(`transcription failed: HTTP ${response.status}`, this.provider, response.status);
    const body = (await response.json()) as { text?: string };
    return (body.text ?? '').trim();
  }
}

export class OpenAISynthesizer implements SpeechSynthesizer {
  readonly provider = 'openai';
  private readonly apiKey: string;
  private readonly model: string;
  private readonly voice: string;
  private readonly fetchImpl: typeof fetch;

  constructor(
    apiKey: string,
    model: string = 'gpt-4o-mini-tts',
    voice: string = 'alloy',
    fetchImpl: typeof fetch = fetch,
  ) {
    this.apiKey = apiKey;
    this.model = model;
    this.voice = voice;
    this.fetchImpl = fetchImpl;
  }

  async synthesize(text: string, options?: SynthesizeOptions): Promise<{ audio: Uint8Array; mimeType: string }> {
    const response = await this.fetchImpl(OPENAI_SPEECH_URL, {
      method: 'POST',
      headers: { Authorization: `Bearer ${this.apiKey}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({ model: this.model, voice: options?.voice ?? this.voice, input: text, response_format: 'pcm' }),
      ...(options?.signal !== undefined ? { signal: options.signal } : {}),
    });
    if (!response.ok) throw new SpeechProviderError(`speech synthesis failed: HTTP ${response.status}`, this.provider, response.status);
    // `pcm` is raw 16-bit little-endian mono 24 kHz — exactly the call track format.
    return { audio: new Uint8Array(await response.arrayBuffer()), mimeType: 'audio/pcm;rate=24000' };
  }
}

type Json = Record<string, unknown>;

/**
 * Bidirectional bridge over the OpenAI Realtime WebSocket protocol. Input
 * PCM16 mono 24 kHz is appended to the input buffer and committed per turn
 * (the caller decides turn boundaries — commit()); assistant audio comes back
 * as `response.audio.delta` base64 PCM16 and transcripts as deltas/dones.
 * Server-side speech_started maps to our barge-in signal.
 */
export class OpenAIRealtimeBridge implements RealtimeSession {
  readonly provider = 'openai';
  private readonly queue: RealtimeEvent[] = [];
  private resolvers: Array<(result: IteratorResult<RealtimeEvent>) => void> = [];
  private closed = false;
  private error: string | undefined;
  private socket: OpenAISocketLike | undefined;
  private readonly model: string;
  private readonly socketFactory: OpenAISocketFactory;
  private readonly connectPromise: Promise<void>;

  constructor(
    apiKey: string,
    options: RealtimeSessionOptions,
    model: string,
    socketFactory: OpenAISocketFactory,
  ) {
    this.model = model;
    this.socketFactory = socketFactory;
    const url = `${OPENAI_REALTIME_URL}?model=${encodeURIComponent(this.model)}`;
    this.socket = this.socketFactory(url, ['realtime', `openai-insecure-api-key.${apiKey}`, 'openai-beta.realtime-v1']);
    this.connectPromise = new Promise<void>((resolve, reject) => {
      const ws = this.socket as OpenAISocketLike & {
        onopen: (() => void) | null;
        onmessage: ((event: { data: unknown }) => void) | null;
        onerror: ((event: unknown) => void) | null;
        onclose: (() => void) | null;
      };
      const timer = setTimeout(() => reject(new SpeechProviderError('realtime connect timed out', this.provider)), CONNECT_TIMEOUT_MS);
      ws.onopen = () => {
        clearTimeout(timer);
        this.sendJson({
          type: 'session.update',
          session: {
            type: 'realtime',
            output_modalities: ['text'],
            audio: {
              input: { format: { type: 'audio/pcm', rate: 24000 }, turn_detection: null },
            },
            ...(options.instructions !== undefined ? { instructions: options.instructions } : {}),
          },
        });
        resolve();
      };
      ws.onmessage = event => this.handleMessage(event.data);
      ws.onerror = () => {
        clearTimeout(timer);
        this.fail('realtime connection error');
        reject(new SpeechProviderError('realtime connection error', this.provider));
      };
      ws.onclose = () => {
        clearTimeout(timer);
        this.fail('realtime connection closed');
      };
      if (options.signal) options.signal.addEventListener('abort', () => this.close(), { once: true });
    });
  }

  private sendJson(value: Json): void {
    if (this.socket && !this.closed) this.socket.send(JSON.stringify(value));
  }

  private handleMessage(data: unknown): void {
    if (typeof data !== 'string') return;
    let event: Json;
    try {
      event = JSON.parse(data) as Json;
    } catch {
      return;
    }
    const type = String(event.type ?? '');
    if (type === 'response.audio.delta' && typeof event.delta === 'string') {
      this.push({ type: 'audio', pcm: new Int16Array(decodeBase64Pcm(event.delta).buffer) });
    } else if (type === 'response.audio_transcript.delta' && typeof event.delta === 'string') {
      this.push({ type: 'transcript', role: 'assistant', text: event.delta, final: false });
    } else if (type === 'response.audio_transcript.done' && typeof event.transcript === 'string') {
      this.push({ type: 'transcript', role: 'assistant', text: event.transcript, final: true });
    } else if (type === 'conversation.item.input_audio_transcription.completed' && typeof event.transcript === 'string') {
      this.push({ type: 'transcript', role: 'user', text: event.transcript, final: true });
    } else if (type === 'input_audio_buffer.speech_started') {
      this.push({ type: 'speech_started' });
    } else if (type === 'error') {
      const message = typeof event.message === 'string' ? event.message : 'realtime error';
      this.push({ type: 'error', message });
    }
  }

  private push(event: RealtimeEvent): void {
    if (this.closed) return;
    const resolver = this.resolvers.shift();
    if (resolver) resolver({ value: event, done: false });
    else this.queue.push(event);
  }

  private fail(message: string): void {
    if (this.closed) return;
    this.error = message;
    this.closed = true;
    this.push({ type: 'closed' });
  }

  async *events(): AsyncIterable<RealtimeEvent> {
    while (true) {
      if (this.queue.length > 0) {
        yield this.queue.shift()!;
        continue;
      }
      if (this.closed) {
        yield { type: 'closed' };
        return;
      }
      const next = await new Promise<IteratorResult<RealtimeEvent>>(resolve => this.resolvers.push(resolve));
      if (next.done) return;
      yield next.value;
    }
  }

  /** Wait until the socket is open (session.update sent) or the attempt fails. */
  ready(): Promise<void> {
    return this.connectPromise;
  }

  sendAudio(pcm: Int16Array): void {
    if (pcm.length === 0) return;
    const bytes = new Uint8Array(pcm.buffer, pcm.byteOffset, pcm.byteLength);
    let binary = '';
    for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]!);
    this.sendJson({ type: 'input_audio_buffer.append', audio: btoa(binary) });
  }

  commit(): void {
    this.sendJson({ type: 'input_audio_buffer.commit' });
    this.sendJson({ type: 'response.create', response: { modalities: ['text', 'audio'] } });
  }

  interrupt(): void {
    this.sendJson({ type: 'response.cancel' });
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    try {
      this.socket?.close();
    } catch {
      // already gone
    }
    this.socket = undefined;
    const resolvers = this.resolvers;
    this.resolvers = [];
    for (const resolve of resolvers) resolve({ value: { type: 'closed' }, done: false });
  }
}

export function decodeBase64Pcm(value: string): Uint8Array {
  return Uint8Array.from(atob(value), ch => ch.charCodeAt(0));
}

export type OpenAISpeechOptions = {
  apiKey: string;
  transcriptionModel?: string;
  speechModel?: string;
  voice?: string;
  realtimeModel?: string;
  socketFactory: OpenAISocketFactory;
  /** Injectable for tests; production uses global fetch. */
  fetchImpl?: typeof fetch | undefined;
};

export function createOpenAISpeechProvider(options: OpenAISpeechOptions): SpeechProvider & { realtime: NonNullable<SpeechProvider['realtime']> } {
  const transcriber = new OpenAITranscriber(options.apiKey, options.transcriptionModel, options.fetchImpl);
  const synthesizer = new OpenAISynthesizer(options.apiKey, options.speechModel, options.voice, options.fetchImpl);
  return {
    provider: 'openai',
    transcriber,
    synthesizer,
    async realtime(sessionOptions: RealtimeSessionOptions) {
      const bridge = new OpenAIRealtimeBridge(options.apiKey, sessionOptions, options.realtimeModel ?? 'gpt-realtime-2.1', options.socketFactory);
      await bridge.ready();
      return bridge;
    },
  };
}
