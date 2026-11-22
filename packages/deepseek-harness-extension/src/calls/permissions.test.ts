import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { ResolvedWeChatConferenceGroupConfig, ResolvedWeChatVoiceCallsConfig } from './config.ts';
import {
  conferenceCallPolicy,
  directCallPolicy,
  isExactCallOwner,
  restrictCallPolicy,
} from './permissions.ts';

function voice(overrides: Partial<ResolvedWeChatVoiceCallsConfig> = {}): ResolvedWeChatVoiceCallsConfig {
  return {
    enabled: true,
    mode: 'agent',
    transcription: { provider: 'openai' },
    inboundPolicy: 'disabled',
    inboundAllowFrom: [],
    outboundPolicy: 'disabled',
    outboundAllowTo: [],
    outboundToolAllowFrom: [],
    agentId: 'agent-x',
    realtime: {
      provider: 'openai',
      agentContext: { enabled: false },
      recentContext: { enabled: false, maxMessages: 10 },
      consultPolicy: 'auto',
      fastContext: { enabled: false, timeoutMs: 800, maxResults: 3, sources: ['memory', 'sessions'], fallbackToConsult: true },
    },
    toolPolicy: 'safe-read-only',
    allowConsultSessionPersistence: true,
    consultation: { timeoutMs: 30_000, summary: true, recentMessages: 10 },
    sessionScope: 'per-contact',
    maxDurationSeconds: 3600,
    conferences: { groups: {} },
    ...overrides,
  };
}

const group = (overrides: Record<string, unknown> = {}): ResolvedWeChatConferenceGroupConfig =>
  ({ autoJoin: false, toolPolicy: 'safe-read-only', ...overrides }) as ResolvedWeChatConferenceGroupConfig;

test('restrictCallPolicy never upgrades', () => {
  assert.equal(restrictCallPolicy('owner', 'safe-read-only'), 'safe-read-only');
  assert.equal(restrictCallPolicy('safe-read-only', 'none'), 'none');
  assert.equal(restrictCallPolicy('owner', 'owner'), 'owner');
});

test('isExactCallOwner requires an exact agent-wechat contact entry', () => {
  assert.ok(isExactCallOwner('wxid_boss', ['agent-wechat:wxid_boss']));
  assert.ok(!isExactCallOwner('wxid_boss', ['agent-wechat:wxid_other']));
  assert.ok(!isExactCallOwner('wxid_boss', 'agent-wechat:wxid_boss'));
  assert.ok(!isExactCallOwner('123@chatroom', ['agent-wechat:123@chatroom']));
  assert.ok(!isExactCallOwner('bad id', ['agent-wechat:bad id']));
  assert.ok(!isExactCallOwner(undefined, ['agent-wechat:x']));
  assert.ok(!isExactCallOwner('', ['agent-wechat:']));
});

test('directCallPolicy: none without persistence or toolPolicy none', () => {
  assert.equal(directCallPolicy(voice({ allowConsultSessionPersistence: false }), 'wxid_a', []), 'none');
  assert.equal(directCallPolicy(voice({ toolPolicy: 'none' }), 'wxid_a', []), 'none');
});

test('directCallPolicy: owner requires exact owner match, otherwise safe-read-only', () => {
  const ownerVoice = voice({ toolPolicy: 'owner' });
  assert.equal(directCallPolicy(ownerVoice, 'wxid_boss', ['agent-wechat:wxid_boss']), 'owner');
  assert.equal(directCallPolicy(ownerVoice, 'wxid_guest', ['agent-wechat:wxid_boss']), 'safe-read-only');
  assert.equal(directCallPolicy(voice(), 'wxid_boss', ['agent-wechat:wxid_boss']), 'safe-read-only');
});

test('conferenceCallPolicy: owner inviter override replaces the group default, still capped by voice', () => {
  const result = conferenceCallPolicy(
    voice({ toolPolicy: 'owner' }),
    group({ toolPolicy: 'none', ownerInviterToolPolicy: 'owner' }),
    { groupId: '123@chatroom', inviterId: 'wxid_boss' },
    ['agent-wechat:wxid_boss', 'agent-wechat:123@chatroom'],
  );
  assert.deepEqual(result, { toolPolicy: 'owner', reason: 'owner_inviter' });
  // A safe-read-only voice policy caps the override as well.
  const capped = conferenceCallPolicy(
    voice({ toolPolicy: 'safe-read-only' }),
    group({ toolPolicy: 'none', ownerInviterToolPolicy: 'owner' }),
    { groupId: '123@chatroom', inviterId: 'wxid_boss' },
    ['agent-wechat:wxid_boss', 'agent-wechat:123@chatroom'],
  );
  assert.deepEqual(capped, { toolPolicy: 'safe-read-only', reason: 'owner_inviter' });
});

test('conferenceCallPolicy: owner policy without the group grant degrades', () => {
  const result = conferenceCallPolicy(
    voice({ toolPolicy: 'owner' }),
    group({ toolPolicy: 'owner' }),
    { groupId: '123@chatroom' },
    ['agent-wechat:wxid_boss'],
  );
  assert.deepEqual(result, { toolPolicy: 'safe-read-only', reason: 'owner_grant_missing' });
});

test('conferenceCallPolicy: admission policy only ever reduces', () => {
  const result = conferenceCallPolicy(
    voice({ toolPolicy: 'owner' }),
    group({ toolPolicy: 'owner' }),
    { groupId: '123@chatroom', policyAtAdmission: 'safe-read-only' },
    ['agent-wechat:123@chatroom'],
  );
  // The admitted level holds even though call-time policy would allow owner.
  assert.equal(result.toolPolicy, 'safe-read-only');
});
