// Actual Rust API, shared SDK and OpenClaw adapter; all call/provider data is synthetic.
// Build wx-calls-fixture with wechat-simulator's calls-fixture feature, then:
// node --experimental-strip-types scripts/native-calls-api-smoke.mjs
// WX_CALLS_FIXTURE_BINARY and WX_BINARY override the adjacent debug executables.
// Trusted already-ACKed offers do not test native pending/staged-Sync ingress.
import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const suffix = process.platform === "win32" ? ".exe" : "";
const fixtureBinary = process.env.WX_CALLS_FIXTURE_BINARY ?? join(repository, "packages/native-client-rust/target/debug", `wx-calls-fixture${suffix}`);
const cliBinary = process.env.WX_BINARY ?? join(dirname(fixtureBinary), `wx${suffix}`);
if (process.argv.includes("--help")) {
  console.log("Account-free Rust call API / SDK / OpenClaw smoke. Set WX_CALLS_FIXTURE_BINARY; optional WX_BINARY enables CLI status and passive monitor checks. --skip-cli isolates SDK/plugin diagnostics. No WeChat or voice-provider connection is made.");
  process.exit(0);
}

// Use the same built SDK module that the extension imports through its workspace
// dependency, so error classes and production wire parsing are identical.
const { NativeCallsClient, NativeCallMediaClient, NativeCallError } = await import("../packages/shared/dist/index.js");
const { NativeCallService, nativeCallSocketFactory } = await import("../packages/openclaw-extension/src/calls-service.ts");
const { resolveWeChatAccount } = await import("../packages/openclaw-extension/src/types.ts");
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
  assert.equal(url.username, ""); assert.equal(url.password, ""); assert.equal(ready.accountId, "calls-fixture");
  const token = (await readFile(ready.callsTokenFile, "utf8")).trim();
  const nonCallsToken = (await readFile(ready.nonCallsTokenFile, "utf8")).trim();
  const rawWebSocketFactory = await nativeCallSocketFactory();
  let holdNativePcm = false, heldNativePcm;
  const webSocketFactory = (url, options) => {
    const socket = rawWebSocketFactory(url, options), send = socket.send.bind(socket);
    socket.send = value => {
      if (holdNativePcm && typeof value !== "string") {
        holdNativePcm = false; const bytes = Buffer.from(value);
        heldNativePcm = () => send(bytes); return;
      }
      send(value);
    };
    return socket;
  };
  const options = { baseUrl: ready.baseUrl, token, accountId: ready.accountId, webSocketFactory, commandTimeoutMs: 5000 };
  const observer = new NativeCallsClient(options); const observing = watch(observer);
  const observedHello = await observer.connect(); assert.equal(observedHello.currentCall, null);
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
  await assert.rejects(loser.dial(randomUUID(), "synthetic-peer"), error => error instanceof NativeCallError && error.code === "busy");
  const loserHello = loserEvents.events.find(event => event.type === "hello");
  const mismatched = new NativeCallMediaClient(options, offer.callId, { type: "media.attach", clientId: loserHello.clientId, ownerToken: winnerResult.ownerToken });
  await assert.rejects(mismatched.connect()); mismatched.close();
  const media = await winner.attachMedia(offer.callId), received = [];
  let mediaClosing = false;
  const mediaReading = (async () => { try { for await (const frame of media.frames()) received.push(frame); } catch (error) { if (!mediaClosing) fixtureError = error; } })();
  const first = pcm(1200); let track = media.tracks().sendTrack;
  media.sendPcm({ trackId: track.id, generation: track.generation, sequence: 0n, timestampMicros: 0n }, samples(first));
  await until("Rust-to-SDK echo PCM", () => received.length === 1); assert.deepEqual(Buffer.from(received[0].payload), first);
  await control("inject_audio", { value: -1700, frames: 1 });
  await until("independent Rust RX PCM", () => received.length === 2); assert.deepEqual(Buffer.from(received[1].payload), pcm(-1700));
  await winner.clearAudio(randomUUID(), offer.callId); const nextTrack = media.tracks().sendTrack;
  assert.equal(nextTrack.generation, track.generation + 1); assert.equal(media.tracks().receiveTrack.generation, 1);
  assert.throws(() => media.sendPcm({ trackId: track.id, generation: track.generation, sequence: 1n, timestampMicros: 20000n }, samples(first)));
  track = nextTrack;
  media.sendPcm({ trackId: track.id, generation: track.generation, sequence: 1n, timestampMicros: 20000n }, samples(pcm(1800)));
  await until("new-generation PCM", () => received.length === 3); assert.equal(received[2].generation, 1);
  assert.equal(received[2].sequence, 2n); assert.equal(received[2].timestampMicros, 40000n);
  assert.equal((await control("stats")).stats.audioFrames, 2);
  if (monitor) await until("CLI observes offer without claiming", () => monitorEvents.some(event => event.type === "call.offered" && event.offer.callId === offer.callId));
  holdNativePcm = true;
  media.sendPcm({ trackId: track.id, generation: track.generation, sequence: 2n, timestampMicros: 40000n }, samples(first));
  assert.equal(typeof heldNativePcm, "function");
  await control("begin_end");
  await until("autonomous ending event", () => winnerEvents.events.some(event => event.type === "call.state" && event.call.state === "ending"));
  const beforeLatePcm = (await control("stats")).stats.audioFrames;
  assert.throws(() => media.sendPcm({ trackId: track.id, generation: track.generation, sequence: 3n, timestampMicros: 60000n }, samples(first)), error => error.code === "call_ending");
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
      if (holdNextPcm && typeof value !== "string") {
        holdNextPcm = false; const frame = Buffer.from(value);
        heldPcm = () => send(frame);
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
    calls: { enabled: true, provider: "synthetic", agentId: "synthetic-agent", inboundPolicy: "disabled", outboundPolicy: "allowlist", outboundAllowTo: ["synthetic-peer"] } } } };
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
  assert.equal(pluginMedia.tracks().receiveTrack.generation, 1);
  callbacks.audio(pcm(2300), { itemId: "interrupted" }); await delay(30);
  assert.equal((await control("stats")).stats.audioFrames, afterClear.audioFrames);
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
  assert.equal(service.status().owned, true); assert.equal(service.status().state, "busy"); assert.equal(providerCloses, 1);
  assert.throws(() => pluginMedia.sendPcm({ trackId: 1, generation: 2, sequence: 9n, timestampMicros: 180000n }, samples(pcm(3500))),
    error => error instanceof NativeCallError && error.code === "call_ending");
  heldPcm(); heldPcm = undefined;
  callbacks.failed(); // A provider may close after local end but before upstream terminal.
  await delay(1200); // Past the real control socket's one-second validity tick.
  const heldEnd = (await control("stats")).stats;
  assert.equal(service.status().owned, true); assert.equal(service.status().state, "busy");
  assert.equal(heldEnd.active, true); assert.equal(heldEnd.hangup, before.hangup + 1); assert.equal(heldEnd.stop, afterClear.stop);
  assert.equal(heldEnd.audioFrames, afterClear.audioFrames, "In-flight PCM after Ending must not reach the backend");
  assert.ok(!pluginEvents.some(event => event.type === "call.state" && ["ended", "uncertain"].includes(event.call.state)), "No terminal result exists before release");
  await control("release_end");
  await until("plugin explicit hangup terminal", () => service.status().state === "available");
  assert.ok(pluginEvents.some(event => event.type === "call.state" && event.call.state === "ended"));
  const final = (await control("stats")).stats;
  assert.equal(final.hangup, before.hangup + 1); assert.equal(final.active, false); assert.equal(providerCloses, 1);
  callbacks.audio(pcm(3200), { itemId: "after-end" }); await delay(30);
  assert.equal((await control("stats")).stats.audioFrames, final.audioFrames);
  await service.stop();
  summary = { state: "passed", fixture: "trusted-acked-and-outgoing-synthetic-calls", pendingIngressIntegrated: false,
    sdkAnswerWinner: winnerIndex, sdkDuplexFrames: received.length, pluginNewGeneration: afterClear.lastTxGeneration, pluginExplicitHangups: final.hangup - before.hangup,
    heldEndingRetained: true, inFlightEndingPcmDropped: true, cli };
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
