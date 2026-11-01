import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import type { PluginRuntime } from "openclaw/plugin-sdk/core";
import { callsConfigSchema, resolveWeChatCallsConfig } from "./calls-config.ts";
import { prepareOpenClawCallVoice, type VoiceCallbacks } from "./calls-voice.ts";

const pcm = { encoding: "pcm16", sampleRateHz: 24000, channels: 1 };
function config(consult = false) { return resolveWeChatCallsConfig({ enabled: true, provider: "openai", agentId: "agent", toolPolicy: consult ? "safe-read-only" : "none", allowConsultSessionPersistence: consult })!; }
const callbacks: VoiceCallbacks = { audio() {}, clear() {}, failed() {} };

test("calling schemas agree across runtime and installed plugin manifests", async () => {
  const manifest = JSON.parse(await readFile(new URL("../openclaw.plugin.json", import.meta.url), "utf8"));
  const pkg = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
  assert.deepEqual(manifest.configSchema.properties.calls, callsConfigSchema);
  assert.deepEqual(manifest.channelConfigs["agent-wechat"].schema.properties.calls, callsConfigSchema);
  assert.deepEqual(pkg.openclaw.channelConfigs["agent-wechat"].schema.properties.calls, callsConfigSchema);
  assert.equal(manifest.toolMetadata.wechat_call.optional, true);
});

test("installed host bridge and streaming resampler work with an account-free fake provider", async () => {
  const voicePath: string = "openclaw/plugin-sdk/realtime-voice";
  const audioPath: string = "openclaw/plugin-sdk/realtime-voice-provider";
  const [sdk, audio] = await Promise.all([import(voicePath), import(audioPath)]);
  let providerRequest: any; let bridgeParams: any; const input: Buffer[] = []; let closed = 0; let connected = false;
  const fake = { id: "openai", createBridge(request: any) { providerRequest = request; return {
    connect: async () => { connected = true; request.onReady(); }, isConnected: () => connected, outputAudioMode: "response",
    sendAudio: (value: Buffer) => input.push(value), setMediaTimestamp() {}, acknowledgeMark() {}, submitToolResult() {},
    close: () => { closed++; connected = false; },
  }; } };
  let resolved = 0;
  const prepared = await prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, {
    voiceSdk: { ...sdk, createRealtimeVoiceBridgeSession: (params: unknown) => { bridgeParams = params; return sdk.createRealtimeVoiceBridgeSession(params); }, resolveConfiguredRealtimeVoiceProvider: () => { resolved++; return { provider: fake, providerConfig: {}, capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true } }; } }, audioSdk: audio,
  });
  assert.equal(resolved, 1); let heard = 0; let clears = 0;
  const session = await prepared.open({ audio: (_value, metadata) => { heard++; assert.equal(metadata?.itemId, "item"); }, clear: () => { clears++; }, failed() { assert.fail("Unexpected provider failure"); } }, { senderId: "synthetic-peer", signal: new AbortController().signal });
  session.sendAudio(Buffer.alloc(960)); assert.equal(input.length, 1);
  providerRequest.onAudio(Buffer.alloc(960), { itemId: "item" }); providerRequest.onClearAudio("barge-in"); assert.equal(heard, 1); assert.equal(clears, 1);
  assert.deepEqual(providerRequest.tools, []); assert.equal(providerRequest.runAgentConsult, undefined); assert.equal(bridgeParams.onTranscript, undefined);
  providerRequest.onTranscript("user", "Synthetic discarded transcript", true);
  await session.close(); providerRequest.onAudio(Buffer.alloc(960), { itemId: "late" }); assert.equal(heard, 1); assert.equal(closed, 1);
  const generated = Buffer.alloc(3200); for (let i = 0; i < 1600; i++) generated.writeInt16LE(Math.round(4000 * Math.sin(i * 2 * Math.PI * 440 / 16000)), i * 2);
  const whole = prepared.resampler(16000, 24000); const expected = Buffer.concat([whole.process(generated), whole.flush()]);
  const stream = prepared.resampler(16000, 24000); const pieces: Buffer[] = [];
  for (let i = 0; i < generated.length; i += 83) pieces.push(stream.process(generated.subarray(i, i + 83)));
  pieces.push(stream.flush()); assert.deepEqual(Buffer.concat(pieces), expected); assert.equal(expected.length, 4800);
});

test("missing SDK and incompatible provider capabilities fail before creating a bridge", async () => {
  await assert.rejects(prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, { voiceSdk: {}, audioSdk: {} }), /SDK unavailable/);
  let created = 0;
  const sdk = { createRealtimeVoiceBridgeSession() { created++; }, resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai" }, providerConfig: {}, capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [], supportsBargeIn: true } }), resolveRealtimeVoiceAgentConsultTools() {}, resolveRealtimeVoiceAgentConsultToolsAllow() {} };
  await assert.rejects(prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, { voiceSdk: sdk, audioSdk: { createStreamingPcmResampler() {} } }), /capabilities unavailable/);
  assert.equal(created, 0);
});

test("opted-in consultation is isolated, non-owner, policy-limited and deduplicated", async () => {
  let bridgeArgs: any; const consultations: any[] = []; const results: unknown[] = []; let closed = 0;
  const sdk = { REALTIME_VOICE_AGENT_CONSULT_TOOL_NAME: "openclaw_agent_consult",
    createRealtimeVoiceBridgeSession(args: any) { bridgeArgs = args; return {
      bridge: { isConnected: () => true, outputAudioMode: "response" }, connect: async () => args.onReady(), sendAudio() {}, setMediaTimestamp() {},
      close() { closed++; }, submitToolResult: (_id: string, value: unknown) => { results.push(value); },
    }; },
    resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai", createBridge() {} }, providerConfig: {}, capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true, supportsToolCalls: true } }),
    resolveRealtimeVoiceAgentConsultTools: () => [{ name: "openclaw_agent_consult" }], resolveRealtimeVoiceAgentConsultToolsAllow: () => ["read"],
    consultRealtimeVoiceAgent: async (args: unknown) => { consultations.push(args); return { text: "Synthetic result" }; },
  };
  const runtime = { agent: { runEmbeddedAgent() {} } } as unknown as PluginRuntime;
  const prepared = await prepareOpenClawCallVoice(config(true), {}, runtime, { voiceSdk: sdk, audioSdk: { createStreamingPcmResampler() {} } });
  const session = await prepared.open(callbacks, { senderId: "synthetic-peer", signal: new AbortController().signal });
  const event = { callId: "tool1", name: "openclaw_agent_consult", args: { question: "Read synthetic data" } };
  await bridgeArgs.onToolCall(event); await bridgeArgs.onToolCall(event);
  assert.equal(consultations.length, 1); assert.equal(results.length, 2); assert.equal(consultations[0].senderIsOwner, false);
  assert.deepEqual(consultations[0].toolsAllow, ["read"]); assert.deepEqual(consultations[0].transcript, []);
  assert.match(consultations[0].sessionKey, /^agent:agent:wechat-call:/); assert.equal(bridgeArgs.onTranscript, undefined);
  await bridgeArgs.onToolCall({ ...event, callId: "tool2", name: "exec" }); assert.equal(consultations.length, 1);
  await session.close(); await bridgeArgs.onToolCall({ ...event, callId: "late" }); assert.equal(consultations.length, 1); assert.equal(closed, 1);
});
test("unverified providers and continuous output fail without connecting or falling back", async () => {
  let connects = 0; let closes = 0;
  const caps = { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true };
  const sdk = { createRealtimeVoiceBridgeSession: () => ({ bridge: { outputAudioMode: "continuous" }, connect: async () => { connects++; }, close() { closes++; }, sendAudio() {}, setMediaTimestamp() {} }),
    resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "other", createBridge() {} }, providerConfig: {}, capabilities: caps }),
    resolveRealtimeVoiceAgentConsultTools: () => [], resolveRealtimeVoiceAgentConsultToolsAllow: () => [] };
  const audioSdk = { createStreamingPcmResampler() {} };
  await assert.rejects(prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, { voiceSdk: sdk, audioSdk }), /provider unsupported/);
  const prepared = await prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, { voiceSdk: { ...sdk, resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai", createBridge() {} }, providerConfig: {}, capabilities: caps }) }, audioSdk });
  await assert.rejects(prepared.open(callbacks, { senderId: "peer", signal: new AbortController().signal }), /provider unavailable/);
  assert.equal(connects, 0); assert.equal(closes, 1);
});
