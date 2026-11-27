import test from 'node:test';
import assert from 'node:assert/strict';
import {
  NativeCallsClient,
  NativeCallError,
  NATIVE_CALL_VIDEO_FLAG_KEYFRAME,
  NATIVE_CALL_VIDEO_FLAG_CODEC_CONFIG,
  NATIVE_CALL_VIDEO_FLAG_ROTATE_90,
} from '../src/native-calls.ts';
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
  protocolVersion = 3;
  mediaProtocolVersion = 3;
  conferenceInvitations = [];
  currentCall = null;
  sockets = []; controls = []; media = []; owner = null; counter = 0; hold = false;
  tracks = { sendTracks: [{ media: 'audio', id: 1, generation: 1, direction: 'send', format: audio }], receiveTracks: [{ media: 'audio', id: 2, generation: 1, direction: 'receive', format: audio }] };
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
          assert.equal(command.type, 'track.start');
          assert.equal(command.trackId, 1);
          assert.equal(command.generation, this.tracks.sendTracks[0].generation);
        } else {
          assert.ok(value instanceof Uint8Array);
          const decoded = decodeWxcm(value);
          assert.equal(decoded.kind, 1);
          assert.equal(decoded.trackId, 1);
          assert.equal(decoded.generation, this.tracks.sendTracks[0].generation);
        }
      });
      this.sockets.push(socket); this.media.push(socket); queueMicrotask(() => socket.open()); return socket;
    }
    const id = `client-${++this.counter}`;
    const socket = new MockSocket(value => {
      const command = JSON.parse(value);
      if (command.type === 'call.hello') { assert.deepEqual(command.supportedProtocolVersions, [4, 3]); return; }
      if (this.hold) return;
      if (command.type === 'call.ready') { socket.ready = command.ready; this.result(socket, command.requestId); }
      else if (command.type === 'call.answer' || command.type === 'call.dial') {
        if (!socket.ready) { this.result(socket, command.requestId, { status: 'error', code: 'media_not_ready' }); return; }
        if (this.owner) { this.result(socket, command.requestId, { status: 'error', code: 'already_claimed' }); return; }
        this.owner = id; this.result(socket, command.requestId, { callId: 'call-1', ownerToken: `owner-${id}` });
      } else if (command.type === 'call.clear_audio') {
        this.tracks.sendTracks[0].generation += 1;
        queueMicrotask(() => socket.receive({ type: 'track.updated', callId: 'call-1', track: structuredClone(this.tracks.sendTracks[0]) }));
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
const rxState = new WeakMap();
function encodeWxcm(trackId, generation, sequence, timestampMicros, bytes) {
  const out = new Uint8Array(36 + bytes.byteLength);
  out.set([0x57, 0x58, 0x43, 0x4d, 3, 1], 0);
  const view = new DataView(out.buffer);
  view.setUint16(6, 0, false);
  view.setUint32(8, trackId, false);
  view.setUint32(12, generation, false);
  view.setBigUint64(16, BigInt(sequence), false);
  view.setBigUint64(24, BigInt(timestampMicros), false);
  view.setUint32(32, bytes.byteLength, false);
  out.set(bytes, 36);
  return out;
}
function decodeWxcm(raw) {
  assert.ok(raw instanceof Uint8Array && raw.byteLength >= 36);
  assert.deepEqual([...raw.subarray(0, 5)], [0x57, 0x58, 0x43, 0x4d, 3]);
  const view = new DataView(raw.buffer, raw.byteOffset, raw.byteLength);
  const kind = raw[5];
  const flags = view.getUint16(6, false);
  const trackId = view.getUint32(8, false);
  const generation = view.getUint32(12, false);
  const sequence = Number(view.getBigUint64(16, false));
  const timestampMicros = Number(view.getBigUint64(24, false));
  const payloadLen = view.getUint32(32, false);
  assert.equal(payloadLen, raw.byteLength - 36);
  return { kind, flags, trackId, generation, sequence, timestampMicros, payload: raw.slice(36) };
}
function receivePcm(socket, bytes, generation = 1, trackId = 2, opts = {}) {
  let perTrack = rxState.get(socket);
  if (!perTrack) { perTrack = new Map(); rxState.set(socket, perTrack); }
  const cur = perTrack.get(trackId) ?? { sequence: 0, sampleOffset: 0 };
  const sequence = opts.sequence ?? cur.sequence;
  const sampleOffset = opts.sampleOffset ?? cur.sampleOffset;
  const timestampMicros = Math.round(sampleOffset * 1_000_000 / audio.sampleRate);
  perTrack.set(trackId, { sequence: sequence + 1, sampleOffset: sampleOffset + Math.floor(bytes.byteLength / 2) });
  socket.receive(encodeWxcm(trackId, generation, sequence, timestampMicros, bytes));
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
  assert.deepEqual(JSON.parse(server.media[0].sent[1]), { type: 'track.start', trackId: 1, generation: 1 });
  assert.deepEqual(Buffer.from(decodeWxcm(server.media[0].sent[2]).payload), Buffer.alloc(640));
  media.sendPcm(new Int16Array([-32768, 32767, -1]), 1);
  assert.deepEqual([...decodeWxcm(server.media[0].sent[3]).payload], [0, 128, 255, 127, 255, 255]);
  await owner.clearAudio('clear-pcm', 'call-1');
  assert.throws(() => media.sendPcm(new Int16Array(1), 1), error => error.code === 'stale');
  media.sendPcm(new Int16Array(1), 2);
  assert.deepEqual(JSON.parse(server.media[0].sent[4]), { type: 'track.start', trackId: 1, generation: 2 });
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


test('binary WXCM media demultiplexes tracks and preserves independent sample offsets', async () => {
  const server = new MockCallServer(); const owner = client(server);
  try {
    await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
    server.tracks.receiveTracks.push({media:'audio',id:3,generation:1,direction:'receive',format:audio});
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
  for (const version of [undefined, 1, 2]) {
    const server = new MockCallServer(); server.protocolVersion = version;
    const owner = client(server);
    try {
      await assert.rejects(owner.connect(), error => error.code === 'unsupported_protocol_version');
      assert.deepEqual(server.controls[0].sent.map(text => JSON.parse(text).type), ['call.hello']);
    } finally { owner.close(); }
  }
  for (const version of [undefined, 1, 2]) {
    const server = new MockCallServer(); server.mediaProtocolVersion = version;
    const owner = client(server);
    try {
      await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
      await assert.rejects(owner.attachMedia('call-1'), error => error.code === 'unsupported_protocol_version');
    } finally { owner.close(); }
  }
});

test('binary WXCM framing permits a full 64KiB PCM chunk and receive generations fence only their own track', async () => {
  const server = new MockCallServer();
  server.tracks.receiveTracks.push({media:'audio',id:3, generation:1, direction:'receive', format:audio});
  const owner = client(server, {maxBufferedBytes:100000});
  try {
    await owner.connect(); await owner.ready('ready', true); await owner.dial('dial', 'synthetic-contact');
    const media = await owner.attachMedia('call-1'); const frames = media.frames()[Symbol.asyncIterator]();
    media.sendPcm(new Int16Array(32768).fill(-32768), 1);
    const sent = decodeWxcm(server.media[0].sent.at(-1));
    const pcm = Buffer.from(sent.payload);
    assert.equal(pcm.length, 65536); assert.equal(pcm.readInt16LE(), -32768);
    receivePcm(server.media[0], new Uint8Array([1,0]), 1, 2);
    receivePcm(server.media[0], new Uint8Array([2,0]), 1, 3);
    server.media[0].receive({type:'track.updated',callId:'call-1',track:{media:'audio',id:2,generation:2,direction:'receive',format:audio}});
    receivePcm(server.media[0], new Uint8Array([3,0]), 1, 2);
    receivePcm(server.media[0], new Uint8Array([4,0]), 2, 2, { sampleOffset: 1 });
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
    receivePcm(server.media[0], new Uint8Array([1, 0]), 1, 2, { sequence: 5, sampleOffset: 1600 });
    assert.equal((await frames.next()).value.sampleOffset,1600);
    server.media[0].receive({type:'track.removed', callId:'call-1',trackId:2,generation:1,reason:'participant_left'});
    assert.deepEqual(media.tracks().receiveTracks,[]);
    server.media[0].receive({type:'track.added',callId:'call-1',track:{media:'audio',id:2,generation:2,direction:'receive',format:audio,participantId:'p-a'}});
    receivePcm(server.media[0], new Uint8Array([2, 0]), 2, 2, { sequence: 6, sampleOffset: 5000 });
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
    receivePcm(server.media[0], new Uint8Array([1, 0, 2, 0]), 1, 2, { sequence: 91, sampleOffset: 32000 });
    const { value } = await frames.next(); assert.equal(value.sequence, 91); assert.equal(value.sampleOffset, 32000); assert.deepEqual([...value.payload], [1, 0, 2, 0]);
  } finally { owner.close(); }
});


test('participant signaling statuses survive control snapshots and updates without becoming membership', async () => {
  const server = new MockCallServer();
  server.currentCall = {callId:'conference',revision:1,kind:'conference',callType:'voice',direction:'incoming',state:'connected',ownedByThisClient:false,
    participants:[{participantId:'p',contactId:'contact',membership:'invited',status:{code:3,kind:'acknowledged'}}]};
  const owner = client(server);
  try {
    const hello = await owner.connect();
    assert.deepEqual(hello.currentCall.participants[0].status, {code:3,kind:'acknowledged'});
    const events = owner.events()[Symbol.asyncIterator]();
    let revision = 1;
    for (const [code,kind] of [[0,'undefined'],[1,'joined'],[2,'invited'],[3,'acknowledged'],[4,'left'],[5,'cancelled'],[6,'busy'],[7,'rejected'],[8,'no_response'],[9,'unknown'],[10,'unknown'],[4294967295,'unknown']]) {
      const update = {type:'participant.updated',callId:'conference',revision:++revision,participant:{participantId:'p',contactId:'contact',membership:code===1?'joined':code===4?'left':code===2||code===3?'invited':'unknown',status:{code,kind}}};
      server.controls[0].receive(update);
      let event; do { event=(await events.next()).value; } while (event.type!=='participant.updated');
      assert.deepEqual(event,update);
    }
  } finally { owner.close(); }
});


test('invalid or contradictory participant status objects fail admission', async () => {
  for (const status of [null, {code:-1,kind:'unknown'}, {code:4294967296,kind:'unknown'}, {code:4,kind:'joined'}, {code:9,kind:'join_failed'}]) {
    const server = new MockCallServer();
    server.currentCall = {callId:'conference',revision:1,kind:'conference',callType:'voice',direction:'incoming',state:'connected',ownedByThisClient:false,
      participants:[{participantId:'p',contactId:'contact',membership:'unknown',status}]};
    const owner = client(server);
    try { await assert.rejects(owner.connect(), error => error.code==='invalid_event'); } finally { owner.close(); }
  }
});

test('video helpers exchange v3 capabilities, camera/mute controls, binary video frames and keyframes', async () => {
  const videoFmt = { codec: 'h265', mode: 'passthrough', maxWidth: 640, maxHeight: 480, maxFps: 15 };
  const server = new MockCallServer();
  server.capabilities = { directAudio: true, directVideo: true, conferenceAudio: true, conferenceVideo: true };
  server.tracks = {
    sendTracks: [
      { media: 'audio', id: 1, generation: 1, direction: 'send', format: audio },
      { media: 'video', id: 65537, generation: 1, direction: 'send', format: videoFmt, active: true },
    ],
    receiveTracks: [
      { media: 'audio', id: 2, generation: 1, direction: 'receive', format: audio },
      { media: 'video', id: 65538, generation: 1, direction: 'receive', format: videoFmt, active: true },
    ],
  };
  const origFactory = server.factory;
  const mediaCommands = [];
  const mediaFrames = [];
  server.factory = (url, options) => {
    if (!url.endsWith('/media')) return origFactory(url, options);
    let attached = false;
    const socket = new MockSocket(value => {
      if (!attached) {
        attached = true;
        queueMicrotask(() => socket.receive({ type: 'media.ready', protocolVersion: 3, ...structuredClone(server.tracks) }));
      } else if (typeof value === 'string') {
        mediaCommands.push(JSON.parse(value));
      } else {
        mediaFrames.push(decodeWxcm(value));
      }
    });
    server.sockets.push(socket); server.media.push(socket); queueMicrotask(() => socket.open());
    return socket;
  };
  const owner = client(server);
  try {
    await owner.connect();
    await owner.ready('ready-v', true, audio, undefined, { sendCodecs: ['h265'], receiveCodecs: ['h265'], maxWidth: 640, maxHeight: 480, maxFps: 15 });
    await owner.dial('dial-v', 'alice', { callType: 'video', cameraEnabled: true, muted: true });
    await owner.setMute('mute-v', 'call-1', false);
    await owner.setCamera('cam-v', 'call-1', false);
    const cmds = server.controls[0].sent.map(s => JSON.parse(s));
    assert.deepEqual(cmds.find(c => c.requestId === 'ready-v').video, { sendCodecs: ['h265'], receiveCodecs: ['h265'], maxWidth: 640, maxHeight: 480, maxFps: 15 });
    assert.deepEqual(cmds.find(c => c.requestId === 'dial-v'), { type: 'call.dial', requestId: 'dial-v', target: { kind: 'direct', contactId: 'alice' }, callType: 'video', cameraEnabled: true, muted: true });
    assert.deepEqual(cmds.find(c => c.requestId === 'mute-v'), { type: 'call.mute', requestId: 'mute-v', callId: 'call-1', muted: false });
    assert.deepEqual(cmds.find(c => c.requestId === 'cam-v'), { type: 'call.camera', requestId: 'cam-v', callId: 'call-1', enabled: false });

    const media = await owner.attachMedia('call-1');
    assert.equal(media.tracks().receiveTracks.length, 1);
    assert.equal(media.videoTracks().sendTrack.id, 65537);
    assert.equal(media.videoTracks().receiveTracks.length, 1);

    const sample = new Uint8Array([0, 0, 0, 1, 0x26, 0x01, 0xaa, 0xbb]);
    media.sendVideo(sample, 1, { keyframe: true, codecConfig: true, timestampMicros: 33333 });
    assert.deepEqual(mediaCommands[0], { type: 'track.start', trackId: 65537, generation: 1 });
    assert.equal(mediaFrames[0].kind, 2);
    assert.equal(mediaFrames[0].flags, NATIVE_CALL_VIDEO_FLAG_KEYFRAME | NATIVE_CALL_VIDEO_FLAG_CODEC_CONFIG);
    assert.equal(mediaFrames[0].trackId, 65537);
    assert.equal(mediaFrames[0].timestampMicros, 33333);
    assert.deepEqual([...mediaFrames[0].payload], [...sample]);

    media.requestKeyframe(65538);
    assert.deepEqual(mediaCommands[1], { type: 'video.request_keyframe', trackId: 65538 });

    // Receive video frame with rotation flag and keyframe request from server.
    const rxPacket = new Uint8Array(36 + sample.byteLength);
    rxPacket.set([0x57, 0x58, 0x43, 0x4d, 3, 2], 0);
    const dv = new DataView(rxPacket.buffer);
    dv.setUint16(6, NATIVE_CALL_VIDEO_FLAG_KEYFRAME | NATIVE_CALL_VIDEO_FLAG_ROTATE_90, false);
    dv.setUint32(8, 65538, false);
    dv.setUint32(12, 1, false);
    dv.setBigUint64(16, 7n, false);
    dv.setBigUint64(24, 66666n, false);
    dv.setUint32(32, sample.byteLength, false);
    rxPacket.set(sample, 36);
    server.media[0].receive(rxPacket);
    const vframes = media.videoFrames()[Symbol.asyncIterator]();
    const { value: vf } = await vframes.next();
    assert.equal(vf.trackId, 65538);
    assert.equal(vf.codec, 'h265');
    assert.equal(vf.keyframe, true);
    assert.equal(vf.codecConfig, false);
    assert.equal(vf.rotationDegrees, 90);
    assert.equal(vf.sequence, 7);
    assert.deepEqual([...vf.payload], [...sample]);

    server.media[0].receive({ type: 'video.keyframe_requested', callId: 'call-1', trackId: 65537, generation: 1 });
    const kfs = media.keyframeRequests()[Symbol.asyncIterator]();
    const { value: kf } = await kfs.next();
    assert.deepEqual(kf, { callId: 'call-1', trackId: 65537, generation: 1 });
  } finally { owner.close(); }
});

test('ticketProvider authenticates control and media sockets via ?ticket= without bearer header', async () => {
  const controlTicket = 'a'.repeat(64);
  const mediaTicket = 'b'.repeat(64);
  const requested = [];
  const seenUrls = [];
  const server = new MockCallServer();
  const owner = new NativeCallsClient({
    baseUrl: 'http://127.0.0.1:6174',
    accountId: 'test-account',
    ticketProvider: async (socket, callId) => {
      requested.push({ socket, callId });
      return socket === 'control' ? controlTicket : mediaTicket;
    },
    webSocketFactory: (url, options) => {
      seenUrls.push({ url, headers: options.headers });
      assert.deepEqual(options.headers, {});
      const parsed = new URL(url);
      if (parsed.pathname.endsWith('/media')) {
        assert.equal(parsed.searchParams.get('ticket'), mediaTicket);
        return server.factory(url.replace(/\?.*$/, ''), { ...options, headers: { Authorization: 'Bearer synthetic-api-token' } });
      }
      assert.equal(parsed.searchParams.get('ticket'), controlTicket);
      return server.factory(url.replace(/\?.*$/, ''), { ...options, headers: { Authorization: 'Bearer synthetic-api-token' } });
    },
  });
  try {
    await owner.connect();
    await owner.ready('r', true);
    await owner.dial('d', 'alice');
    await owner.attachMedia('call-1');
    assert.deepEqual(requested, [{ socket: 'control', callId: undefined }, { socket: 'media', callId: 'call-1' }]);
    assert.equal(seenUrls.length, 2);
  } finally { owner.close(); }
});

test('default maxBufferedBytes permits legal max PCM and video frames and sendVideo validates inputs', async () => {
  const videoFmt = { codec: 'h265', mode: 'passthrough', maxWidth: 640, maxHeight: 480, maxFps: 15 };
  const server = new MockCallServer();
  server.capabilities = { directAudio: true, directVideo: true };
  server.tracks = {
    sendTracks: [
      { media: 'audio', id: 1, generation: 1, direction: 'send', format: audio },
      { media: 'video', id: 65537, generation: 1, direction: 'send', format: videoFmt, active: true },
    ],
    receiveTracks: [
      { media: 'audio', id: 2, generation: 1, direction: 'receive', format: audio },
      { media: 'video', id: 65538, generation: 1, direction: 'receive', format: videoFmt, active: true },
    ],
  };
  const origFactory = server.factory;
  const mediaFrames = [];
  server.factory = (url, options) => {
    if (!url.endsWith('/media')) return origFactory(url, options);
    let attached = false;
    const socket = new MockSocket(value => {
      if (!attached) {
        attached = true;
        queueMicrotask(() => socket.receive({ type: 'media.ready', protocolVersion: 3, ...structuredClone(server.tracks) }));
      } else if (value instanceof Uint8Array) {
        mediaFrames.push(decodeWxcm(value));
      }
    });
    server.sockets.push(socket); server.media.push(socket); queueMicrotask(() => socket.open());
    return socket;
  };
  // Use default maxBufferedBytes (64 KiB) without overriding it.
  const owner = client(server);
  try {
    await owner.connect();
    await owner.ready('r-max', true, audio, undefined, { sendCodecs: ['h265'], receiveCodecs: ['h265'] });
    await owner.dial('d-max', 'alice', { callType: 'video', cameraEnabled: true });
    const media = await owner.attachMedia('call-1');

    // Full 64 KiB PCM chunk + initial track.start succeeds on an idle socket.
    media.sendPcm(new Int16Array(32768).fill(1234), 1);
    assert.equal(mediaFrames.at(-1).payload.byteLength, 65536);

    // Full 512 KiB video frame + initial track.start succeeds on an idle socket.
    const maxVideo = new Uint8Array(512 * 1024);
    maxVideo.set([0, 0, 0, 1, 0x26, 0x01], 0);
    media.sendVideo(maxVideo, 1, { keyframe: true, timestampMicros: 1000 });
    assert.equal(mediaFrames.at(-1).payload.byteLength, 512 * 1024);

    // Backpressure still triggers when the socket already has buffered bytes past the floor.
    server.media[0].bufferedAmount = 524 * 1024;
    assert.throws(() => media.sendVideo(new Uint8Array([0, 0, 0, 1, 0x02, 0x01]), 1), error => error.code === 'media_backpressure');
    server.media[0].bufferedAmount = 0;

    // Input validation checks on sendVideo.
    const validNal = new Uint8Array([0, 0, 0, 1, 0x02, 0x01]);
    assert.throws(() => media.sendVideo(validNal, 2), error => error.code === 'stale');
    assert.throws(() => media.sendVideo(new Uint8Array(0), 1), error => error.code === 'invalid_media');
    assert.throws(() => media.sendVideo(new Uint8Array(512 * 1024 + 1), 1), error => error.code === 'invalid_media');
    assert.throws(() => media.sendVideo(validNal, 1, { codec: 'h264' }), error => error.code === 'invalid_media');
    assert.throws(() => media.sendVideo(validNal, 1, { timestampMicros: -1 }), error => error.code === 'invalid_media');
    assert.throws(() => media.sendVideo(validNal, 1, { timestampMicros: Number.MAX_SAFE_INTEGER + 1 }), error => error.code === 'invalid_media');
    assert.throws(() => media.sendVideo(validNal, 1, { rotationDegrees: 45 }), error => error.code === 'invalid_media');
  } finally { owner.close(); }
});

test('call.peer_status is delivered and unknown future server event types are ignored safely', async () => {
  const server = new MockCallServer();
  const owner = client(server);
  try {
    await owner.connect();
    await owner.ready('ready-unknown', true);
    await owner.dial('dial-unknown', 'synthetic-contact');
    const media = await owner.attachMedia('call-1');
    const events = owner.events()[Symbol.asyncIterator]();

    // Unknown future events on both control and media sockets are ignored without closing.
    server.controls[0].receive({ type: 'call.future_notice', callId: 'call-1', extra: { ok: true } });
    server.media[0].receive({ type: 'media.future_hint', callId: 'call-1', bitrateKbps: 500 });
    assert.equal(server.controls[0].readyState, 1);
    assert.equal(server.media[0].readyState, 1);

    // call.peer_status is validated and delivered in order.
    const ack = {
      type: 'call.peer_status',
      callId: 'call-1',
      participantId: 'synthetic-contact',
      status: { code: 3, kind: 'acknowledged' },
    };
    server.controls[0].receive(ack);
    let event;
    do { event = (await events.next()).value; } while (event.type !== 'call.peer_status');
    assert.deepEqual(event, ack);

    // Malformed call.peer_status still fails closed.
    server.controls[0].receive({
      type: 'call.peer_status',
      callId: 'call-1',
      participantId: 'synthetic-contact',
      status: { code: 3, kind: 'joined' },
    });
    assert.equal(server.controls[0].readyState, 3);
    media.close();
  } finally { owner.close(); }

  // Unknown event before hello must fail closed with missing_hello.
  const preHelloServer = {
    factory: () => {
      const socket = new MockSocket(() => {});
      queueMicrotask(() => {
        socket.open();
        socket.receive({ type: 'call.future_before_hello' });
      });
      return socket;
    },
  };
  const preHelloClient = client(preHelloServer);
  await assert.rejects(preHelloClient.connect(), error => error.code === 'missing_hello');
  preHelloClient.close();

  // Control socket accepts both protocolVersion 4 and legacy 3.
  for (const version of [3, 4]) {
    const versioned = new MockCallServer();
    versioned.protocolVersion = version;
    const versionedClient = client(versioned);
    const hello = await versionedClient.connect();
    assert.equal(hello.protocolVersion, version);
    versionedClient.close();
  }
});
