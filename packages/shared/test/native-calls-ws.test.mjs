import test from 'node:test';
import assert from 'node:assert/strict';
import WebSocket, { WebSocketServer } from 'ws';
import { once } from 'node:events';
import { NativeCallsClient, encodeNativeCallPcm, decodeNativeCallPcm } from '../src/native-calls.ts';

// This server emulates the authenticated daemon API, never a WeChat endpoint.
test('headless SDK exchanges authenticated control and PCM over real loopback WebSockets', async () => {
  const format = { sampleRate: 16000, channels: 1, frameMs: 20 };
  const sendTrack = { id: 1, generation: 1, direction: 'send', format };
  const receiveTrack = { id: 2, generation: 1, direction: 'receive', format };
  let receivedFrames = 0; let connection = 0; let owner = null;
  const server = new WebSocketServer({ host: '127.0.0.1', port: 0, maxPayload: 65536,
    verifyClient: ({ req }) => req.headers.authorization === 'Bearer synthetic-token' });
  await once(server, 'listening');
  const port = server.address().port;
  server.on('connection', (socket, request) => {
    if (request.url.endsWith('/media')) {
      let attached = false;
      socket.on('message', (data, binary) => {
        if (!attached) {
          assert.equal(binary, false);
          const attach = JSON.parse(data.toString());
          assert.deepEqual(attach, { type: 'media.attach', clientId: owner, ownerToken: 'synthetic-owner-token' });
          attached = true;
          socket.send(JSON.stringify({ type: 'media.ready', sendTrack, receiveTrack }));
          return;
        }
        assert.equal(binary, true);
        const incoming = decodeNativeCallPcm(data, format);
        assert.equal(incoming.trackId, sendTrack.id); receivedFrames += 1;
        const pcm = new Int16Array(format.sampleRate * format.frameMs / 1000);
        const view = new DataView(incoming.payload.buffer, incoming.payload.byteOffset, incoming.payload.byteLength);
        for (let i = 0; i < pcm.length; i++) pcm[i] = view.getInt16(i * 2, true);
        socket.send(encodeNativeCallPcm({ trackId: receiveTrack.id, generation: receiveTrack.generation, sequence: incoming.sequence, timestampMicros: incoming.timestampMicros }, format, pcm));
      });
      return;
    }
    const id = `client-${++connection}`;
    socket.send(JSON.stringify({ type: 'hello', protocolVersion: 1, clientId: id,
      capabilities: { directAudio: true, directVideo: false, conferenceAudio: false, conferenceVideo: false },
      availability: { busy: false, ownedByThisClient: false }, currentOffer: null, currentCall: null, audioFormat: format }));
    socket.on('message', data => {
      const command = JSON.parse(data.toString());
      const result = { type: 'command.result', requestId: command.requestId, status: 'accepted', callId: null, ownerToken: null, code: null };
      if (command.type === 'call.dial') {
        if (owner) { result.status = 'error'; result.code = 'busy'; }
        else { owner = id; result.callId = 'synthetic-call'; result.ownerToken = 'synthetic-owner-token'; }
      }
      socket.send(JSON.stringify(result));
    });
  });
  const factory = (url, options) => new WebSocket(url, { headers: options.headers, maxPayload: options.maxPayload });
  const options = { baseUrl: `http://127.0.0.1:${port}`, accountId: 'synthetic-account', token: 'synthetic-token', webSocketFactory: factory };
  const client = new NativeCallsClient(options); const other = new NativeCallsClient(options);
  try {
    await client.connect(); await other.connect();
    await client.ready('ready', true); await client.dial('dial', 'synthetic-contact');
    await assert.rejects(other.dial('dial-other', 'synthetic-contact'), error => error.code === 'busy');
    const media = await client.attachMedia('synthetic-call');
    const frames = media.frames()[Symbol.asyncIterator]();
    const samples = Int16Array.from({ length: 320 }, (_, index) => Math.round(Math.sin(index * 2 * Math.PI * 880 / 16000) * 4000));
    media.sendPcm({ trackId: 1, generation: 1, sequence: 0n, timestampMicros: 0n }, samples);
    const received = await frames.next();
    assert.equal(received.value.payload.byteLength, samples.byteLength);
    assert.equal(receivedFrames, 1);
    const view = new DataView(received.value.payload.buffer, received.value.payload.byteOffset, received.value.payload.byteLength);
    assert.deepEqual(Array.from({ length: 320 }, (_, i) => view.getInt16(i * 2, true)), [...samples]);
  } finally {
    client.close(); other.close();
    for (const socket of server.clients) socket.terminate();
    await new Promise(resolve => server.close(resolve));
  }
});

test('late candidate A readiness stays pinned after cancellation and outgoing readiness cannot ACK B', async () => {
  const format = { sampleRate: 16000, channels: 1, frameMs: 20 };
  const pending = id => ({ callId: id, revision: 1, kind: 'direct', callerId: 'synthetic-peer', offeredMedia: { audio: true, video: false }, phase: 'waiting_for_ready' });
  let acknowledgements = 0;
  const currentCandidate = 'candidate-b';
  const server = new WebSocketServer({ host: '127.0.0.1', port: 0,
    verifyClient: ({ req }) => req.headers.authorization === 'Bearer synthetic-token' });
  await once(server, 'listening');
  server.on('connection', socket => {
    socket.send(JSON.stringify({ type: 'hello', protocolVersion: 1, clientId: 'scoped-client',
      capabilities: { directAudio: true, directVideo: false, conferenceAudio: false, conferenceVideo: false },
      availability: { busy: false, ownedByThisClient: false }, currentOffer: null, currentPendingOffer: pending('candidate-a'), currentCall: null, audioFormat: format }));
    socket.send(JSON.stringify({ type: 'call.pending', offer: null }));
    socket.send(JSON.stringify({ type: 'call.pending', offer: pending(currentCandidate) }));
    socket.on('message', data => {
      const command = JSON.parse(data.toString());
      assert.equal(command.type, 'call.ready');
      if (!command.scope || command.scope.kind === 'all' || (command.scope.kind === 'offer' && command.scope.callId === currentCandidate)) acknowledgements += 1;
      assert.notEqual(command.scope?.kind, 'all');
      const expired = command.scope?.kind === 'offer';
      if (expired) {
        assert.deepEqual(command.scope, { kind: 'offer', callId: 'candidate-a', expectedRevision: 1 });
      } else assert.equal(command.scope?.kind, 'outgoing');
      socket.send(JSON.stringify({ type: 'command.result', requestId: command.requestId,
        status: expired ? 'error' : 'accepted', callId: null, ownerToken: null, code: expired ? 'offer_expired' : null }));
    });
  });
  const client = new NativeCallsClient({ baseUrl: `http://127.0.0.1:${server.address().port}`, accountId: 'synthetic-account', token: 'synthetic-token',
    webSocketFactory: (url, options) => new WebSocket(url, { headers: options.headers, maxPayload: options.maxPayload }) });
  try {
    const hello = await client.connect();
    assert.equal(hello.currentOffer, null);
    const original = hello.currentPendingOffer;
    await assert.rejects(client.ready('pin-a', true, format, { kind: 'offer', callId: original.callId, expectedRevision: original.revision }), error => error.code === 'offer_expired');
    await client.ready('outgoing-only', true, format, { kind: 'outgoing' });
    const events = client.events()[Symbol.asyncIterator]();
    assert.equal((await events.next()).value.type, 'hello');
    assert.equal((await events.next()).value.offer, null);
    const replacement = (await events.next()).value;
    assert.equal(replacement.type, 'call.pending');
    assert.equal(replacement.offer.callId, 'candidate-b');
    assert.equal(original.callId, 'candidate-a');
    assert.equal(acknowledgements, 0);
  } finally {
    client.close();
    for (const socket of server.clients) socket.terminate();
    await new Promise(resolve => server.close(resolve));
  }
});
