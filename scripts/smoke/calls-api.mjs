// Actual Rust API, shared SDK and OpenClaw adapter; all call/provider data is synthetic.
// Build wx-calls-fixture with wechat-simulator's calls-fixture feature, then:
// node --experimental-strip-types scripts/smoke/calls-api.mjs
// WX_CALLS_FIXTURE_BINARY and WX_BINARY override the adjacent debug executables.
// Trusted already-ACKed offers do not test native pending/staged-Sync ingress.
import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { createRequire } from "node:module";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const suffix = process.platform === "win32" ? ".exe" : "";
const fixtureBinary = process.env.WX_CALLS_FIXTURE_BINARY ?? join(repository, "packages/native-client-rust/target/debug", `wx-calls-fixture${suffix}`);
const cliBinary = process.env.WX_BINARY ?? join(dirname(fixtureBinary), `wx${suffix}`);
if (process.argv.includes("--help")) {
  console.log("Account-free Rust call API / SDK / OpenClaw smoke. Set WX_CALLS_FIXTURE_BINARY; optional WX_BINARY enables CLI status and passive monitor checks. --skip-cli isolates SDK/plugin diagnostics. --check-heartbeat adds a real 60-second no-Pong owner cleanup check. No WeChat or voice-provider connection is made.");
  process.exit(0);
}

// Use the same built SDK module that the extension imports through its workspace
// dependency, so error classes and production wire parsing are identical.
const { NativeWeChatClient, NativeCallsClient, NativeCallMediaClient, NativeCallError } = await import("../../packages/shared/dist/index.js");
const { NativeCallService, nativeCallSocketFactory } = await import("../../packages/openclaw-extension/src/calls-service.ts");
const { resolveWeChatAccount } = await import("../../packages/openclaw-extension/src/types.ts");
const execute = promisify(execFile);
const temporary = await mkdtemp(join(tmpdir(), "agent-wechat-calls-api-"));
const environment = Object.fromEntries(["PATH", "LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "SystemRoot", "WINDIR", "TEMP", "TMP", "TMPDIR", "LANG", "LC_ALL"]
  .filter(key => process.env[key] !== undefined).map(key => [key, process.env[key]]));
environment.XDG_CONFIG_HOME = join(temporary, "config");
environment.XDG_DATA_HOME = join(temporary, "data");
environment.XDG_RUNTIME_DIR = join(temporary, "runtime");
await mkdir(environment.XDG_RUNTIME_DIR, { mode: 0o700 });
const fixture = spawn(fixtureBinary, [], { stdio: ["pipe", "pipe", "pipe"], env: environment });
const exit = new Promise(resolve => fixture.once("exit", (code, signal) => resolve({ code, signal })));
let diagnostics = "", fixtureError, readyResolve, readyReject, shutdown = false;
const pending = new Map();
const readyPromise = new Promise((yes, no) => { readyResolve = yes; readyReject = no; });
void readyPromise.catch(() => {});
fixture.stderr.on("data", value => { diagnostics = (diagnostics + value).slice(-4000); });
function fixtureFailed(error) {
  if (shutdown) return;
  fixtureError = error; readyReject(error);
  for (const request of pending.values()) request.reject(error);
  pending.clear();
}
fixture.once("error", fixtureFailed);
fixture.once("exit", () => fixtureFailed(Error("Synthetic call fixture exited unexpectedly")));
const input = createInterface({ input: fixture.stdout, crlfDelay: Infinity });
const reading = (async () => {
  for await (const line of input) {
    try {
      assert.ok(line.length <= 16 * 1024, "Fixture record exceeds its limit");
      const record = JSON.parse(line);
      if (record.event === "ready") readyResolve(record);
      else {
        const request = pending.get(record.id); assert.ok(request, "Unexpected fixture reply");
        pending.delete(record.id); record.ok ? request.resolve(record) : request.reject(Error(`Fixture operation rejected: ${record.code}`));
      }
    } catch (error) { fixtureFailed(error); break; }
  }
})();
const sockets = [], readers = [];
let service, monitor, summary;
async function bounded(promise, label, ms = 10000) {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(Error(`Timed out: ${label}`)), ms); })]); }
  finally { clearTimeout(timer); }
}
async function control(op, fields = {}) {
  if (fixtureError) throw fixtureError;
  const id = randomUUID();
  const response = new Promise((yes, no) => pending.set(id, { resolve: yes, reject: no }));
  fixture.stdin.write(JSON.stringify({ id, op, ...fields }) + "\n");
  return bounded(response, `fixture ${op}`);
}
async function until(label, condition) {
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    if (fixtureError) throw fixtureError;
    for (const reader of readers) if (reader.error && !reader.closing) throw reader.error;
    const result = await condition(); if (result) return result;
    await delay(10);
  }
  throw Error(`Timed out: ${label}`);
}
function watch(client, paused = false) {
  const reader = { events: [], closing: false, error: undefined, started: false };
  reader.start = () => {
    assert.equal(reader.started, false); reader.started = true;
    reader.done = (async () => { try { for await (const event of client.events()) { reader.events.push(event); if (reader.events.length > 128) reader.events.shift(); } } catch (error) { reader.error = error; } })();
  };
  if (!paused) reader.start();
  readers.push(reader); sockets.push({ client, reader }); return reader;
}
function availability(reader) {
  return reader.events.findLast(event => event.type === "calls.availability")?.availability
    ?? reader.events.find(event => event.type === "hello")?.availability;
}
function pcm(value) { const bytes = Buffer.alloc(640); for (let n = 0; n < bytes.length; n += 2) bytes.writeInt16LE(value, n); return bytes; }
function samples(bytes) { return Int16Array.from({ length: bytes.length / 2 }, (_, n) => bytes.readInt16LE(n * 2)); }

try {
  const ready = await bounded(readyPromise, "fixture startup");
  const url = new URL(ready.baseUrl);
  assert.equal(url.protocol, "http:"); assert.equal(url.hostname, "127.0.0.1"); assert.ok(url.port);
  assert.deepEqual(ready.restartRecovery, { localClosed: true, remoteConfirmed: false, requestOutcome: "needs_review" });
  assert.equal(url.username, ""); assert.equal(url.password, ""); assert.equal(ready.accountId, "calls-fixture");
  const token = (await readFile(ready.callsTokenFile, "utf8")).trim();
  const nonCallsToken = (await readFile(ready.nonCallsTokenFile, "utf8")).trim();
  const rawWebSocketFactory = await nativeCallSocketFactory();
  let holdNativePcm = false, heldNativePcm;
  const webSocketFactory = (url, options) => {
    const socket = rawWebSocketFactory(url, options), send = socket.send.bind(socket);
    socket.send = value => {
      if (holdNativePcm && value instanceof Uint8Array && value.byteLength >= 36 && value[5] === 1) {
        holdNativePcm = false;
        heldNativePcm = () => send(value); return;
      }
      send(value);
    };
    return socket;
  };
  const options = { baseUrl: ready.baseUrl, token, accountId: ready.accountId, webSocketFactory, commandTimeoutMs: 5000 };
  const observer = new NativeCallsClient(options); const observing = watch(observer);
  const observedHello = await observer.connect(); assert.equal(observedHello.currentCall, null);
  assert.deepEqual(observedHello.capabilities, { directAudio: true });
  // The same calls grant permits participation, but connecting alone stays passive.
  const blocked = new NativeCallsClient({ ...options, token: nonCallsToken }); const blockedEvents = watch(blocked); blockedEvents.closing = true;
  await assert.rejects(blocked.connect()); blocked.close();
  const deniedCurrent = await fetch(new URL(`v1/accounts/${ready.accountId}/calls/current`, ready.baseUrl), {
    headers: { Authorization: `Bearer ${nonCallsToken}` }, signal: AbortSignal.timeout(5000),
  });
  assert.equal(deniedCurrent.status, 403); await deniedCurrent.arrayBuffer();
  const invalid = new NativeCallsClient({ ...options, token: "invalid-synthetic-bearer" }); const denied = watch(invalid); denied.closing = true;
  await assert.rejects(invalid.connect()); invalid.close();

  const left = new NativeCallsClient(options), right = new NativeCallsClient(options);
  const leftEvents = watch(left, true), rightEvents = watch(right);
  const initialHello = await left.connect(); assert.equal(initialHello.currentOffer, null);
  await left.ready(randomUUID(), true);
  let cli = process.argv.includes("--skip-cli") ? "explicitly-skipped" : "not-built", monitorEvents = [];
  const cliAvailable = await stat(cliBinary).then(value => value.isFile(), () => false);
  if (process.env.WX_BINARY) assert.ok(cliAvailable, "Requested WX_BINARY is missing");
  if (cliAvailable && !process.argv.includes("--skip-cli")) {
    const config = join(temporary, "client.toml"); await writeFile(config, "schemaVersion = 1\n", { mode: 0o600 });
    const args = ["--json", "--config", config, "--server", ready.baseUrl, "--token-file", ready.callsTokenFile, "--account", ready.accountId, "calls"];
    const status = JSON.parse((await execute(cliBinary, [...args, "status"], { env: environment, timeout: 10000 })).stdout);
    assert.equal(status.availability.busy, false);
    monitor = spawn(cliBinary, [...args, "monitor"], { stdio: ["ignore", "pipe", "pipe"], env: environment });
    monitor.once("error", error => { fixtureError = error; });
    monitor.stderr.on("data", () => {});
    const lines = createInterface({ input: monitor.stdout, crlfDelay: Infinity });
    monitor.done = (async () => { for await (const line of lines) { monitorEvents.push(JSON.parse(line)); if (monitorEvents.length > 128) monitorEvents.shift(); } })();
    void monitor.done.catch(error => { fixtureError = error; });
    await until("passive CLI hello", () => monitorEvents.some(event => event.type === "hello")); cli = "status-and-passive-monitor";
  }

  await assert.rejects(left.command({ type: "call.dial", requestId: randomUUID(), target: { kind: "direct", contactId: "synthetic-peer" }, callType: "video" }), error => error instanceof NativeCallError && error.code === "unsupported");
  assert.equal((await control("stats")).stats.dial, 0, "Unsupported call type must not reach the backend");
  const offer = await control("offer", { callerId: "synthetic-peer" });
  // A reply on the same control socket follows the already-queued offer.
  // Start consuming late: both the initial hello and new offer must survive.
  await left.ready(randomUUID(), true); leftEvents.start();
  const currentHello = await right.connect();
  assert.equal(currentHello.currentOffer?.callId, offer.callId);
  assert.equal(currentHello.currentOffer.callType, "voice");
  await right.ready(randomUUID(), true);
  await until("late SDK reader keeps hello and queued offer", () => leftEvents.events.some(event => event.type === "call.offered" && event.offer.callId === offer.callId));
  assert.equal(leftEvents.events[0].type, "hello");
  assert.ok(leftEvents.events.findIndex(event => event.type === "call.offered" && event.offer.callId === offer.callId) > 0);
  const claims = await Promise.allSettled([left.answer(randomUUID(), offer.callId, offer.revision), right.answer(randomUUID(), offer.callId, offer.revision)]);
  assert.equal(claims.filter(result => result.status === "fulfilled").length, 1);
  const winnerIndex = claims.findIndex(result => result.status === "fulfilled");
  const winner = winnerIndex === 0 ? left : right, loser = winnerIndex === 0 ? right : left;
  const winnerEvents = winnerIndex === 0 ? leftEvents : rightEvents;
  const loserEvents = winnerIndex === 0 ? rightEvents : leftEvents;
  const rejected = claims[1 - winnerIndex].reason;
  assert.ok(rejected instanceof NativeCallError); assert.equal(rejected.code, "already_claimed");
  const winnerResult = claims[winnerIndex].value;
  await until("passive SDK observes busy state", () => availability(observing)?.busy === true);
  assert.equal(availability(observing).ownedByThisClient, false);
  await until("Rust connected state", () => winnerEvents.events.some(event => event.type === "call.state" && event.call.callId === offer.callId && event.call.state === "connected"));
  await until("observer receives typed call metadata", () => observing.events.some(event => event.type === "call.state" && event.call.callId === offer.callId && event.call.state === "connected" && event.call.callType === "voice" && !event.call.ownedByThisClient));
  const currentResponse = await fetch(new URL(`v1/accounts/${ready.accountId}/calls/current`, ready.baseUrl), {
    headers: { Authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(5000),
  });
  assert.equal(currentResponse.status, 200);
  const currentSnapshot = await currentResponse.json();
  assert.deepEqual(currentSnapshot.currentCall, { callId: offer.callId, revision: winnerEvents.events.findLast(event => event.type === "call.state" && event.call.state === "connected").call.revision, kind: "direct", callType: "voice", direction: "incoming", state: "connected", ownedByThisClient: false });
  assert.equal("ownerToken" in currentSnapshot, false);
  assert.equal((await control("stats")).stats.answer, 1, "The losing claim must not reach the backend");
  for (const muted of [true, false]) {
    await control("peer_microphone", { muted });
    await until("peer control delivered to owner and observer", () => [winnerEvents, observing].every(reader => {
      const update = reader.events.findLast(event => event.type === "call.control");
      return update?.callId === offer.callId && update.participantId === "synthetic-peer"
        && update.control.kind === "microphone" && update.control.muted === muted;
    }));
  }
  await control("peer_status", { code: 3 });
  await until("peer status delivered to owner and observer", () => [winnerEvents, observing].every(reader => {
    const update = reader.events.findLast(event => event.type === "call.peer_status");
    return update?.callId === offer.callId && update.participantId === "synthetic-peer"
      && update.status.code === 3 && update.status.kind === "acknowledged";
  }));
  const lateObserver = new NativeCallsClient(options); watch(lateObserver);
  const lateHello = await lateObserver.connect();
  assert.deepEqual(lateHello.currentCall.controls, [{ participantId: "synthetic-peer", control: { kind: "microphone", muted: false } }]);
  assert.equal(lateHello.currentCall.ownedByThisClient, false);
  if (cli === "status-and-passive-monitor") await until("CLI peer control event", () => monitorEvents.some(event => event.type === "call.control"));
  await assert.rejects(loser.dial(randomUUID(), "synthetic-peer"), error => error instanceof NativeCallError && error.code === "busy");
  const loserHello = loserEvents.events.find(event => event.type === "hello");
  const mismatched = new NativeCallMediaClient(options, offer.callId, { type: "media.attach", supportedProtocolVersions: [3], clientId: loserHello.clientId, ownerToken: winnerResult.ownerToken });
  await assert.rejects(mismatched.connect()); mismatched.close();
  const media = await winner.attachMedia(offer.callId), received = [];
  let mediaClosing = false;
  const mediaReading = (async () => { try { for await (const frame of media.frames()) received.push(frame); } catch (error) { if (!mediaClosing) fixtureError = error; } })();
  const first = pcm(1200); let track = media.tracks().sendTrack;
  media.sendPcm(samples(first).subarray(0, 3), track.generation);
  media.sendPcm(samples(first).subarray(3), track.generation);
  await until("Rust-to-SDK echo PCM", () => received.length === 1); assert.deepEqual(Buffer.from(received[0].payload), first);
  await control("inject_audio", { value: -1700, frames: 1 });
  await until("independent Rust RX PCM", () => received.length === 2); assert.deepEqual(Buffer.from(received[1].payload), pcm(-1700));
  await winner.clearAudio(randomUUID(), offer.callId); const nextTrack = media.tracks().sendTrack;
  assert.equal(nextTrack.generation, track.generation + 1); assert.equal(media.tracks().receiveTracks[0].generation, 1);
  assert.throws(() => media.sendPcm(samples(first), track.generation));
  track = nextTrack;
  media.sendPcm(samples(pcm(1800)), track.generation);
  await until("new-generation PCM", () => received.length === 3); assert.deepEqual(Buffer.from(received[2].payload), pcm(1800));
  assert.equal((await control("stats")).stats.audioFrames, 2);
  if (monitor) await until("CLI observes offer without claiming", () => monitorEvents.some(event => event.type === "call.offered" && event.offer.callId === offer.callId));
  holdNativePcm = true;
  media.sendPcm(samples(first), track.generation);
  assert.equal(typeof heldNativePcm, "function");
  await control("begin_end");
  await until("autonomous ending event", () => winnerEvents.events.some(event => event.type === "call.state" && event.call.state === "ending"));
  const beforeLatePcm = (await control("stats")).stats.audioFrames;
  assert.throws(() => media.sendPcm(samples(first), track.generation), error => error.code === "call_ending");
  heldNativePcm();
  await delay(100);
  assert.equal((await control("stats")).stats.audioFrames, beforeLatePcm, "Late PCM during autonomous teardown is ignored");
  mediaClosing = true; await control("end");
  await until("synthetic remote terminal", () => availability(leftEvents)?.busy === false && availability(rightEvents)?.busy === false
    && winnerEvents.events.some(event => event.type === "call.state" && event.call.callId === offer.callId && event.call.state === "ended"));
  media.close(); await mediaReading;
  for (const connection of sockets) { connection.reader.closing = true; connection.client.close(); }

  let holdNextPcm = false, heldPcm;
  const pluginSocketFactory = (url, options) => {
    const socket = webSocketFactory(url, options), send = socket.send.bind(socket);
    socket.send = value => {
      if (holdNextPcm && value instanceof Uint8Array && value.byteLength >= 36 && value[5] === 1) {
        holdNextPcm = false;
        heldPcm = () => send(value);
      } else send(value);
    };
    return socket;
  };
  const pluginClient = new NativeCallsClient({ ...options, webSocketFactory: pluginSocketFactory });
  const pluginEvents = [], events = pluginClient.events.bind(pluginClient);
  pluginClient.events = () => (async function* () {
    for await (const event of events()) { pluginEvents.push(event); if (pluginEvents.length > 128) pluginEvents.shift(); yield event; }
  })();
  let pluginMedia, callbacks, providerOpens = 0, providerCloses = 0, releaseProvider;
  const providerInput = [], providerGate = new Promise(resolve => { releaseProvider = resolve; });
  const attach = pluginClient.attachMedia.bind(pluginClient);
  pluginClient.attachMedia = async callId => { const value = await attach(callId); pluginMedia = value; return value; };
  // Force a scheduling gap at the real end command: closing media before this
  // command revokes its owner in the actual Rust daemon and must fail this test.
  const end = pluginClient.end.bind(pluginClient);
  pluginClient.end = async (...args) => { await delay(40); return end(...args); };
  const cfg = { channels: { "agent-wechat": { dmPolicy: "open", native: { mode: "external", accountId: ready.accountId, endpoint: ready.baseUrl, token, consumerStateDir: join(temporary, "unused-consumer") },
    calls: { voice: { enabled: true, realtime: { provider: "synthetic" }, agentId: "synthetic-agent", inboundPolicy: "disabled", outboundPolicy: "allowlist", outboundAllowTo: ["synthetic-peer"] }, video: { enabled: false } } } } };
  const prepared = { sampleRate: 16000, resampler: () => ({ process: value => Buffer.from(value), flush: () => Buffer.alloc(0) }),
    async open(value) { providerOpens++; callbacks = value; await providerGate; return { sendAudio: value => providerInput.push(Buffer.from(value)), setMediaTimestamp() {}, close() { providerCloses++; } }; } };
  service = new NativeCallService(resolveWeChatAccount(cfg), cfg, {}, { client: pluginClient, prepareVoice: async () => prepared });
  await service.start(); assert.equal(service.status().state, "available");
  await assert.rejects(service.dial("blocked-peer", { assertCurrent() {} }), /destination/); assert.equal(providerOpens, 0);
  const before = (await control("stats")).stats;
  const dialing = service.dial("synthetic-peer", { assertCurrent() {} });
  void dialing.catch(() => {});
  await until("fake provider preflight", () => providerOpens === 1);
  assert.equal((await control("stats")).stats.dial, before.dial);
  releaseProvider(); await dialing; await until("plugin media ready", () => !!pluginMedia);
  const old = pcm(2100); callbacks.audio(old, { itemId: "interrupted" });
  await until("plugin PCM traverses Rust and returns to provider", () => providerInput.some(value => value.equals(old)));
  const beforeClear = (await control("stats")).stats;
  await control("hold_clear", { enabled: true }); callbacks.clear();
  await until("held real Rust clear", async () => (await control("stats")).stats.clearRequested === beforeClear.clearRequested + 1);
  callbacks.audio(pcm(2200), { itemId: "interrupted" }); callbacks.audio(pcm(3100), { itemId: "new-response" });
  await delay(50); assert.equal((await control("stats")).stats.audioFrames, beforeClear.audioFrames);
  await control("release_clear");
  await until("post-clear new response crosses real API", () => providerInput.some(value => value.equals(pcm(3100))));
  const afterClear = (await control("stats")).stats;
  assert.equal(afterClear.audioFrames, beforeClear.audioFrames + 1); assert.equal(afterClear.lastTxGeneration, 2); assert.equal(afterClear.lastTxPeak, 3100);
  await control("hold_clear", { enabled: false });
  assert.equal(pluginMedia.tracks().receiveTracks[0].generation, 1);
  // Playout now sends paced silence between replies, so frame counts keep
  // growing while the call is active. Check the stale chunk itself instead.
  callbacks.audio(pcm(2300), { itemId: "interrupted" }); await delay(60);
  assert.ok(!providerInput.some(value => value.equals(pcm(2300))), "A late chunk from a cleared reply never reaches the backend");
  // Delay one genuine SDK frame before the WebSocket write, then deliver it
  // after Ending. Cross-socket in-flight audio must not detach the owner while
  // the real backend's termination future still awaits its terminal result.
  await control("hold_end", { enabled: true }); holdNextPcm = true;
  callbacks.audio(pcm(3400), { itemId: "in-flight-at-end" });
  await until("one SDK PCM frame held before network write", () => !!heldPcm);
  const ending = service.end();
  callbacks.audio(pcm(3600), { itemId: "after-local-end" });
  await ending;
  await until("plugin observes Ending before backend terminal", () => pluginEvents.some(event => event.type === "call.state" && event.call.state === "ending"));
  // Local end closed playout, so paced frames stopped before this count.
  const atEnding = (await control("stats")).stats;
  assert.equal(service.status().owned, true); assert.equal(service.status().state, "busy"); assert.equal(providerCloses, 1);
  assert.throws(() => pluginMedia.sendPcm(samples(pcm(3500)), 2),
    error => error instanceof NativeCallError && error.code === "call_ending");
  heldPcm(); heldPcm = undefined;
  callbacks.failed(); // A provider may close after local end but before upstream terminal.
  await delay(1200); // Past the real control socket's one-second validity tick.
  const heldEnd = (await control("stats")).stats;
  assert.equal(service.status().owned, true); assert.equal(service.status().state, "busy");
  assert.equal(heldEnd.active, true); assert.equal(heldEnd.hangup, before.hangup + 1); assert.equal(heldEnd.stop, afterClear.stop);
  assert.equal(heldEnd.audioFrames, atEnding.audioFrames, "In-flight PCM after Ending must not reach the backend");
  assert.notEqual(heldEnd.lastTxPeak, 3400, "The held in-flight frame must not reach the backend");
  assert.ok(!pluginEvents.some(event => event.type === "call.state" && ["ended", "uncertain"].includes(event.call.state)), "No terminal result exists before release");
  await control("release_end");
  await until("plugin explicit hangup terminal", () => service.status().state === "available");
  assert.ok(pluginEvents.some(event => event.type === "call.state" && event.call.state === "ended"));
  const final = (await control("stats")).stats;
  assert.equal(final.hangup, before.hangup + 1); assert.equal(final.active, false); assert.equal(providerCloses, 1);
  callbacks.audio(pcm(3200), { itemId: "after-end" }); await delay(30);
  assert.equal((await control("stats")).stats.audioFrames, final.audioFrames);
  await service.stop();

  // --- Video v3, ?ticket= auth, pre-answer mute/camera, conference video & OpenClaw v3 coexistence ---
  await control("configure_video", { enabled: true, echo: true });
  await control("configure_conference", { enabled: true });
  const httpClient = new NativeWeChatClient({ baseUrl: ready.baseUrl, token });
  const issuedTickets = [];
  const ticketClient = new NativeCallsClient({
    baseUrl: ready.baseUrl,
    accountId: ready.accountId,
    webSocketFactory: rawWebSocketFactory,
    commandTimeoutMs: 5000,
    ticketProvider: async (socket, callId) => {
      const issued = await httpClient.issueCallTicket(ready.accountId, callId ? { socket, callId } : { socket });
      issuedTickets.push(issued.ticket);
      return issued.ticket;
    },
  });
  const ticketEvents = watch(ticketClient);
  const videoHello = await ticketClient.connect();
  assert.equal(videoHello.capabilities.directVideo, true);
  assert.equal(videoHello.capabilities.conferenceVideo, true);
  assert.ok(videoHello.videoCapabilities?.sendCodecs.includes("h265"));
  assert.equal(issuedTickets.length, 1);

  // Single-use ticket cannot be replayed on a second socket.
  const replay = new NativeCallsClient({
    baseUrl: ready.baseUrl,
    accountId: ready.accountId,
    webSocketFactory: rawWebSocketFactory,
    ticketProvider: async () => issuedTickets[0],
  });
  const replayEvents = watch(replay); replayEvents.closing = true;
  await assert.rejects(replay.connect()); replay.close();

  await ticketClient.ready(randomUUID(), true, videoHello.audioFormat, undefined, {
    sendCodecs: ["h265"],
    receiveCodecs: ["h265"],
    maxWidth: 640,
    maxHeight: 480,
    maxFps: 15,
  });
  const videoDial = await ticketClient.dial(randomUUID(), "synthetic-peer", {
    callType: "video",
    cameraEnabled: true,
    muted: true,
  });
  await until("video direct connected", () => ticketEvents.events.some(e => e.type === "call.state" && e.call.callId === videoDial.callId && e.call.state === "connected" && e.call.callType === "video"));
  let vstats = (await control("stats")).stats;
  assert.equal(vstats.cameraEnabled, true);
  assert.equal(vstats.muted, true);

  await ticketClient.setMute(randomUUID(), videoDial.callId, false);
  await ticketClient.setCamera(randomUUID(), videoDial.callId, false);
  await ticketClient.setCamera(randomUUID(), videoDial.callId, true);
  vstats = (await control("stats")).stats;
  assert.equal(vstats.muted, false);
  assert.equal(vstats.muteToggles, 1);
  assert.equal(vstats.cameraEnabled, true);
  assert.equal(vstats.cameraToggles, 2);

  const videoMedia = await ticketClient.attachMedia(videoDial.callId);
  assert.equal(issuedTickets.length, 2, "media socket used a separate single-use ticket");
  const vtracks = videoMedia.videoTracks();
  assert.ok(vtracks.sendTrack && vtracks.sendTrack.id === 65537);
  assert.equal(vtracks.receiveTracks.length, 1);
  const rxVideoFrames = [], rxKeyframes = [];
  let videoMediaClosing = false;
  const rxVideoLoop = (async () => { try { for await (const f of videoMedia.videoFrames()) rxVideoFrames.push(f); } catch (e) { if (!videoMediaClosing) fixtureError = e; } })();
  const rxKfLoop = (async () => { try { for await (const k of videoMedia.keyframeRequests()) rxKeyframes.push(k); } catch (e) { if (!videoMediaClosing) fixtureError = e; } })();

  const sampleHevc = new Uint8Array([0, 0, 0, 1, 0x40, 0x01, 0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xde, 0xad, 0xbe, 0xef]);
  videoMedia.sendVideo(sampleHevc, vtracks.sendTrack.generation, { keyframe: true, codecConfig: true, timestampMicros: 33333 });
  await until("echoed binary WXCM v3 H.265 frame", () => rxVideoFrames.length === 1);
  assert.equal(rxVideoFrames[0].codec, "h265");
  assert.equal(rxVideoFrames[0].keyframe, true);
  assert.equal(rxVideoFrames[0].codecConfig, true);
  assert.deepEqual([...rxVideoFrames[0].payload], [...sampleHevc]);

  videoMedia.requestKeyframe(vtracks.receiveTracks[0].id);
  await until("backend received keyframe request", async () => (await control("stats")).stats.keyframeRequests >= 1);
  await control("peer_keyframe_requested");
  await until("client received video.keyframe_requested", () => rxKeyframes.length === 1);
  assert.equal(rxKeyframes[0].callId, videoDial.callId);
  await control("peer_camera", { enabled: false });
  await until("peer camera control delivered", () => ticketEvents.events.some(e => e.type === "call.control" && e.callId === videoDial.callId && e.control.kind === "camera" && e.control.enabled === false));

  videoMediaClosing = true;
  await control("end");
  await until("video direct ended", () => availability(ticketEvents)?.busy === false);
  videoMedia.close(); await Promise.all([rxVideoLoop, rxKfLoop]);

  // OpenClaw audio-only v3 ignores incoming video offers cleanly and stays healthy in conferences with video peers.
  const openclawVideoClient = new NativeCallsClient({ ...options, webSocketFactory: rawWebSocketFactory });
  let ocMedia;
  const ocAttach = openclawVideoClient.attachMedia.bind(openclawVideoClient);
  openclawVideoClient.attachMedia = async (callId, observePcm) => { const m = await ocAttach(callId, observePcm); ocMedia = m; return m; };
  const ocCfg = {
    channels: {
      "agent-wechat": {
        dmPolicy: "open",
        native: { mode: "external", accountId: ready.accountId, endpoint: ready.baseUrl, token, consumerStateDir: join(temporary, "unused-consumer-2") },
        calls: {
          voice: {
            enabled: true,
            realtime: { provider: "synthetic" },
            agentId: "synthetic-agent",
            inboundPolicy: "allowlist",
            inboundAllowFrom: ["synthetic-peer"],
            outboundPolicy: "allowlist",
            outboundAllowTo: ["synthetic-peer"],
            conferences: { groups: { "synthetic-group@chatroom": { autoJoin: false } } },
          },
          video: { enabled: false },
        },
      },
    },
  };
  let ocOpens = 0;
  const ocAudioIn = [];
  const ocPrepared = {
    sampleRate: 16000,
    resampler: () => ({ process: value => Buffer.from(value), flush: () => Buffer.alloc(0) }),
    async open() { ocOpens++; return { sendAudio: value => ocAudioIn.push(Buffer.from(value)), setMediaTimestamp() {}, close() {} }; },
  };
  const ocService = new NativeCallService(resolveWeChatAccount(ocCfg), ocCfg, {}, { client: openclawVideoClient, prepareVoice: async () => ocPrepared });
  await ocService.start();
  assert.equal(ocService.status().state, "available");

  const videoOffer = await control("offer", { callerId: "synthetic-peer", callType: "video" });
  await until("video offer reached SDK client", () => ticketEvents.events.some(e => e.type === "call.offered" && e.offer.callId === videoOffer.callId && e.offer.callType === "video"));
  await delay(80);
  assert.equal(ocOpens, 0, "OpenClaw must ignore incoming video offers");
  assert.equal(ocService.status().owned, false);
  assert.equal(ocService.status().state, "busy");

  // Answer the incoming video offer with camera OFF and mic MUTED via SDK.
  await ticketClient.answer(randomUUID(), videoOffer.callId, videoOffer.revision, { cameraEnabled: false, muted: true });
  await until("incoming video call connected", () => ticketEvents.events.some(e => e.type === "call.state" && e.call.callId === videoOffer.callId && e.call.state === "connected"));
  vstats = (await control("stats")).stats;
  assert.equal(vstats.cameraEnabled, false);
  assert.equal(vstats.muted, true);
  await control("end");
  await until("incoming video call ended", () => availability(ticketEvents)?.busy === false && ocService.status().state === "available");

  // OpenClaw joins a v3 conference where peers have cameraEnabled: true and still mixes audio cleanly.
  await ocService.joinConference("synthetic-group@chatroom", { assertCurrent() {} });
  await until("OpenClaw joined conference and attached media", () => ocService.status().owned === true && !!ocMedia);
  await control("conference_sources", {
    sources: [
      { trackId: 3, generation: 1, contactId: "alice", microphoneEnabled: true, cameraEnabled: true, videoTrackId: 65541, videoGeneration: 1 },
      { trackId: 4, generation: 1, contactId: "bob", microphoneEnabled: true, cameraEnabled: true, videoTrackId: 65542, videoGeneration: 1 },
    ],
  });
  await until("OpenClaw media learned conference audio track 3", () => ocMedia.tracks().receiveTracks.some(t => t.id === 3));
  const conferenceOrigin = performance.now();
  let conferenceSeq = 0;
  let nextConferenceSlot = 0;
  await until("OpenClaw receives conference audio alongside peer cameras", async () => {
    if (ocAudioIn.some(buf => buf.length >= 2 && buf.readInt16LE(0) === 1500)) return true;
    const currentSlot = Math.floor((performance.now() - conferenceOrigin) / 20);
    const targetSlot = currentSlot + 1;
    if (nextConferenceSlot < currentSlot) nextConferenceSlot = currentSlot;
    while (nextConferenceSlot <= targetSlot) {
      await control("conference_audio", {
        trackId: 3,
        generation: 1,
        sequence: conferenceSeq++,
        sampleOffset: nextConferenceSlot * 320,
        value: 1500,
      });
      nextConferenceSlot++;
    }
    return ocAudioIn.some(buf => buf.length >= 2 && buf.readInt16LE(0) === 1500);
  });
  assert.equal(ocService.status().participants?.find(p => p.contactId === "alice")?.cameraEnabled, true);
  await control("end");
  await until("OpenClaw conference ended", () => ocService.status().state === "available");
  await ocService.stop();

  // Conference video multi-track RX & dynamic camera-off removal via SDK.
  const confCall = await ticketClient.startConference(randomUUID(), "synthetic-group@chatroom", ["alice", "bob"], { callType: "video", cameraEnabled: true });
  await until("conference video connected", () => ticketEvents.events.some(e => e.type === "call.state" && e.call.callId === confCall.callId && e.call.state === "connected"));
  const confMedia = await ticketClient.attachMedia(confCall.callId);
  const confFrames = [];
  let confClosing = false;
  const confLoop = (async () => { try { for await (const f of confMedia.videoFrames()) confFrames.push(f); } catch (e) { if (!confClosing) fixtureError = e; } })();
  await control("conference_sources", {
    sources: [
      { trackId: 3, generation: 1, contactId: "alice", microphoneEnabled: true, cameraEnabled: true, videoTrackId: 65541, videoGeneration: 1 },
      { trackId: 4, generation: 1, contactId: "bob", microphoneEnabled: true, cameraEnabled: true, videoTrackId: 65542, videoGeneration: 1 },
    ],
  });
  await until("conference media learned 2 video receive tracks", () => confMedia.videoTracks().receiveTracks.length === 2);
  await control("inject_video", { trackId: 65541, generation: 1, codec: "h265", sequence: 0, timestampMicros: 1000, payloadBase64: Buffer.from(sampleHevc).toString("base64") });
  await control("inject_video", { trackId: 65542, generation: 1, codec: "h265", sequence: 0, timestampMicros: 1000, payloadBase64: Buffer.from(sampleHevc).toString("base64") });
  await until("conference media received video from both peers", () => new Set(confFrames.map(f => f.trackId)).size === 2);
  await control("conference_sources", {
    sources: [
      { trackId: 3, generation: 1, contactId: "alice", microphoneEnabled: true, cameraEnabled: true, videoTrackId: 65541, videoGeneration: 1 },
      { trackId: 4, generation: 1, contactId: "bob", microphoneEnabled: true, cameraEnabled: false },
    ],
  });
  await until("conference removed bob video track when camera turned off", () => confMedia.videoTracks().receiveTracks.length === 1);
  confClosing = true;
  await control("end");
  await until("conference video ended", () => availability(ticketEvents)?.busy === false);
  confMedia.close(); await confLoop;
  ticketEvents.closing = true; ticketClient.close();

  let heartbeatOwnerLoss = "not-run";
  if (process.argv.includes("--check-heartbeat")) {
    const WebSocket = createRequire(new URL("../../packages/shared/package.json", import.meta.url))("ws");
    const heartbeatObserver = new NativeCallsClient(options);
    const heartbeatEvents = watch(heartbeatObserver);
    await heartbeatObserver.connect();
    let pings = 0;
    const silent = new NativeCallsClient({ ...options, webSocketFactory: (url, settings) => {
      const socket = new WebSocket(url, { headers: settings.headers, maxPayload: settings.maxPayload, autoPong: false });
      socket.on("ping", () => { pings++; });
      return socket;
    } });
    const silentEvents = watch(silent); silentEvents.closing = true;
    await silent.connect(); await silent.ready(randomUUID(), true);
    const oldControls = heartbeatEvents.events.filter(event => event.type === "calls.availability").length;
    const began = Date.now();
    await silent.dial(randomUUID(), "synthetic-peer");
    await until("unresponsive owner is connected", () => availability(heartbeatEvents)?.busy === true);
    await bounded((async () => {
      while (heartbeatEvents.events.filter(event => event.type === "calls.availability").length <= oldControls || availability(heartbeatEvents)?.busy !== false) await delay(25);
    })(), "60-second WebSocket liveness cleanup", 75000);
    assert.ok(Date.now() - began >= 59000, "A healthy observer must not change the owner's idle deadline");
    assert.ok(pings >= 3, "The server must probe the owning connection");
    assert.equal((await control("stats")).stats.active, false);
    heartbeatOwnerLoss = "passed";
  }
  summary = { state: "passed", fixture: "trusted-acked-and-outgoing-synthetic-calls", pendingIngressIntegrated: false,
    sdkAnswerWinner: winnerIndex, sdkDuplexFrames: received.length, pluginNewGeneration: afterClear.lastTxGeneration, pluginExplicitHangups: final.hangup - before.hangup,
    heldEndingRetained: true, inFlightEndingPcmDropped: true, peerControlsAndLateSnapshot: true, restartRecovery: true, heartbeatOwnerLoss, cli };
} finally {
  let cleanupError, fixtureExit;
  await service?.stop().catch(() => {});
  for (const connection of sockets) { connection.reader.closing = true; connection.client.close(); }
  if (monitor) { monitor.kill("SIGINT"); await bounded(monitor.done, "CLI monitor cleanup", 2000).catch(() => monitor.kill("SIGTERM")); }
  if (fixture.exitCode === null && !fixture.killed) {
    shutdown = true;
    await control("shutdown").catch(error => { cleanupError = error; });
    fixtureExit = await bounded(exit, "fixture cleanup", 3000).catch(error => { cleanupError ??= error; fixture.kill("SIGTERM"); });
  } else {
    fixtureExit = await exit;
  }
  input.close(); await reading.catch(() => {}); await rm(temporary, { recursive: true, force: true });
  if (summary) {
    if (cleanupError) throw cleanupError;
    assert.equal(fixtureExit?.code, 0, "Synthetic fixture must exit successfully after teardown");
    assert.equal(fixtureExit.signal, null);
  }
  if (fixtureError && !shutdown) console.error("Synthetic fixture failed:", fixtureError.message, diagnostics);
}
console.log(JSON.stringify(summary));
