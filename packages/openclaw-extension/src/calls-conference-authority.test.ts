import test from 'node:test';
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import type { OpenClawPluginApi, PluginRuntime } from 'openclaw/plugin-sdk/core';
import { prepareOpenClawCallVoice } from './calls-voice.ts';
import { resolveWeChatCallsConfig } from './calls-config.ts';
import { beginCallConsultAuthority, installCallConsultAuthority } from './calls-consult-authority.ts';
import { callSessionKey } from './calls-session.ts';

const groupId = 'synthetic-group@chatroom';
const pcm = { encoding: 'pcm16', sampleRateHz: 24000, channels: 1 };
async function until(predicate: () => boolean) { for (let i = 0; i < 200; i++) { if (predicate()) return; await delay(5); } assert.fail('Condition did not become true'); }

test('conference configuration is exact, defaults to read-only and requires the existing persistence/owner opt-ins', () => {
  const voice = { enabled: true, agentId: 'main', realtime: { provider: 'openai' }, allowConsultSessionPersistence: true };
  assert.deepEqual(Object.keys(resolveWeChatCallsConfig({ voice })!.voice!.conferences.groups), []);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { ...voice, conferences: { groups: { '*': { autoJoin: true } } } } }), /conference/);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { ...voice, conferences: { groups: { [groupId]: { toolPolicy: 'owner' } } } } }), /conference/);
  const config = resolveWeChatCallsConfig({ voice: { ...voice, conferences: { groups: { [groupId]: { autoJoin: true } } } } })!.voice!;
  assert.equal(config.conferences.groups[groupId].toolPolicy, 'safe-read-only');
  assert.equal(config.conferences.groups[groupId].autoJoin, true);
  assert.notEqual(callSessionKey('main', 'per-contact', groupId, true, { accountId: 'a', groupId }), callSessionKey('main', 'per-contact', groupId, false, { accountId: 'a', groupId }));
  assert.notEqual(callSessionKey('main', 'per-contact', groupId, false, { accountId: 'a', groupId }), callSessionKey('main', 'per-contact', groupId, false, { accountId: 'b', groupId }));
});
test('wildcard conference rules require an explicit inviter filter for auto-join and retain owner opt-ins', () => {
  const voice = { enabled: true, agentId: 'main', realtime: { provider: 'openai' }, allowConsultSessionPersistence: true };
  for (const inviter of ['trusted-inviter', '*']) {
    const resolved = resolveWeChatCallsConfig({ voice: { ...voice, conferences: { groups: { '*': { autoJoin: true, autoJoinAllowFrom: [inviter] } } } } })!.voice!;
    assert.equal(resolved.conferences.groups['*'].autoJoin, true);
    assert.deepEqual(resolved.conferences.groups['*'].autoJoinAllowFrom, [inviter]);
    assert.equal(resolved.conferences.groups['*'].toolPolicy, 'safe-read-only');
  }
  assert.throws(() => resolveWeChatCallsConfig({ voice: { ...voice, conferences: { groups: { 'not-a-room': { autoJoin: true } } } } }), /conference/);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { ...voice, conferences: { groups: { '*': { autoJoin: true, autoJoinAllowFrom: ['*'], ownerInviterToolPolicy: 'owner' } } } } }), /conference/);
});

test('conference consultation rechecks authority before tools, denies retired runs and never returns a broad allow', () => {
  const runtime = {} as PluginRuntime; let evaluate: any; let current = true; let aborted = false;
  installCallConsultAuthority({ runtime, registerTrustedToolPolicy: (policy: any) => { evaluate = policy.evaluate; } } as OpenClawPluginApi);
  const stop = beginCallConsultAuthority(runtime, 'wechat-conference-synthetic', 'agent:main:group', () => { if (!current) throw Error('revoked'); }, () => { aborted = true; });
  const event = { runId: 'wechat-conference-synthetic', toolName: 'read', params: {} };
  const context = { runId: event.runId, sessionKey: 'agent:main:group', toolName: 'read' };
  assert.equal(evaluate(event, context), undefined);
  current = false; assert.equal(evaluate(event, context).block, true); assert.equal(aborted, true);
  stop(); assert.equal(evaluate(event, context).block, true);
  assert.equal(evaluate({ ...event, runId: 'unrelated' }, { ...context, runId: 'unrelated' }), undefined);
});
test('group consultation needs both exact grants, uses the group principal and isolates a revoked owner result', async () => {
  const path: string = 'openclaw/plugin-sdk/realtime-voice'; const sdk = await import(path);
  const voice = { enabled: true, agentId: 'main', toolPolicy: 'owner' as const, allowConsultSessionPersistence: true,
    realtime: { provider: 'openai', providers: { openai: { apiKey: 'synthetic-openai-key' } } },
    conferences: { groups: { [groupId]: { autoJoin: true, toolPolicy: 'owner' as 'owner' | 'safe-read-only' } } },
  };
  const cfg = { commands: { ownerAllowFrom: ['agent-wechat:human-owner'] }, channels: { 'agent-wechat': { enabled: true,
    native: { mode: 'external', accountId: 'a', endpoint: 'http://127.0.0.1:6174', token: 'synthetic-token-'.repeat(4), consumerStateDir: '/synthetic/consumer' }, calls: { voice } } } };
  let bridge: any; const dispatches: any[] = [], results: any[] = [], spoken: string[] = []; let clears = 0; let hold: Promise<void> | undefined;
  const runtime = { config: { current: () => cfg }, channel: { reply: {
    finalizeInboundContext: (ctx: any) => ctx,
    dispatchReplyWithBufferedBlockDispatcher: async (params: any) => {
      dispatches.push(params); params.replyOptions.onAgentRunStart(); await hold;
      await params.dispatcherOptions.deliver({ text: 'synthetic group answer' }, { kind: 'final' }); params.replyOptions.onAgentRunTerminalOutcome('completed');
    },
  } }, agent: { session: { resolveStorePath: () => '/synthetic/store', getSessionEntry: () => undefined } } } as unknown as PluginRuntime;
  installCallConsultAuthority({ runtime, registerTrustedToolPolicy() {} } as unknown as OpenClawPluginApi);
  const prepared = await prepareOpenClawCallVoice(resolveWeChatCallsConfig({ voice })!.voice!, cfg, runtime, {
    voiceSdk: { ...sdk, resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: 'openai', createBridge() {} }, providerConfig: {}, capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true, supportsToolCalls: true } }),
      createRealtimeVoiceBridgeSession: (params: any) => { bridge = params; return { bridge: { isConnected: () => true, outputAudioMode: 'response' }, connect: async () => params.onReady(), sendAudio() {}, setMediaTimestamp() {}, handleBargeIn() {}, close() {}, sendUserMessage: (text: string) => { spoken.push(text); }, submitToolResult: (_id: string, result: unknown) => { results.push(result); } }; },
    }, audioSdk: { createStreamingPcmResampler() {} },
  });
  const open = () => prepared.open({ audio() {}, clear() { clears++; }, failed() { assert.fail('Unexpected provider failure'); } },
    { senderId: groupId, conference: { groupId }, channelAccountId: 'default', nativeAccountId: 'a', signal: new AbortController().signal, connectedPeer: () => groupId });
  let session = await open(); assert.equal(session.binding?.().owner, false);
  await bridge.onToolCall({ name: sdk.REALTIME_VOICE_AGENT_CONSULT_TOOL_NAME, callId: 'readonly', args: { question: 'Look up a synthetic value' } });
  await until(() => spoken.length === 1); assert.ok(dispatches[0].replyOptions.toolsAllow.includes('read'));
  cfg.commands.ownerAllowFrom.push(`agent-wechat:${groupId}`);
  assert.equal(session.binding?.().owner, false, 'a live call must not gain owner authority'); await session.close();
  session = await open(); assert.equal(session.binding?.().owner, true);
  assert.match(bridge.instructions, /group voice conference/); assert.match(bridge.instructions, /All answers are audible to the group/);
  const before = JSON.stringify(cfg);
  await bridge.onToolCall({ name: sdk.REALTIME_VOICE_AGENT_CONSULT_TOOL_NAME, callId: 'owner', args: { question: 'Use a synthetic owner action' } });
  await until(() => spoken.length === 2);
  const request = dispatches[1]; assert.equal(request.ctx.SenderId, groupId); assert.equal(request.ctx.ChatType, 'group'); assert.equal(request.replyOptions.toolsAllow, undefined);
  assert.equal(JSON.stringify(cfg), before); assert.equal(request.cfg, cfg);
  let release!: () => void; hold = new Promise(resolve => { release = resolve; });
  await bridge.onToolCall({ name: sdk.REALTIME_VOICE_AGENT_CONSULT_TOOL_NAME, callId: 'revoked', args: { question: 'Hold a synthetic result' } });
  await until(() => dispatches.length === 3);
  voice.conferences.groups[groupId].toolPolicy = 'safe-read-only';
  await until(() => clears > 0); release();
  await delay(40); assert.equal(spoken.length, 2, 'a revoked owner result must not be spoken'); assert.equal(session.binding?.().owner, false);
  assert.notEqual(session.binding?.().sessionKey, request.ctx.SessionKey); await session.close();
});

test('owner inviter policy binds to the invitation, rechecks revocation and cannot rise again within the call', async () => {
  const path: string = 'openclaw/plugin-sdk/realtime-voice'; const sdk = await import(path);
  const voice = { enabled: true, agentId: 'main', toolPolicy: 'owner' as const, allowConsultSessionPersistence: true,
    realtime: { provider: 'openai' }, conferences: { groups: { [groupId]: { toolPolicy: 'safe-read-only' as const, ownerInviterToolPolicy: 'owner' as 'owner' | 'none' } } } };
  const cfg = { commands: { ownerAllowFrom: ['agent-wechat:trusted', `agent-wechat:${groupId}`] }, channels: { 'agent-wechat': { enabled: true,
    native: { mode: 'external', accountId: 'a', endpoint: 'http://127.0.0.1:6174', token: 'synthetic-token-'.repeat(4), consumerStateDir: '/synthetic/consumer' }, calls: { voice } } } };
  let bridge: any, evaluate: any, hold: Promise<void> | undefined; let clears = 0;
  const dispatches: any[] = [], spoken: string[] = [];
  const runtime = { config: { current: () => cfg }, channel: { reply: {
    finalizeInboundContext: (ctx: any) => ctx,
    dispatchReplyWithBufferedBlockDispatcher: async (params: any) => {
      dispatches.push(params); params.replyOptions.onAgentRunStart(); await hold;
      await params.dispatcherOptions.deliver({ text: 'synthetic result' }, { kind: 'final' }); params.replyOptions.onAgentRunTerminalOutcome('completed');
    },
  } }, agent: { session: { resolveStorePath: () => '/synthetic/store', getSessionEntry: () => undefined } } } as unknown as PluginRuntime;
  installCallConsultAuthority({ runtime, registerTrustedToolPolicy(policy: any) { evaluate = policy.evaluate; } } as unknown as OpenClawPluginApi);
  const prepared = await prepareOpenClawCallVoice(resolveWeChatCallsConfig({ voice })!.voice!, cfg, runtime, {
    voiceSdk: { ...sdk, resolveConfiguredRealtimeVoiceProvider: () => ({ provider: { id: 'openai', createBridge() {} }, providerConfig: {}, capabilities: { inputAudioFormats: [pcm], outputAudioFormats: [pcm], supportsBargeIn: true, supportsToolCalls: true } }),
      createRealtimeVoiceBridgeSession: (params: any) => { bridge = params; return { bridge: { isConnected: () => true, outputAudioMode: 'response' }, connect: async () => params.onReady(), sendAudio() {}, setMediaTimestamp() {}, handleBargeIn() {}, close() {}, sendUserMessage: (text: string) => spoken.push(text), submitToolResult() {} }; },
    }, audioSdk: { createStreamingPcmResampler() {} },
  });
  const open = (inviterId?: string) => prepared.open({ audio() {}, clear() { clears++; }, failed() {} },
    { senderId: groupId, conference: { groupId, inviterId }, channelAccountId: 'default', nativeAccountId: 'a', signal: new AbortController().signal, connectedPeer: () => groupId });
  const consult = (id: string) => bridge.onToolCall({ name: sdk.REALTIME_VOICE_AGENT_CONSULT_TOOL_NAME, callId: id, args: { question: 'Synthetic lookup' } });
  let session = await open('trusted'); assert.equal(session.binding!().toolPolicy, 'owner'); assert.equal(session.binding!().policyReason, 'owner_inviter');
  await consult('owner'); await until(() => spoken.length === 1);
  const ownerSession = session.binding!().sessionKey;
  assert.equal(dispatches[0].replyOptions.toolsAllow, undefined); assert.equal(dispatches[0].ctx.SenderId, groupId);
  await session.close();
  for (const inviter of ['other', undefined]) {
    session = await open(inviter); assert.equal(session.binding!().owner, false);
    assert.notEqual(session.binding!().sessionKey, ownerSession);
    await consult(`readonly-${inviter}`); await until(() => dispatches.length === (inviter ? 2 : 3));
    assert.ok(dispatches.at(-1).replyOptions.toolsAllow.includes('read'));
    await until(() => spoken.length === dispatches.length); await session.close();
  }
  session = await open('trusted'); let release!: () => void; hold = new Promise(resolve => { release = resolve; });
  await consult('revoking'); await until(() => dispatches.length === 4);
  const run = dispatches.at(-1).replyOptions.runId;
  cfg.commands.ownerAllowFrom = [`agent-wechat:${groupId}`];
  assert.equal(evaluate({ runId: run }, { runId: run, sessionKey: ownerSession }).block, true);
  release(); await delay(40); assert.equal(spoken.length, 3); assert.ok(clears > 0);
  assert.equal(session.binding!().toolPolicy, 'safe-read-only'); assert.equal(session.binding!().summaryAllowed, false);
  cfg.commands.ownerAllowFrom.push('agent-wechat:trusted');
  assert.equal(session.binding!().toolPolicy, 'safe-read-only', 'restoring owner status cannot upgrade this call');
  assert.equal(session.binding!().owner, false); await session.close();
  session = await open('trusted'); assert.equal(session.binding!().owner, true); await session.close();
  voice.conferences.groups[groupId].ownerInviterToolPolicy = 'none';
  session = await open('trusted'); assert.equal(session.binding!().toolPolicy, 'none');
  assert.deepEqual(bridge.tools, []); voice.conferences.groups[groupId].ownerInviterToolPolicy = 'owner';
  assert.equal(session.binding!().toolPolicy, 'none'); await session.close();
});
