import assert from 'node:assert/strict';
import { test } from 'node:test';
import { ownerTools, wechatGroupInfoTool } from './owner.ts';
import { ToolAccessError } from '../tool-context.ts';
import type { ToolDeps } from './shared.ts';
import type { ToolPrincipal } from '../contracts.ts';

// ---------------------------------------------------------------------------
// Shared fakes
// ---------------------------------------------------------------------------

const ownerPrincipal: ToolPrincipal = {
  agentId: 'agent-1',
  routeKey: 'default:wxid_alice' as ToolPrincipal['routeKey'],
  accountId: 'default' as ToolPrincipal['accountId'],
  chatJid: 'wxid_alice' as ToolPrincipal['chatJid'],
  isOwner: true,
  grants: [
    { scope: 'current-chat', tools: [] },
    {
      scope: 'owner',
      tools: [
        'wechat_contacts',
        'wechat_contact_request',
        'wechat_group_info',
        'wechat_archive_threads',
        'wechat_archive_search',
        'wechat_archive_history',
        'wechat_archive_message',
        'wechat_archive_media',
      ],
    },
    { scope: 'archive', tools: [] },
    { scope: 'call', tools: [] },
  ],
  generation: 1,
};

const memberPrincipal: ToolPrincipal = {
  ...ownerPrincipal,
  isOwner: false,
  grants: [{ scope: 'current-chat', tools: [] }],
};

const exec = (): { agent: { id: string }; signal: AbortSignal } => ({
  agent: { id: 'agent-1' },
  signal: new AbortController().signal,
});

interface FakeState {
  revalidates: number;
  removedContacts: string[];
  sentRequests: unknown[];
  acceptedRequests: unknown[];
  deletedRequests: unknown[];
  contacts: Array<Record<string, unknown>>;
  threads: Array<Record<string, unknown>>;
  messages: Array<Record<string, unknown>>;
}

function makeDeps(principal: ToolPrincipal = ownerPrincipal): { deps: ToolDeps; state: FakeState } {
  const state: FakeState = {
    revalidates: 0,
    removedContacts: [],
    sentRequests: [],
    acceptedRequests: [],
    deletedRequests: [],
    contacts: [
      { username: 'wxid_alice', nickname: 'Alice', remark: '' },
      { username: 'wxid_bob', nickname: 'Bob', remark: 'coworker' },
    ],
    threads: [
      { threadId: 'wxid_alice', kind: 'dm' },
      { threadId: 'room@chatroom', kind: 'group' },
    ],
    messages: [
      {
        id: 'm1',
        threadId: 'wxid_alice',
        serverId: '111',
        senderId: 'wxid_alice',
        direction: 'incoming',
        type: 1,
        body: 'hello there',
        createdAt: '2026-01-01T00:00:00Z',
      },
      {
        id: 'm2',
        threadId: 'room@chatroom',
        serverId: '222',
        senderId: 'wxid_bob',
        direction: 'incoming',
        type: 1,
        body: 'meeting at noon',
        createdAt: '2026-01-02T00:00:00Z',
      },
    ],
  };

  const client = {
    threads: async (_a: string, options?: Record<string, unknown>) => ({
      items: state.threads,
      ...(typeof options?.['cursor'] === 'string' ? {} : { nextCursor: 't-next' }),
    }),
    messages: async (_a: string, threadId: string) => ({
      items: state.messages.filter((m) => m['threadId'] === threadId),
    }),
    message: async (_a: string, messageId: string) =>
      state.messages.find((m) => m['id'] === messageId) ??
      (() => {
        throw new Error('message not found');
      })(),
    searchMessages: async (_a: string, options: Record<string, unknown>) => {
      const query = String(options['query'] ?? '').toLowerCase();
      const threadId = options['threadId'];
      return {
        items: state.messages.filter(
          (m) =>
            String(m['body']).toLowerCase().includes(query) &&
            (threadId === undefined || m['threadId'] === threadId),
        ),
      };
    },
    mediaAssets: async () => ({
      items: [
        {
          id: 'asset-1',
          accountId: 'default',
          messageId: 'm1',
          kind: 'image',
          variant: 'original',
          actualVariant: 'original',
          state: 'available',
          contentType: 'image/jpeg',
          byteLength: 1024,
          sha256: 'abc',
          filename: 'photo.jpg',
          expiresAt: null,
        },
      ],
    }),
    contacts: async () => ({ items: state.contacts }),
    contact: async (_a: string, username: string) =>
      state.contacts.find((c) => c['username'] === username) ??
      (() => {
        throw new Error('contact not found');
      })(),
    lookupContacts: async (_a: string, ids: string[]) => ({
      items: state.contacts.filter((c) => ids.includes(String(c['username']))),
    }),
    searchContacts: async (_a: string, query: string) => ({
      items: state.contacts.filter((c) =>
        String(c['nickname']).toLowerCase().includes(query.toLowerCase()),
      ),
    }),
    removeContact: async (_a: string, username: string, _key: string) => {
      state.removedContacts.push(username);
      return { id: 'op-remove' };
    },
    contactRequests: async () => ({ items: [] }),
    contactRequest: async (_a: string, requestId: string) => ({ id: requestId, version: 'v1' }),
    sendContactRequest: async (_a: string, input: unknown, _key: string) => {
      state.sentRequests.push(input);
      return { id: 'op-send' };
    },
    acceptContactRequest: async (
      _a: string,
      requestId: string,
      input: unknown,
      _key: string,
    ) => {
      state.acceptedRequests.push({ requestId, input });
      return { id: 'op-accept' };
    },
    deleteContactRequest: async (
      _a: string,
      requestId: string,
      expectedVersion: string,
      _key: string,
    ) => {
      state.deletedRequests.push({ requestId, expectedVersion });
      return { id: 'op-delete' };
    },
    group: async (_a: string, roomId: string) => ({
      threadId: roomId,
      name: 'Test group',
      memberCount: 3,
    }),
    groupMembers: async (_a: string, roomId: string) => ({
      items: [
        { username: 'wxid_alice', displayName: 'Alice' },
        { username: 'wxid_bob', displayName: 'Bob' },
      ],
      nextCursor: 'm-next',
    }),
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
// Contacts
// ---------------------------------------------------------------------------

test('contacts list returns the address book', async () => {
  const { deps } = makeDeps();
  const tool = ownerTools(deps)[0]!;
  const value = (await tool.execute({ action: 'list' }, exec())) as { items: unknown[] };
  assert.equal(value.items.length, 2);
});

test('contacts find filters locally', async () => {
  const { deps } = makeDeps();
  const tool = ownerTools(deps)[0]!;
  const value = (await tool.execute({ action: 'find', query: 'bob' }, exec())) as {
    items: Array<Record<string, unknown>>;
  };
  assert.equal(value.items.length, 1);
  assert.equal(value.items[0]!['username'], 'wxid_bob');
});

test('contacts remove requires idempotencyKey and revalidates', async () => {
  const { deps, state } = makeDeps();
  const tool = ownerTools(deps)[0]!;
  await assert.rejects(
    tool.execute({ action: 'remove', username: 'wxid_bob' }, exec()),
    /idempotencyKey/,
  );
  const value = (await tool.execute(
    { action: 'remove', username: 'wxid_bob', idempotencyKey: 'k1' },
    exec(),
  )) as { status: string };
  assert.equal(value.status, 'submitted');
  assert.equal(state.removedContacts[0], 'wxid_bob');
  assert.equal(state.revalidates, 1);
});

test('contacts rejects non-owner', async () => {
  const { deps } = makeDeps(memberPrincipal);
  const tool = ownerTools(deps)[0]!;
  await assert.rejects(tool.execute({ action: 'list' }, exec()), ToolAccessError);
});

// ---------------------------------------------------------------------------
// Contact requests
// ---------------------------------------------------------------------------

test('contact request send requires recipientRef and key', async () => {
  const { deps, state } = makeDeps();
  const tool = ownerTools(deps)[1]!;
  await assert.rejects(tool.execute({ action: 'send' }, exec()), /recipientRef/);
  await tool.execute(
    { action: 'send', recipientRef: 'wxid_new', greeting: 'hi', idempotencyKey: 'k' },
    exec(),
  );
  assert.equal(state.sentRequests.length, 1);
  assert.equal(state.revalidates, 1);
});

test('contact request accept and delete require expectedVersion', async () => {
  const { deps, state } = makeDeps();
  const tool = ownerTools(deps)[1]!;
  await assert.rejects(
    tool.execute({ action: 'accept', requestId: 'r1', idempotencyKey: 'k' }, exec()),
    /expectedVersion/,
  );
  await tool.execute(
    { action: 'accept', requestId: 'r1', expectedVersion: 'v1', idempotencyKey: 'k1' },
    exec(),
  );
  await tool.execute(
    { action: 'delete', requestId: 'r1', expectedVersion: 'v2', idempotencyKey: 'k2' },
    exec(),
  );
  assert.equal(state.acceptedRequests.length, 1);
  assert.deepEqual(state.deletedRequests[0], { requestId: 'r1', expectedVersion: 'v2' });
});

// ---------------------------------------------------------------------------
// Group info
// ---------------------------------------------------------------------------

test('group info returns members and marks the page unconfirmed when a cursor remains', async () => {
  const { deps } = makeDeps();
  const tool = wechatGroupInfoTool(deps);
  const value = (await tool.execute({ groupId: '123@chatroom' }, exec())) as {
    members: unknown[];
    unconfirmed: boolean;
  };
  assert.equal(value.members.length, 2);
  assert.equal(value.unconfirmed, true, 'remaining cursor means the list is incomplete');
});

test('group info rejects a non-chatroom id', async () => {
  const { deps } = makeDeps();
  const tool = wechatGroupInfoTool(deps);
  await assert.rejects(tool.execute({ groupId: 'wxid_alice' }, exec()), /@chatroom/);
});

// ---------------------------------------------------------------------------
// Archive tools
// ---------------------------------------------------------------------------

test('archive threads list whole-account chats', async () => {
  const { deps } = makeDeps();
  const tool = ownerTools(deps)[3]!;
  const value = (await tool.execute({}, exec())) as { items: unknown[]; nextCursor?: string };
  assert.equal(value.items.length, 2);
  assert.equal(value.nextCursor, 't-next');
});

test('archive search scopes by threadId when given', async () => {
  const { deps } = makeDeps();
  const tool = ownerTools(deps)[4]!;
  const all = (await tool.execute({ query: 'meeting' }, exec())) as { items: unknown[] };
  assert.equal(all.items.length, 1);
  const scoped = (await tool.execute(
    { query: 'hello', threadId: 'wxid_alice' },
    exec(),
  )) as { items: Array<Record<string, unknown>> };
  assert.equal(scoped.items.length, 1);
  assert.equal(scoped.items[0]!['threadId'], 'wxid_alice');
});

test('archive history compacts messages', async () => {
  const { deps } = makeDeps();
  const tool = ownerTools(deps)[5]!;
  const value = (await tool.execute({ threadId: 'wxid_alice' }, exec())) as {
    items: Array<Record<string, unknown>>;
  };
  assert.equal(value.items.length, 1);
  assert.equal(value.items[0]!['body'], 'hello there');
  assert.ok(!('deleted' in value.items[0]!));
});

test('archive message reads one stored message', async () => {
  const { deps } = makeDeps();
  const tool = ownerTools(deps)[6]!;
  const value = (await tool.execute({ messageId: 'm2' }, exec())) as {
    message: Record<string, unknown>;
  };
  assert.equal(value.message['id'], 'm2');
});

test('archive media reports metadata without downloading bytes', async () => {
  const { deps } = makeDeps();
  const tool = ownerTools(deps)[7]!;
  const value = (await tool.execute({ messageId: 'm1' }, exec())) as {
    assets: Array<Record<string, unknown>>;
  };
  assert.equal(value.assets.length, 1);
  assert.equal(value.assets[0]!['state'], 'available');
  assert.equal(value.assets[0]!['contentType'], 'image/jpeg');
});

test('archive tools reject non-owner principals', async () => {
  const { deps } = makeDeps(memberPrincipal);
  for (const index of [3, 4, 5, 6, 7]) {
    const tool = ownerTools(deps)[index]!;
    await assert.rejects(tool.execute(index === 5 ? { threadId: 'x' } : {}, exec()), ToolAccessError);
  }
});

test('ownerTools exports all eight tools in order', () => {
  const { deps } = makeDeps();
  assert.deepEqual(
    ownerTools(deps).map((tool) => tool.name),
    [
      'wechat_contacts',
      'wechat_contact_request',
      'wechat_group_info',
      'wechat_archive_threads',
      'wechat_archive_search',
      'wechat_archive_history',
      'wechat_archive_message',
      'wechat_archive_media',
    ],
  );
});
