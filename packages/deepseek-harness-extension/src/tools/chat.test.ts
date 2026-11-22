import assert from 'node:assert/strict';
import { test } from 'node:test';
import { currentChatTools, wechatMediaTool, wechatRecallTool, wechatSendShareTool } from './chat.ts';
import { ToolAccessError } from '../tool-context.ts';
import type { ToolDeps } from './shared.ts';
import type { ToolPrincipal } from '../contracts.ts';

// ---------------------------------------------------------------------------
// Shared fakes
// ---------------------------------------------------------------------------

const principal: ToolPrincipal = {
  agentId: 'agent-1',
  routeKey: 'default:room@chatroom' as ToolPrincipal['routeKey'],
  accountId: 'default' as ToolPrincipal['accountId'],
  chatJid: 'room@chatroom' as ToolPrincipal['chatJid'],
  isOwner: false,
  grants: [{ scope: 'current-chat', tools: [] }],
  generation: 1,
};

const exec = (): { agent: { id: string }; signal: AbortSignal } => ({
  agent: { id: 'agent-1' },
  signal: new AbortController().signal,
});

interface FakeState {
  messagesSeen: Array<{ accountId: string; messageId: string }>;
  recalls: Array<{ messageId: string; serverId: string; key: string }>;
  shares: Array<Record<string, unknown>>;
  revalidates: number;
}

function makeDeps(options: { media?: ToolDeps['media'] } = {}): {
  deps: ToolDeps;
  state: FakeState;
} {
  const stored = [
    {
      id: 'm1',
      accountId: 'default',
      threadId: 'room@chatroom',
      serverId: '111',
      senderId: 'wxid_bot',
      direction: 'outgoing',
      type: 1,
      body: 'my own message',
      createdAt: '2026-01-01T00:00:00Z',
    },
    {
      id: 'm2',
      accountId: 'default',
      threadId: 'room@chatroom',
      serverId: '222',
      senderId: 'wxid_other',
      direction: 'incoming',
      type: 3,
      body: '',
      createdAt: '2026-01-02T00:00:00Z',
    },
    {
      id: 'm3',
      accountId: 'default',
      threadId: 'wxid_private',
      serverId: '333',
      senderId: 'wxid_private',
      direction: 'incoming',
      type: 1,
      body: 'not this chat',
      createdAt: '2026-01-03T00:00:00Z',
    },
  ];
  const state: FakeState = {
    messagesSeen: [],
    recalls: [],
    shares: [],
    revalidates: 0,
  };

  const client = {
    messages: async (
      accountId: string,
      threadId: string,
      options?: Record<string, unknown>,
    ) => ({
      items: stored
        .filter((m) => m.threadId === threadId)
        .sort((a, b) => String(b.createdAt).localeCompare(String(a.createdAt)))
        .slice(0, Number(options?.['limit'] ?? 20)),
    }),
    message: async (accountId: string, messageId: string) => {
      state.messagesSeen.push({ accountId, messageId });
      const found = stored.find((m) => m.id === messageId);
      if (found === undefined) throw new Error('message not found');
      return found;
    },
    searchMessages: async (
      _a: string,
      options: Record<string, unknown>,
    ) => {
      const query = String(options['query'] ?? '').toLowerCase();
      return {
        items: stored.filter((m) => String(m['body']).toLowerCase().includes(query)),
        nextCursor: undefined,
      };
    },
    mediaAssets: async (_a: string, messageId: string) => ({
      items:
        messageId === 'm2'
          ? [
              {
                id: 'asset-1',
                accountId: 'default',
                messageId: 'm2',
                kind: 'image',
                variant: 'original',
                actualVariant: 'original',
                state: 'available',
                contentType: 'image/jpeg',
                byteLength: 4,
                sha256: 'aa',
                filename: 'pic.jpg',
                expiresAt: null,
              },
              {
                id: 'asset-2',
                accountId: 'default',
                messageId: 'm2',
                kind: 'image',
                variant: 'thumb',
                actualVariant: null,
                state: 'pending',
                contentType: null,
                byteLength: null,
                sha256: null,
                filename: null,
                expiresAt: null,
              },
            ]
          : [],
    }),
    mediaContentStream: async (_a: string, assetId: string) => {
      const bytes = new TextEncoder().encode('JPEG');
      async function* gen(): AsyncIterable<Uint8Array> {
        yield bytes;
      }
      return { stream: gen(), contentType: 'image/jpeg' };
    },
    recallMessage: async (
      _a: string,
      messageId: string,
      serverId: string,
      key: string,
    ) => {
      state.recalls.push({ messageId, serverId, key });
      return { id: 'op-recall', state: 'pending' };
    },
    sendShare: async (_a: string, input: Record<string, unknown>, _key: string) => {
      state.shares.push(input);
      return { id: 'send-1', state: 'pending' };
    },
  };

  const deps: ToolDeps = {
    client: client as never,
    context: {
      require: async (_agentId: string, toolName: string) => {
        void toolName;
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
    ...(options.media === undefined ? {} : { media: options.media }),
  };
  return { deps, state };
}

// ---------------------------------------------------------------------------
// history / search / message
// ---------------------------------------------------------------------------

test('history returns this chat newest first and sorts by createdAt', async () => {
  const { deps } = makeDeps();
  const tool = currentChatTools(deps)[0]!;
  const value = (await tool.execute({ limit: 10 }, exec())) as {
    items: Array<Record<string, unknown>>;
  };
  assert.equal(value.items.length, 2, 'only this chat');
  assert.equal(value.items[0]!['id'], 'm2', 'newest first');
});

test('search filters to this chat only', async () => {
  const { deps } = makeDeps();
  const tool = currentChatTools(deps)[1]!;
  const value = (await tool.execute({ query: 'message' }, exec())) as {
    items: Array<Record<string, unknown>>;
  };
  // 'my own message' is in this chat; 'not this chat' is NOT.
  assert.equal(value.items.length, 1);
  assert.equal(value.items[0]!['id'], 'm1');
});

test('message reads a scoped message and rejects other chats', async () => {
  const { deps } = makeDeps();
  const tool = currentChatTools(deps)[2]!;
  const ok = (await tool.execute({ messageId: 'm1' }, exec())) as {
    message: Record<string, unknown>;
  };
  assert.equal(ok.message['id'], 'm1');
  await assert.rejects(
    tool.execute({ messageId: 'm3' }, exec()),
    /does not belong to the current chat/,
  );
});

test('every tool rejects a missing agent', async () => {
  const { deps } = makeDeps();
  for (const tool of currentChatTools(deps)) {
    await assert.rejects(
      tool.execute({}, { agent: undefined, signal: new AbortController().signal }),
      ToolAccessError,
    );
  }
});

// ---------------------------------------------------------------------------
// media
// ---------------------------------------------------------------------------

test('media downloads images through the sink and reports pending honestly', async () => {
  const saved: Array<{ data: Uint8Array; mimeType: string }> = [];
  const { deps } = makeDeps({
    media: {
      saveImage: async (input) => {
        saved.push({ data: input.data, mimeType: input.mimeType });
        return { ref: 'att-1' };
      },
      saveFile: async () => ({ ref: 'att-file' }),
    },
  });
  const tool = wechatMediaTool(deps);
  const value = (await tool.execute({ messageId: 'm2' }, exec())) as {
    assets: Array<Record<string, unknown>>;
  };
  assert.equal(value.assets.length, 2);
  assert.equal(value.assets[0]!['status'], 'staged');
  assert.equal(value.assets[0]!['ref'], 'att-1');
  assert.equal(value.assets[1]!['status'], 'pending', 'pending is never faked');
  assert.equal(saved.length, 1);
  assert.equal(new TextDecoder().decode(saved[0]!.data), 'JPEG');
});

test('media verifies chat scope before touching bytes', async () => {
  const { deps } = makeDeps();
  const tool = wechatMediaTool(deps);
  await assert.rejects(
    tool.execute({ messageId: 'm3' }, exec()),
    /does not belong to the current chat/,
  );
});

// ---------------------------------------------------------------------------
// recall
// ---------------------------------------------------------------------------

test('recall requires an outgoing message in this chat and revalidates', async () => {
  const { deps, state } = makeDeps();
  const tool = wechatRecallTool(deps);
  await assert.rejects(
    tool.execute({ messageId: 'm2', serverId: '222' }, exec()),
    /only messages sent by this account/,
  );
  await assert.rejects(
    tool.execute({ messageId: 'm1', serverId: '999' }, exec()),
    /serverId does not match/,
  );
  const value = (await tool.execute(
    { messageId: 'm1', serverId: '111' },
    exec(),
  )) as { status: string };
  assert.equal(value.status, 'requested');
  assert.equal(state.recalls.length, 1);
  assert.equal(state.revalidates, 1, 'mutation revalidated');
});

// ---------------------------------------------------------------------------
// send_share
// ---------------------------------------------------------------------------

test('send_share validates url and key, and revalidates', async () => {
  const { deps, state } = makeDeps();
  const tool = wechatSendShareTool(deps);
  await assert.rejects(
    tool.execute({ title: 'x', url: 'ftp://x', idempotencyKey: 'k' }, exec()),
    /http\(s\)/,
  );
  await assert.rejects(
    tool.execute({ title: 'x', url: 'https://x.example' }, exec()),
    /idempotencyKey/,
  );
  const value = (await tool.execute(
    { title: 'Link', url: 'https://x.example', idempotencyKey: 'k1' },
    exec(),
  )) as { status: string };
  assert.equal(value.status, 'accepted');
  assert.equal(state.shares.length, 1);
  assert.equal(state.shares[0]!['threadId'], 'room@chatroom');
  assert.equal(state.revalidates, 1);
});
