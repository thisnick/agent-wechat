import test from "node:test";
import assert from "node:assert/strict";
import { NativeCallError } from "@agent-wechat/shared";
import { CallController } from "../src/calls/call-controller.ts";
import {
  DesktopCallTransport,
  decodeBase64Bytes,
} from "../src/calls/desktop-transport.ts";

class FakeChannel {
  onmessage = () => {};
}

const HELLO = {
  type: "hello",
  protocolVersion: 3,
  clientId: "desktop-1",
  capabilities: {
    directAudio: true,
    directVideo: true,
    conferenceCreate: true,
    conferenceJoin: true,
    conferenceInvite: true,
    conferenceVideo: true,
  },
  availability: { busy: false, ownedByThisClient: false },
  currentOffer: null,
  currentPendingOffer: null,
  currentCall: null,
  audioFormat: { sampleRate: 16000, channels: 1, frameMs: 20 },
  conferenceInvitations: [],
};

test("DesktopCallTransport bridges Tauri IPC commands, channels, base64 JPEG frames, and fullscreen ownership", async () => {
  const calls = [];
  let activeChannel = null;
  let previewChannel = null;

  const invoke = async (cmd, args = {}) => {
    calls.push({ cmd, args });
    if (cmd === "call_connect") {
      activeChannel = args.channel;
      return { sessionId: 7, hello: HELLO };
    }
    if (cmd === "call_dial") return { callId: "call-99" };
    if (cmd === "call_preview_start") {
      previewChannel = args.channel;
      return 11;
    }
    if (cmd === "call_devices") {
      return {
        audioInputs: [{ id: "mic-1", label: "USB Mic", kind: "audioinput", isDefault: true }],
        audioOutputs: [{ id: "spk-1", label: "Headset", kind: "audiooutput", isDefault: true }],
        videoInputs: [{ id: "0", label: "FaceTime HD", kind: "videoinput", isDefault: true }],
        permissionState: "granted",
        supportsOutputSelection: true,
      };
    }
    if (cmd === "call_set_fullscreen") {
      return Boolean(args.enabled);
    }
    return null;
  };

  const client = {
    async callSnapshot(account) {
      assert.equal(account, "acct-1");
      return {
        capabilities: HELLO.capabilities,
        availability: HELLO.availability,
        currentOffer: null,
        currentPendingOffer: null,
        currentCall: null,
        audioFormat: HELLO.audioFormat,
        conferenceInvitations: [],
      };
    },
  };

  const transport = new DesktopCallTransport("conn-1", "acct-1", client, {
    invoke,
    createCallChannel: () => new FakeChannel(),
    createPreviewChannel: () => new FakeChannel(),
  });

  const received = {
    events: [],
    tracks: [],
    remoteFrames: [],
    localJpegs: [],
    cleared: [],
    levels: [],
    errors: [],
    osAlerts: [],
    disconnected: [],
  };

  const hello = await transport.connect({
    onEvent: (e) => received.events.push(e),
    onDisconnected: (code) => received.disconnected.push(code),
    onTracksSynced: (tracks) => received.tracks.push(tracks),
    onLocalStream: () => {},
    onLocalJpeg: (bytes) => received.localJpegs.push(bytes),
    onRemoteFrame: (frame) => received.remoteFrames.push(frame),
    onTrackCleared: (id) => received.cleared.push(id),
    onLevels: (levels) => received.levels.push(levels),
    onMediaError: (code, msg) => received.errors.push({ code, msg }),
    onOsCallAlert: (callId) => received.osAlerts.push(callId),
  });
  assert.equal(hello.clientId, "desktop-1");

  // Push IPC packets through the fake channel.
  const sampleJpeg = Buffer.from([0xff, 0xd8, 0xff, 0xd9]).toString("base64");
  activeChannel.onmessage({
    type: "remoteFrame",
    trackId: 65538,
    participantId: "peer-1",
    rotationDegrees: 90,
    jpeg: sampleJpeg,
  });
  activeChannel.onmessage({ type: "localJpeg", jpeg: sampleJpeg });
  activeChannel.onmessage({
    type: "levels",
    micLevel: 0.4,
    speakerLevel: 0.2,
    trackLevels: { 2: 0.6 },
  });
  activeChannel.onmessage({ type: "trackCleared", trackId: 65538 });
  activeChannel.onmessage({ type: "osCallAlert", callId: "offer-9" });
  activeChannel.onmessage({ type: "osCallAlert", callId: null });

  assert.equal(received.remoteFrames.length, 1);
  assert.equal(received.remoteFrames[0].rotationDegrees, 90);
  assert.deepEqual([...received.remoteFrames[0].jpegBytes], [0xff, 0xd8, 0xff, 0xd9]);
  assert.deepEqual([...received.localJpegs[0]], [0xff, 0xd8, 0xff, 0xd9]);
  assert.equal(received.levels[0].micLevel, 0.4);
  assert.deepEqual(received.cleared, [65538]);
  assert.deepEqual(received.osAlerts, ["offer-9", null]);

  // Exiting fullscreen when we never entered it is a no-op (never clobbers user window state).
  assert.equal(await transport.setFullscreen(false), false);
  assert.equal(calls.filter((c) => c.cmd === "call_set_fullscreen").length, 0);

  // Entering fullscreen tracks ownership and stopMedia exits it cleanly.
  assert.equal(await transport.setFullscreen(true), true);
  transport.stopMedia();
  assert.equal(
    calls.filter((c) => c.cmd === "call_set_fullscreen" && c.args.enabled === false).length,
    1,
  );

  // Preview streams decoded JPEG frames and stops only its own generation.
  const preview = await transport.startLocalPreview("0");
  const previewFrames = [];
  const unsub = preview.subscribeJpeg((bytes) => previewFrames.push(bytes));
  previewChannel.onmessage({ type: "frame", jpeg: sampleJpeg });
  assert.equal(previewFrames.length, 1);
  unsub();
  preview.stop();
  assert.ok(
    calls.some((c) => c.cmd === "call_preview_stop" && c.args.generation === 11),
  );

  const dial = await transport.dialDirect("req-1", "alice", { callType: "video" });
  assert.equal(dial.callId, "call-99");
  transport.disconnect();
  assert.ok(
    calls.some(
      (c) =>
        c.cmd === "call_disconnect" &&
        c.args.connectionId === "conn-1" &&
        c.args.accountId === "acct-1" &&
        c.args.sessionId === 7,
    ),
  );
});

test("DesktopCallTransport maps forbidden and outcome_unknown errors into NativeCallError", async () => {
  const client = { async callSnapshot() { throw new Error("unused"); } };
  const forbiddenTransport = new DesktopCallTransport("conn-1", "acct-1", client, {
    invoke: async () => {
      throw "forbidden";
    },
    createCallChannel: () => new FakeChannel(),
  });
  await assert.rejects(
    () =>
      forbiddenTransport.connect({
        onEvent: () => {},
        onDisconnected: () => {},
        onTracksSynced: () => {},
        onLocalStream: () => {},
        onLocalJpeg: () => {},
        onRemoteFrame: () => {},
        onTrackCleared: () => {},
        onLevels: () => {},
        onMediaError: () => {},
      }),
    (err) => err instanceof NativeCallError && err.code === "forbidden",
  );

  const unknownTransport = new DesktopCallTransport("conn-1", "acct-1", client, {
    invoke: async () => {
      throw "outcome_unknown";
    },
    createCallChannel: () => new FakeChannel(),
  });
  await assert.rejects(
    () => unknownTransport.dialDirect("req-1", "alice", { callType: "voice" }),
    (err) => err instanceof NativeCallError && err.outcomeUnknown === true,
  );
  assert.deepEqual([...decodeBase64Bytes(Buffer.from("hi").toString("base64"))], [104, 105]);
});

test("DesktopCallTransport scopes preview stop by generation so a stale start cannot kill a newer preview", async () => {
  let nextGen = 0;
  let activeGen = 0;
  const stops = [];
  let resolveA;

  const invoke = async (cmd, args = {}) => {
    if (cmd === "call_preview_start") {
      const gen = ++nextGen;
      activeGen = gen;
      if (gen === 1) {
        return await new Promise((resolve) => {
          resolveA = () => resolve(gen);
        });
      }
      return gen;
    }
    if (cmd === "call_preview_stop") {
      stops.push(args.generation);
      if (args.generation === activeGen) {
        activeGen = 0;
      }
      return null;
    }
    return null;
  };

  const client = { async callSnapshot() { throw new Error("unused"); } };
  const transport = new DesktopCallTransport("conn-1", "acct-1", client, {
    invoke,
    createPreviewChannel: () => new FakeChannel(),
  });

  // Start preview A (still booting), then switch camera to start preview B.
  const startA = transport.startLocalPreview("cam-a");
  const surfaceB = await transport.startLocalPreview("cam-b");
  assert.equal(activeGen, 2);

  // Preview A resolves late and its cancelled cleanup calls stop(); B must remain active.
  resolveA();
  const surfaceA = await startA;
  surfaceA.stop();
  assert.deepEqual(stops, [1]);
  assert.equal(activeGen, 2, "stale preview A stop must not tear down preview B");

  // Stopping B tears down generation 2.
  surfaceB.stop();
  assert.deepEqual(stops, [1, 2]);
  assert.equal(activeGen, 0);
});

test("CallController suppresses Web Audio ringtone on Desktop only while Rust confirms a sounding OS alert", async () => {
  let playing = false;
  let ringStarts = 0;
  let ringStops = 0;
  let callbacks = null;

  const fakeRingtone = {
    start() {
      if (!playing) ringStarts += 1;
      playing = true;
    },
    stop() {
      if (playing) ringStops += 1;
      playing = false;
    },
    setMuted() {},
    dispose() {
      playing = false;
    },
  };

  const fakeTransport = {
    kind: "desktop",
    async probe() {
      return {
        capabilities: HELLO.capabilities,
        availability: HELLO.availability,
        currentOffer: null,
        currentPendingOffer: null,
        currentCall: null,
        audioFormat: HELLO.audioFormat,
        conferenceInvitations: [],
      };
    },
    async connect(cb) {
      callbacks = cb;
      return HELLO;
    },
    disconnect() {},
    async listDevices() {
      return {
        audioInputs: [],
        audioOutputs: [],
        videoInputs: [],
        permissionState: "granted",
        supportsOutputSelection: true,
      };
    },
    async startLocalPreview() {
      return { stop() {} };
    },
    async dialDirect() { return { callId: "c1" }; },
    async startConference() { return { callId: "c1" }; },
    async joinConference() { return { callId: "c1" }; },
    async answer() {},
    async attachConnectedMedia() {},
    async reject() {},
    async invite() {},
    async setMute() {},
    async setCamera() {},
    setSpeakerMuted() {},
    async switchDevices() {},
    async end() {},
    stopLocalMedia() {},
    stopMedia() {},
  };

  const controller = new CallController({
    transport: fakeTransport,
    ringtone: fakeRingtone,
  });
  controller.start();
  await new Promise((resolve) => setTimeout(resolve, 10));

  // Case 1: Offer arrives with no confirmed sounding OS alert (window focused, or unfocused
  // with desktop notifications/sound disabled) -> in-app ringtone rings and never goes silent
  // if the user switches away without an OS alert.
  callbacks.onEvent({
    type: "call.offered",
    offer: {
      callId: "offer-1",
      revision: 1,
      kind: "direct",
      callType: "voice",
      callerId: "alice",
    },
  });
  assert.equal(playing, true);
  assert.equal(ringStarts, 1);

  // Case 2: Rust confirms a sounding OS alert for a DIFFERENT callId -> must not silence offer-1.
  callbacks.onOsCallAlert("offer-other");
  assert.equal(playing, true);

  // Case 3: Rust confirms a sounding OS alert for offer-1 -> suppresses in-app Web Audio ringtone.
  callbacks.onOsCallAlert("offer-1");
  assert.equal(playing, false);
  assert.equal(ringStops, 1);

  // Offer revision bump for the same call stays suppressed while the OS alert is still ringing.
  callbacks.onEvent({
    type: "call.offered",
    offer: {
      callId: "offer-1",
      revision: 2,
      kind: "direct",
      callType: "voice",
      callerId: "alice",
    },
  });
  assert.equal(playing, false);

  // Case 4: User focuses the Desktop window and Rust clears the OS alert -> in-app ringtone resumes.
  callbacks.onOsCallAlert(null);
  assert.equal(playing, true);
  assert.equal(ringStarts, 2);

  controller.close();
});

test("DesktopCallTransport coalesces video frame acks per animation frame and cancels on stopMedia", async () => {
  const invoked = [];
  let activeChannel = null;
  let nextRafId = 1;
  const rafQueue = new Map();
  const prevRaf = globalThis.requestAnimationFrame;
  const prevCancelRaf = globalThis.cancelAnimationFrame;
  globalThis.requestAnimationFrame = (cb) => {
    const id = nextRafId++;
    rafQueue.set(id, cb);
    return id;
  };
  globalThis.cancelAnimationFrame = (id) => {
    rafQueue.delete(id);
  };

  try {
    const transport = new DesktopCallTransport(
      "conn-ack",
      "acct-ack",
      { async callSnapshot() { return {}; } },
      {
        invoke: async (cmd, args = {}) => {
          invoked.push({ cmd, args });
          if (cmd === "call_connect") {
            activeChannel = args.channel;
            return { sessionId: 1, hello: HELLO };
          }
          return null;
        },
        createCallChannel: () => new FakeChannel(),
        createPreviewChannel: () => new FakeChannel(),
      },
    );

    await transport.connect({
      onEvent: () => {},
      onDisconnected: () => {},
      onTracksSynced: () => {},
      onLocalStream: () => {},
      onLocalJpeg: () => {},
      onRemoteFrame: () => {},
      onTrackCleared: () => {},
      onLevels: () => {},
      onMediaError: () => {},
    });

    const sampleJpeg = Buffer.from([0xff, 0xd8, 0xff, 0xd9]).toString("base64");
    activeChannel.onmessage({
      type: "remoteFrame",
      trackId: 65538,
      participantId: "peer-1",
      rotationDegrees: 0,
      jpeg: sampleJpeg,
    });
    activeChannel.onmessage({
      type: "remoteFrame",
      trackId: 65539,
      participantId: "peer-2",
      rotationDegrees: 0,
      jpeg: sampleJpeg,
    });
    activeChannel.onmessage({ type: "localJpeg", jpeg: sampleJpeg });

    assert.equal(rafQueue.size, 1, "multiple video packets in one paint frame schedule a single rAF ack");
    assert.equal(invoked.filter((c) => c.cmd === "call_video_ack").length, 0);

    for (const [id, cb] of [...rafQueue.entries()]) {
      rafQueue.delete(id);
      cb(16);
    }
    const acks = invoked.filter((c) => c.cmd === "call_video_ack");
    assert.equal(acks.length, 1);
    assert.deepEqual(acks[0].args, { connectionId: "conn-ack", accountId: "acct-ack" });

    // A pending ack scheduled right before stopMedia must be cancelled.
    activeChannel.onmessage({ type: "localJpeg", jpeg: sampleJpeg });
    assert.equal(rafQueue.size, 1);
    transport.stopMedia();
    assert.equal(rafQueue.size, 0);
    assert.equal(invoked.filter((c) => c.cmd === "call_video_ack").length, 1);

    // Without requestAnimationFrame (microtask fallback), stopMedia also cancels the pending ack.
    globalThis.requestAnimationFrame = undefined;
    globalThis.cancelAnimationFrame = undefined;
    activeChannel.onmessage({ type: "localJpeg", jpeg: sampleJpeg });
    transport.stopMedia();
    await Promise.resolve();
    assert.equal(invoked.filter((c) => c.cmd === "call_video_ack").length, 1);
  } finally {
    globalThis.requestAnimationFrame = prevRaf;
    globalThis.cancelAnimationFrame = prevCancelRaf;
  }
});
