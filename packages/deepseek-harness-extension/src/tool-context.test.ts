import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  createTrustedToolContext,
  DEFAULT_GRANTS,
  ToolAccessError,
  type ToolContextDeps,
} from './tool-context.ts';
import {
  asAccountId,
  asChatJid,
  asRouteKey,
  asSessionId,
  type RouteBinding,
} from './contracts.ts';

const binding = (overrides: Partial<RouteBinding> = {}): RouteBinding => ({
  routeKey: asRouteKey('acct:wxid_alice'),
  sessionId: asSessionId('sess-1'),
  accountId: asAccountId('acct'),
  chatJid: asChatJid('wxid_alice'),
  policyFingerprint: 'fp',
  revision: 1,
  lastActivityAt: new Date().toISOString(),
  ...overrides,
});

interface MutableConfig {
  ownerAllowFrom: readonly string[];
  grants: typeof DEFAULT_GRANTS;
}

function makeDeps(options: {
  binding?: RouteBinding | undefined;
  ownerAllowFrom?: readonly string[];
  generation?: number;
} = {}): {
  deps: ToolContextDeps;
  config: MutableConfig;
  setBinding: (b: RouteBinding | undefined) => void;
} {
  let current = 'binding' in options ? options.binding : binding();
  const config: MutableConfig = {
    ownerAllowFrom: options.ownerAllowFrom ?? ['wxid_alice'],
    grants: DEFAULT_GRANTS,
  };
  return {
    deps: {
      bindingForAgent: async () => current,
      config,
      generation: () => options.generation ?? 3,
    },
    config,
    setBinding(next) {
      current = next;
    },
  };
}

test('owner DM chat resolves an owner principal with all scopes', async () => {
  const host = makeDeps();
  const ctx = createTrustedToolContext(host.deps);
  const principal = await ctx.forAgent('agent-1');
  assert.ok(principal);
  assert.equal(principal.isOwner, true);
  assert.equal(principal.chatJid, 'wxid_alice');
  assert.equal(principal.generation, 3);
  assert.deepEqual(
    principal.grants.map((g) => g.scope),
    ['current-chat', 'owner', 'archive', 'call'],
  );
});

test('group chat is never owner even when the group id were allowlisted', async () => {
  const host = makeDeps({
    binding: binding({ chatJid: asChatJid('room@chatroom') }),
    ownerAllowFrom: ['room@chatroom'],
  });
  const ctx = createTrustedToolContext(host.deps);
  const principal = await ctx.forAgent('agent-1');
  assert.ok(principal);
  assert.equal(principal.isOwner, false);
  assert.deepEqual(
    principal.grants.map((g) => g.scope),
    ['current-chat'],
  );
});

test('non-allowlisted DM is not owner', async () => {
  const host = makeDeps({ ownerAllowFrom: ['wxid_other'] });
  const ctx = createTrustedToolContext(host.deps);
  const principal = await ctx.forAgent('agent-1');
  assert.ok(principal);
  assert.equal(principal.isOwner, false);
});

test('an Agent with no WeChat route resolves to undefined', async () => {
  const host = makeDeps({ binding: undefined });
  const ctx = createTrustedToolContext(host.deps);
  assert.equal(await ctx.forAgent('agent-x'), undefined);
});

test('require throws ToolAccessError for an unbound Agent', async () => {
  const host = makeDeps({ binding: undefined });
  const ctx = createTrustedToolContext(host.deps);
  await assert.rejects(ctx.require('agent-x', 'wechat_history'), ToolAccessError);
});

test('non-owner DMs only get current-chat tools', async () => {
  const host = makeDeps({ ownerAllowFrom: [] });
  const ctx = createTrustedToolContext(host.deps);
  const principal = await ctx.forAgent('agent-1');
  assert.ok(principal);
  assert.equal(ctx.allows(principal, 'wechat_history'), true);
  assert.equal(ctx.allows(principal, 'wechat_contacts'), false);
  assert.equal(ctx.allows(principal, 'wechat_archive_history'), false);
});

test('owner principal is granted explicitly configured owner/archive tools', async () => {
  const host = makeDeps();
  host.config.grants = {
    currentChat: DEFAULT_GRANTS.currentChat,
    owner: ['wechat_contacts'],
    archive: ['wechat_archive_history'],
    call: [],
  };
  const ctx = createTrustedToolContext(host.deps);
  const principal = await ctx.require('agent-1', 'wechat_contacts');
  assert.equal(principal.isOwner, true);
  assert.equal(ctx.allows(principal, 'wechat_archive_history'), true);
  assert.equal(ctx.allows(principal, 'wechat_call'), false);
});

test('require rejects a tool outside the granted set', async () => {
  const host = makeDeps({ ownerAllowFrom: [] });
  const ctx = createTrustedToolContext(host.deps);
  await assert.rejects(ctx.require('agent-1', 'wechat_contacts'), /not granted/);
});

test('revalidate rejects when the route disappeared during the call', async () => {
  const host = makeDeps();
  const ctx = createTrustedToolContext(host.deps);
  const principal = await ctx.require('agent-1', 'wechat_history');
  host.setBinding(undefined);
  await assert.rejects(ctx.revalidate(principal, 'wechat_history'), /route was removed/);
});

test('revalidate rejects when owner authority was revoked mid-call', async () => {
  const host = makeDeps();
  host.config.grants = {
    currentChat: DEFAULT_GRANTS.currentChat,
    owner: ['wechat_contacts'],
    archive: [],
    call: [],
  };
  const ctx = createTrustedToolContext(host.deps);
  const principal = await ctx.require('agent-1', 'wechat_contacts');
  host.config.ownerAllowFrom = [];
  await assert.rejects(
    ctx.revalidate(principal, 'wechat_contacts'),
    /access was revoked|owner binding no longer holds/,
  );
});

test('revalidate passes when nothing changed', async () => {
  const host = makeDeps();
  const ctx = createTrustedToolContext(host.deps);
  const principal = await ctx.require('agent-1', 'wechat_history');
  const again = await ctx.revalidate(principal, 'wechat_history');
  assert.equal(again.agentId, 'agent-1');
  assert.equal(again.isOwner, true);
});

test('default grants arm the five owner tools for an owner binding', async () => {
  const host = makeDeps(); // makeDeps defaults ownerAllowFrom to the owner wxid
  const ctx = createTrustedToolContext({ ...host.deps, config: { ...host.deps.config, grants: DEFAULT_GRANTS } });
  const principal = await ctx.require('agent-1', 'wechat_contacts');
  assert.equal(principal.isOwner, true);
  for (const name of [
    'wechat_stickers',
    'wechat_login',
    'wechat_contacts',
    'wechat_contact_request',
    'wechat_group_info',
  ]) {
    assert.equal(ctx.allows(principal, name), true, `${name} armed by owner binding`);
  }
  // Archive and call stay default-deny even for the owner.
  for (const name of [
    'wechat_archive_threads',
    'wechat_archive_search',
    'wechat_archive_history',
    'wechat_archive_message',
    'wechat_archive_media',
    'wechat_call',
  ]) {
    assert.equal(ctx.allows(principal, name), false, `${name} default-deny`);
  }
});

test('archive tools stay invisible until config explicitly grants them', async () => {
  const host = makeDeps();
  host.config.grants = { ...DEFAULT_GRANTS, archive: ['wechat_archive_history'] };
  const ctx = createTrustedToolContext(host.deps);
  await ctx.require('agent-1', 'wechat_archive_history'); // granted now
  await assert.rejects(ctx.require('agent-1', 'wechat_archive_search'), /not granted/);
});
