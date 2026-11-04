import test from "node:test";
import assert from "node:assert/strict";
import { AsyncLocalStorage } from "node:async_hooks";
import { link, mkdir, mkdtemp, readFile, rm, unlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import type { PluginRuntime } from "openclaw/plugin-sdk/core";
import { callsConfigSchema, resolveWeChatCallsConfig } from "./calls-config.ts";
import { prepareOpenClawCallVoice, type VoiceCallbacks } from "./calls-voice.ts";

const pcm = { encoding: "pcm16", sampleRateHz: 24000, channels: 1 };
function config(consult = false) { return resolveWeChatCallsConfig({ voice: { enabled: true, realtime: { provider: "openai" }, agentId: "agent", toolPolicy: consult ? "safe-read-only" : "none", allowConsultSessionPersistence: consult } })!.voice!; }
const callbacks: VoiceCallbacks = { audio() {}, clear() {}, failed() {} };

test("calling schemas agree across runtime and installed plugin manifests", async () => {
  const manifest = JSON.parse(await readFile(new URL("../openclaw.plugin.json", import.meta.url), "utf8"));
  const pkg = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
  assert.deepEqual(manifest.configSchema.properties.calls, callsConfigSchema);
  assert.deepEqual(manifest.channelConfigs["agent-wechat"].schema.properties.calls, callsConfigSchema);
  assert.deepEqual(pkg.openclaw.channelConfigs["agent-wechat"].schema.properties.calls, callsConfigSchema);
  assert.equal(manifest.toolMetadata.wechat_call.optional, true);
  const credentialPath = "calls.voice.realtime.providers.openai.apiKey";
  assert.ok(manifest.configContracts.secretInputs.paths.some((item: any) => item.path === credentialPath && item.expected === "string"));
  assert.equal(manifest.uiHints[credentialPath].sensitive, true);

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
  let resolved = 0; let selection: any;
  const customized = resolveWeChatCallsConfig({ voice: { enabled: true, agentId: "agent", realtime: {
    provider: "openai", model: "synthetic-realtime", voice: "synthetic-voice", instructions: "Use short sentences.", consultPolicy: "always",
    providers: { openai: { apiKey: "synthetic-openai-secret" } },
  } } })!.voice!;
  const cfg = { agents: { list: [
    { id: "main", default: true, identity: { name: "Synthetic default" } },
    { id: "agent", identity: { name: "Synthetic selected", theme: "Dry and precise" } },
  ] } };
  const prepared = await prepareOpenClawCallVoice(customized, cfg, {} as PluginRuntime, {
    voiceSdk: { ...sdk, createRealtimeVoiceBridgeSession: (params: unknown) => { bridgeParams = params; return sdk.createRealtimeVoiceBridgeSession(params); }, resolveConfiguredRealtimeVoiceProvider: (params: any) => { selection = params; resolved++; return { provider: fake, providerConfig: {}, capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true } }; } }, audioSdk: audio,
  });
  assert.deepEqual(selection.providerConfigOverrides, { model: "synthetic-realtime", voice: "synthetic-voice" });
  assert.equal(selection.providerConfigs.openai.apiKey, "synthetic-openai-secret");
  assert.equal(JSON.stringify(customized).includes("synthetic-openai-secret"), false);
  assert.equal(resolved, 1); let heard = 0; let clears = 0;
  const session = await prepared.open({ audio: (_value, metadata) => { heard++; assert.equal(metadata?.itemId, "item"); }, clear: () => { clears++; }, failed() { assert.fail("Unexpected provider failure"); } }, { senderId: "synthetic-peer", signal: new AbortController().signal });
  assert.equal(providerRequest.instructions, bridgeParams.instructions);
  assert.equal(providerRequest.agentId, "agent");
  assert.match(bridgeParams.instructions, /Synthetic selected/);
  assert.match(bridgeParams.instructions, /Dry and precise/);
  assert.match(bridgeParams.instructions, /Use short sentences/);
  assert.match(bridgeParams.instructions, /does not grant account-owner authority/);
  assert.match(bridgeParams.instructions, /conversation-only/);
  assert.doesNotMatch(bridgeParams.instructions, /Synthetic default|openclaw_agent_consult|synthetic-openai-secret/);
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

test("opted-in default-agent voice profile uses only bounded identity and persona files", async t => {
  const dir = await mkdtemp(join(tmpdir(), "wechat-call-voice-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const workspace = join(dir, "workspace");
  await mkdir(workspace);
  const identity = join(workspace, "IDENTITY.md");
  const privateFile = join(dir, "unrelated.md");
  await Promise.all([
    writeFile(identity, "- Name: Synthetic Main Voice"),
    writeFile(join(workspace, "SOUL.md"), "Speak with quiet curiosity.\n" + "x".repeat(8000)),
    writeFile(join(workspace, "USER.md"), "private-user-sentinel"),
    writeFile(join(workspace, "AGENTS.md"), "private-workspace-sentinel"),
    writeFile(privateFile, "private-hardlink-sentinel"),
  ]);
  const voice = { enabled: true, agentId: "main", realtime: { provider: "openai" } };
  const base = resolveWeChatCallsConfig({ voice })!.voice!;
  const optedIn = resolveWeChatCallsConfig({ voice: { ...voice, realtime: { ...voice.realtime, agentContext: { enabled: true } } } })!.voice!;
  assert.equal(base.realtime.agentContext.enabled, false);
  assert.equal(base.realtime.consultPolicy, "auto");
  assert.throws(() => resolveWeChatCallsConfig({ voice: { ...voice, realtime: {
    ...voice.realtime, agentContext: { enabled: true, files: ["USER.md"] } as any,
  } } }), /Invalid calling configuration/);
  let instructions = "";
  const voiceSdk = {
    createRealtimeVoiceBridgeSession(args: any) {
      instructions = args.instructions;
      return { bridge: { isConnected: () => true, outputAudioMode: "response" }, connect: async () => args.onReady(),
        sendAudio() {}, setMediaTimestamp() {}, close() {} };
    },
    resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai", createBridge() {} }, providerConfig: {},
      capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true } }),
    resolveRealtimeVoiceAgentConsultTools: () => [],
    resolveRealtimeVoiceAgentConsultToolsAllow: () => [],
  };
  // Legacy main resolves the host default workspace without an agents.list entry.
  const cfg = { agents: { defaults: { workspace } } };
  const prepare = (value: typeof base) => prepareOpenClawCallVoice(value, cfg, {} as PluginRuntime,
    { voiceSdk, audioSdk: { createStreamingPcmResampler() {} } });
  async function prompt(prepared: Awaited<ReturnType<typeof prepare>>) {
    const session = await prepared.open(callbacks, { senderId: "peer", signal: new AbortController().signal });
    await session.close();
    return instructions;
  }
  assert.doesNotMatch(await prompt(await prepare(base)), /Synthetic Main Voice|quiet curiosity/);
  const prepared = await prepare(optedIn);
  const profile = await prompt(prepared);
  assert.match(profile, /IDENTITY\.md\):\n- Name: Synthetic Main Voice/);
  assert.match(profile, /SOUL\.md\):\nSpeak with quiet curiosity/);
  assert.match(profile, /\[truncated\]/);
  assert.ok(profile.length < 7200);
  assert.doesNotMatch(profile, /USER\.md|AGENTS\.md|private-/);
  await unlink(identity);
  await link(privateFile, identity);
  assert.doesNotMatch(await prompt(prepared), /private-hardlink-sentinel|Synthetic Main Voice/);
});

test("missing SDK and incompatible provider capabilities fail before creating a bridge", async () => {
  await assert.rejects(prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, { voiceSdk: {}, audioSdk: {} }), /SDK unavailable/);
  let created = 0;
  const sdk = { createRealtimeVoiceBridgeSession() { created++; }, resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai" }, providerConfig: {}, capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [], supportsBargeIn: true } }), resolveRealtimeVoiceAgentConsultTools() {}, resolveRealtimeVoiceAgentConsultToolsAllow() {} };
  await assert.rejects(prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, { voiceSdk: sdk, audioSdk: { createStreamingPcmResampler() {} } }), /capabilities unavailable/);
  assert.equal(created, 0);
  await assert.rejects(prepareOpenClawCallVoice(config(), {}, {} as PluginRuntime, { voiceSdk: { ...sdk,
    resolveConfiguredRealtimeVoiceProvider() { throw Error("synthetic-private-provider-detail"); },
  }, audioSdk: { createStreamingPcmResampler() {} } }), error => error instanceof Error && error.message === "Calling realtime provider configuration unavailable");

});

test("opted-in consultation is isolated, non-owner, policy-limited and deduplicated", async () => {
  const voicePath: string = "openclaw/plugin-sdk/realtime-voice";
  const host = await import(voicePath);
  let bridgeArgs: any; const consultations: any[] = []; const results: unknown[] = []; const spoken: string[] = []; let closed = 0;
  const instance = new AsyncLocalStorage<{ active: boolean }>();
  const runInBackgroundContext = <T>(run: () => T): T => {
    const lease = { active: true };
    const result = instance.run(lease, run);
    if (result instanceof Promise) return result.finally(() => { lease.active = false; }) as T;
    lease.active = false;
    return result;
  };
  const voiceContext = { senderId: "synthetic-peer", signal: new AbortController().signal, runInBackgroundContext };
  const sdk = { ...host,
    createRealtimeVoiceBridgeSession(args: any) { bridgeArgs = args; return {
      bridge: { isConnected: () => true, outputAudioMode: "response" }, connect: async () => args.onReady(), sendAudio() {}, setMediaTimestamp() {},
      close() { closed++; }, submitToolResult: (_id: string, value: unknown) => { results.push(value); },
      sendUserMessage(text: string) { spoken.push(text); },
    }; },
    resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai", createBridge() {} }, providerConfig: {}, capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true, supportsToolCalls: true } }),
    resolveRealtimeVoiceAgentConsultTools: host.resolveRealtimeVoiceAgentConsultTools, resolveRealtimeVoiceAgentConsultToolsAllow: host.resolveRealtimeVoiceAgentConsultToolsAllow,
    consultRealtimeVoiceAgent: async (args: unknown) => {
      assert.equal(instance.getStore()?.active, true, "SDK must enter the current plugin instance");
      await delay(0);
      assert.equal(instance.getStore()?.active, true, "SDK work must retain the instance across awaits");
      consultations.push(args);
      return { text: "Synthetic result" };
    },
  };
  const lookups: string[] = []; const thinkingModels: Array<{ provider: string; model: string }> = [];
  let parentId = "parent-id";
  const runtime = { agent: { runEmbeddedAgent() {}, resolveThinkingDefault({ provider, model }: { provider: string; model: string }) {
    thinkingModels.push({ provider, model });
    return "low";
  }, session: {
    resolveStorePath: () => "/synthetic/store",
    getSessionEntry: (args: { sessionKey: string }) => {
      assert.equal(instance.getStore()?.active, true, "parent reads require the current plugin instance");
      lookups.push(args.sessionKey);
      return args.sessionKey === "agent:agent:conversation" ? { sessionId: parentId } : undefined;
    },
  } } } as unknown as PluginRuntime;
  const consulted = config(true); consulted.consultation = { model: "synthetic-provider/synthetic-model", timeoutMs: 15000 };
  consulted.realtime.consultPolicy = "substantive";
  consulted.realtime.consultThinkingLevel = "minimal";
  consulted.realtime.consultFastMode = true;
  const hostCfg = { session: { mainKey: "conversation" } };
  const dependencies = { voiceSdk: sdk, audioSdk: { createStreamingPcmResampler() {} } };
  const prepared = await prepareOpenClawCallVoice(consulted, hostCfg, runtime, dependencies);
  const session = await runInBackgroundContext(() => prepared.open(callbacks, voiceContext));
  const invoke = async (event: unknown) => {
    await bridgeArgs.onToolCall(event);
    await delay(0);
  };
  assert.deepEqual(bridgeArgs.tools.map((tool: { name: string }) => tool.name), ["openclaw_agent_consult"]);
  assert.match(bridgeArgs.instructions, /Call openclaw_agent_consult before answering/);
  assert.match(bridgeArgs.instructions, /Consultation is read-only/);
  const event = { callId: "tool1", name: "openclaw_agent_consult", args: { question: "Read synthetic data" } };
  // A provider callback may inherit an already released request context.
  await instance.run({ active: false }, () => invoke(event));
  await invoke(event);
  assert.equal(consultations.length, 1); assert.equal(results.length, 1); assert.equal(consultations[0].senderIsOwner, false);
  assert.match(spoken[0], /Synthetic result/);
  assert.equal(consultations[0].provider, "synthetic-provider"); assert.equal(consultations[0].model, "synthetic-model"); assert.equal(consultations[0].timeoutMs, 15000);
  assert.equal(consultations[0].thinkLevel, "minimal"); assert.equal(consultations[0].fastMode, true);
  assert.deepEqual(consultations[0].toolsAllow, ["read", "web_search", "web_fetch", "x_search", "memory_search", "memory_get"]);
  assert.deepEqual(consultations[0].transcript, []);
  assert.equal(consultations[0].contextMode, "isolated");
  assert.match(consultations[0].extraSystemPrompt, /non-owner WeChat caller/);
  assert.match(consultations[0].sessionKey, /^agent:agent:wechat-call:/); assert.equal(bridgeArgs.onTranscript, undefined);
  await bridgeArgs.onToolCall({ ...event, callId: "tool2", name: "exec" }); assert.equal(consultations.length, 1);
  assert.deepEqual(await bridgeArgs.runAgentConsult({ prompt: "Check a synthetic fact" }), { text: "Synthetic result" });
  assert.equal(consultations[1].sessionKey, consultations[0].sessionKey);
  await session.close(); await bridgeArgs.onToolCall({ ...event, callId: "late" }); assert.equal(consultations.length, 2); assert.equal(closed, 1);

  const parentCases = [
    { key: "agent:agent:main", expectedSessionId: "parent-id", forked: true },
    { key: "agent:agent:main", expectedSessionId: "replaced-parent", forked: false },
    { key: "agent:agent:missing", forked: false },
    { key: "agent:main:main", forked: false },
    { key: "global", forked: false },
    { key: "agent:agent:dashboard:incognito-private", forked: false },
  ];
  for (const { forked, ...parentSession } of parentCases) {
    const next = await runInBackgroundContext(() => prepared.open(callbacks, { ...voiceContext, parentSession }));
    await invoke(event);
    const request = consultations.at(-1);
    assert.equal(request.contextMode, forked ? "fork" : "isolated");
    assert.equal(request.spawnedBy, forked ? "agent:agent:conversation" : undefined);
    assert.equal(request.senderIsOwner, false);
    assert.deepEqual(request.transcript, []);
    await next.close();
  }
  assert.deepEqual(lookups, ["agent:agent:conversation", "agent:agent:conversation", "agent:agent:conversation", "agent:agent:missing"]);
  assert.equal(new Set(consultations.map(request => request.sessionKey)).size, parentCases.length + 1);
  const afterReset = await runInBackgroundContext(() => prepared.open(callbacks, { ...voiceContext,
    parentSession: { key: "agent:agent:main", expectedSessionId: "parent-id" } }));
  parentId = "new-parent-id";
  await invoke(event);
  assert.equal(consultations.at(-1).contextMode, "isolated");
  assert.equal(consultations.at(-1).spawnedBy, undefined);
  await afterReset.close();
  const invalidInbound = config(true);
  invalidInbound.consultation.inboundSessionKey = "agent:main:main";
  await assert.rejects(prepareOpenClawCallVoice(invalidInbound, hostCfg, runtime, dependencies), /Invalid calling consultation session/);
  invalidInbound.consultation.inboundSessionKey = "agent:agent:main";
  await assert.rejects(prepareOpenClawCallVoice(invalidInbound, { session: { scope: "global" } }, runtime, dependencies), /Invalid calling consultation session/);

  const always = config(true);
  always.realtime.consultPolicy = "always";
  const configuredHost = { agents: { list: [{ id: "agent", model: "synthetic-provider/synthetic-model" }] } };
  const alwaysPrepared = await prepareOpenClawCallVoice(always, configuredHost, runtime, dependencies);
  spoken.length = 0;
  let clears = 0;
  const alwaysSession = await runInBackgroundContext(() => alwaysPrepared.open({ ...callbacks, clear: () => { clears++; } }, voiceContext));
  assert.match(bridgeArgs.instructions, /before every substantive answer/);
  assert.deepEqual(thinkingModels, [{ provider: "synthetic-provider", model: "synthetic-model" }]);
  instance.run({ active: false }, () => bridgeArgs.onTranscript("user", "Check the status of the synthetic deployment", true));
  for (let i = 0; i < 100 && !spoken.length; i++) await delay(10);
  assert.equal(spoken.length, 1);
  assert.match(spoken[0], /Internal OpenClaw voice playback result/);
  assert.match(spoken[0], /Synthetic result/);
  assert.equal(clears, 2);
  const fallback = consultations.at(-1);
  assert.deepEqual(fallback.args, { question: "Check the status of the synthetic deployment" });
  assert.deepEqual(fallback.transcript, []);
  assert.equal(fallback.thinkLevel, "low");
  assert.equal(fallback.provider, undefined);
  assert.equal(fallback.model, undefined);
  assert.equal(fallback.senderIsOwner, false);
  await alwaysSession.close();
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

test("owner consultation is scoped to the connected WeChat peer and drops privileges on revocation", async () => {
  const voicePath: string = "openclaw/plugin-sdk/realtime-voice";
  const host = await import(voicePath);
  const ownerPeer = "synthetic-owner";
  const otherPeer = "synthetic-other";
  const parentKey = "agent:agent:owner-chat";
  const settings = (owners: string[], toolPolicy: "owner" | "safe-read-only" | "none" = "owner") => ({
    commands: { ownerAllowFrom: owners },
    channels: { "agent-wechat": {
      native: { mode: "external", accountId: "native-account", endpoint: "http://127.0.0.1:1",
        token: "synthetic-token-".repeat(4), consumerStateDir: "/synthetic/consumer" },
      calls: { voice: { enabled: true, agentId: "agent", toolPolicy, allowConsultSessionPersistence: true, realtime: { provider: "openai" } } },
    } },
  });
  const exact = settings([`agent-wechat:${ownerPeer}`]);
  let live: ReturnType<typeof settings> | undefined = settings([
    `signal:${ownerPeer}`, `wechat:${ownerPeer}`, `agent-wechat:${ownerPeer.toUpperCase()}`, ownerPeer, "agent-wechat:*", "*",
  ]);
  let deliveryChannel = "agent-wechat";
  const runtime = { config: { current: () => live }, agent: { runEmbeddedAgent() {}, session: {
    resolveStorePath: () => "/synthetic/store",
    getSessionEntry: ({ sessionKey }: { sessionKey: string }) => sessionKey === parentKey ? {
      sessionId: "trusted-parent",
      delivery: { kind: "external", context: { channel: deliveryChannel, to: `wechat:${ownerPeer}`, accountId: "default" } },
    } : undefined,
  } } } as unknown as PluginRuntime;
  const consulted = resolveWeChatCallsConfig(exact.channels["agent-wechat"].calls)!.voice!;
  let bridgeArgs: any;
  const requests: any[] = [];
  const toolPolicies: string[] = [];
  const results: unknown[] = [];
  const spoken: string[] = [];
  const trace: string[] = [];
  let release!: () => void;
  const hold = new Promise<void>(resolve => { release = resolve; });
  const sdk = { ...host,
    createRealtimeVoiceBridgeSession(args: any) { bridgeArgs = args; return {
      bridge: { isConnected: () => true, outputAudioMode: "response" }, connect: async () => args.onReady(),
      sendAudio() {}, setMediaTimestamp() {}, close() {},
      submitToolResult(_id: string, result: unknown) { results.push(result); },
      sendUserMessage(text: string) { spoken.push(text); },
    }; },
    resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: "openai", createBridge() {} }, providerConfig: {},
      capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true, supportsToolCalls: true } }),
    resolveRealtimeVoiceAgentConsultTools(policy: string) {
      toolPolicies.push(policy);
      return host.resolveRealtimeVoiceAgentConsultTools(policy);
    },
    async consultRealtimeVoiceAgent(args: any) {
      requests.push(args);
      if (args.args.question === "simulate registry failure") {
        throw new Error("synthetic-private-value", { cause: new Error("Plugin registry scope is no longer available") });
      }
      if (args.args.question === "simulate workspace failure") {
        throw Object.assign(new Error("/synthetic/private/workspace"), { code: "EACCES" });
      }
      if (args.args.question === "simulate run failure") {
        args.onRunStarted();
        throw Error("synthetic-private-value");
      }
      if (args.args.question === "hold owner result") await hold;
      return { text: args.senderIsOwner ? "synthetic-privileged-result" : "synthetic-read-only-result" };
    },
  };
  const dependencies = { voiceSdk: sdk, audioSdk: { createStreamingPcmResampler() {} } };
  await assert.rejects(prepareOpenClawCallVoice(consulted, exact, runtime, {
    ...dependencies, voiceSdk: { ...sdk, REALTIME_VOICE_AGENT_CONSULT_SENDER_AUTH_VERSION: 0 },
  }), /Calling owner consultation unavailable/);
  const prepared = await prepareOpenClawCallVoice(consulted, exact, runtime, dependencies);
  const open = (senderId: string, connectedPeer = () => senderId, signal = new AbortController().signal) => prepared.open(callbacks, {
    senderId, connectedPeer, nativeAccountId: "native-account", channelAccountId: "default", logAudioTrace: line => trace.push(line),
    parentSession: { key: parentKey, expectedSessionId: "trusted-parent" }, signal,
  });
  const consult = async (callId: string, question = "Read a synthetic value") => {
    await bridgeArgs.onToolCall({
      name: host.REALTIME_VOICE_AGENT_CONSULT_TOOL_NAME, callId, args: { question, senderIsOwner: true },
    });
    await delay(0);
  };
  const assertReadOnly = (peer: string) => {
    const request = requests.at(-1);
    assert.equal(request.senderId, peer);
    assert.equal(request.senderIsOwner, false);
    assert.equal(request.contextMode, "isolated");
    assert.equal(request.spawnedBy, undefined);
    assert.deepEqual(request.toolsAllow, host.resolveRealtimeVoiceAgentConsultToolsAllow("safe-read-only"));
  };

  const wrongProvider = await open(ownerPeer);
  assert.match(bridgeArgs.instructions, /Consultation is read-only/);
  assert.equal(toolPolicies.at(-1), "safe-read-only");
  await consult("wrong-provider");
  assertReadOnly(ownerPeer);
  live = exact;
  await consult("never-promote");
  assertReadOnly(ownerPeer);
  await wrongProvider.close();

  const nonOwner = await open(otherPeer);
  await consult("owner-requester-is-not-the-peer");
  assertReadOnly(otherPeer);
  await nonOwner.close();

  deliveryChannel = "signal";
  const foreignRoute = await open(ownerPeer);
  await consult("no-foreign-route");
  assert.equal(requests.at(-1).senderIsOwner, true);
  assert.equal(requests.at(-1).contextMode, "isolated");
  assert.equal(requests.at(-1).spawnedBy, undefined);
  await foreignRoute.close();

  deliveryChannel = "agent-wechat";
  let connected = otherPeer;
  const owner = await open(ownerPeer, () => connected);
  assert.match(bridgeArgs.instructions, /Normal agent tools may be available/);
  assert.equal(toolPolicies.at(-1), "owner");
  const beforeAttach = requests.length;
  await consult("preflight-peer-mismatch");
  assert.equal(requests.length, beforeAttach);
  assert.deepEqual(results.at(-1), { error: "Calling consultation not admitted" });
  assert.equal(trace.length, beforeAttach * 2);
  connected = ownerPeer;
  await consult("cached-owner-result");
  const privileged = requests.at(-1);
  assert.equal(privileged.senderId, ownerPeer);
  assert.equal(privileged.senderIsOwner, true);
  assert.equal(privileged.messageProvider, "agent-wechat");
  assert.equal(privileged.toolsAllow, undefined);
  assert.equal(privileged.contextMode, "fork");
  assert.equal(privileged.spawnedBy, parentKey);
  assert.deepEqual(privileged.transcript, []);
  assert.equal(privileged.cfg, live);
  assert.match(spoken.at(-1)!, /synthetic-privileged-result/);
  const spokenBeforePending = spoken.length;
  await consult("pending-owner-result", "hold owner result");
  assert.equal((results.at(-1) as { status: string }).status, "working");
  assert.equal(spoken.length, spokenBeforePending);
  assert.equal(requests.at(-1).senderIsOwner, true);
  live = settings([]);
  release();
  await delay(0);
  assert.equal(spoken.length, spokenBeforePending);
  assert.deepEqual(trace.slice(-2), [
    "[wechat-call-audio] plugin consult mode=owner stage=started",
    "[wechat-call-audio] plugin consult mode=owner stage=failed category=authority-changed",
  ]);
  live = undefined;
  await consult("unavailable-snapshot");
  assertReadOnly(ownerPeer);
  live = exact;
  await consult("cached-owner-result");
  assertReadOnly(ownerPeer);
  assert.notEqual(requests.at(-1).sessionKey, privileged.sessionKey);
  assert.match(spoken.at(-1)!, /synthetic-read-only-result/);
  assert.deepEqual(trace.slice(-2), [
    "[wechat-call-audio] plugin consult mode=safe-read-only stage=started",
    "[wechat-call-audio] plugin consult mode=safe-read-only stage=ok",
  ]);
  for (const [question, category] of [
    ["simulate registry failure", "inactive-registry"],
    ["simulate workspace failure", "filesystem"],
    ["simulate run failure", "embedded-run"],
  ]) {
    await consult(category, question);
    assert.match(spoken.at(-1)!, /couldn't get an answer/);
    assert.equal(trace.at(-1), `[wechat-call-audio] plugin consult mode=safe-read-only stage=failed category=${category}`);
  }
  assert.ok(trace.every(line => /^\[wechat-call-audio\] plugin consult mode=(owner|safe-read-only) stage=(started|ok|failed category=(authority-changed|inactive-registry|filesystem|embedded-run))$/.test(line)));
  live = settings([`agent-wechat:${ownerPeer}`], "none");
  const beforeDisabled = requests.length;
  await consult("live-opt-out");
  assert.equal(requests.length, beforeDisabled);
  await owner.close();

  const dispatches: any[] = [];
  Object.assign(runtime, { channel: { reply: {
    finalizeInboundContext: (ctx: unknown) => ctx,
    async dispatchReplyWithBufferedBlockDispatcher(params: any) {
      dispatches.push(params);
      params.replyOptions.onAgentRunStart();
      await params.dispatcherOptions.deliver({ text: "synthetic tool progress" }, { kind: "tool" });
      await params.dispatcherOptions.deliver({ text: "synthetic normal session reply" }, { kind: "final" });
      if (!params.ctx.Body.includes("simulate deferred run")) params.replyOptions.onAgentRunTerminalOutcome("completed");
    },
  } } });
  live = exact;
  const normalCall = new AbortController();
  const normalOwner = await open(ownerPeer, () => ownerPeer, normalCall.signal);
  const embeddedBefore = requests.length;
  await consult("normal-owner-session");
  assert.equal(requests.length, embeddedBefore);
  assert.equal(dispatches.length, 1);
  const normal = dispatches[0];
  assert.equal(normal.cfg, live);
  assert.equal(normal.ctx.Provider, "agent-wechat");
  assert.equal(normal.ctx.Surface, "agent-wechat");
  assert.equal(normal.ctx.SenderId, ownerPeer);
  assert.equal(normal.ctx.ParentSessionKey, parentKey);
  assert.match(normal.ctx.SessionKey, /^agent:agent:wechat-call:/);
  assert.equal(normal.ctx.OriginatingTo, undefined);
  assert.equal(normal.ctx.CommandInterpretationSuppressed, true);
  assert.equal(normal.ctx.SuppressMessageReceivedHooks, true);
  assert.equal(normal.replyOptions.thinkingLevelOverride, undefined);
  assert.match(spoken.at(-1)!, /synthetic normal session reply/);
  assert.doesNotMatch(spoken.at(-1)!, /synthetic tool progress/);
  await consult("deferred-owner-session", "simulate deferred run");
  assert.match(spoken.at(-1)!, /couldn't get an answer/);
  normalCall.abort();
  await normalOwner.close();
  assert.equal(normal.replyOptions.abortSignal.aborted, true);

  const normalNonOwner = await open(otherPeer);
  await consult("restricted-session");
  assert.equal(dispatches.length, 2);
  assertReadOnly(otherPeer);
  await normalNonOwner.close();
});
