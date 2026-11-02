import test from 'node:test';
import assert from 'node:assert/strict';
import { NativeCallsClient, NativeCallError } from '../src/native-calls.ts';
const audio = { sampleRate: 16000, channels: 1, frameMs: 20 };
const offer = { callId: 'call-1', revision: 1, kind: 'direct', callType: 'voice', callerId: 'synthetic-contact' };
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
  capabilities = { directAudio: true };
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
        } else if (typeof value === 'string') {
          const start = JSON.parse(value); assert.equal(start.type, 'audio.start');
          assert.equal(start.generation, this.tracks.sendTrack.generation);
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
        capabilities: this.capabilities,
        availability: { busy: false, ownedByThisClient: false }, currentOffer: offer, currentCall: null, audioFormat: audio });
    });
    return socket;
  };
}
function client(server, extra = {}) { return new NativeCallsClient({ baseUrl: 'http://127.0.0.1:6174', accountId: 'test-account', token: 'synthetic-api-token', webSocketFactory: server.factory, ...extra }); }

test('pending request ID deduplicates locally, detects changed payload and never retries a lost reply', async () => {
  const server = new MockCallServer(); const owner = client(server, { commandTimeoutMs: 5, maxBufferedBytes: 1024 });
  try {
    await owner.connect(); server.hold = true;
    const unicode = { type: 'call.dial', requestId: 'unicode', target: { kind: 'direct', contactId: '合'.repeat(20) }, callType: 'voice' };
    server.controls[0].bufferedAmount = 1024 - JSON.stringify(unicode).length;
    await assert.rejects(owner.command(unicode), error => error.code === 'control_backpressure');
    server.controls[0].bufferedAmount = 0;
    const command = { type: 'call.dial', requestId: 'same', target: { kind: 'direct', contactId: 'synthetic-contact' }, callType: 'voice' };
    const pending = owner.command(command);
    assert.equal(owner.command(command), pending);
    await assert.rejects(owner.command({ ...command, target: { kind: 'direct', contactId: 'different-contact' } }), error => error.code === 'idempotency_conflict');
    await assert.rejects(pending, error => error.code === 'command_timeout' && error.outcomeUnknown);
    assert.equal(server.controls[0].sent.length, 1);
  } finally { owner.close(); }
});

test('ending stops audio but retains owner transports until terminal confirmation', async () => {
  for (const terminal of ['ended', 'uncertain']) {
    const server = new MockCallServer(); const owner = client(server);
    try {
      await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
      const media = await owner.attachMedia('call-1');
      const snapshot = { callId: 'call-1', revision: 2, kind: 'direct', callType: 'voice', direction: 'outgoing', state: 'ending', ownedByThisClient: true };
      server.controls[0].receive({ type: 'call.state', call: snapshot });
      assert.equal(server.controls[0].readyState, 1);
      assert.equal(server.media[0].readyState, 1);
      assert.throws(() => media.sendPcm(new Int16Array(320), 1), error => error.code === 'call_ending');
      // Cross-socket data already queued before Ending must neither play nor
      // overflow a stopped consumer and detach its still-terminating owner.
      for (let i = 0; i < 20; i++) server.media[0].receive(new Uint8Array(640));
      assert.equal(server.media[0].readyState, 1);
      server.controls[0].receive({ type: 'call.state', call: { ...snapshot, revision: 3, state: terminal } });
      assert.equal(server.media[0].readyState, 3);
      assert.equal(server.controls[0].readyState, 1);
      await assert.rejects(owner.attachMedia('call-1'), error => error.code === 'not_owner');
    } finally { owner.close(); }
  }
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
  assert.throws(() => media.sendPcm(new Int16Array(320), 1), error => error.code === 'media_not_ready');
});

test('slow control consumer and malformed media fail closed with fixed queue limits', async () => {
  const server = new MockCallServer(); const observer = client(server, { maxEvents: 1 });
  await observer.connect();
  server.controls[0].receive({ type: 'calls.availability', availability: { busy: true, ownedByThisClient: false } });
  assert.equal(server.controls[0].readyState, 3);
  await assert.rejects(observer.command({ type: 'call.ready', requestId: 'ready', ready: true, format: audio }), error => error.code === 'not_connected');
  const owner = client(server); await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
  const media = await owner.attachMedia('call-1');
  server.media[0].bufferedAmount = 65536;
  assert.throws(() => media.sendPcm(new Int16Array(320), 1), error => error.code === 'media_backpressure');
  server.media[0].bufferedAmount = 0;
  media.sendPcm(new Int16Array(320), 1);
  assert.deepEqual(JSON.parse(server.media[0].sent[1]), { type: 'audio.start', generation: 1 });
  assert.deepEqual(server.media[0].sent[2], new Uint8Array(640));
  media.sendPcm(new Int16Array([-32768, 32767, -1]), 1);
  assert.deepEqual([...server.media[0].sent[3]], [0, 128, 255, 127, 255, 255]);
  await owner.clearAudio('clear-pcm', 'call-1');
  assert.throws(() => media.sendPcm(new Int16Array(1), 1), error => error.code === 'stale');
  media.sendPcm(new Int16Array(1), 2);
  assert.deepEqual(JSON.parse(server.media[0].sent[4]), { type: 'audio.start', generation: 2 });
  server.media[0].receive(new Uint8Array(3));
  await assert.rejects(media.frames()[Symbol.asyncIterator]().next(), error => error.code === 'invalid_media');
  owner.close();
  for (const capabilities of [{}, { directAudio: true }, { directAudio: false }, { futureCallingMode: true }]) {
    const capabilityServer = new MockCallServer(); capabilityServer.capabilities = capabilities;
    const capabilityClient = client(capabilityServer);
    const hello = await capabilityClient.connect();
    assert.equal(hello.capabilities.directAudio ?? false, capabilities.directAudio ?? false);
    capabilityClient.close();
  }
  const invalidCapabilityServer = new MockCallServer(); invalidCapabilityServer.capabilities = { directAudio: 'yes' };
  const invalidCapabilityClient = client(invalidCapabilityServer);
  await assert.rejects(invalidCapabilityClient.connect(), error => error.code === 'invalid_event');
  invalidCapabilityClient.close();
  for (const malformed of [
    { ...offer, callType: 'conference' },
    { ...offer, callType: undefined },
    { ...offer, callerId: 42 },
  ]) {
    const invalidServer = new MockCallServer(); const invalidClient = client(invalidServer);
    await invalidClient.connect();
    invalidServer.controls[0].receive({ type: 'call.offered', offer: { ...offer, callType: 'video' } });
    assert.equal(invalidServer.controls[0].readyState, 1);
    invalidServer.controls[0].receive({ type: 'call.offered', offer: malformed });
    assert.equal(invalidServer.controls[0].readyState, 3);
    invalidClient.close();
  }
});
