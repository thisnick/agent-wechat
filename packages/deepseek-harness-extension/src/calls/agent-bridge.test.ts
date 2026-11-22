import assert from 'node:assert/strict';
import { test } from 'node:test';
import { AgentCallBridge } from './agent-bridge.ts';
import type { SpeechProvider } from './speech.ts';

const loud = (n = 80): Int16Array => new Int16Array(n).fill(3000);
const quiet = (n = 80): Int16Array => new Int16Array(n);

type Harness = {
  bridge: AgentCallBridge;
  sent: Array<{ samples: number; generation: number }>;
  turns: string[];
  transcripts: Array<{ role: 'user' | 'assistant'; text: string; final: boolean }>;
  clears: number;
  setReply: (reply: string) => void;
};

function makeHarness(reply = 'sure thing', synthesizerMime = 'audio/pcm;rate=24000'): Harness {
  const sent: Harness['sent'] = [];
  const turns: string[] = [];
  const transcripts: Harness['transcripts'] = [];
  let clears = 0;
  let latestReply = reply;
  const provider: SpeechProvider = {
    provider: 'fake',
    transcriber: {
      provider: 'fake',
      transcribe: async () => 'hello there',
    },
    synthesizer: {
      provider: 'fake',
      synthesize: async () => {
        const pcm = new Int16Array(48_000); // one full second of audio
        return { audio: new Uint8Array(pcm.buffer), mimeType: synthesizerMime };
      },
    },
  };
  const bridge = new AgentCallBridge({
    provider,
    onUserUtterance: async text => {
      turns.push(text);
      return latestReply;
    },
    onTranscript: (role, text, final) => transcripts.push({ role, text, final }),
    onClearAudio: () => {
      clears += 1;
    },
    sendPcm: (samples, generation) => {
      sent.push({ samples: samples.length, generation });
    },
    now: Date.now,
  });
  return {
    bridge,
    sent,
    turns,
    transcripts,
    get clears() { return clears; },
    setReply: (next: string) => { latestReply = next; },
  };
}

const until = async (condition: () => boolean, timeoutMs = 2_000): Promise<void> => {
  const deadline = Date.now() + timeoutMs;
  while (!condition()) {
    if (Date.now() > deadline) throw new Error('condition not reached');
    await new Promise(resolve => setTimeout(resolve, 10));
  }
};

test('a gated utterance becomes a full turn: transcribe, reply, synthesize, pace', async () => {
  const h = makeHarness();
  for (let i = 0; i < 4; i++) h.bridge.acceptFrame(loud(), 300 + i * 20);
  for (let i = 0; i < 10; i++) h.bridge.acceptFrame(quiet(), 400 + i * 20); // silence run closes the gate
  await until(() => h.sent.reduce((total, chunk) => total + chunk.samples, 0) >= 48_000, 4_000);
  assert.deepEqual(h.turns, ['hello there']);
  assert.ok(h.transcripts.some(entry => entry.role === 'user' && entry.text === 'hello there' && entry.final));
  assert.ok(h.transcripts.some(entry => entry.role === 'assistant' && entry.text === 'sure thing' && entry.final));
  // One full second of audio drains under the 200 ms pacing lookahead.
  assert.equal(h.sent.reduce((total, chunk) => total + chunk.samples, 0), 48_000);
  assert.ok(h.sent.every(chunk => chunk.generation === 0));
});

test('barge-in bumps the generation, clears daemon audio and annotates the next turn', async () => {
  const h = makeHarness();
  for (let i = 0; i < 4; i++) h.bridge.acceptFrame(loud(), 300 + i * 20);
  for (let i = 0; i < 10; i++) h.bridge.acceptFrame(quiet(), 400 + i * 20);
  await until(() => h.sent.length > 0, 4_000);
  // A fresh speech run interrupts the playback while it is still pacing out.
  for (let i = 0; i < 4; i++) h.bridge.acceptFrame(loud(), 1_200 + i * 20);
  for (let i = 0; i < 10; i++) h.bridge.acceptFrame(quiet(), 1_300 + i * 20);
  await until(() => h.clears > 0, 4_000);
  await until(() => h.turns.length >= 2, 4_000);
  assert.ok(h.turns[1]!.includes('[The caller interrupted the previous reply after approximately'));
  await until(() => h.sent.some(chunk => chunk.generation >= 1), 4_000);
});

test('enqueue drains queued turns serially and caps the backlog at four', async () => {
  const h = makeHarness();
  for (let i = 1; i <= 6; i++) h.bridge.enqueue(`turn ${i}`);
  await until(() => h.turns.length >= 5, 12_000); // turn 1 + the four that survived the cap
  await new Promise(resolve => setTimeout(resolve, 60)); // nothing more should run
  assert.equal(h.turns.length, 5);
  // The backlog cap (4) dropped the two oldest queued turns.
  assert.deepEqual(h.turns.map(text => Number(text.replace('turn ', ''))), [1, 3, 4, 5, 6]);
});

test('non-PCM synthesis is reported as an assistant turn but never played', async () => {
  const h = makeHarness('spoken reply', 'audio/mp3');
  h.bridge.enqueue('hi');
  await until(() => h.transcripts.some(entry => entry.role === 'assistant'));
  await new Promise(resolve => setTimeout(resolve, 40));
  assert.equal(h.sent.length, 0);
});

test('a failing turn never retries; the input stays open for the next utterance', async () => {
  const sent: Array<{ samples: number; generation: number }> = [];
  let failing = true;
  const provider: SpeechProvider = {
    provider: 'fake',
    transcriber: {
      provider: 'fake',
      transcribe: async () => {
        if (failing) throw new Error('transcription down');
        return 'second try';
      },
    },
  };
  const bridge = new AgentCallBridge({
    provider,
    onUserUtterance: async () => 'unused',
    sendPcm: samples => {
      sent.push({ samples: samples.length, generation: 0 });
    },
    now: () => 1_000,
  });
  for (let i = 0; i < 4; i++) bridge.acceptFrame(loud(), 300 + i * 20);
  for (let i = 0; i < 10; i++) bridge.acceptFrame(quiet(), 400 + i * 20);
  await new Promise(resolve => setTimeout(resolve, 40));
  // Gate reopens after the cooldown; the next utterance still runs a turn.
  for (let i = 0; i < 4; i++) bridge.acceptFrame(loud(), 1_500 + i * 20);
  for (let i = 0; i < 10; i++) bridge.acceptFrame(quiet(), 1_600 + i * 20);
  await new Promise(resolve => setTimeout(resolve, 40));
  assert.equal(sent.length, 0); // no reply path ever ran
  bridge.close();
});
