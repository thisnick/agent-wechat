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
  protocolVersion = 2;
  mediaProtocolVersion = 2;
  conferenceInvitations = [];
  currentCall = null;
  sockets = []; controls = []; media = []; owner = null; counter = 0; hold = false;
  tracks = { sendTrack: { id: 1, generation: 1, direction: 'send', format: audio }, receiveTracks: [{ id: 2, generation: 1, direction: 'receive', format: audio }] };
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
          attached = true; queueMicrotask(() => socket.receive({ type: 'media.ready', protocolVersion: this.mediaProtocolVersion, ...structuredClone(this.tracks) }));
        } else if (typeof value === 'string') {
          const command = JSON.parse(value);
          assert.ok(['audio.start', 'audio.chunk'].includes(command.type));
          assert.equal(command.trackId, 1);
          assert.equal(command.generation, this.tracks.sendTrack.generation);
        } else assert.ok(value instanceof Uint8Array);
      });
      this.sockets.push(socket); this.media.push(socket); queueMicrotask(() => socket.open()); return socket;
    }
    const id = `client-${++this.counter}`;
    const socket = new MockSocket(value => {
      const command = JSON.parse(value);
      if (command.type === 'call.hello') { assert.deepEqual(command.supportedProtocolVersions, [2]); return; }
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
      socket.open(); socket.receive({ type: 'hello', protocolVersion: this.protocolVersion, clientId: id,
        capabilities: this.capabilities,
        availability: { busy: false, ownedByThisClient: false }, conferenceInvitations: this.conferenceInvitations, currentOffer: offer, currentCall: this.currentCall, audioFormat: audio });
    });
    return socket;
  };
}
function receivePcm(socket, bytes, generation = 1, trackId = 2) {
  socket.receive({type:'audio.chunk',trackId,generation,pcmBase64:Buffer.from(bytes).toString('base64')});
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
    assert.equal(server.controls[0].sent.length, 2);
  } finally { owner.close(); }
});

test('ending stops audio but retains owner transports until terminal confirmation', async () => {
  for (const terminal of ['ended', 'uncertain']) {
    const server = new MockCallServer(); const owner = client(server);
    try {
      await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
      const media = await owner.attachMedia('call-1');
      const events = owner.events()[Symbol.asyncIterator]();
      for (const muted of [false, true, false]) {
        const control = { type: 'call.control', callId: 'call-1', participantId: 'synthetic-contact', control: { kind: 'microphone', muted } };
        server.controls[0].receive(control);
        let event;
        do { event = (await events.next()).value; } while (event.type !== 'call.control');
        assert.deepEqual(event, control);
        assert.equal(server.media[0].readyState, 1);
      }
      const snapshot = { callId: 'call-1', revision: 2, kind: 'direct', callType: 'voice', direction: 'outgoing', state: 'ending', ownedByThisClient: true };
      server.controls[0].receive({ type: 'call.state', call: snapshot });
      assert.equal(server.controls[0].readyState, 1);
      assert.equal(server.media[0].readyState, 1);
      assert.throws(() => media.sendPcm(new Int16Array(320), 1), error => error.code === 'call_ending');
      // Cross-socket data already queued before Ending must neither play nor
      // overflow a stopped consumer and detach its still-terminating owner.
      for (let i = 0; i < 20; i++) receivePcm(server.media[0], new Uint8Array(640));
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

test('PCM overrun keeps the latest chunks without detaching the owner or rewinding sample offsets', async () => {
  const server = new MockCallServer(); const owner = client(server);
  try {
    await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
    const arrivals = [];
    const media = await owner.attachMedia('call-1', arrival => arrivals.push(arrival));
    const socket = server.media[0];
    for (let i = 0; i < 20; i++) receivePcm(socket, new Uint8Array(640).fill(i));
    assert.equal(socket.readyState, 1);
    assert.equal(arrivals.length, 20);
    assert.equal(Math.max(...arrivals.map(arrival => arrival.queueDepth)), 16);
    assert.equal(arrivals.reduce((sum, arrival) => sum + arrival.droppedSamples, 0), 4 * 320);
    const iterator = media.frames()[Symbol.asyncIterator]();
    for (let i = 4; i < 20; i++) {
      const { value: frame } = await iterator.next();
      assert.equal(frame.payload[0], i);
      assert.equal(frame.sampleOffset, i * 320);
      assert.equal(frame.receivedAtMs, arrivals[i].receivedAtMs);
    }
    receivePcm(socket, new Uint8Array(640).fill(20));
    socket.receive({ type: 'track.updated', callId: 'call-1', track: { ...server.tracks.receiveTracks[0], generation: 2 } });
    receivePcm(socket, new Uint8Array(640).fill(21), 2);
    const { value: current } = await iterator.next();
    assert.equal(current.payload[0], 21);
    assert.equal(current.sampleOffset, 21 * 320);
    const pending = iterator.next();
    socket.close();
    await assert.rejects(pending, error => error.code === 'media_closed');
  } finally { owner.close(); }
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
  assert.deepEqual(JSON.parse(server.media[0].sent[1]), { type: 'audio.start', trackId: 1, generation: 1 });
  assert.deepEqual(Buffer.from(JSON.parse(server.media[0].sent[2]).pcmBase64, "base64"), Buffer.alloc(640));
  media.sendPcm(new Int16Array([-32768, 32767, -1]), 1);
  assert.deepEqual([...Buffer.from(JSON.parse(server.media[0].sent[3]).pcmBase64, "base64")], [0, 128, 255, 127, 255, 255]);
  await owner.clearAudio('clear-pcm', 'call-1');
  assert.throws(() => media.sendPcm(new Int16Array(1), 1), error => error.code === 'stale');
  media.sendPcm(new Int16Array(1), 2);
  assert.deepEqual(JSON.parse(server.media[0].sent[4]), { type: 'audio.start', trackId: 1, generation: 2 });
  receivePcm(server.media[0], new Uint8Array(3));
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
  for (const control of [{ kind: 'microphone', muted: 'yes' }, { kind: 'microphone' }]) {
    const invalidServer = new MockCallServer(); const invalidClient = client(invalidServer);
    await invalidClient.connect();
    invalidServer.controls[0].receive({ type: 'call.control', callId: 'call-1', participantId: 'synthetic-contact', control });
    assert.equal(invalidServer.controls[0].readyState, 3);
    invalidClient.close();
  }
});


test('JSON media demultiplexes tracks and preserves independent sample offsets', async () => {
  const server = new MockCallServer(); const owner = client(server);
  try {
    await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
    server.tracks.receiveTracks.push({id:3,generation:1,direction:'receive',format:audio});
    const media = await owner.attachMedia('call-1');
    receivePcm(server.media[0], new Uint8Array([1,0]), 1, 2);
    receivePcm(server.media[0], new Uint8Array([2,0]), 1, 3);
    receivePcm(server.media[0], new Uint8Array([3,0]), 1, 2);
    const frames = media.frames()[Symbol.asyncIterator]();
    for (const [trackId, sampleOffset, firstByte] of [[2,0,1],[3,0,2],[2,1,3]]) {
      const {value} = await frames.next();
      assert.equal(value.trackId, trackId);
      assert.equal(value.sampleOffset, sampleOffset);
      assert.equal(value.payload[0], firstByte);
    }
    receivePcm(server.media[0], new Uint8Array([1,0]), 1, 99);
    await assert.rejects(frames.next(), error => error.code === 'invalid_media');
  } finally { owner.close(); }
});


test('missing and incompatible server versions fail before readiness or media', async () => {
  for (const version of [undefined, 1, 3]) {
    const server = new MockCallServer(); server.protocolVersion = version;
    const owner = client(server);
    try {
      await assert.rejects(owner.connect(), error => error.code === 'unsupported_protocol_version');
      assert.deepEqual(server.controls[0].sent.map(text => JSON.parse(text).type), ['call.hello']);
    } finally { owner.close(); }
  }
  for (const version of [undefined, 1, 3]) {
    const server = new MockCallServer(); server.mediaProtocolVersion = version;
    const owner = client(server);
    try {
      await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
      await assert.rejects(owner.attachMedia('call-1'), error => error.code === 'unsupported_protocol_version');
    } finally { owner.close(); }
  }
});

test('base64 expansion permits a full decoded chunk and receive generations fence only their own track', async () => {
  const server = new MockCallServer();
  server.tracks.receiveTracks.push({id:3, generation:1, direction:'receive', format:audio});
  const owner = client(server, {maxBufferedBytes:100000});
  try {
    await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
    const media = await owner.attachMedia('call-1'); const frames = media.frames()[Symbol.asyncIterator]();
    media.sendPcm(new Int16Array(32768).fill(-32768), 1);
    const sent = JSON.parse(server.media[0].sent.at(-1));
    const pcm = Buffer.from(sent.pcmBase64, 'base64');
    assert.equal(pcm.length, 65536); assert.equal(pcm.readInt16LE(), -32768);
    receivePcm(server.media[0], new Uint8Array([1,0]), 1, 2);
    receivePcm(server.media[0], new Uint8Array([2,0]), 1, 3);
    server.media[0].receive({type:'track.updated',callId:'call-1',track:{id:2,generation:2,direction:'receive',format:audio}});
    receivePcm(server.media[0], new Uint8Array([3,0]), 1, 2);
    receivePcm(server.media[0], new Uint8Array([4,0]), 2, 2);
    const a = (await frames.next()).value; const b = (await frames.next()).value;
    assert.equal(a.trackId, 3); assert.equal(a.payload[0], 2);
    assert.equal(b.trackId, 2); assert.equal(b.payload[0], 4); assert.equal(b.sampleOffset, 1);
  } finally { owner.close(); }
});

test('conference metadata does not detach the owner and media socket owns source ordering and clock', async () => {
  const server = new MockCallServer(); const owner = client(server);
  try {
    await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
    const media = await owner.attachMedia('call-1'); const frames = media.frames()[Symbol.asyncIterator]();
    server.controls[0].receive({type:'conference.invitations', invitations:[{invitationId:'inv-1', revision:1, groupId:'test@chatroom', callerId:'contact-a'}]});
    server.controls[0].receive({type:'participant.updated', callId:'call-1', revision:3, participant:{participantId:'p-a', contactId:'contact-a', membership:'joined', microphoneEnabled:false}});
    server.controls[0].receive({type:'call.invitation_result', callId:'call-1', requestId:'invite-1', results:[{contactId:'contact-b',status:'submitted',verificationRequired:false}]});
    // Control can overtake PCM. Only the media socket removes this source.
    server.controls[0].receive({type:'track.removed', callId:'call-1', trackId:2, generation:1, reason:'participant_left'});
    server.media[0].receive({type:'audio.chunk',trackId:2,generation:1,sequence:5,sampleOffset:1600,pcmBase64:'AQA='});
    assert.equal((await frames.next()).value.sampleOffset,1600);
    server.media[0].receive({type:'track.removed', callId:'call-1',trackId:2,generation:1,reason:'participant_left'});
    assert.deepEqual(media.tracks().receiveTracks,[]);
    server.media[0].receive({type:'track.added',callId:'call-1',track:{id:2,generation:2,direction:'receive',format:audio,participantId:'p-a'}});
    server.media[0].receive({type:'audio.chunk',trackId:2,generation:2,sequence:6,sampleOffset:5000,pcmBase64:'AgA='});
    const frame=(await frames.next()).value;
    assert.equal(frame.generation,2);assert.equal(frame.sampleOffset,5000);
    assert.equal(server.controls[0].readyState,1);assert.equal(server.media[0].readyState,1);
  } finally { owner.close(); }
});

test('conference helpers use exact opaque invitations and keep submission separate from membership', async () => {
  const server = new MockCallServer(); const owner = client(server);
  try {
    await owner.connect(); await owner.ready('conference-ready', true);
    await owner.startConference('conference-create', 'group@chatroom', ['contact-a', 'contact-b']);
    await owner.joinConference('conference-join', 'opaque-invitation', 7);
    await owner.invite('conference-invite', 'call-1', ['contact-c']);
    const commands = server.controls[0].sent.map(value => JSON.parse(value));
    assert.deepEqual(commands.find(value => value.requestId === 'conference-create'), { type: 'call.dial', requestId: 'conference-create', target: { kind: 'conference', groupId: 'group@chatroom', invitees: ['contact-a', 'contact-b'] }, callType: 'voice' });
    assert.deepEqual(commands.find(value => value.requestId === 'conference-join'), { type: 'call.join', requestId: 'conference-join', invitationId: 'opaque-invitation', invitationRevision: 7 });
    assert.deepEqual(commands.find(value => value.requestId === 'conference-invite'), { type: 'call.invite', requestId: 'conference-invite', callId: 'call-1', contactIds: ['contact-c'] });
  } finally { owner.close(); }
});
test('hello validates conference invitation and participant authority fields before commands', async () => {
  const invitation = { invitationId: 'inv', revision: 1, groupId: 'group@chatroom', callerId: 'contact-a' };
  for (const invalid of [null, [{ ...invitation, revision: '1' }], [{ ...invitation, groupId: null }]]) {
    const server = new MockCallServer(); server.conferenceInvitations = invalid;
    const owner = client(server); await assert.rejects(owner.connect(), error => error.code === 'invalid_event'); owner.close();
  }
  const participant = { participantId: 'a', contactId: 'contact-a', membership: 'joined' };
  for (const participants of [[{ ...participant, microphoneEnabled: 'false' }], [participant, participant]]) {
    const server = new MockCallServer(); server.currentCall = { ...offer, direction: 'incoming', state: 'connected', ownedByThisClient: false, kind: 'conference', groupId: 'group@chatroom', participants };
    const owner = client(server); await assert.rejects(owner.connect(), error => error.code === 'invalid_event'); owner.close();
  }
});
test('received audio preserves its independent sequence and shared presentation position', async () => {
  const server = new MockCallServer(); const owner = client(server);
  try {
    await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'contact-a');
    const media = await owner.attachMedia('call-1');
    const frames = media.frames()[Symbol.asyncIterator]();
    server.media[0].receive({ type: 'audio.chunk', trackId: 2, generation: 1, sequence: 91, sampleOffset: 32000, pcmBase64: 'AQACAA==' });
    const { value } = await frames.next(); assert.equal(value.sequence, 91); assert.equal(value.sampleOffset, 32000); assert.deepEqual([...value.payload], [1, 0, 2, 0]);
  } finally { owner.close(); }
});
