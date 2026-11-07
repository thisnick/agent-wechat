import test from "node:test";
import assert from "node:assert/strict";
import { setTimeout as delay } from "node:timers/promises";
import type { PluginRuntime, OpenClawPluginApi } from "openclaw/plugin-sdk/core";
import { prepareOpenClawCallVoice, type CallVoiceSession } from "./calls-voice.ts";
import { resolveWeChatCallsConfig } from "./calls-config.ts";
import { installCallConsultAuthority } from "./calls-consult-authority.ts";
import { resolveCallTranscriptionApiKey, callsConfigurationKey } from "./calls-secrets.ts";

async function until(predicate: () => boolean) { for (let i = 0; i < 250; i++) { if (predicate()) return; await delay(5); } assert.fail("Condition did not become true"); }
const groupId = "test@chatroom";
async function fixture(policy: "owner" | "safe-read-only" | "none" = "safe-read-only", conference = false, recentContext?: string) {
  const path: string = "openclaw/plugin-sdk/realtime-voice";
  const audioPath: string = "openclaw/plugin-sdk/realtime-voice-provider";
  const meetingPath: string = "openclaw/plugin-sdk/meeting-runtime";
  const [voiceSdk, audioSdk, meetingSdk] = await Promise.all([import(path), import(audioPath), import(meetingPath)]);
  const raw = { enabled: true, mode: "agent" as const, agentId: "main", toolPolicy: policy, allowConsultSessionPersistence: true,
    transcription: { provider: "synthetic-stt", model: "synthetic-transcriber" },
    conferences: { groups: { [groupId]: { toolPolicy: "safe-read-only" as const, ownerInviterToolPolicy: "owner" as const } } } };
  // Owner overrides require the global owner cap, even if this test is direct.
  if (policy !== "owner") delete (raw.conferences.groups[groupId] as any).ownerInviterToolPolicy;
  const cfg: any = { commands: { ownerAllowFrom: ["agent-wechat:trusted", `agent-wechat:${groupId}`] }, channels: { "agent-wechat": {
    enabled: true, native: { mode: "external", accountId: "a", endpoint: "http://127.0.0.1:6174", token: "synthetic-token-".repeat(4), consumerStateDir: "/synthetic/consumer" }, calls: { voice: raw },
  } } };
  const state = { stt: undefined as any, providerConfig: undefined as any, dispatches: [] as any[], ttsTexts: [] as string[], audio: [] as Array<{ pcm: Buffer; itemId?: string }>, input: [] as Buffer[],
    transcripts: [] as Array<[string, string]>, clears: 0, failed: 0, closed: 0, holdAgent: undefined as Promise<void> | undefined, holdTts: undefined as Promise<void> | undefined, connected: false };
  const provider = { resolveConfig: ({ rawConfig }: any) => rawConfig, isConfigured: () => true, createSession(request: any) {
    state.stt = request; state.providerConfig = request.providerConfig;
    return { connect: async () => { state.connected = true; }, isConnected: () => state.connected,
      sendAudio: (pcm: Buffer) => state.input.push(pcm), close: () => { state.closed++; state.connected = false; } };
  } };
  const runtime = { config: { current: () => cfg }, tts: { textToSpeechTelephony: async ({ text }: any) => {
    state.ttsTexts.push(text); await state.holdTts; return { success: true, audioBuffer: Buffer.alloc(2400, 1), sampleRate: 24000, outputFormat: "pcm" };
  } }, channel: { reply: { finalizeInboundContext: (ctx: any) => ctx, dispatchReplyWithBufferedBlockDispatcher: async (params: any) => {
    state.dispatches.push(params); params.replyOptions.onAgentRunStart(); await state.holdAgent;
    await params.dispatcherOptions.deliver({ text: "Synthetic spoken answer" }, { kind: "final" }); params.replyOptions.onAgentRunTerminalOutcome("completed");
  } } }, agent: { session: { resolveStorePath: () => "/synthetic/store", getSessionEntry: () => undefined } } } as unknown as PluginRuntime;
  installCallConsultAuthority({ runtime, registerTrustedToolPolicy() {} } as unknown as OpenClawPluginApi);
  const prepared = await prepareOpenClawCallVoice(resolveWeChatCallsConfig({ voice: raw })!.voice!, cfg, runtime, {
    voiceSdk, audioSdk, meetingSdk, transcriptionSdk: { getRealtimeTranscriptionProvider: () => provider },
  });
  const signal = new AbortController(); const peer = conference ? groupId : "trusted";
  const session = await prepared.open({ audio: (pcm, metadata) => state.audio.push({ pcm, itemId: metadata?.itemId }), clear: () => { state.clears++; },
    failed: () => { state.failed++; }, transcript: (role, text) => state.transcripts.push([role, text]) },
    { senderId: peer, recentContext, policyAtAdmission: policy, signal: signal.signal, channelAccountId: "default", nativeAccountId: "a", connectedPeer: () => peer,
      ...(conference ? { conference: { groupId, inviterId: "trusted" } } : {}) });
  return { session, signal, state, cfg, prepared };
}

test("agent mode is an explicit pipeline with persistence opt-in and separate hidden transcription credentials", async () => {
  const base = { enabled: true, mode: "agent" as const, agentId: "main" };
  assert.throws(() => resolveWeChatCallsConfig({ voice: base }), /configuration/);
  const resolved = resolveWeChatCallsConfig({ voice: { ...base, allowConsultSessionPersistence: true, transcription: { apiKey: "synthetic-transcription-key" } } })!;
  assert.equal(resolved.voice!.mode, "agent"); assert.equal(resolved.voice!.transcription.provider, "openai");
  assert.equal(JSON.stringify(resolved).includes("synthetic-transcription-key"), false);
  assert.equal(await resolveCallTranscriptionApiKey(resolved.voice!, {}), "synthetic-transcription-key");
  const changed = resolveWeChatCallsConfig({ voice: { ...base, allowConsultSessionPersistence: true, transcription: { apiKey: "different-key" } } })!;
  assert.notEqual(callsConfigurationKey(resolved), callsConfigurationKey(changed));
});

test("direct agent calls use public STT and TTS, coalesce fragments, retain transcripts and keep input flowing", async t => {
  const { session, state } = await fixture(); t.after(() => session.close());
  assert.equal(state.dispatches.length, 0, "preflight must not speak");
  assert.equal(state.providerConfig.model, "synthetic-transcriber");
  session.sendAudio(Buffer.alloc(960)); assert.equal(state.input[0].length, 160, "the shared STT contract receives 8 kHz mu-law");
  state.stt.onTranscript("What is"); state.stt.onTranscript("the time?");
  session.sendAudio(Buffer.alloc(960)); assert.equal(state.input.length, 2);
  await until(() => state.audio.length > 0);
  assert.equal(state.dispatches.length, 1); assert.equal(state.dispatches[0].ctx.Body, "What is\nthe time?");
  assert.ok(state.dispatches[0].replyOptions.toolsAllow.includes("read"));
  assert.match(state.audio[0].itemId!, /^agent-/); assert.ok(state.audio.every(item => item.pcm.length <= 1920));
  assert.deepEqual(state.transcripts.map(([role]) => role), ["user", "user", "assistant"]);
  assert.equal(state.failed, 0);
});

test("agent conference greetings carry context with no tools; speech keeps the group owner principal", async t => {
  const { session, state } = await fixture("owner", true); t.after(() => session.close());
  session.updateCallContext({ direction: "incoming", revision: 1, conference: { groupId, name: "Test group", inviter: { contactId: "trusted", name: "Haogang" } }, participants: [{ contactId: "peer", name: "Alice" }] }, { initial: true });
  await until(() => state.audio.length > 0);
  assert.deepEqual(state.dispatches[0].replyOptions.toolsAllow, []); assert.match(state.dispatches[0].ctx.Body, /Alice/);
  assert.equal(session.binding!().owner, true);
  state.stt.onTranscript("Use an allowed tool"); await until(() => state.dispatches.length === 2);
  assert.equal(state.dispatches[1].ctx.SenderId, groupId); assert.equal(state.dispatches[1].ctx.ChatType, "group");
  assert.equal(state.dispatches[1].replyOptions.toolsAllow, undefined);
  assert.match(state.dispatches[1].ctx.BodyForAgent, /Haogang/);
});

test("no-tools agent conversations still answer through a separate session", async t => {
  const { session, state } = await fixture("none"); t.after(() => session.close());
  state.stt.onTranscript("Hello"); await until(() => state.audio.length > 0);
  assert.deepEqual(state.dispatches[0].replyOptions.toolsAllow, []);
  assert.match(state.dispatches[0].ctx.SessionKey, /:no-tools$/); assert.equal(state.failed, 0);
});

test("speech interruption cancels stale agent work and TTS without retrying it or blocking input", async t => {
  const { session, state } = await fixture(); t.after(() => session.close());
  let release!: () => void; state.holdTts = new Promise(resolve => { release = resolve; });
  state.stt.onTranscript("First question"); await until(() => state.ttsTexts.length === 1);
  state.stt.onSpeechStart(); state.stt.onTranscript("Second question");
  session.sendAudio(Buffer.alloc(960)); assert.equal(state.input.length, 1);
  state.holdTts = undefined;
  await until(() => state.dispatches.length === 2 && state.audio.length > 0);
  release(); await delay(50);
  assert.equal(state.dispatches.filter(p => p.ctx.Body === "First question").length, 1);
  assert.equal(state.transcripts.filter(([role]) => role === "assistant").length, 1, "canceled TTS is never recorded as spoken");
  assert.equal(state.failed, 0);
});

test("late TTS and late transcripts after closing cannot produce output", async () => {
  const { session, state } = await fixture(); let release!: () => void;
  state.holdTts = new Promise(resolve => { release = resolve; }); state.stt.onTranscript("Pending"); await until(() => state.ttsTexts.length === 1);
  await session.close(); release(); state.stt.onTranscript("Late"); await delay(250);
  assert.equal(state.audio.length, 0); assert.equal(state.dispatches.length, 1); assert.ok(state.closed > 0);
});

test("revocation while TTS is pending discards privileged speech and the next turn uses read-only authority", async t => {
  const { session, state, cfg } = await fixture("owner", true, "Owner-only recap sentinel"); t.after(() => session.close());
  let release!: () => void; state.holdTts = new Promise(resolve => { release = resolve; });
  state.stt.onTranscript("Privileged question"); await until(() => state.ttsTexts.length === 1);
  const previous = state.dispatches[0].ctx.SessionKey;
  cfg.commands.ownerAllowFrom = [`agent-wechat:${groupId}`]; release(); state.holdTts = undefined;
  await delay(80); assert.equal(state.audio.length, 0);
  assert.equal(session.binding!().owner, false); assert.equal(session.binding!().summaryAllowed, false);
  state.stt.onTranscript("A read-only question"); await until(() => state.audio.length > 0);
  assert.notEqual(state.dispatches[1].ctx.SessionKey, previous); assert.ok(state.dispatches[1].replyOptions.toolsAllow.includes("read"));
  assert.match(state.dispatches[0].ctx.BodyForAgent, /Owner-only recap sentinel/);
  assert.doesNotMatch(state.dispatches[1].ctx.BodyForAgent, /Owner-only recap sentinel/);
  assert.equal(state.failed, 0);
});

test("failed agent work leaves the call available without retrying the request; transcription failure is terminal", async t => {
  const { session, state } = await fixture(); t.after(() => session.close());
  let reject!: (error: Error) => void; state.holdAgent = new Promise((_, fail) => { reject = fail; });
  state.stt.onTranscript("Failed question"); await until(() => state.dispatches.length === 1);
  reject(Error("Synthetic model unavailable")); state.holdAgent = undefined; await delay(40);
  assert.equal(state.failed, 0); state.stt.onTranscript("New question"); await until(() => state.audio.length > 0);
  assert.equal(state.dispatches.length, 2);
  state.stt.onError(Error("Synthetic transport stopped")); assert.equal(state.failed, 1);
});
