import assert from 'node:assert/strict';
import { test } from 'node:test';
import { resolveConferenceGroupConfig, resolveWeChatCallsConfig } from './config.ts';

test('calls config is absent unless voice.enabled is true', () => {
  assert.equal(resolveWeChatCallsConfig(undefined), undefined);
  assert.equal(resolveWeChatCallsConfig({}), undefined);
  assert.equal(resolveWeChatCallsConfig({ voice: { enabled: false } }), undefined);
});

test('agent mode requires allowConsultSessionPersistence and an agent id', () => {
  assert.throws(() => resolveWeChatCallsConfig({ voice: { enabled: true, mode: 'agent', agentId: 'agent-x' } }), /Invalid calling configuration/);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { enabled: true, allowConsultSessionPersistence: true } }), /Invalid calling configuration/);
  assert.throws(() => resolveWeChatCallsConfig({ voice: { enabled: true, agentId: 'agent-x', allowConsultSessionPersistence: true } }), /Invalid calling configuration/);
});

test('a minimal agent-mode config resolves with documented defaults', () => {
  const resolved = resolveWeChatCallsConfig({
    voice: {
      enabled: true,
      mode: 'agent',
      agentId: 'agent-x',
      allowConsultSessionPersistence: true,
      realtime: { apiKeyCredentialRef: 'SPEECH_API_KEY' },
    },
  });
  assert.ok(resolved);
  const voice = resolved.voice!;
  assert.equal(voice.mode, 'agent');
  assert.equal(voice.toolPolicy, 'safe-read-only'); // persistence → safe-read-only default
  assert.equal(voice.inboundPolicy, 'disabled');
  assert.equal(voice.outboundPolicy, 'disabled');
  assert.equal(voice.consultation.timeoutMs, 30_000);
  assert.equal(voice.consultation.summary, true);
  assert.equal(voice.sessionScope, 'per-contact');
  assert.equal(voice.maxDurationSeconds, 3600);
  assert.equal(voice.realtime.provider, 'openai');
  assert.equal(voice.realtime.apiKeyCredentialRef, 'SPEECH_API_KEY');
});

test('invalid policies, identifiers and unknown keys are rejected', () => {
  const base = { enabled: true, agentId: 'agent-x', allowConsultSessionPersistence: true };
  // The resolver reports a single generic message — config authors debug via
  // the README shape, not via error output.
  const invalid = (value: unknown): void => {
    assert.throws(() => resolveWeChatCallsConfig(value), /Invalid calling configuration/);
  };
  invalid({ voice: { ...base, toolPolicy: 'superuser' } });
  invalid({ voice: { ...base, agentId: 'bad id' } });
  invalid({ voice: { ...base, bogusKey: 1 } });
  invalid({ voice: { ...base, maxDurationSeconds: 1 } });
  invalid({ voice: { ...base, realtime: { apiKeyCredentialRef: 'bad-ref' } } });
});

test('conference groups: star autoJoin demands an allowlist; precise rules override star', () => {
  const base = { enabled: true, mode: 'agent', agentId: 'agent-x', allowConsultSessionPersistence: true };
  assert.throws(
    () => resolveWeChatCallsConfig({ voice: { ...base, conferences: { groups: { '*': { autoJoin: true } } } } }),
    /Invalid conference group policy/,
  );
  const resolved = resolveWeChatCallsConfig({
    voice: {
      ...base,
      conferences: {
        groups: {
          '*': { autoJoin: true, autoJoinAllowFrom: ['wxid_boss'] },
          '123@chatroom': { autoJoin: false },
        },
      },
    },
  });
  assert.ok(resolved);
  const voice = resolved.voice!;
  const star = resolveConferenceGroupConfig(voice, '456@chatroom');
  assert.equal(star?.autoJoin, true);
  // Precise rule wins over star; non-group ids never match.
  assert.equal(resolveConferenceGroupConfig(voice, '123@chatroom')?.autoJoin, false);
  assert.equal(resolveConferenceGroupConfig(voice, 'wxid_alice'), undefined);
  assert.equal(resolveConferenceGroupConfig(voice, '*'), undefined);
});

test('top-level keys are restricted to voice and video', () => {
  assert.throws(() => resolveWeChatCallsConfig({ telephony: {} }), /Invalid calling configuration/);
});
