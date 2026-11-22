import assert from 'node:assert/strict';
import { test } from 'node:test';
import { CallCoordinator, type CallsClientLike, type CallMediaClientLike } from './coordinator.ts';
import { CallSummaryStore, memoryCallMemoryTable } from './memory.ts';
import type { ResolvedWeChatVoiceCallsConfig } from './config.ts';
import type { CallEvent } from '@agent-wechat/shared';
import type { SpeechProvider } from './speech.ts';

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

const voice = (overrides: Partial<ResolvedWeChatVoiceCallsConfig> = {}): ResolvedWeChatVoiceCallsConfig =>
  ({
    enabled: true,
    mode: 'agent',
    transcription: { provider: 'openai' },
    inboundPolicy: 'allowlist',
    inboundAllowFrom: ['wxid_alice'],
    outboundPolicy: 'allowlist',
    outboundAllowTo: ['wxid_bob'],
    outboundToolAllowFrom: ['wxid_alice'],
    agentId: 'agent-x',
    realtime: {
      provider: 'openai',
      agentContext: { enabled: false },
      recentContext: { enabled: false, maxMessages: 10 },
      consultPolicy: 'auto',
      fastContext: { enabled: false, timeoutMs: 800, maxResults: 3, sources: ['memory', 'sessions'], fallbackToConsult: true },
    },
    toolPolicy: 'safe-read-only',
    allowConsultSessionPersistence: true,
    consultation: { timeoutMs: 30_000, summary: true, recentMessages: 10 },
    sessionScope: 'per-contact',
    maxDurationSeconds: 3_600,
    conferences: { groups: {} },
    ...overrides,
  }) as ResolvedWeChatVoiceCallsConfig;

const speechProvider = (): SpeechProvider => ({
  provider: 'fake',
  transcriber: { provider: 'fake', transcribe: async () => 'caller words' },
  synthesizer: {
    provider: 'fake',
    synthesize: async () => ({ audio: new Uint8Array(new Int16Array(480).buffer), mimeType: 'audio/pcm;rate=24000' }),
  },
});

type QueueClient = CallsClientLike & {
  push: (event: CallEvent) => void;
  pushFrame: (payload: Uint8Array) => void;
  commands: string[];
  endMedia: () => void;
  closed: boolean;
};

function fakeClient(): QueueClient {
  const events: CallEvent[] = [];
  const eventWake: Array<() => void> = [];
  let eventsClosed = false;
  const commands: string[] = [];
  const frames: Array<{ payload: Uint8Array }> = [];
  const frameWake: Array<() => void> = [];
  let mediaClosed = false;
  const media: CallMediaClientLike & { closed: boolean } = {
    closed: false,
    tracks: () => ({ sendTrack: { generation: 7 } }),
    frames: () => ({
      [Symbol.asyncIterator]() {
        return {
          next: async () => {
            if (frames.length === 0 && !mediaClosed) await new Promise<void>(resolve => frameWake.push(resolve));
            if (frames.length > 0) return { value: frames.shift()!, done: false };
            return { value: undefined, done: true };
          },
        };
      },
    }),
    sendPcm: () => {},
    close: () => {
      media.closed = true;
    },
  };
  const client: QueueClient = {
    push: event => {
      events.push(event);
      eventWake.shift()?.();
    },
    pushFrame: payload => {
      frames.push({ payload });
      frameWake.shift()?.();
    },
    commands,
    endMedia: () => {
      mediaClosed = true;
      while (frameWake.length > 0) frameWake.shift()!();
    },
    get closed() {
      return clientClosed;
    },
    connect: async () => ({}),
    events: () => ({
      [Symbol.asyncIterator]() {
        return {
          next: async () => {
            if (events.length === 0 && !clientClosed) await new Promise<void>(resolve => eventWake.push(resolve));
            if (events.length > 0) return { value: events.shift()!, done: false };
            return { value: undefined, done: true };
          },
        };
      },
    }),
    dial: async (requestId, contactId) => {
      commands.push(`dial:${contactId}`);
      return { status: 'accepted', callId: 'call-out' };
    },
    startConference: async (requestId, groupId, invitees) => {
      commands.push(`conf:${groupId}:${invitees.join(',')}`);
      return { status: 'accepted', callId: 'call-conf' };
    },
    invite: async (requestId, callId, contactIds) => {
      commands.push(`invite:${callId}:${contactIds.join(',')}`);
      return { status: 'accepted' };
    },
    joinConference: async (requestId, invitationId, revision) => {
      commands.push(`join:${invitationId}:${revision}`);
      return { status: 'accepted' };
    },
    answer: async (requestId, callId, revision) => {
      commands.push(`answer:${callId}:${revision}`);
      return { status: 'accepted' };
    },
    end: async (type, requestId, callId, revision) => {
      commands.push(`end:${type}:${callId}`);
      return { status: 'accepted' };
    },
    clearAudio: async () => ({}),
    attachMedia: async () => media,
    close: () => {
      clientClosed = true;
      while (eventWake.length > 0) eventWake.shift()!();
      mediaClosed = true;
      while (frameWake.length > 0) frameWake.shift()!();
    },
  };
  let clientClosed = false;
  return client;
}

// The async iterator resolves with `done` after close; drain pending microtasks.
const settle = async (): Promise<void> => {
  for (let i = 0; i < 6; i++) await new Promise(resolve => setTimeout(resolve, 1));
};

type Harness = {
  coordinator: CallCoordinator;
  client: QueueClient;
  callMemory: CallSummaryStore;
  turns: Array<{ sessionKey: string; text: string }>;
  summaries: Array<{ sessionKey: string; text: string }>;
  records: Array<{ contactId: string; summary: string; direction: string }>;
  stop: () => Promise<void>;
};

async function makeHarness(voiceOverrides: Partial<ResolvedWeChatVoiceCallsConfig> = {}): Promise<Harness> {
  const client = fakeClient();
  const turns: Harness['turns'] = [];
  const summaries: Harness['summaries'] = [];
  const records: Harness['records'] = [];
  const callMemory = new CallSummaryStore(memoryCallMemoryTable());
  const coordinator = new CallCoordinator({
    config: { voice: voice(voiceOverrides), video: { enabled: false } },
    accountId: 'default',
    ownerAllowFrom: ['wxid_alice'],
    speech: speechProvider(),
    createClient: () => client,
    runCallTurn: async (sessionKey, text) => {
      turns.push({ sessionKey, text });
      return 'agent reply';
    },
    summarizeCall: async (sessionKey, text) => {
      summaries.push({ sessionKey, text });
      return 'the caller said hello';
    },
    callMemory,
    now: () => 1_000,
  });
  coordinator.start();
  await settle(); // let connectLoop reach pumpEvents
  return {
    coordinator,
    client,
    callMemory,
    turns,
    summaries,
    records,
    stop: async () => {
      client.close();
      coordinator.stop();
      await settle();
    },
  };
}

const frame = (): { payload: Uint8Array } => ({ payload: new Uint8Array(new Int16Array(80).fill(3000).buffer) });

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

test('inbound allowlisted calls are answered and bridged to a consultation turn', async () => {
  const h = await makeHarness();
  h.client.push({
    type: 'call.offered',
    offer: { callId: 'call-1', revision: 1, kind: 'direct', callType: 'audio', callerId: 'wxid_alice' },
  } as unknown as CallEvent);
  h.client.push({
    type: 'call.state',
    call: {
      callId: 'call-1', revision: 3, kind: 'direct', callType: 'audio', direction: 'incoming',
      state: 'connected', ownedByThisClient: true,
      participants: [{ participantId: 'p1', contactId: 'wxid_alice', membership: 'joined' }],
    },
  } as unknown as CallEvent);
  await settle();
  assert.ok(h.client.commands.includes('answer:call-1:1'));
  assert.equal(h.turns.length, 0); // no turn until the caller speaks
  h.client.pushFrame(frame().payload);
  for (let i = 0; i < 4; i++) h.client.pushFrame(frame().payload);
  for (let i = 0; i < 10; i++) h.client.pushFrame(new Uint8Array(new Int16Array(80).buffer));
  await settle();
  await new Promise(resolve => setTimeout(resolve, 120)); // pacing tick
  assert.equal(h.turns.length, 1);
  assert.match(h.turns[0]!.sessionKey, /^agent:agent-x:wechat-call:wxid_alice:read-only$/);
  // No stored summaries for this contact yet, so no memory recap is prefixed.
  assert.equal(h.turns[0]!.text, 'caller words');
  await h.stop();
});

test('calls from unknown callers are ignored and policy-none calls hang up', async () => {
  const h = await makeHarness();
  h.client.push({
    type: 'call.offered',
    offer: { callId: 'call-2', revision: 1, kind: 'direct', callType: 'audio', callerId: 'wxid_stranger' },
  } as unknown as CallEvent);
  await settle();
  assert.ok(!h.client.commands.some(command => command.startsWith('answer:')));

  // A connected call whose policy resolves to none (owner toolPolicy + non-owner peer).
  const strict = await makeHarness({ toolPolicy: 'none' });
  strict.client.push({
    type: 'call.state',
    call: {
      callId: 'call-3', revision: 1, kind: 'direct', callType: 'audio', direction: 'outgoing',
      state: 'connected', ownedByThisClient: true,
      participants: [{ participantId: 'p1', contactId: 'wxid_bob', membership: 'joined' }],
    },
  } as unknown as CallEvent);
  await settle();
  assert.ok(strict.client.commands.includes('end:call.hangup:call-3'));
  await h.stop();
  await strict.stop();
});

test('ended calls tear down, summarize the transcript and record call memory', async () => {
  const h = await makeHarness();
  h.client.push({
    type: 'call.state',
    call: {
      callId: 'call-4', revision: 1, kind: 'direct', callType: 'audio', direction: 'incoming',
      state: 'connected', ownedByThisClient: true,
      participants: [{ participantId: 'p1', contactId: 'wxid_alice', membership: 'joined' }],
    },
  } as unknown as CallEvent);
  await settle();
  h.client.pushFrame(frame().payload);
  for (let i = 0; i < 4; i++) h.client.pushFrame(frame().payload);
  for (let i = 0; i < 10; i++) h.client.pushFrame(new Uint8Array(new Int16Array(80).buffer));
  await new Promise(resolve => setTimeout(resolve, 120));
  assert.equal(h.turns.length, 1);
  h.client.push({ type: 'call.state', call: { callId: 'call-4', state: 'ended' } } as unknown as CallEvent);
  await settle();
  await new Promise(resolve => setTimeout(resolve, 20)); // summary promise chain
  assert.equal(h.summaries.length, 1);
  assert.match(h.summaries[0]!.text, /caller words/);
  const stored = await h.callMemory.recent('default', 'wxid_alice', 'safe-read-only');
  assert.equal(stored.length, 1);
  assert.equal(stored[0]!.summary, 'the caller said hello');
  assert.equal(stored[0]!.direction, 'incoming');
  assert.equal(h.coordinator.status().calls.length, 0);
  await h.stop();
});

test('dial enforces the outbound allowlist and tool actions reach the client', async () => {
  const h = await makeHarness();
  await assert.rejects(() => h.coordinator.dial('wxid_unknown'), /not allowed by calls.voice.outboundAllowTo/);
  await h.coordinator.dial('wxid_bob');
  assert.ok(h.client.commands.includes('dial:wxid_bob'));
  await h.coordinator.conferenceStart('123@chatroom', ['wxid_bob']);
  assert.ok(h.client.commands.includes('conf:123@chatroom:wxid_bob'));
  await h.coordinator.invite('call-9', ['wxid_bob']);
  assert.ok(h.client.commands.includes('invite:call-9:wxid_bob'));
  assert.equal(await h.coordinator.hangupCall(), false); // no active call target
  await h.stop();
  await assert.rejects(() => h.coordinator.dial('wxid_bob'), /control socket is not connected/);
});

test('without a speech provider the coordinator stays offline (fail-closed)', async () => {
  const client = fakeClient();
  const coordinator = new CallCoordinator({
    config: { voice: voice(), video: { enabled: false } },
    accountId: 'default',
    ownerAllowFrom: [],
    speech: undefined,
    createClient: () => client,
    runCallTurn: async () => '',
    callMemory: new CallSummaryStore(memoryCallMemoryTable()),
  });
  coordinator.start();
  await settle();
  assert.equal(coordinator.status().state, 'disabled');
  assert.equal(coordinator.status().available, false);
  assert.ok(!client.closed); // never even connected
  coordinator.stop();
  await settle();
});

test('conference invitations auto-join only for owner inviters on auto-join groups', async () => {
  const h = await makeHarness({
    conferences: {
      groups: {
        '123@chatroom': { autoJoin: true, toolPolicy: 'safe-read-only', autoJoinAllowFrom: ['agent-wechat:wxid_alice'] },
      },
    },
  } as Partial<ResolvedWeChatVoiceCallsConfig>);
  h.client.push({
    type: 'conference.invitations',
    invitations: [{ invitationId: 'inv-1', revision: 2, groupId: '123@chatroom', callerId: 'wxid_alice' }],
  } as unknown as CallEvent);
  await settle();
  assert.ok(h.client.commands.includes('join:inv-1:2'));

  const stranger = await makeHarness({
    conferences: {
      groups: {
        '123@chatroom': { autoJoin: true, toolPolicy: 'safe-read-only', autoJoinAllowFrom: ['agent-wechat:wxid_alice'] },
      },
    },
  } as Partial<ResolvedWeChatVoiceCallsConfig>);
  stranger.client.push({
    type: 'conference.invitations',
    invitations: [{ invitationId: 'inv-2', revision: 1, groupId: '123@chatroom', callerId: 'wxid_stranger' }],
  } as unknown as CallEvent);
  await settle();
  assert.ok(!stranger.client.commands.some(command => command.startsWith('join:')));
  await h.stop();
  await stranger.stop();
});
