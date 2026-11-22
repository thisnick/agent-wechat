import assert from 'node:assert/strict';
import { test } from 'node:test';
import { accountTools, wechatLoginTool, wechatStickersTool } from './account.ts';
import { ToolAccessError } from '../tool-context.ts';
import type { ToolDeps } from './shared.ts';
import type { ToolPrincipal } from '../contracts.ts';

// ---------------------------------------------------------------------------
// Shared fakes
// ---------------------------------------------------------------------------

export const ownerPrincipal: ToolPrincipal = {
  agentId: 'agent-1',
  routeKey: 'default:wxid_alice' as ToolPrincipal['routeKey'],
  accountId: 'default' as ToolPrincipal['accountId'],
  chatJid: 'wxid_alice' as ToolPrincipal['chatJid'],
  isOwner: true,
  grants: [
    { scope: 'current-chat', tools: [] },
    { scope: 'owner', tools: ['wechat_stickers', 'wechat_login'] },
    { scope: 'archive', tools: [] },
    { scope: 'call', tools: [] },
  ],
  generation: 1,
};

export const memberPrincipal: ToolPrincipal = {
  ...ownerPrincipal,
  isOwner: false,
  grants: [{ scope: 'current-chat', tools: [] }],
};

export const exec = (): { agent: { id: string }; signal: AbortSignal } => ({
  agent: { id: 'agent-1' },
  signal: new AbortController().signal,
});

function makeDeps(principal: ToolPrincipal = ownerPrincipal): {
  deps: ToolDeps;
  state: {
    revalidates: number;
    stickers: Array<Record<string, unknown>>;
    prepareJobs: Array<Record<string, unknown>>;
    addJobs: Array<Record<string, unknown>>;
    moveJobs: Array<Record<string, unknown>>;
    removeJobs: Array<Record<string, unknown>>;
    accounts: Array<Record<string, unknown>>;
    logins: Array<unknown>;
    operations: Array<Record<string, unknown>>;
    disconnects: number;
    logouts: number;
    forgets: number;
  };
} {
  const state = {
    revalidates: 0,
    stickers: [{ id: 's1', emoji: '🙂' }] as Array<Record<string, unknown>>,
    prepareJobs: [] as Array<Record<string, unknown>>,
    addJobs: [] as Array<Record<string, unknown>>,
    moveJobs: [] as Array<Record<string, unknown>>,
    removeJobs: [] as Array<Record<string, unknown>>,
    accounts: [{ id: 'default', state: 'online' }] as Array<Record<string, unknown>>,
    logins: [] as Array<unknown>,
    operations: [] as Array<Record<string, unknown>>,
    disconnects: 0,
    logouts: 0,
    forgets: 0,
  };

  const client = {
    stickerCollections: async () => ({ items: state.stickers }),
    customSticker: async (_a: string, stickerId: string) =>
      state.stickers.find((s) => s['id'] === stickerId) ?? { id: stickerId },
    prepareSticker: async (_a: string, input: unknown, key: string) => {
      const job = { id: `prep-${key}`, state: 'pending', input };
      state.prepareJobs.push(job);
      return job;
    },
    addCustomSticker: async (_a: string, input: unknown, key: string) => {
      const job = { id: `add-${key}`, state: 'pending', input };
      state.addJobs.push(job);
      return job;
    },
    moveCustomSticker: async (
      _a: string,
      stickerId: string,
      input: unknown,
      revision: string,
      key: string,
    ) => {
      const job = { id: `move-${key}`, stickerId, revision, input };
      state.moveJobs.push(job);
      return job;
    },
    removeCustomSticker: async (_a: string, stickerId: string, revision: string, key: string) => {
      const job = { id: `remove-${key}`, stickerId, revision };
      state.removeJobs.push(job);
      return job;
    },
    accounts: async () => ({ items: state.accounts }),
    login: async (_a: string, options: unknown, key: string) => {
      const operation = { id: `login-${key}`, state: 'pending', options };
      state.logins.push(operation);
      return operation;
    },
    operation: async (_a: string, operationId: string) =>
      state.operations.find((o) => o['id'] === operationId) ?? { id: operationId, state: 'pending' },
    disconnect: async () => {
      state.disconnects += 1;
      return {};
    },
    logoutLocal: async () => {
      state.logouts += 1;
      return {};
    },
    forgetCredentials: async (_a: string, _key: string, confirm: true) => {
      state.forgets += 1;
      return { id: 'forget', confirm };
    },
  };

  const deps: ToolDeps = {
    client: client as never,
    context: {
      require: async (_agentId: string, toolName: string) => {
        if (!principal.isOwner) {
          throw new ToolAccessError(`${toolName}: not granted to this Agent`, toolName);
        }
        return principal;
      },
      revalidate: async () => {
        state.revalidates += 1;
        return principal;
      },
      allows: () => true,
      forAgent: async () => principal,
    },
    accountId: 'default' as ToolDeps['accountId'],
    log: () => {},
    now: () => Date.now(),
  };
  return { deps, state };
}

// ---------------------------------------------------------------------------
// wechat_stickers
// ---------------------------------------------------------------------------

test('stickers list returns collections for the owner', async () => {
  const { deps, state } = makeDeps();
  const tool = wechatStickersTool(deps);
  const value = (await tool.execute({ action: 'list' }, exec())) as { collections: unknown[] };
  assert.equal(value.collections.length, 1);
  assert.equal(state.revalidates, 0, 'reads do not revalidate');
});

test('stickers rejects a non-owner principal', async () => {
  const { deps } = makeDeps(memberPrincipal);
  const tool = wechatStickersTool(deps);
  await assert.rejects(tool.execute({ action: 'list' }, exec()), ToolAccessError);
});

test('stickers get returns sticker metadata', async () => {
  const { deps } = makeDeps();
  const tool = wechatStickersTool(deps);
  const value = (await tool.execute({ action: 'get', stickerId: 's1' }, exec())) as {
    sticker: Record<string, unknown>;
  };
  assert.equal(value.sticker['emoji'], '🙂');
});

test('stickers prepare requires an idempotency key and revalidates', async () => {
  const { deps, state } = makeDeps();
  const tool = wechatStickersTool(deps);
  await assert.rejects(
    tool.execute({ action: 'prepare', file: '/tmp/x.png' }, exec()),
    /idempotencyKey/,
  );
  await tool.execute({ action: 'prepare', file: '/tmp/x.png', idempotencyKey: 'k1' }, exec());
  assert.equal(state.prepareJobs.length, 1);
  assert.equal(state.revalidates, 1, 'mutation revalidated');
});

test('stickers add requires exactly one of preparationId or messageId', async () => {
  const { deps, state } = makeDeps();
  const tool = wechatStickersTool(deps);
  await assert.rejects(tool.execute({ action: 'add', idempotencyKey: 'k' }, exec()), /exactly one/);
  await assert.rejects(
    tool.execute({ action: 'add', preparationId: 'p', messageId: 'm', idempotencyKey: 'k' }, exec()),
    /exactly one/,
  );
  await tool.execute({ action: 'add', preparationId: 'p', idempotencyKey: 'k1' }, exec());
  assert.equal(state.addJobs.length, 1);
});

test('stickers move accepts afterId null and remove revalidates', async () => {
  const { deps, state } = makeDeps();
  const tool = wechatStickersTool(deps);
  const moved = (await tool.execute(
    { action: 'move', stickerId: 's1', revision: 'r1', afterId: null, idempotencyKey: 'k1' },
    exec(),
  )) as { status: string };
  assert.equal(moved.status, 'submitted');
  const removed = (await tool.execute(
    { action: 'remove', stickerId: 's1', revision: 'r2', idempotencyKey: 'k2' },
    exec(),
  )) as { status: string };
  assert.equal(removed.status, 'submitted');
  assert.equal(state.moveJobs.length, 1);
  assert.equal(state.removeJobs.length, 1);
  assert.equal(state.revalidates, 2);
});

test('stickers download and preview never fabricate staged bytes', async () => {
  const { deps } = makeDeps();
  const tool = wechatStickersTool(deps);
  const downloaded = (await tool.execute({ action: 'download', stickerId: 's1' }, exec())) as {
    status: string;
  };
  assert.equal(downloaded.status, 'not_staged');
  const preview = (await tool.execute({ action: 'preview', stickerId: 's1' }, exec())) as {
    status: string;
  };
  assert.equal(preview.status, 'unavailable');
});

test('stickers unknown action is rejected', async () => {
  const { deps } = makeDeps();
  const tool = wechatStickersTool(deps);
  await assert.rejects(tool.execute({ action: 'explode' }, exec()), /unknown action/);
});

// ---------------------------------------------------------------------------
// wechat_login
// ---------------------------------------------------------------------------

test('login status lists accounts for the owner', async () => {
  const { deps, state } = makeDeps();
  const tool = wechatLoginTool(deps);
  const value = (await tool.execute({ action: 'status' }, exec())) as { accounts: unknown[] };
  assert.equal(value.accounts.length, 1);
  assert.equal(state.revalidates, 0);
});

test('login start submits an operation and revalidates', async () => {
  const { deps, state } = makeDeps();
  const tool = wechatLoginTool(deps);
  const value = (await tool.execute({ action: 'start', force: true }, exec())) as {
    status: string;
    operation: { state: string };
  };
  assert.equal(value.status, 'submitted');
  assert.equal(value.operation.state, 'pending');
  assert.equal(state.logins.length, 1);
  assert.equal(state.revalidates, 1);
});

test('login wait reports timeout instead of success', async () => {
  const { deps } = makeDeps();
  const tool = wechatLoginTool(deps);
  const value = (await tool.execute(
    { action: 'wait', operationId: 'login-x', timeoutMs: 1000 },
    exec(),
  )) as { timedOut: boolean; state: unknown };
  assert.equal(value.timedOut, true);
  assert.equal(value.state, 'pending');
});

test('login wait returns terminal operation without timeout', async () => {
  const { deps, state } = makeDeps();
  state.operations.push({ id: 'login-ok', state: 'done' });
  const tool = wechatLoginTool(deps);
  const value = (await tool.execute(
    { action: 'wait', operationId: 'login-ok', timeoutMs: 5000 },
    exec(),
  )) as { timedOut: boolean; state: unknown };
  assert.equal(value.timedOut, false);
  assert.equal(value.state, 'done');
});

test('login wait validates timeoutMs bounds', async () => {
  const { deps } = makeDeps();
  const tool = wechatLoginTool(deps);
  await assert.rejects(
    tool.execute({ action: 'wait', operationId: 'x', timeoutMs: 50 }, exec()),
    /timeoutMs/,
  );
  await assert.rejects(
    tool.execute({ action: 'wait', operationId: 'x', timeoutMs: 999999 }, exec()),
    /timeoutMs/,
  );
});

test('login forget requires confirm true', async () => {
  const { deps, state } = makeDeps();
  const tool = wechatLoginTool(deps);
  await assert.rejects(tool.execute({ action: 'forget' }, exec()), /confirm/);
  await assert.rejects(tool.execute({ action: 'forget', confirm: false }, exec()), /confirm/);
  await tool.execute({ action: 'forget', confirm: true }, exec());
  assert.equal(state.forgets, 1);
  assert.equal(state.revalidates, 1);
});

test('login disconnect and logout are local state changes', async () => {
  const { deps, state } = makeDeps();
  const tool = wechatLoginTool(deps);
  const d = (await tool.execute({ action: 'disconnect' }, exec())) as { status: string };
  assert.equal(d.status, 'disconnected_local_trust_kept');
  const l = (await tool.execute({ action: 'logout' }, exec())) as { status: string };
  assert.equal(l.status, 'logged_out_local');
  assert.equal(state.disconnects, 1);
  assert.equal(state.logouts, 1);
});

test('login rejects non-owner and unknown action', async () => {
  const member = makeDeps(memberPrincipal);
  await assert.rejects(
    wechatLoginTool(member.deps).execute({ action: 'status' }, exec()),
    ToolAccessError,
  );
  const { deps } = makeDeps();
  await assert.rejects(wechatLoginTool(deps).execute({ action: 'root' }, exec()), /unknown action/);
});

test('accountTools exports both tools in order', () => {
  const { deps } = makeDeps();
  const tools = accountTools(deps);
  assert.deepEqual(
    tools.map((tool) => tool.name),
    ['wechat_stickers', 'wechat_login'],
  );
});
