import test from 'node:test';
import assert from 'node:assert/strict';
import {
  asAccountId,
  asChatJid,
  asSenderId,
  type AdmissionDecision,
  type SenderId,
} from './contracts.ts';
import {
  admitInbound,
  type AdmissionConfig,
  type AdmissionInput,
} from './admission.ts';

const ACCOUNT = asAccountId('account-1');
const CHAT = asChatJid('room@chatroom');
const SENDER = asSenderId('wxid_alice');

function config(overrides: Partial<AdmissionConfig> = {}): AdmissionConfig {
  return {
    dmPolicy: 'allowlist',
    groupPolicy: 'allowlist',
    dmAllowFrom: ['wxid_alice'],
    groupAllowFrom: [],
    groupSenderAllowFrom: [],
    requireMentionInGroups: true,
    ...overrides,
  };
}

// Allow callers to explicitly pass `senderId: undefined` (to exercise the
// missing-sender path) while still producing a valid AdmissionInput under
// `exactOptionalPropertyTypes`: undefined-valued senderId keys are dropped.
type InputOverrides = Partial<Omit<AdmissionInput, 'senderId'>> & {
  senderId?: SenderId | undefined;
};

function input(overrides: InputOverrides = {}): AdmissionInput {
  const { senderId, ...rest } = overrides;
  const base: AdmissionInput = {
    accountId: ACCOUNT,
    chatJid: CHAT,
    senderId: SENDER,
    isGroup: false,
    mentioned: false,
    ...rest,
  };
  if (senderId === undefined && 'senderId' in overrides) {
    const { senderId: _dropped, ...withoutSender } = base;
    return withoutSender;
  }
  return senderId === undefined ? base : { ...base, senderId };
}

function reason(cfg: AdmissionConfig, inp: AdmissionInput): AdmissionDecision['reason'] {
  return admitInbound(cfg, inp).reason;
}

// --- Tombstone short-circuit ------------------------------------------------

test('tombstone is rejected even when DM policy is open', () => {
  const d = admitInbound(config({ dmPolicy: 'open' }), input({ tombstone: true }));
  assert.equal(d.admitted, false);
  assert.equal(d.reason, 'tombstone');
});

test('tombstone takes precedence over every other gate in groups', () => {
  const d = admitInbound(
    config({ groupPolicy: 'disabled', requireMentionInGroups: true }),
    input({ isGroup: true, tombstone: true, mentioned: false }),
  );
  assert.equal(d.admitted, false);
  assert.equal(d.reason, 'tombstone');
});

test('tombstone false behaves like absent', () => {
  assert.equal(reason(config({ dmPolicy: 'open' }), input({ tombstone: false })), 'admitted');
});

// --- DM gates ---------------------------------------------------------------

test('DM disabled rejects with disabled', () => {
  const d = admitInbound(config({ dmPolicy: 'disabled' }), input());
  assert.equal(d.admitted, false);
  assert.equal(d.reason, 'disabled');
});

test('DM allowlist hit admits', () => {
  const d = admitInbound(config(), input());
  assert.equal(d.admitted, true);
  assert.equal(d.reason, 'admitted');
});

test('DM allowlist miss rejects with sender-not-allowed', () => {
  const d = admitInbound(config({ dmAllowFrom: ['wxid_bob'] }), input());
  assert.equal(d.admitted, false);
  assert.equal(d.reason, 'sender-not-allowed');
});

test('DM allowlist with missing sender rejects with sender-not-allowed', () => {
  const d = admitInbound(config(), input({ senderId: undefined }));
  assert.equal(d.admitted, false);
  assert.equal(d.reason, 'sender-not-allowed');
});

test('DM open admits without an allowlist hit', () => {
  const d = admitInbound(
    config({ dmPolicy: 'open', dmAllowFrom: [] }),
    input({ senderId: undefined }),
  );
  assert.equal(d.admitted, true);
  assert.equal(d.reason, 'admitted');
});

test('DM wildcard allowlist admits any sender, case-sensitively', () => {
  assert.equal(reason(config({ dmAllowFrom: ['*'] }), input({ senderId: asSenderId('anyone') })), 'admitted');
  // Comparison is exact and case-sensitive.
  assert.equal(reason(config({ dmAllowFrom: ['WXID_ALICE'] }), input()), 'sender-not-allowed');
});

// --- Group gates ------------------------------------------------------------

test('group disabled rejects with disabled', () => {
  const d = admitInbound(config({ groupPolicy: 'disabled' }), input({ isGroup: true, mentioned: true }));
  assert.equal(d.admitted, false);
  assert.equal(d.reason, 'disabled');
});

test('group wildcard allowlist admits any group', () => {
  const d = admitInbound(
    config({ groupAllowFrom: ['*'], requireMentionInGroups: false }),
    input({ isGroup: true, chatJid: asChatJid('other@chatroom') }),
  );
  assert.equal(d.admitted, true);
  assert.equal(d.reason, 'admitted');
});

test('group exact allowlist hit admits', () => {
  const d = admitInbound(
    config({ groupAllowFrom: [CHAT], requireMentionInGroups: false }),
    input({ isGroup: true }),
  );
  assert.equal(d.admitted, true);
  assert.equal(d.reason, 'admitted');
});

test('group exact allowlist miss rejects with group-not-allowed', () => {
  const d = admitInbound(
    config({ groupAllowFrom: [asChatJid('allowed@chatroom')] }),
    input({ isGroup: true, mentioned: true }),
  );
  assert.equal(d.admitted, false);
  assert.equal(d.reason, 'group-not-allowed');
});

test('empty groupAllowFrom does not reject on its own', () => {
  const d = admitInbound(
    config({ groupAllowFrom: [], requireMentionInGroups: false }),
    input({ isGroup: true }),
  );
  assert.equal(d.admitted, true);
  assert.equal(d.reason, 'admitted');
});

test('group sender gate: exact hit admits, miss and missing sender reject', () => {
  const cfg = config({
    groupAllowFrom: [],
    groupSenderAllowFrom: ['wxid_alice'],
    requireMentionInGroups: false,
  });
  assert.equal(reason(cfg, input({ isGroup: true })), 'admitted');
  assert.equal(
    reason(cfg, input({ isGroup: true, senderId: asSenderId('wxid_bob') })),
    'sender-not-allowed',
  );
  assert.equal(
    reason(cfg, input({ isGroup: true, senderId: undefined })),
    'sender-not-allowed',
  );
});

test('group sender wildcard admits any sender', () => {
  const cfg = config({
    groupSenderAllowFrom: ['*'],
    requireMentionInGroups: false,
  });
  assert.equal(
    reason(cfg, input({ isGroup: true, senderId: asSenderId('wxid_unknown') })),
    'admitted',
  );
});

test('empty groupSenderAllowFrom imposes no sender restriction', () => {
  const cfg = config({ groupSenderAllowFrom: [], requireMentionInGroups: false });
  assert.equal(
    reason(cfg, input({ isGroup: true, senderId: undefined })),
    'admitted',
  );
});

// --- Mention gate -----------------------------------------------------------

test('missing mention rejects with missing-mention', () => {
  const d = admitInbound(config(), input({ isGroup: true, mentioned: false }));
  assert.equal(d.admitted, false);
  assert.equal(d.reason, 'missing-mention');
});

test('mention present admits', () => {
  const d = admitInbound(config(), input({ isGroup: true, mentioned: true }));
  assert.equal(d.admitted, true);
  assert.equal(d.reason, 'admitted');
});

test('mention gate disabled admits without a mention', () => {
  const d = admitInbound(
    config({ requireMentionInGroups: false }),
    input({ isGroup: true, mentioned: false }),
  );
  assert.equal(d.admitted, true);
  assert.equal(d.reason, 'admitted');
});

// --- Literal matching (no glob/prefix/regex) --------------------------------

test("'wxid_*' is a literal entry, not a wildcard", () => {
  // As a sender allowlist entry it must NOT match a different sender.
  assert.equal(
    reason(
      config({ dmAllowFrom: ['wxid_*'] }),
      input({ senderId: asSenderId('wxid_alice') }),
    ),
    'sender-not-allowed',
  );
  // It matches only the exact literal string.
  assert.equal(
    reason(
      config({ dmAllowFrom: ['wxid_*'] }),
      input({ senderId: asSenderId('wxid_*') }),
    ),
    'admitted',
  );
  // Same for group JIDs: 'room_*' must not match 'room_1@chatroom'.
  assert.equal(
    reason(
      config({ groupAllowFrom: ['room_*'], requireMentionInGroups: false }),
      input({ isGroup: true, chatJid: asChatJid('room_1@chatroom') }),
    ),
    'group-not-allowed',
  );
});

// --- Fingerprint ------------------------------------------------------------

test('fingerprint is deterministic and covers every config field', () => {
  const base = config();
  const fp = (cfg: AdmissionConfig) => admitInbound(cfg, input()).policyFingerprint;

  const baseline = fp(base);
  assert.match(baseline, /^policy-[0-9a-f]{8}$/);
  assert.equal(fp(config()), baseline, 'identical config -> identical fingerprint');

  const variants: Array<[string, AdmissionConfig]> = [
    ['dmPolicy', config({ dmPolicy: 'open' })],
    ['groupPolicy', config({ groupPolicy: 'disabled' })],
    ['dmAllowFrom', config({ dmAllowFrom: ['wxid_bob'] })],
    ['groupAllowFrom', config({ groupAllowFrom: [CHAT] })],
    ['groupSenderAllowFrom', config({ groupSenderAllowFrom: ['wxid_bob'] })],
    ['requireMentionInGroups', config({ requireMentionInGroups: false })],
  ];
  for (const [field, variant] of variants) {
    assert.notEqual(fp(variant), baseline, `changing ${field} must change the fingerprint`);
  }

  // Array order is normalized, so reordering is not a policy change.
  assert.equal(
    fp(config({ dmAllowFrom: ['wxid_bob', 'wxid_alice'] })),
    fp(config({ dmAllowFrom: ['wxid_alice', 'wxid_bob'] })),
    'array order must not affect the fingerprint',
  );

  // Raw config values never appear verbatim in the fingerprint.
  assert.ok(!baseline.includes('wxid_alice'));
  assert.ok(!baseline.includes('allowlist'));
});

// --- Reason precedence ------------------------------------------------------

test('precedence: disabled beats missing-mention', () => {
  assert.equal(
    reason(
      config({ groupPolicy: 'disabled' }),
      input({ isGroup: true, mentioned: false }),
    ),
    'disabled',
  );
});

test('precedence: group-not-allowed beats missing-mention', () => {
  assert.equal(
    reason(
      config({ groupAllowFrom: [asChatJid('other@chatroom')] }),
      input({ isGroup: true, mentioned: false }),
    ),
    'group-not-allowed',
  );
});

test('precedence: sender-not-allowed beats missing-mention', () => {
  assert.equal(
    reason(
      config({ groupSenderAllowFrom: ['wxid_bob'] }),
      input({ isGroup: true, mentioned: false }),
    ),
    'sender-not-allowed',
  );
});

test('precedence: tombstone beats disabled', () => {
  assert.equal(
    reason(
      config({ groupPolicy: 'disabled' }),
      input({ isGroup: true, tombstone: true }),
    ),
    'tombstone',
  );
});
