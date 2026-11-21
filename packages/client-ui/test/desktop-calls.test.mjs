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
      return HELLO;
    }
    if (cmd === "call_dial") return { callId: "call-99" };
    if (cmd === "call_preview_start") {
      previewChannel = args.channel;
      return null;
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

  assert.equal(received.remoteFrames.length, 1);
  assert.equal(received.remoteFrames[0].rotationDegrees, 90);
  assert.deepEqual([...received.remoteFrames[0].jpegBytes], [0xff, 0xd8, 0xff, 0xd9]);
  assert.deepEqual([...received.localJpegs[0]], [0xff, 0xd8, 0xff, 0xd9]);
  assert.equal(received.levels[0].micLevel, 0.4);
  assert.deepEqual(received.cleared, [65538]);

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

  // Preview streams decoded JPEG frames and stops cleanly.
  const preview = await transport.startLocalPreview("0");
  const previewFrames = [];
  const unsub = preview.subscribeJpeg((bytes) => previewFrames.push(bytes));
  previewChannel.onmessage({ type: "frame", jpeg: sampleJpeg });
  assert.equal(previewFrames.length, 1);
  unsub();
  preview.stop();
  assert.ok(calls.some((c) => c.cmd === "call_preview_stop"));

  const dial = await transport.dialDirect("req-1", "alice", { callType: "video" });
  assert.equal(dial.callId, "call-99");
  transport.disconnect();
  assert.ok(calls.some((c) => c.cmd === "call_disconnect"));
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

test("CallController suppresses Web Audio ringtone on Desktop while window is unfocused", async () => {
  let focused = false;
  let ringStarted = 0;
  let ringStopped = 0;
  let callbacks = null;

  const fakeRingtone = {
    start() {
      ringStarted += 1;
    },
    stop() {
      ringStopped += 1;
    },
    setMuted() {},
    dispose() {},
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
    isWindowFocused: () => focused,
  });
  controller.start();
  await new Promise((resolve) => setTimeout(resolve, 10));

  // Unfocused Desktop window: OS notification handles AgentWechatSoftRing.wav, so Web Audio must NOT start.
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
  assert.equal(ringStarted, 0);
  assert.ok(ringStopped > 0);

  // Once window is focused, in-app Web Audio ringtone starts on next offer/sync.
  focused = true;
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
  assert.equal(ringStarted, 1);

  controller.close();
});
