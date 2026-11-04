import test from "node:test";
import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import type { OpenClawPluginApi, OpenClawPluginToolContext, PluginRuntime } from "openclaw/plugin-sdk/core";
import { NativeCallError, type CallEvent, type CallOffer, type CallReadyScope, type CallView, type NativeCallMediaFrame, type NativeCallPcmObserver } from "@agent-wechat/shared";
import { resolveWeChatAccount } from "./types.ts";
import { NativeCallService, type CallControlPort, type CallMediaPort } from "./calls-service.ts";
import { type PreparedCallVoice, type VoiceCallbacks } from "./calls-voice.ts";
import { resolveWeChatCallsConfig } from "./calls-config.ts";
import { callsConfigurationKey, resolveCallVoiceApiKey } from "./calls-secrets.ts";
import { nativeConfigurationKey } from "./native-secrets.ts";
import { assertCallingToolAccess, registerNativeCallTool } from "./calls-tool.ts";
import { CallPcmOutput } from "./calls-audio.ts";
import { CallAudioTrace } from "./calls-audio-trace.ts";

class Queue<T> implements AsyncIterableIterator<T> {
  values: T[] = []; waiter?: (value: IteratorResult<T>) => void; closed = false;
  push(value: T) { if (this.waiter) { const waiter = this.waiter; this.waiter = undefined; waiter({ done: false, value }); } else this.values.push(value); }
  next(): Promise<IteratorResult<T>> { const value = this.values.shift(); if (value) return Promise.resolve({ done: false, value }); if (this.closed) return Promise.resolve({ done: true, value: undefined }); return new Promise(resolve => { this.waiter = resolve; }); }
  close() { this.closed = true; this.waiter?.({ done: true, value: undefined }); this.waiter = undefined; }
  [Symbol.asyncIterator]() { return this; }
}
const format = { sampleRate: 16000, channels: 1, frameMs: 20 };
const capabilities = { directAudio: true };
const offer: CallOffer = { callId: "offer", revision: 1, callerId: "allowed-peer", kind: "direct", callType: "voice" };
const accepted = (callId: string | null = null, ownerToken: string | null = null): Extract<CallEvent, { type: "command.result" }> => ({ type: "command.result", requestId: "synthetic", status: "accepted", callId, ownerToken, code: null });
const view = (callId: string, state: CallView["state"] = "connected", ownedByThisClient = true): CallView => ({ callId, revision: 2, kind: "direct", callType: "voice", direction: "incoming", state, ownedByThisClient });
function configuration() {
  return { channels: { "agent-wechat": { dmPolicy: "open", native: { mode: "external", accountId: "a", endpoint: "http://localhost:6174", token: "synthetic-token-".repeat(4), consumerStateDir: "/synthetic/consumer" }, calls: {
    voice: { enabled: true, realtime: { provider: "fake" }, agentId: "voice-agent", inboundPolicy: "allowlist", inboundAllowFrom: ["allowed-peer"], outboundPolicy: "allowlist", outboundAllowTo: ["allowed-peer"], outboundToolAllowFrom: ["owner"],
    }, video: { enabled: false },
  } } } };
}
class Media implements CallMediaPort {
  input = new Queue<NativeCallMediaFrame>(); sent: Array<{ generation: number; samples: Int16Array }> = []; generation = 1; closed = false; stopped = false;
  observePcm?: NativeCallPcmObserver;
  tracks() { return { sendTrack: { id: 1, generation: this.generation, direction: "send" as const, format }, receiveTrack: { id: 2, generation: 1, direction: "receive" as const, format } }; }
  frames() { return this.input; }
  sendPcm(samples: Int16Array, generation: number) { if (this.closed || this.stopped) throw Error("Closed"); this.sent.push({ generation, samples: samples.slice() }); }
  stopAudio() { this.stopped = true; this.input.values = []; }
  close() { this.closed = true; this.input.close(); }
}
class Control implements CallControlPort {
  eventsQueue = new Queue<CallEvent>(); media = new Media(); directAudio = true; closed = false; connects = 0; readied = 0; answers = 0; dials = 0; ends = 0; attaches = 0; clears = 0;
  offerOnReady = true;
  scopes: Array<CallReadyScope | undefined> = [];
  answerHook?: () => Promise<void>; dialHook?: () => Promise<void>; clearHook?: () => Promise<void>; readyHook?: () => Promise<void>;
  async connect(): Promise<Extract<CallEvent, { type: "hello" }>> { this.connects++; const hello = { type: "hello" as const, protocolVersion: 1, clientId: "client", capabilities: this.directAudio ? capabilities : {}, availability: { busy: false, ownedByThisClient: false }, currentPendingOffer: null, currentOffer: null, currentCall: null, audioFormat: format }; this.eventsQueue.push(hello); return hello; }
  events() { return this.eventsQueue; }
  async ready(_id: string, _ready: boolean, _format = format, scope?: CallReadyScope) {
    this.readied++; this.scopes.push(scope);
    await this.readyHook?.();
    if (scope?.kind === "offer" && this.offerOnReady) this.eventsQueue.push({ type: "call.offered", offer: { ...offer, callId: scope.callId, revision: scope.expectedRevision + 1 } });
    return accepted();
  }
  async answer(_request: string, callId: string) { this.answers++; await this.answerHook?.(); this.eventsQueue.push({ type: "call.state", call: view(callId) }); return accepted(callId, "private-lease"); }
  async dial() { this.dials++; await this.dialHook?.(); this.eventsQueue.push({ type: "call.state", call: { ...view("dial"), direction: "outgoing" } }); return accepted("dial", "private-lease"); }
  async end() { this.ends++; return accepted(); }
  async clearAudio() { this.clears++; await this.clearHook?.(); this.media.generation++; return accepted(); }
  async attachMedia(_callId: string, observePcm?: NativeCallPcmObserver) { this.attaches++; this.media.observePcm = observePcm; return this.media; }
  close() { this.closed = true; this.eventsQueue.close(); this.media.close(); }
}
function voiceFactory() {
  const state = { opens: 0, closes: 0, incoming: [] as Buffer[], timestamps: [] as number[],
    contexts: [] as Array<Parameters<PreparedCallVoice["open"]>[1]>,
    callbacks: undefined as VoiceCallbacks | undefined, opening: undefined as Promise<void> | undefined };
  const prepared: PreparedCallVoice = { sampleRate: 16000, continuous: false, resampler: () => ({ process: value => Buffer.from(value), flush: () => Buffer.alloc(0) }),
    async open(callbacks, context) { state.opens++; state.contexts.push(context); state.callbacks = callbacks; await state.opening; return { sendAudio: value => state.incoming.push(value), setMediaTimestamp: ms => state.timestamps.push(ms), close() { state.closes++; } }; } };
  return { state, prepared };
}
async function until(predicate: () => boolean) { for (let i = 0; i < 100; i++) { if (predicate()) return; await delay(5); } assert.fail("Condition did not become true"); }
async function fixture(logAudioTrace?: (line: string) => void, cfg = configuration()) { const client = new Control(); const voice = voiceFactory(); const account = resolveWeChatAccount(cfg as any)!; const service = new NativeCallService(account, cfg, {} as PluginRuntime, { client, prepareVoice: async () => voice.prepared, logAudioTrace }); await service.start(); return { service, client, voice }; }

test("calling stays disabled independently of open messaging and requires its own complete configuration", async () => {
  assert.equal(resolveWeChatCallsConfig(undefined), undefined);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { enabled: true } }), /configuration/);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { enabled: true, realtime: { provider: "p" }, agentId: "a", inboundAllowFrom: ["*"] } }), /allowlist/);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { enabled: true, realtime: { provider: "p" }, agentId: "a", toolPolicy: "safe-read-only" } }), /persistence/);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { enabled: true, realtime: { provider: "p" }, agentId: "a", toolPolicy: "owner" } }), /persistence/);
  const consultation = { enabled: true, realtime: { provider: "p" }, agentId: "a" };
  assert.equal(resolveWeChatCallsConfig({ voice: consultation })!.voice!.toolPolicy, "none");
  assert.equal(resolveWeChatCallsConfig({ voice: { ...consultation, allowConsultSessionPersistence: true } })!.voice!.toolPolicy, "safe-read-only");
  assert.equal(resolveWeChatCallsConfig({ voice: { ...consultation, allowConsultSessionPersistence: true, toolPolicy: "none" } })!.voice!.toolPolicy, "none");
  const cfg = configuration(); delete (cfg.channels["agent-wechat"] as any).calls;
  const account = resolveWeChatAccount(cfg as any)!; assert.equal(account.dmPolicy, "open"); assert.equal(account.calls, undefined);
  const apiKey = "synthetic-openai-secret";
  const credentials = resolveWeChatCallsConfig({ voice: { enabled: true, agentId: "a", realtime: { provider: "openai", providers: { openai: {
    apiKey: { source: "env", provider: "voiceenv", id: "WECHAT_TEST_VOICE_KEY" },
  } } } } })!;
  assert.equal(await resolveCallVoiceApiKey(credentials.voice!, { secrets: { providers: { voiceenv: { source: "env", allowlist: ["WECHAT_TEST_VOICE_KEY"] } } } }, { WECHAT_TEST_VOICE_KEY: apiKey }), apiKey);
  assert.equal(JSON.stringify(credentials).includes("WECHAT_TEST_VOICE_KEY"), false);
  await assert.rejects(resolveCallVoiceApiKey(credentials.voice!, {}, {}), /API key could not be resolved/);
  const changed = resolveWeChatCallsConfig({ voice: { enabled: true, agentId: "a", realtime: { provider: "openai", providers: { openai: { apiKey } } } } })!;
  assert.notEqual(callsConfigurationKey(credentials), callsConfigurationKey(changed));
  assert.equal(JSON.stringify(changed).includes(apiKey), false);

  for (const calls of [undefined, { voice: { enabled: false }, video: { enabled: false } }, { voice: { enabled: false }, video: { enabled: true } }]) {
    const disabledCfg = configuration(); (disabledCfg.channels["agent-wechat"] as any).calls = calls;
    const disabledAccount = resolveWeChatAccount(disabledCfg as any)!;
    const control = new Control(); let prepared = 0;
    const disabled = new NativeCallService(disabledAccount, disabledCfg, {} as PluginRuntime, {
      client: control, prepareVoice: async () => { prepared++; throw Error("Disabled voice must not prepare a provider"); },
    });
    await disabled.start();
    control.eventsQueue.push({ type: "call.pending", offer: { ...offer, phase: "waiting_for_ready" } });
    control.eventsQueue.push({ type: "call.offered", offer });
    await delay(1);
    assert.equal(disabled.status().state, calls?.video?.enabled ? "unavailable" : "disabled");
    assert.deepEqual([prepared, control.connects, control.readied, control.answers, control.ends, control.dials], [0, 0, 0, 0, 0, 0]);
    await disabled.stop();
  }
});
test("provider and daemon capability failures never advertise readiness or claim", async () => {
  const cfg = configuration(); const client = new Control();
  const failed = new NativeCallService(resolveWeChatAccount(cfg as any)!, cfg, {} as PluginRuntime, { client, prepareVoice: async () => { throw Error("provider missing"); } });
  await failed.start(); assert.equal(client.readied, 0); assert.equal(failed.status().state, "unavailable"); await failed.stop();
  const voice = voiceFactory(); client.directAudio = false;
  const unsupported = new NativeCallService(resolveWeChatAccount(cfg as any)!, cfg, {} as PluginRuntime, { client, prepareVoice: async () => voice.prepared });
  await unsupported.start(); assert.equal(client.readied, 0); assert.equal(voice.state.opens, 0); assert.equal(client.closed, true); await unsupported.stop();
});
test("inbound policy ignores unlisted callers and video; allowed voice preflights before its single claim", async () => {
  const { service, client, voice } = await fixture();
  client.eventsQueue.push({ type: "call.offered", offer: { ...offer, callerId: "unlisted" } });
  client.eventsQueue.push({ type: "call.offered", offer: { ...offer, callType: "video" } });
  await delay(10); assert.equal(client.answers, 0); assert.equal(voice.state.opens, 0);
  client.answerHook = async () => { assert.equal(voice.state.opens, 1); };
  client.eventsQueue.push({ type: "call.offered", offer }); await until(() => client.attaches === 1);
  client.eventsQueue.push({ type: "call.offered", offer }); await delay(10); assert.equal(client.answers, 1); await service.stop();
});
test("pending calls are observed without global readiness; only a preflighted allowed offer permits ACK", async () => {
  const { service, client, voice } = await fixture(); assert.equal(client.readied, 0);
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, callerId: "unlisted", phase: "waiting_for_ready" } });
  await delay(10); assert.equal(client.readied, 0); assert.equal(voice.state.opens, 0);
  let release!: () => void; voice.state.opening = new Promise(resolve => { release = resolve; });
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, phase: "waiting_for_ready" } });
  await until(() => voice.state.opens === 1); assert.equal(client.readied, 0); assert.equal(client.answers, 0);
  release(); await until(() => client.attaches === 1);
  assert.deepEqual(client.scopes, [{ kind: "offer", callId: "offer", expectedRevision: 1 }]); assert.equal(client.answers, 1); await service.stop();
});
test("uncertain exact-offer readiness never retries or guesses that ACK failed", async () => {
  const { service, client, voice } = await fixture(); client.readyHook = async () => { throw new NativeCallError("command_timeout", true); };
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, phase: "waiting_for_ready" } });
  await until(() => voice.state.closes === 1); assert.equal(client.readied, 1); assert.equal(client.answers, 0); assert.equal(service.status().state, "needs_review");
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, phase: "waiting_for_ready" } }); await delay(10); assert.equal(client.readied, 1); await service.stop();
});
test("a withdrawn pre-answer offer releases the listener for the next call", async t => {
  const { service, client, voice } = await fixture();
  t.after(() => service.stop());
  client.offerOnReady = false;
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, phase: "waiting_for_ready" } });
  await until(() => client.readied === 1);
  client.eventsQueue.push({ type: "calls.availability", availability: { busy: true, ownedByThisClient: false } });
  client.eventsQueue.push({ type: "call.pending", offer: null });
  await delay(10);
  assert.equal(service.status().state, "preparing");
  assert.equal(voice.state.closes, 0);
  client.eventsQueue.push({ type: "calls.availability", availability: { busy: false, ownedByThisClient: false } });
  client.eventsQueue.push({ type: "call.pending", offer: null });
  await until(() => voice.state.closes === 1);
  assert.equal(service.status().state, "available");
  assert.equal(client.closed, false);
  assert.equal(client.answers, 0);

  client.offerOnReady = true;
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, callId: "next-offer", phase: "waiting_for_ready" } });
  await until(() => client.attaches === 1);
  assert.equal(client.readied, 2);
  assert.equal(client.answers, 1);
  assert.equal(service.status().callId, "next-offer");
});
test("competing plugin clients accept one winner, close the losing provider and never retry", async () => {
  const first = await fixture(); const second = await fixture(); let won = false;
  const compete = async () => { if (won) throw new NativeCallError("already_claimed"); won = true; };
  first.client.answerHook = compete; second.client.answerHook = compete;
  first.client.eventsQueue.push({ type: "call.offered", offer }); second.client.eventsQueue.push({ type: "call.offered", offer });
  await until(() => first.client.answers + second.client.answers === 2); await delay(15);
  assert.equal(first.client.attaches + second.client.attaches, 1); assert.equal(first.voice.state.closes + second.voice.state.closes, 1);
  assert.equal(first.service.status().owned, true); assert.equal(second.service.status().owned, false);
  await first.service.stop(); await second.service.stop();
});
test("lost offer during provider preparation cannot issue a delayed answer", async () => {
  const { service, client, voice } = await fixture(); let release!: () => void; voice.state.opening = new Promise(resolve => { release = resolve; });
  client.eventsQueue.push({ type: "call.offered", offer }); await until(() => voice.state.opens === 1);
  client.eventsQueue.push({ type: "call.state", call: view(offer.callId, "connected", false) }); await delay(10); release();
  await until(() => voice.state.closes > 0); assert.equal(client.answers, 0); assert.equal(service.status().state, "busy"); await service.stop();
});
test("outgoing target and tool requester permissions are separate from chat and caller permissions", async () => {
  const { service, client } = await fixture(); await assert.rejects(service.dial("unlisted", { assertCurrent() {} }), /destination/); assert.equal(client.dials, 0);
  const cfg = configuration();
  (cfg.channels["agent-wechat"].calls.voice.realtime as any).providers = { openai: { apiKey: "synthetic-original-key" } };
  const account = resolveWeChatAccount(cfg as any)!;
  const context = { config: cfg as any, agentId: "voice-agent", requesterSenderId: "owner", senderIsOwner: true, assertInvocationCurrent() {} };
  const key = nativeConfigurationKey(account.native!); const calls = callsConfigurationKey(account.calls!);
  assert.doesNotThrow(() => assertCallingToolAccess(context, key, calls));
  assert.throws(() => assertCallingToolAccess({ ...context, senderIsOwner: false }, key, calls), /not permitted/);
  assert.throws(() => assertCallingToolAccess({ ...context, requesterSenderId: "allowed-peer" }, key, calls), /not permitted/);
  assert.throws(() => assertCallingToolAccess({ ...context, agentId: "another" }, key, calls), /not permitted/);
  (cfg.channels["agent-wechat"].calls.voice.realtime as any).providers.openai.apiKey = "synthetic-replaced-key";
  assert.throws(() => assertCallingToolAccess(context, key, calls), /not permitted/);

  await service.stop();
});
test("only the trusted outbound invocation or configured inbound route supplies a parent session", async t => {
  const cfg = configuration();
  const inboundKey = "agent:voice-agent:wechat:direct:synthetic-inbound";
  Object.assign(cfg.channels["agent-wechat"].calls.voice, { consultation: { inboundSessionKey: inboundKey } });
  const outbound = await fixture(undefined, cfg);
  const inbound = await fixture(undefined, cfg);
  t.after(async () => { await outbound.service.stop(); await inbound.service.stop(); });
  const trusted: OpenClawPluginToolContext<2> = {
    config: cfg as any, agentId: "voice-agent", requesterSenderId: "owner", senderIsOwner: true,
    sessionKey: "agent:voice-agent:wechat:direct:synthetic-owner", sessionId: "owner-session", assertInvocationCurrent() {},
  };
  let tool: { execute(id: string, input: unknown): Promise<unknown> } | undefined;
  const api = { registerTool(factory: { create(context: OpenClawPluginToolContext<2>): NonNullable<typeof tool>[] }) {
    [tool] = factory.create(trusted);
  } } as unknown as OpenClawPluginApi;
  registerNativeCallTool(api, () => outbound.service);
  await assert.rejects(tool!.execute("synthetic", { action: "dial", contactId: "allowed-peer", sessionKey: inboundKey }), /Invalid calling arguments/);
  assert.equal(outbound.client.dials, 0);
  await tool!.execute("synthetic", { action: "dial", contactId: "allowed-peer" });
  assert.deepEqual(outbound.voice.state.contexts[0].parentSession, { key: trusted.sessionKey, expectedSessionId: trusted.sessionId });
  await until(() => outbound.voice.state.contexts[0].connectedPeer?.() === "allowed-peer");
  inbound.client.eventsQueue.push({ type: "call.offered", offer });
  await until(() => inbound.client.answers === 1);
  assert.deepEqual(inbound.voice.state.contexts[0].parentSession, { key: inboundKey });
  await until(() => inbound.voice.state.contexts[0].connectedPeer?.() === offer.callerId);
});
test("conference targets cannot be dialed through the direct-only plugin", async () => {
  const cfg = configuration(); cfg.channels["agent-wechat"].calls.video.enabled = true; cfg.channels["agent-wechat"].calls.voice.outboundAllowTo.push("group@chatroom");
  const client = new Control(); const voice = voiceFactory(); const service = new NativeCallService(resolveWeChatAccount(cfg as any)!, cfg, {} as PluginRuntime, { client, prepareVoice: async () => voice.prepared });
  await service.start(); await assert.rejects(service.dial("group@chatroom", { assertCurrent() {} }), /destination/);
  client.eventsQueue.push({ type: "call.pending", offer: { ...offer, callType: "video", phase: "waiting_for_ready" } });
  client.eventsQueue.push({ type: "call.offered", offer: { ...offer, callType: "video" } });
  await delay(10);
  assert.equal(client.dials, 0); assert.equal(voice.state.opens, 0); assert.equal(client.answers, 0); assert.equal(client.readied, 0); await service.stop();
});
test("outgoing preflight rechecks invocation authority and releases its provider without dialing", async () => {
  const { service, client, voice } = await fixture(); let checks = 0;
  await assert.rejects(service.dial("allowed-peer", { assertCurrent() { if (++checks === 2) throw Error("revoked"); } }));
  assert.equal(client.dials, 0); assert.equal(voice.state.closes, 1); assert.equal(service.status().state, "unavailable"); await service.stop();
});
test("aborting provider preparation fences a late session and cannot send a call", async () => {
  const { service, client, voice } = await fixture(); let release!: () => void;
  voice.state.opening = new Promise(resolve => { release = resolve; }); const abort = new AbortController();
  const dialing = service.dial("allowed-peer", { assertCurrent() {}, signal: abort.signal });
  await until(() => voice.state.opens === 1); abort.abort(); release(); await assert.rejects(dialing);
  await until(() => voice.state.closes === 1); assert.equal(client.dials, 0); assert.equal(client.closed, true); await service.stop();
});
test("unknown dial result is retained as needs_review and cannot cause automatic redial", async () => {
  const { service, client, voice } = await fixture(); client.dialHook = async () => { throw new NativeCallError("command_timeout", true); };
  await assert.rejects(service.dial("allowed-peer", { assertCurrent() {} }), /needs review/);
  await assert.rejects(service.dial("allowed-peer", { assertCurrent() {} }), /not available/);
  assert.deepEqual(client.scopes, [{ kind: "outgoing" }]);
  assert.equal(client.dials, 1); assert.equal(service.status().state, "needs_review"); assert.equal(voice.state.closes, 1); await service.stop();
});
test("a clear discards the paced backlog and fresh PCM waits for the new track", async t => {
  const media = new Media(); const voice = voiceFactory(); let failures = 0; let resamplers = 0;
  voice.prepared.resampler = () => {
    resamplers++; let previous = 0;
    return { process(value) {
      const converted = Buffer.from(value); converted.writeInt16LE(converted.readInt16LE(0) + previous, 0);
      previous = value.readInt16LE(value.length - 2); return converted;
    }, flush: () => Buffer.alloc(0) };
  };
  let release!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  const output = new CallPcmOutput(media, format, voice.prepared, async () => { await held; media.generation = 2; }, () => { failures++; });
  t.after(() => output.close());
  const old = Buffer.alloc(3840); const fresh = Buffer.alloc(1280);
  for (let i = 0; i < old.length; i += 2) old.writeInt16LE(100, i);
  for (let i = 0; i < fresh.length; i += 2) fresh.writeInt16LE(200, i);
  output.enqueue(old, { itemId: "interrupted" });
  output.start();
  assert.deepEqual(media.sent.map(frame => frame.samples.length), [640]);
  const clearing = output.clear();
  output.enqueue(fresh, { itemId: "fresh-response" });
  await delay(30); assert.equal(media.sent.length, 1); assert.equal(resamplers, 2);
  release(); await clearing; await until(() => media.sent.length === 2);
  assert.equal(failures, 0);
  for (const frame of media.sent.slice(1)) {
    assert.equal(frame.generation, 2);
    assert.deepEqual(frame.samples, new Int16Array(640).fill(200));
  }
});
test("response PCM stays ordered and resumes with small chunks after a timer stall", async t => {
  const media = new Media(); const voice = voiceFactory(); let failures = 0;
  const output = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => { failures++; });
  t.after(() => output.close());
  const burst = Buffer.alloc(640 * 8);
  for (let i = 0; i < burst.length / 2; i++) burst.writeInt16LE(i + 1, i * 2);
  output.enqueue(burst, { itemId: "burst" });
  output.start();
  assert.deepEqual(media.sent.map(frame => frame.samples.length), [640]);
  // Hold the event loop across several slots. A late timer must not send its debt.
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 65);
  await until(() => media.sent.length >= 2);
  assert.ok(media.sent[1].samples.length <= 640);
  const speech = () => media.sent.flatMap(frame => Array.from(frame.samples).filter(Boolean));
  await until(() => speech().length === burst.length / 2);
  assert.deepEqual(speech(), Array.from({ length: burst.length / 2 }, (_, i) => i + 1));
  assert.equal(failures, 0);
});
test("continuous PCM plays unlabeled audio and accepts new audio after each provider clear", async t => {
  const media = new Media(); const voice = voiceFactory(); let failures = 0; let clears = 0;
  const output = new CallPcmOutput(media, format, voice.prepared, async () => { clears++; }, () => { failures++; }, undefined, { continuous: true });
  t.after(() => output.close());
  const speech = () => media.sent.flatMap(frame => Array.from(frame.samples).filter(Boolean));
  output.start();
  output.enqueue(Buffer.alloc(640, 1));
  await until(() => speech().length === 320);
  await output.clear();
  output.enqueue(Buffer.alloc(640, 2));
  await until(() => speech().length === 640);
  assert.ok(speech().slice(320).every(value => value === 0x0202));
  assert.equal(clears, 1); assert.equal(failures, 0);
});
test("continuous PCM keeps only the newest three seconds when its backlog grows", t => {
  const media = new Media(); const voice = voiceFactory();
  let now = 100;
  t.mock.method(performance, "now", () => now);
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const output = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => assert.fail("PCM send failed"), undefined, { continuous: true });
  t.after(() => output.close());
  output.start();
  output.enqueue(Buffer.alloc(32 * 1000, 1));
  for (let i = 0; i < 3; i++) output.enqueue(Buffer.alloc(32 * 1000, 2));
  for (let i = 0; i < 220; i++) { now += 20; t.mock.timers.tick(20); }
  const samples = media.sent.flatMap(frame => Array.from(frame.samples)).filter(Boolean);
  assert.equal(samples.filter(value => value === 0x0101).length, 0);
  assert.equal(samples.filter(value => value === 0x0202).length, 3000 * 16);
});
test("playout starts with silence and reports only the time played from each reply item", t => {
  const media = new Media();
  const voice = voiceFactory();
  let now = 100;
  t.mock.method(performance, "now", () => now);
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const output = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => assert.fail("PCM send failed"));
  t.after(() => output.close());
  output.start();
  assert.deepEqual(media.sent[0].samples, new Int16Array(640));
  assert.deepEqual(output.playbackState(), []);
  output.enqueue(Buffer.alloc(320, 1), { itemId: "one" });
  output.enqueue(Buffer.alloc(320, 2), { itemId: "two" });
  now = 120;
  t.mock.timers.tick(20);
  now = 145;
  assert.deepEqual(output.playbackState(), [{ itemId: "one", audioEndMs: 5 }, { itemId: "two", audioEndMs: 0 }]);
});
test("audio trace aggregates each boundary and a discarded PCM chunk still advances provider time", async t => {
  const previous = process.env.WX_CALL_AUDIO_TRACE;
  process.env.WX_CALL_AUDIO_TRACE = "1";
  t.after(() => { if (previous === undefined) delete process.env.WX_CALL_AUDIO_TRACE; else process.env.WX_CALL_AUDIO_TRACE = previous; });
  const lines: string[] = [];
  const { service, client, voice } = await fixture(line => lines.push(line));
  t.after(() => service.stop());
  await service.dial("allowed-peer", { assertCurrent() {} });
  await until(() => client.media.observePcm !== undefined);
  const receivedAtMs = performance.now();
  client.media.observePcm!({ receivedAtMs, samples: 320, queueDepth: 16, droppedSamples: 320 });
  client.media.input.push({ payload: new Uint8Array(640), receivedAtMs, sampleOffset: 320 });
  const reply = Buffer.alloc(640, 1);
  voice.state.callbacks!.audio(reply, { itemId: "private-response-item" });
  await until(() => voice.state.incoming.length === 1 && client.media.sent.some(frame => frame.samples.includes(257)));
  assert.deepEqual(voice.state.timestamps, [20]);
  await service.stop();
  const prefix = "[wechat-call-audio] plugin ";
  assert.equal(lines[0], prefix + "service-ready state=available");
  const report = JSON.parse(lines[1].slice(prefix.length));
  assert.equal(report.final, true);
  assert.equal(report.counts.wsRxDroppedSamples, 320);
  assert.equal(report.counts.inputConsumedSamples, 320);
  assert.equal(report.counts.realtimeInputSamples, 320);
  assert.equal(report.counts.daemonOutputSamples - report.counts.outputSilenceSamples, 320);
  assert.equal(report.measures.sdkRxQueueDepth.max, 16);
  assert.equal(report.measures.sdkRxWaitMs.count, 1);
  assert.equal(typeof report.eventLoopDelayMs.count, "number");
  assert.doesNotMatch(lines.join("\n"), /allowed-peer|private-response-item|private-lease|synthetic-token/);
});
test("overlapping clears keep interrupted late chunks revoked and stale acknowledgements paused", async t => {
  const media = new Media(); const voice = voiceFactory(); let failures = 0;
  const releases: Array<() => void> = [];
  const output = new CallPcmOutput(media, format, voice.prepared,
    () => new Promise<void>(resolve => { releases.push(resolve); }), () => { failures++; });
  t.after(() => output.close());
  const pcm = (sample: number) => { const value = Buffer.alloc(640); for (let i = 0; i < value.length; i += 2) value.writeInt16LE(sample, i); return value; };
  output.enqueue(pcm(100), { itemId: "first-response" });
  output.start();
  const first = output.clear();
  output.enqueue(pcm(200), { itemId: "second-response" });
  const second = output.clear();
  output.enqueue(pcm(300), { itemId: "current-response" });
  output.enqueue(pcm(101), { itemId: "first-response" });
  output.enqueue(pcm(201), { itemId: "second-response" });
  media.generation = 2; releases[0](); await first;
  await delay(30); assert.equal(media.sent.length, 1);
  media.generation = 3; releases[1](); await second;
  await until(() => media.sent.length === 2);
  assert.equal(media.sent[1].generation, 3);
  assert.deepEqual(media.sent[1].samples.slice(0, 320), new Int16Array(320).fill(300));
  assert.ok(media.sent[1].samples.slice(320).every(value => value === 0));
  output.enqueue(pcm(102), { itemId: "first-response" });
  output.enqueue(pcm(202), { itemId: "second-response" });
  await delay(30);
  assert.equal(media.sent.filter(frame => frame.samples.some(Boolean)).length, 2);
  assert.equal(failures, 0);
});
test("terminal ownership revocation stops old audio callbacks and does not manufacture a hangup", async () => {
  const { service, client, voice } = await fixture(); await service.dial("allowed-peer", { assertCurrent() {} }); await until(() => client.attaches === 1);
  client.eventsQueue.push({ type: "call.state", call: view("dial", "uncertain") }); await until(() => voice.state.closes === 1);
  const sentBeforeLateCallback = client.media.sent.length;
  voice.state.callbacks!.audio(Buffer.alloc(640), { itemId: "late" }); await delay(10); assert.equal(client.media.sent.length, sentBeforeLateCallback); assert.equal(client.ends, 0);
  client.eventsQueue.push({ type: "call.state", call: view("other-call", "ended") });
  await delay(1); assert.equal(service.status().state, "needs_review");
  client.eventsQueue.push({ type: "call.state", call: view("dial", "ended") });
  client.eventsQueue.push({ type: "calls.availability", availability: { busy: false, ownedByThisClient: false } });
  await until(() => service.status().state === "available");
  assert.equal(client.dials, 1); assert.equal(client.ends, 0);
  await service.stop();
});
test("minimal availability revocation also tears down the former owner's provider", async () => {
  const { service, client, voice } = await fixture(); await service.dial("allowed-peer", { assertCurrent() {} }); await until(() => client.attaches === 1);
  client.eventsQueue.push({ type: "calls.availability", availability: { busy: true, ownedByThisClient: false } });
  await until(() => voice.state.closes === 1); assert.equal(service.status().owned, false); assert.equal(client.ends, 0); await service.stop();
});
test("provider failure requests one end while retaining the owner until terminal", async () => {
  const { service, client, voice } = await fixture(); await service.dial("allowed-peer", { assertCurrent() {} }); await until(() => client.attaches === 1);
  voice.state.callbacks!.failed(); await until(() => voice.state.closes === 1); assert.equal(client.ends, 1);
  assert.equal(client.media.closed, false); assert.equal(client.closed, false); assert.equal(service.status().owned, true);
  client.eventsQueue.push({ type: "call.state", call: view("dial", "ended") });
  await until(() => service.status().state === "available"); await service.stop(); assert.equal(client.ends, 1); assert.equal(client.closed, true);
});
test("media attachment completing after end is retained without starting audio until terminal", async t => {
  const { service, client, voice } = await fixture();
  let release!: () => void; const held = new Promise<void>(resolve => { release = resolve; });
  const attach = client.attachMedia.bind(client);
  client.attachMedia = async (callId, observePcm) => { const media = await attach(callId, observePcm); await held; return media; };
  t.after(async () => { release(); await service.stop(); });
  await service.dial("allowed-peer", { assertCurrent() {} }); await until(() => client.attaches === 1);
  await service.end(); assert.equal(client.media.closed, false); assert.equal(client.media.stopped, false);
  release(); await until(() => client.media.stopped);
  assert.equal(client.media.closed, false); assert.equal(client.closed, false); assert.equal(service.status().owned, true);
  voice.state.callbacks!.audio(Buffer.alloc(640), { itemId: "late-attachment" }); voice.state.callbacks!.failed();
  await delay(20); assert.equal(client.media.sent.length, 0); assert.equal(client.ends, 1);
  client.eventsQueue.push({ type: "call.state", call: view("dial", "ended") });
  await until(() => service.status().state === "available"); assert.equal(client.media.closed, true);
});
test("a missing terminal reply reaches needs_review once and repeated ending cannot extend the deadline", async t => {
  const { service, client, voice } = await fixture(); await service.dial("allowed-peer", { assertCurrent() {} }); await until(() => client.attaches === 1);
  t.after(async () => { t.mock.timers.reset(); await service.stop(); });
  t.mock.timers.enable({ apis: ["setTimeout"] });
  await service.end(); await new Promise(resolve => setImmediate(resolve));
  assert.equal(client.ends, 1); assert.equal(voice.state.closes, 1); assert.equal(client.media.closed, false);
  t.mock.timers.tick(20_000);
  client.eventsQueue.push({ type: "call.state", call: view("dial", "ending") });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(service.status().owned, true); assert.equal(client.closed, false);
  t.mock.timers.tick(10_001); await new Promise(resolve => setImmediate(resolve));
  assert.equal(service.status().state, "needs_review"); assert.equal(service.status().owned, false);
  assert.equal(client.closed, true); assert.equal(client.media.closed, true); assert.equal(client.ends, 1);
});
test("the PCM budget truncates a reply without ending the call or stitching later chunks into it", async t => {
  const media = new Media(); const voice = voiceFactory(); let failures = 0;
  const output = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => { failures++; });
  output.enqueue(Buffer.alloc(640), undefined); output.enqueue(Buffer.alloc(640), { itemId: "late" }); assert.equal(failures, 1); assert.equal(media.sent.length, 0);
  const malformed = new CallPcmOutput(media, format, voice.prepared, async () => {}, () => { failures++; });
  malformed.enqueue(Buffer.alloc(64002), { itemId: "oversized-chunk" });
  assert.equal(failures, 2);

  let converted = 0;
  voice.prepared.resampler = () => ({ process: value => { converted++; return value; }, flush: () => Buffer.alloc(0) });
  let release!: () => void;
  const held = new Promise<void>(resolve => { release = resolve; });
  const lines: string[] = [];
  const trace = new CallAudioTrace(line => lines.push(line), { daemonHz: 16000, realtimeHz: 16000 });
  const bounded = new CallPcmOutput(media, format, voice.prepared, () => held, () => { failures++; }, trace);
  t.after(() => { bounded.close(); trace.close(); });
  const clearing = bounded.clear();
  bounded.start();
  for (const size of [64000, 64000, 64000, 63400]) bounded.enqueue(Buffer.alloc(size, 0x11), { itemId: "first" });
  bounded.enqueue(Buffer.alloc(640, 0x22), { itemId: "first" });
  assert.equal(media.sent.length, 0);
  release(); await clearing;
  assert.deepEqual(media.sent[0].samples, new Int16Array(640).fill(0x1111));
  bounded.enqueue(Buffer.alloc(640), { itemId: "first" });
  assert.equal(converted, 5);
  bounded.enqueue(Buffer.alloc(640), { itemId: "next" });
  assert.equal(converted, 6);
  assert.equal(failures, 2);
  bounded.close(); trace.close();
  const report = JSON.parse(lines[0].slice("[wechat-call-audio] plugin ".length));
  assert.equal(report.counts.outputTruncatedItems, 1);
  assert.equal(report.counts.outputOverflowSamples, 20);
  assert.equal(report.counts.outputOverflowProviderSamples, 320);
  assert.equal(report.measures.outputQueuedAudioMs.max, 8000);
});
