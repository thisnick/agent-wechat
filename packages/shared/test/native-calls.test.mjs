import test from 'node:test';
import assert from 'node:assert/strict';
import { encodeNativeCallPcm, decodeNativeCallPcm, NativeCallMediaStream } from '../src/native-calls.ts';

const format = { sampleRate: 8000, channels: 1, frameMs: 10 };
const header = { trackId: 0x01020304, generation: 2, sequence: 3n, timestampMicros: 10_000n };
test('WXCM literal bytes match the Rust media format and retain the input slice', () => {
  const samples = new Int16Array(80); samples.set([-32768, 32767, -1]);
  const bytes = encodeNativeCallPcm(header, format, samples);
  assert.deepEqual([...bytes.subarray(0, 36)], [
    0x57, 0x58, 0x43, 0x4d, 1, 1, 0, 0,
    1, 2, 3, 4, 0, 0, 0, 2,
    0, 0, 0, 0, 0, 0, 0, 3,
    0, 0, 0, 0, 0, 0, 0x27, 0x10,
    0, 0, 0, 0xa0,
  ]);
  assert.deepEqual([...bytes.subarray(36, 42)], [0, 0x80, 0xff, 0x7f, 0xff, 0xff]);
  const decoded = decodeNativeCallPcm(bytes, format);
  assert.equal(decoded.payload.buffer, bytes.buffer);
  assert.equal(decoded.sequence, 3n);
  assert.equal(decoded.timestampMicros, 10_000n);
  const padded = new Uint8Array(bytes.length + 14); padded.set(bytes, 7);
  assert.deepEqual(decodeNativeCallPcm(padded.subarray(7, 7 + bytes.length), format), decoded);
});
test('WXCM rejects malformed extents, kind/version/reserved fields and unsafe IDs', () => {
  const bytes = encodeNativeCallPcm(header, format, new Int16Array(80));
  for (let i = 0; i < bytes.length; i++) assert.throws(() => decodeNativeCallPcm(bytes.subarray(0, i), format));
  for (const index of [0, 4, 5, 6, 7, 35]) {
    const corrupt = bytes.slice(); corrupt[index] ^= 0xff;
    assert.throws(() => decodeNativeCallPcm(corrupt, format));
  }
  assert.throws(() => decodeNativeCallPcm(new Uint8Array(65537), format));
  assert.throws(() => encodeNativeCallPcm({ ...header, sequence: -1n }, format, new Int16Array(80)));
  assert.throws(() => encodeNativeCallPcm({ ...header, trackId: 0 }, format, new Int16Array(80)));
  assert.throws(() => encodeNativeCallPcm(header, format, new Int16Array(79)));
  assert.throws(() => decodeNativeCallPcm(bytes, { ...format, channels: 2 }));
});
test('media stream fences stale output after clear and rejects replayed or foreign tracks', () => {
  const frame = decodeNativeCallPcm(encodeNativeCallPcm(header, format, new Int16Array(80)), format);
  const stream = new NativeCallMediaStream(header.trackId, header.generation);
  assert.throws(() => stream.admit({ ...frame, trackId: header.trackId + 1 }), /track/);
  assert.throws(() => stream.admit({ ...frame, generation: 1 }), /generation/);
  stream.admit(frame);
  assert.throws(() => stream.admit(frame), /Out-of-order/);
  assert.throws(() => stream.admit({ ...frame, sequence: 4n }), /Out-of-order/);
  stream.advanceGeneration(3);
  assert.throws(() => stream.admit(frame), /generation/);
  stream.admit({ ...frame, generation: 3, sequence: 0n, timestampMicros: 0n });
});

import { NativeCallsClient, NativeCallError } from '../src/native-calls.ts';
const audio = { sampleRate: 16000, channels: 1, frameMs: 20 };
const offer = { callId: 'call-1', revision: 1, kind: 'direct', callerId: 'synthetic-contact', offeredMedia: { audio: true, video: false } };
class MockSocket {
  readyState = 0; bufferedAmount = 0; binaryType = '';
  onopen = null; onmessage = null; onerror = null; onclose = null;
  sent = []; received = [];
  constructor(handler) { this.handler = handler; }
  open() { this.readyState = 1; this.onopen?.({}); }
  receive(value) { this.received.push(value); this.onmessage?.({ data: typeof value === 'object' && !(value instanceof Uint8Array) ? JSON.stringify(value) : value }); }
  send(value) { if (this.readyState !== 1) throw Error('closed'); this.sent.push(value); this.handler(value); }
  close() { if (this.readyState === 3) return; this.readyState = 3; this.onclose?.({}); }
}
class MockCallServer {
  sockets = []; controls = []; media = []; owner = null; counter = 0; hold = false;
  tracks = { sendTrack: { id: 1, generation: 1, direction: 'send', format: audio }, receiveTrack: { id: 2, generation: 1, direction: 'receive', format: audio } };
  result(socket, requestId, extra = {}) { queueMicrotask(() => socket.receive({ type: 'command.result', requestId, status: 'accepted', callId: null, ownerToken: null, code: null, ...extra })); }
  factory = (url, options) => {
    assert.equal(options.headers.Authorization, 'Bearer synthetic-api-token');
    assert.equal(new URL(url).search, '');
    assert.ok(!url.includes('synthetic-api-token'));
    if (url.endsWith('/media')) {
      let attached = false;
      const socket = new MockSocket(value => {
        if (!attached) {
          const command = JSON.parse(value);
          assert.equal(command.type, 'media.attach');
          assert.equal(command.clientId, this.owner);
          assert.equal(command.ownerToken, `owner-${this.owner}`);
          attached = true; queueMicrotask(() => socket.receive({ type: 'media.ready', ...structuredClone(this.tracks) }));
        } else assert.ok(value instanceof Uint8Array);
      });
      this.sockets.push(socket); this.media.push(socket); queueMicrotask(() => socket.open()); return socket;
    }
    const id = `client-${++this.counter}`;
    const socket = new MockSocket(value => {
      const command = JSON.parse(value);
      if (this.hold) return;
      if (command.type === 'call.ready') { socket.ready = command.ready; this.result(socket, command.requestId); }
      else if (command.type === 'call.answer' || command.type === 'call.dial') {
        if (!socket.ready) { this.result(socket, command.requestId, { status: 'error', code: 'media_not_ready' }); return; }
        if (this.owner) { this.result(socket, command.requestId, { status: 'error', code: 'already_claimed' }); return; }
        this.owner = id; this.result(socket, command.requestId, { callId: 'call-1', ownerToken: `owner-${id}` });
      } else if (command.type === 'call.clear_audio') {
        this.tracks.sendTrack.generation += 1;
        queueMicrotask(() => socket.receive({ type: 'track.updated', callId: 'call-1', track: structuredClone(this.tracks.sendTrack) }));
        this.result(socket, command.requestId, { callId: 'call-1' });
      } else this.result(socket, command.requestId);
    });
    socket.id = id; this.sockets.push(socket); this.controls.push(socket);
    queueMicrotask(() => {
      socket.open(); socket.receive({ type: 'hello', protocolVersion: 1, clientId: id,
        capabilities: { directAudio: true, directVideo: false, conferenceAudio: false, conferenceVideo: false },
        availability: { busy: false, ownedByThisClient: false }, currentOffer: offer, currentCall: null, audioFormat: audio });
    });
    return socket;
  };
}
function client(server, extra = {}) { return new NativeCallsClient({ baseUrl: 'http://127.0.0.1:6174', accountId: 'test-account', token: 'synthetic-api-token', webSocketFactory: server.factory, ...extra }); }

test('initial snapshot and queued events have no subscription gap; competing answer loses without media access', async () => {
  const server = new MockCallServer(); const one = client(server); const two = client(server);
  try {
    const hello = await one.connect(); await two.connect();
    assert.equal(hello.currentOffer.callId, 'call-1');
    const events = one.events()[Symbol.asyncIterator]();
    server.controls[0].receive({ type: 'call.offered', offer: { ...offer, revision: 2 } });
    assert.equal((await events.next()).value.type, 'hello');
    assert.equal((await events.next()).value.offer.revision, 2);
    await one.ready('ready-one', true); await two.ready('ready-two', true);
    const results = await Promise.allSettled([one.answer('answer-one', 'call-1', 1), two.answer('answer-two', 'call-1', 1)]);
    assert.equal(results.filter(result => result.status === 'fulfilled').length, 1);
    const lost = results.find(result => result.status === 'rejected');
    assert.equal(lost.reason.code, 'already_claimed');
    await assert.rejects(two.attachMedia('call-1'), error => error.code === 'not_owner');
    const media = await one.attachMedia('call-1');
    assert.deepEqual(media.tracks(), server.tracks);
  } finally { one.close(); two.close(); }
});

test('media waits for attachment acknowledgement and fences generations after clear', async () => {
  const server = new MockCallServer(); const owner = client(server);
  try {
    await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
    const media = await owner.attachMedia('call-1');
    const first = { trackId: 1, generation: 1, sequence: 0n, timestampMicros: 0n };
    media.sendPcm(first, new Int16Array(320));
    assert.ok(server.media[0].sent[1] instanceof Uint8Array);
    await owner.clearAudio('clear', 'call-1');
    assert.equal(media.tracks().sendTrack.generation, 2);
    assert.throws(() => media.sendPcm({ ...first, sequence: 1n, timestampMicros: 20_000n }, new Int16Array(320)), /generation/);
    media.sendPcm({ ...first, generation: 2 }, new Int16Array(320));
    const frames = media.frames()[Symbol.asyncIterator]();
    server.media[0].receive(encodeNativeCallPcm({ trackId: 2, generation: 1, sequence: 0n, timestampMicros: 0n }, audio, new Int16Array(320)));
    assert.equal((await frames.next()).value.trackId, 2);
    server.media[0].receive(encodeNativeCallPcm({ trackId: 2, generation: 1, sequence: 1n, timestampMicros: 20_000n }, audio, new Int16Array(320)));
    server.controls[0].receive({ type: 'track.updated', callId: 'call-1', track: { ...server.tracks.receiveTrack, generation: 2 } });
    server.media[0].receive(encodeNativeCallPcm({ trackId: 2, generation: 2, sequence: 0n, timestampMicros: 0n }, audio, new Int16Array(320)));
    assert.equal((await frames.next()).value.generation, 2);
    server.media[0].bufferedAmount = 65536;
    assert.throws(() => media.sendPcm({ ...first, generation: 2, sequence: 1n, timestampMicros: 20_000n }, new Int16Array(320)), error => error.code === 'media_backpressure');
    server.media[0].bufferedAmount = 0;
    media.sendPcm({ ...first, generation: 2, sequence: 1n, timestampMicros: 20_000n }, new Int16Array(320));
  } finally { owner.close(); }
});

test('pending request ID deduplicates locally, detects changed payload and never retries a lost reply', async () => {
  const server = new MockCallServer(); const owner = client(server, { commandTimeoutMs: 5, maxBufferedBytes: 1024 });
  try {
    await owner.connect(); server.hold = true;
    const unicode = { type: 'call.dial', requestId: 'unicode', target: { kind: 'direct', contactId: '合'.repeat(20) }, media: { audio: true, video: false } };
    server.controls[0].bufferedAmount = 1024 - JSON.stringify(unicode).length;
    await assert.rejects(owner.command(unicode), error => error.code === 'control_backpressure');
    server.controls[0].bufferedAmount = 0;
    const command = { type: 'call.dial', requestId: 'same', target: { kind: 'direct', contactId: 'synthetic-contact' }, media: { audio: true, video: false } };
    const pending = owner.command(command);
    assert.equal(owner.command(command), pending);
    await assert.rejects(owner.command({ ...command, target: { kind: 'direct', contactId: 'different-contact' } }), error => error.code === 'idempotency_conflict');
    await assert.rejects(pending, error => error.code === 'command_timeout' && error.outcomeUnknown);
    assert.equal(server.controls[0].sent.length, 1);
  } finally { owner.close(); }
});

test('revocation closes media and rejects pending control operations without exposing server text', async () => {
  const server = new MockCallServer(); const owner = client(server);
  await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
  const media = await owner.attachMedia('call-1');
  server.hold = true;
  const pending = owner.clearAudio('pending', 'call-1');
  server.controls[0].receive({ type: 'call.error', code: 'forbidden' });
  await assert.rejects(pending, error => error.code === 'forbidden' && error.outcomeUnknown);
  assert.equal(server.media[0].readyState, 3);
  assert.throws(() => media.sendPcm({ trackId: 1, generation: 1, sequence: 0n, timestampMicros: 0n }, new Int16Array(320)), error => error.code === 'media_not_ready');
});

test('slow control consumer and malformed media fail closed with fixed queue limits', async () => {
  const server = new MockCallServer(); const observer = client(server, { maxEvents: 1 });
  await observer.connect();
  server.controls[0].receive({ type: 'calls.availability', availability: { busy: true, ownedByThisClient: false } });
  assert.equal(server.controls[0].readyState, 3);
  await assert.rejects(observer.command({ type: 'call.ready', requestId: 'ready', ready: true, format: audio }), error => error.code === 'not_connected');
  const owner = client(server); await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
  const media = await owner.attachMedia('call-1');
  server.media[0].receive(new Uint8Array(3));
  await assert.rejects(media.frames()[Symbol.asyncIterator]().next(), error => error.code === 'invalid_media');
  owner.close();
});
