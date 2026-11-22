import assert from 'node:assert/strict';
import { test } from 'node:test';
import { isCallSessionKey } from './config.ts';
import { callSessionKey } from './session-keys.ts';

test('DM per-contact keys are stable and lowercase; non-owner keys carry a read-only suffix', () => {
  const a = callSessionKey('agent-x', 'per-contact', 'WXID_Alice', false);
  const b = callSessionKey('agent-x', 'per-contact', 'wxid_alice', false);
  assert.equal(a, b);
  assert.match(a, /^agent:agent-x:wechat-call:wxid_alice:read-only$/);
  const owner = callSessionKey('agent-x', 'per-contact', 'wxid_alice', true);
  assert.equal(owner, 'agent:agent-x:wechat-call:wxid_alice');
});

test('per-call scope generates a fresh key per invocation', () => {
  const a = callSessionKey('agent-x', 'per-call', 'wxid_a', false);
  const b = callSessionKey('agent-x', 'per-call', 'wxid_a', false);
  assert.notEqual(a, b);
  assert.match(a, /^agent:agent-x:wechat-call:[0-9a-f-]{36}$/);
});

test('conference keys hash the (account, group) principal and honor the owner flag', () => {
  const principal = { accountId: 'default', groupId: '123@chatroom' };
  const a = callSessionKey('agent-x', 'per-contact', 'wxid_a', false, principal);
  const b = callSessionKey('agent-x', 'per-contact', 'wxid_a', false, { ...principal });
  assert.equal(a, b);
  assert.match(a, /^agent:agent-x:wechat-conference:[0-9a-f]{64}:read-only$/);
  const owner = callSessionKey('agent-x', 'per-contact', 'wxid_a', true, principal);
  assert.match(owner, /:owner$/);
  assert.notEqual(a.split(':')[4], owner.split(':')[4]);
});

test('keys satisfy the call session key pattern', () => {
  assert.ok(isCallSessionKey(callSessionKey('agent-x', 'per-contact', 'wxid_a', false)));
  assert.ok(!isCallSessionKey('agent:Agent-X:wechat-call:wxid_a')); // agent ids are lowercase
  assert.ok(!isCallSessionKey('agent:agent-x:wechat-call:wx\r\nid_a')); // control chars rejected
});
