import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createOpenAISpeechProvider, OPENAI_REALTIME_URL, type OpenAISocketLike } from './speech-openai.ts';

type Recorded = { url: string; init: RequestInit | undefined };

function captureFetch(response: unknown): { calls: Recorded[]; fetch: typeof fetch } {
  const calls: Recorded[] = [];
  const impl = (async (input: unknown, init?: RequestInit) => {
    calls.push({ url: String(input), init });
    return response;
  }) as unknown as typeof fetch;
  return { calls, fetch: impl };
}

const jsonResponse = (body: unknown): Response =>
  ({ ok: true, status: 200, json: async () => body }) as unknown as Response;

test('transcriber posts the utterance as a WAV attachment', async () => {
  const { calls, fetch } = captureFetch(jsonResponse({ text: ' hello world ' }));
  const provider = createOpenAISpeechProvider({ apiKey: 'sk-test', fetchImpl: fetch, socketFactory: () => { throw new Error('unused'); } });
  const transcriber = provider.transcriber!;
  const text = await transcriber.transcribe(new Int16Array([100, -100]), 24_000);
  assert.equal(text, 'hello world');
  const call = calls[0]!;
  assert.equal(call.url, 'https://api.openai.com/v1/audio/transcriptions');
  assert.equal(call.init?.method, 'POST');
  const form = call.init?.body as FormData;
  assert.equal(form.get('model'), 'gpt-4o-mini-transcribe');
  assert.ok(form.get('file'));
});

test('synthesizer requests raw PCM and reports the call-track mime type', async () => {
  const bytes = new Uint8Array([1, 2, 3, 4]);
  const calls: Recorded[] = [];
  const fetchImpl: typeof fetch = (async (input: unknown, init?: RequestInit): Promise<Response> => {
    calls.push({ url: String(input), init });
    return { ok: true, status: 200, arrayBuffer: async () => bytes.buffer } as unknown as Response;
  }) as unknown as typeof fetch;
  const provider = createOpenAISpeechProvider({ apiKey: 'sk-test', fetchImpl, socketFactory: () => { throw new Error('unused'); } });
  const audio = await provider.synthesizer!.synthesize('hi', { voice: 'nova' });
  assert.deepEqual(Array.from(audio.audio), [1, 2, 3, 4]);
  assert.equal(audio.mimeType, 'audio/pcm;rate=24000');
  const body = JSON.parse(String(calls[0]!.init?.body)) as Record<string, unknown>;
  assert.equal(body.model, 'gpt-4o-mini-tts');
  assert.equal(body.voice, 'nova');
  assert.equal(body.response_format, 'pcm');
});

type FakeSocket = OpenAISocketLike & {
  onopen: (() => void) | null;
  onmessage: ((event: { data: unknown }) => void) | null;
  onerror: (() => void) | null;
  onclose: (() => void) | null;
  sent: string[];
};

function fakeSocket(): { socket: FakeSocket; open: () => void } {
  const socket = {
    readyState: 0,
    sent: [],
    send(data: string) { socket.sent.push(data); },
    close() {},
    onopen: null,
    onmessage: null,
    onerror: null,
    onclose: null,
  } as FakeSocket;
  return { socket, open: () => socket.onopen?.() };
}

test('realtime bridge maps OpenAI events and sends PCM frames', async () => {
  const { socket, open } = fakeSocket();
  const provider = createOpenAISpeechProvider({
    apiKey: 'sk-test',
    realtimeModel: 'gpt-realtime-x',
    socketFactory: () => socket,
  });
  // The factory's returned promise settles when the socket opens.
  const sessionPromise = provider.realtime!({ instructions: 'be brief' });
  open();
  const session = await sessionPromise;

  // session.update was sent on open with the custom model and instructions.
  const sessionUpdate = JSON.parse(socket.sent[0]!) as Record<string, never>;
  assert.equal(sessionUpdate.type, 'session.update');
  assert.ok(socket.sent[0]!.includes('be brief'));

  socket.onmessage?.({ data: JSON.stringify({ type: 'response.audio_transcript.delta', delta: 'Hel' }) });
  socket.onmessage?.({ data: JSON.stringify({ type: 'response.audio_transcript.done', transcript: 'Hello' }) });
  socket.onmessage?.({ data: JSON.stringify({ type: 'conversation.item.input_audio_transcription.completed', transcript: 'hi there' }) });
  socket.onmessage?.({ data: JSON.stringify({ type: 'input_audio_buffer.speech_started' }) });

  session.sendAudio(new Int16Array([1, -1, 2]));
  session.commit();
  session.interrupt();

  const events = [];
  for await (const event of session.events()) {
    events.push(event);
    if (events.length >= 4) break;
  }
  assert.deepEqual(events.map(event => event.type), ['transcript', 'transcript', 'transcript', 'speech_started']);
  assert.deepEqual(events[1], { type: 'transcript', role: 'assistant', text: 'Hello', final: true });
  assert.deepEqual(events[2], { type: 'transcript', role: 'user', text: 'hi there', final: true });

  const append = socket.sent.find(data => data.includes('input_audio_buffer.append'))!;
  assert.ok(append.length > 0);
  assert.ok(socket.sent.some(data => data.includes('input_audio_buffer.commit')));
  assert.ok(socket.sent.some(data => data.includes('response.create')));
  assert.ok(socket.sent.some(data => data.includes('response.cancel')));
  const url = OPENAI_REALTIME_URL;
  assert.ok(url.startsWith('wss://api.openai.com'));
  session.close();
});

test('realtime bridge surfaces protocol errors and close', async () => {
  const { socket, open } = fakeSocket();
  const provider = createOpenAISpeechProvider({ apiKey: 'sk-test', socketFactory: () => socket });
  const sessionPromise = provider.realtime!({});
  open();
  const session = await sessionPromise;
  socket.onmessage?.({ data: JSON.stringify({ type: 'error', message: 'bad request' }) });
  socket.onclose?.();
  const events = [];
  for await (const event of session.events()) {
    events.push(event);
    if (event.type === 'closed') break;
  }
  assert.deepEqual(events.map(event => event.type), ['error', 'closed']);
  assert.equal((events[0] as { message: string }).message, 'bad request');
});
