// Loaded only by the account-free, isolated conference Gateway test.
import base from '../../index.ts';
import { appendFileSync, existsSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { consultCallSession } from '../../src/calls-consult-session.ts';
import { callSessionKey } from '../../src/calls-session.ts';
import { validateCallContextVoice } from './call-context-voice.ts';
import { resolveWeChatCallsConfig } from '../../src/calls-config.ts';
import { conferenceCallPolicy } from '../../src/calls-permissions.ts';

export default { ...base, register(api: any) {
  base.register(api);
  const directory = process.env.NATIVE_CONFERENCE_FIXTURE_DIR;
  if (!directory) throw Error('Conference fixture needs isolated storage');
  const groupId = 'synthetic-group@chatroom';
  const calls: Array<{ senderId?: string; owner?: boolean; sessionKey?: string }> = [];
  const toolErrors: string[] = [];
  const factories: unknown[] = [];
  for (const name of ['before_tool_call', 'after_tool_call']) api.on(name, (event: any, context: any) => appendFileSync(join(directory, 'tool-events.jsonl'), JSON.stringify({ name, toolName: event.toolName, error: event.error, runId: context.runId, sessionKey: context.sessionKey }) + '\n'));
  api.registerToolMetadata({ toolName: 'conference_owner_probe', risk: 'low', description: 'Read-only isolated test fixture value.' });
  api.registerTool({ contextVersion: 2, create(context: any) {
    factories.push({ senderId: context.requesterSenderId, owner: context.senderIsOwner, sessionKey: context.sessionKey });
    if (context.senderIsOwner !== true) return [];
    return [{ name: 'conference_owner_probe', catalogMode: 'direct-only', description: 'Read a synthetic fixture value for this test only.',
      parameters: { type: 'object', additionalProperties: false, properties: {} },
      execute: async () => {
        try { context.assertInvocationCurrent(); } catch (error) { toolErrors.push(String(error)); throw error; }
        calls.push({ senderId: context.requesterSenderId, owner: context.senderIsOwner, sessionKey: context.sessionKey });
        return { content: [{ type: 'text', text: 'conference probe value' }] };
      },
    }];
  } }, { names: ['conference_owner_probe'], optional: true });
  api.registerService({ id: 'wechat-conference-model-validation', start: async () => {
    const run = async () => {
      try {
        const cfg = api.runtime.config.current();
        const voiceContext = await validateCallContextVoice(api.runtime, cfg);
        const originalOwners = JSON.stringify(cfg.commands?.ownerAllowFrom);
        const voice = resolveWeChatCallsConfig({ voice: { enabled: true, agentId: 'main', toolPolicy: 'owner', allowConsultSessionPersistence: true,
          realtime: { provider: 'openai' }, conferences: { groups: { [groupId]: { toolPolicy: 'safe-read-only', ownerInviterToolPolicy: 'owner' } } } } })!.voice!;
        const voicePath: string = 'openclaw/plugin-sdk/realtime-voice'; const sdk = await import(voicePath);
        const session = (owner: boolean) => callSessionKey('main', 'per-contact', groupId, owner, { accountId: 'default', nativeAccountId: 'a', groupId });
        const request = (inviterId: string, question: string, suffix = '', toolsAllow?: string[]) => {
          const policy = conferenceCallPolicy(voice, voice.conferences.groups[groupId], { groupId, inviterId }, cfg.commands?.ownerAllowFrom);
          const owner = policy.toolPolicy === 'owner';
          return consultCallSession({
          cfg, reply: api.runtime.channel.reply, runtime: api.runtime, sessionKey: session(owner) + suffix,
          senderId: groupId, accountId: 'default', conference: { groupId, owner }, question,
          instructions: 'This is an isolated synthetic group consultation. Use conference_owner_probe exactly once only when asked for group-owner-probe. Otherwise answer briefly.',
          toolsAllow: toolsAllow ?? sdk.resolveRealtimeVoiceAgentConsultToolsAllow(owner ? 'owner' : 'safe-read-only'), signal: AbortSignal.timeout(20000),
          assertCurrent: () => { if (existsSync(join(directory, 'revoked'))) throw Error('Calling consultation authority changed'); }, onRunStarted() {},
        }); };
        const owner = await request('synthetic-human-owner', 'group-owner-probe');
        const readOnly = await request('synthetic-other-inviter', 'group-read-only-probe');
        const restricted = await request('synthetic-human-owner', 'group-restricted-probe', ':restricted', []);
        const direct = await consultCallSession({ cfg, reply: api.runtime.channel.reply, runtime: api.runtime,
          sessionKey: 'agent:main:wechat-call:synthetic-human-owner', senderId: 'synthetic-human-owner', accountId: 'default',
          question: 'direct-owner-probe', instructions: 'Use the isolated fixture tool once, then answer briefly.',
          signal: AbortSignal.timeout(20000), assertCurrent() {}, onRunStarted() {},
        });
        let revoked = false;
        try { await request('synthetic-human-owner', 'group-revoked-probe', ':revoked'); } catch { revoked = true; }
        writeFileSync(join(directory, 'result.json'), JSON.stringify({ voiceContext, owner, readOnly, restricted, direct, revoked, calls, toolErrors, factories,
          originalOwnersUnchanged: originalOwners === JSON.stringify(api.runtime.config.current().commands?.ownerAllowFrom),
          separateSessions: session(true) !== session(false), groupId }));
      } catch (error) { writeFileSync(join(directory, 'result.json'), JSON.stringify({ error: String(error), factories, toolErrors, owners: api.runtime.config.current().commands?.ownerAllowFrom })); }
    };
    setTimeout(() => { void api.lifecycle.runInBackgroundContext(run); }, 500);
  }, stop: async () => {} });
} };
