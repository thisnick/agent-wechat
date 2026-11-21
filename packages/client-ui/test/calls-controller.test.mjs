import test from "node:test";
import assert from "node:assert/strict";
import { NativeApiError, NativeCallError } from "@agent-wechat/shared";
import {
  CALL_CAP_WARNING_SECONDS,
  CallController,
  canCallThread,
  formatCallDuration,
} from "../src/calls/call-controller.ts";
import { EMPTY_CALL_DEVICES } from "../src/calls/media-backend.ts";
import {
  RINGTONE_LOOP_SECONDS,
  renderMarimbaDoubleSamples,
} from "../src/calls/ringtone.ts";

function makeRingtoneSpy() {
  let playing = false;
  let muted = false;
  return {
    starts: 0,
    stops: 0,
    isPlaying: () => playing,
    isMuted: () => muted,
    isAutoplayBlocked: () => false,
    setMuted(v) {
      muted = v;
    },
    start() {
      if (!playing) {
        playing = true;
        this.starts++;
      }
    },
    stop() {
      if (playing) {
        playing = false;
        this.stops++;
      }
    },
    dispose() {
      this.stop();
    },
  };
}

function makeFakeTransport(overrides = {}) {
  let callbacks = null;
  const actions = [];
  const baseHello = {
    type: "hello",
    protocolVersion: 3,
    clientId: "client-1",
    capabilities: {
      directAudio: true,
      directVideo: true,
      conferenceCreate: true,
      conferenceJoin: true,
      conferenceVideo: true,
    },
    availability: { busy: false, ownedByThisClient: false },
    currentOffer: null,
    currentPendingOffer: null,
    currentCall: null,
    audioFormat: { sampleRate: 16000, channels: 1, frameMs: 20 },
    conferenceInvitations: [],
  };
  const transport = {
    kind: "browser",
    actions,
    emit(event) {
      callbacks?.onEvent(event);
    },
    triggerDisconnect(code = "closed") {
      callbacks?.onDisconnected(code);
    },
    async probe() {
      return {
        capabilities: baseHello.capabilities,
        availability: baseHello.availability,
        currentOffer: null,
        currentPendingOffer: null,
        currentCall: null,
        audioFormat: baseHello.audioFormat,
        conferenceInvitations: [],
      };
    },
    async connect(cb) {
      callbacks = cb;
      return baseHello;
    },
    disconnect() {
      actions.push({ type: "disconnect" });
    },
    async listDevices() {
      return { ...EMPTY_CALL_DEVICES };
    },
    async startLocalPreview() {
      return { stop() {} };
    },
    async dialDirect(requestId, contactId, options) {
      actions.push({ type: "dialDirect", requestId, contactId, options });
      return { callId: "call-1" };
    },
    async startConference(requestId, groupId, invitees, options) {
      actions.push({ type: "startConference", requestId, groupId, invitees, options });
      return { callId: "conf-1" };
    },
    async joinConference(requestId, invitationId, invitationRevision, options) {
      actions.push({ type: "joinConference", requestId, invitationId, invitationRevision, options });
      return { callId: "conf-2" };
    },
    async answer(requestId, callId, expectedRevision, callType, options) {
      actions.push({ type: "answer", requestId, callId, expectedRevision, callType, options });
    },
    async reject(requestId, callId, expectedRevision) {
      actions.push({ type: "reject", requestId, callId, expectedRevision });
    },
    async invite(requestId, callId, contactIds) {
      actions.push({ type: "invite", requestId, callId, contactIds });
    },
    async setMute(requestId, callId, muted) {
      actions.push({ type: "setMute", requestId, callId, muted });
    },
    async setCamera(requestId, callId, enabled) {
      actions.push({ type: "setCamera", requestId, callId, enabled });
    },
    setSpeakerMuted(muted) {
      actions.push({ type: "setSpeakerMuted", muted });
    },
    async switchDevices(selection) {
      actions.push({ type: "switchDevices", selection });
    },
    async end(type, requestId, callId, expectedRevision) {
      actions.push({ type, requestId, callId, expectedRevision });
    },
    stopMedia() {
      actions.push({ type: "stopMedia" });
    },
    ...overrides,
  };
  return transport;
}

test("MarimbaDouble ringtone renders exact 3s loop normalized near -12.5 dBFS", () => {
  const samples = renderMarimbaDoubleSamples(48000);
  assert.equal(samples.length, 48000 * RINGTONE_LOOP_SECONDS);
  let peak = 0;
  for (let i = 0; i < samples.length; i++) {
    const v = Math.abs(samples[i]);
    if (v > peak) peak = v;
  }
  const dbfs = 20 * Math.log10(peak);
  assert.ok(dbfs <= -12.4 && dbfs >= -12.6, `expected -12.5 dBFS peak, got ${dbfs.toFixed(2)}`);
  // Loop seam (last 0.5 s) is quiet so looping has zero click.
  let tailPeak = 0;
  for (let i = samples.length - 24000; i < samples.length; i++) {
    tailPeak = Math.max(tailPeak, Math.abs(samples[i]));
  }
  assert.equal(tailPeak, 0);
});

test("canCallThread gates filehelper, self, and capability flags", () => {
  const all = {
    directAudio: true,
    directVideo: true,
    conferenceCreate: true,
    conferenceJoin: true,
    conferenceVideo: true,
  };
  assert.deepEqual(canCallThread("filehelper", "wxid_me", all), {
    voice: false,
    video: false,
    isGroup: false,
  });
  assert.deepEqual(canCallThread("wxid_me", "wxid_me", all), {
    voice: false,
    video: false,
    isGroup: false,
  });
  assert.deepEqual(canCallThread("sim_mei", "wxid_me", all), {
    voice: true,
    video: true,
    isGroup: false,
  });
  assert.deepEqual(
    canCallThread("12345@chatroom", "wxid_me", { ...all, conferenceVideo: false }),
    { voice: true, video: false, isGroup: true },
  );
  assert.equal(formatCallDuration(0), "00:00");
  assert.equal(formatCallDuration(65), "01:05");
  assert.equal(formatCallDuration(1110), "18:30");
});

test("missing calls scope or disabled backend stays silent without connecting a socket", async (t) => {
  let connected = false;
  const forbidden = makeFakeTransport({
    async probe() {
      throw new NativeApiError(403, "forbidden");
    },
    async connect() {
      connected = true;
      throw new Error("should not connect");
    },
  });
  const c1 = new CallController({ transport: forbidden, ringtone: makeRingtoneSpy() });
  t.after(() => c1.close());
  c1.start();
  await new Promise((r) => setTimeout(r, 15));
  assert.equal(c1.snapshot().status, "unavailable");
  assert.equal(c1.snapshot().lastError, null);
  assert.equal(connected, false);

  const disabledTransport = makeFakeTransport({
    async probe() {
      return {
        capabilities: {
          directAudio: false,
          directVideo: false,
          conferenceCreate: false,
          conferenceJoin: false,
          conferenceVideo: false,
        },
        availability: { busy: false, ownedByThisClient: false },
        currentOffer: null,
        currentPendingOffer: null,
        currentCall: null,
        audioFormat: { sampleRate: 16000, channels: 1, frameMs: 20 },
        conferenceInvitations: [],
      };
    },
    async connect() {
      connected = true;
      throw new Error("should not connect");
    },
  });
  const c2 = new CallController({ transport: disabledTransport, ringtone: makeRingtoneSpy() });
  t.after(() => c2.close());
  c2.start();
  await new Promise((r) => setTimeout(r, 15));
  assert.equal(c2.snapshot().status, "disabled");
  assert.equal(connected, false);
});

test("incoming offer rings, supports local dismissal without rejecting, and stops on answer", async (t) => {
  const ringtone = makeRingtoneSpy();
  const transport = makeFakeTransport();
  const controller = new CallController({ transport, ringtone });
  t.after(() => controller.close());
  controller.start();
  await new Promise((r) => setTimeout(r, 15));
  assert.equal(controller.snapshot().status, "ready");

  // Pending offer shows preparing state without starting the ringtone yet.
  transport.emit({
    type: "call.pending",
    offer: {
      callId: "inc-1",
      revision: 1,
      kind: "direct",
      callType: "video",
      callerId: "sim_mei",
      phase: "preparing",
    },
  });
  assert.equal(controller.snapshot().currentPendingOffer?.callId, "inc-1");
  assert.equal(ringtone.isPlaying(), false);

  // Full offer starts the ringtone.
  transport.emit({
    type: "call.offered",
    offer: {
      callId: "inc-1",
      revision: 2,
      kind: "direct",
      callType: "video",
      callerId: "sim_mei",
    },
  });
  assert.equal(ringtone.isPlaying(), true);
  assert.equal(ringtone.starts, 1);

  // Local dismissal silences ringtone without sending call.reject.
  controller.dismissOfferLocally();
  assert.equal(ringtone.isPlaying(), false);
  assert.equal(controller.snapshot().dismissedOfferKey, "inc-1:2");
  assert.equal(transport.actions.filter((a) => a.type === "reject").length, 0);

  // New offer revision rings again and answering Audio Only passes cameraEnabled: false.
  transport.emit({
    type: "call.offered",
    offer: {
      callId: "inc-2",
      revision: 1,
      kind: "direct",
      callType: "video",
      callerId: "sim_mei",
    },
  });
  assert.equal(ringtone.isPlaying(), true);
  await controller.answerOffer({ cameraEnabled: false });
  assert.equal(ringtone.isPlaying(), false);
  const answer = transport.actions.find((a) => a.type === "answer");
  assert.equal(answer.callId, "inc-2");
  assert.equal(answer.options.cameraEnabled, false);
});

test("dial distinguishes cancel vs hangup, preserves sibling controls, and flags uncertain/cap", async (t) => {
  let clockMs = 1_700_000_000_000;
  const transport = makeFakeTransport();
  const controller = new CallController({
    transport,
    ringtone: makeRingtoneSpy(),
    now: () => clockMs,
  });
  t.after(() => controller.close());
  controller.start();
  await new Promise((r) => setTimeout(r, 15));

  await controller.dialDirect("sim_mei", { callType: "voice" });
  assert.equal(controller.snapshot().presentationMode, "strip");

  // Outgoing ringing uses call.cancel.
  transport.emit({
    type: "call.state",
    call: {
      callId: "call-1",
      revision: 2,
      kind: "direct",
      callType: "voice",
      direction: "outgoing",
      state: "ringing",
      ownedByThisClient: true,
      participants: [{ participantId: "p1", contactId: "sim_mei", membership: "joined" }],
      controls: [],
    },
  });
  await controller.hangupOrCancel();
  assert.equal(transport.actions.at(-1).type, "call.cancel");

  // Connected call uses call.hangup and preserves both microphone + camera controls.
  transport.emit({
    type: "call.state",
    call: {
      callId: "call-1",
      revision: 3,
      kind: "direct",
      callType: "voice",
      direction: "outgoing",
      state: "connected",
      ownedByThisClient: true,
      participants: [{ participantId: "p1", contactId: "sim_mei", membership: "joined" }],
      controls: [],
    },
  });
  transport.emit({
    type: "call.control",
    callId: "call-1",
    participantId: "p1",
    control: { kind: "microphone", muted: true },
  });
  transport.emit({
    type: "call.control",
    callId: "call-1",
    participantId: "p1",
    control: { kind: "camera", enabled: false },
  });
  const controls = controller.snapshot().currentCall.controls;
  assert.equal(controls.length, 2);
  assert.equal(controller.snapshot().currentCall.participants[0].microphoneEnabled, false);
  assert.equal(controller.snapshot().currentCall.participants[0].cameraEnabled, false);

  await controller.hangupOrCancel();
  assert.equal(transport.actions.at(-1).type, "call.hangup");

  // Advance clock past 18:30 cap threshold and wait one timer tick.
  clockMs += CALL_CAP_WARNING_SECONDS * 1000;
  await new Promise((r) => setTimeout(r, 1100));
  assert.equal(controller.snapshot().capWarning, true);

  // Uncertain terminal state records honest notice and stops media.
  transport.emit({
    type: "call.state",
    call: {
      callId: "call-1",
      revision: 4,
      kind: "direct",
      callType: "voice",
      direction: "outgoing",
      state: "uncertain",
      ownedByThisClient: true,
    },
  });
  assert.equal(controller.snapshot().currentCall, null);
  assert.equal(controller.snapshot().notice?.kind, "uncertain");
});

test("outcome_unknown command failure surfaces honest uncertain notice", async (t) => {
  const transport = makeFakeTransport({
    async dialDirect() {
      throw new NativeCallError("outcome_unknown", true);
    },
  });
  const controller = new CallController({ transport, ringtone: makeRingtoneSpy() });
  t.after(() => controller.close());
  controller.start();
  await new Promise((r) => setTimeout(r, 15));
  await controller.dialDirect("sim_mei", { callType: "voice" });
  assert.equal(controller.snapshot().lastError, "outcome_unknown");
  assert.equal(controller.snapshot().notice?.kind, "uncertain");
});
