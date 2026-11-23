import assert from 'node:assert/strict';
import { describe, it } from 'node:test';

import { pushTools, wechatPushTool } from './push.ts';
import { ToolAccessError } from '../tool-context.ts';
import type { PushSenderLike, ToolDeps } from './shared.ts';

/** Minimal ToolDeps: wechat_push touches only deps.push (never context/client). */
function makeDeps(push: PushSenderLike | undefined): ToolDeps {
  return {
    client: {} as never,
    context: {
      require: async () => {
        throw new Error('context.require must not be consulted by wechat_push');
      },
      revalidate: async () => {
        throw new Error('context.revalidate must not be consulted by wechat_push');
      },
      allows: () => false,
    } as never,
    accountId: 'default' as never,
    log: () => {},
    now: () => 0,
    ...(push === undefined ? {} : { push }),
  };
}

function fakePush(overrides?: Partial<PushSenderLike>): {
  push: PushSenderLike;
  delivered: Array<{ target: string; text: string }>;
} {
  const delivered: Array<{ target: string; text: string }> = [];
  const push: PushSenderLike = {
    targets: ['wxid_alice', '98765@chatroom'],
    deliver: async (target, text) => {
      delivered.push({ target, text });
      return { status: 'sent', key: `push:default:${target}:k1` };
    },
    ...overrides,
  };
  return { push, delivered };
}

const exec = () => ({ agent: { id: 'agent-any' }, signal: new AbortController().signal });

describe('wechat_push', () => {
  it('list returns the configured targets without delivering', async () => {
    const { push, delivered } = fakePush();
    const tool = wechatPushTool(makeDeps(push));
    const result = (await tool.execute({ action: 'list' }, exec())) as {
      targets: string[];
    };
    assert.deepEqual(result.targets, ['wxid_alice', '98765@chatroom']);
    assert.equal(delivered.length, 0);
  });

  it('send delivers verbatim trimmed text to a configured target', async () => {
    const { push, delivered } = fakePush();
    const tool = wechatPushTool(makeDeps(push));
    const result = (await tool.execute(
      { target: 'wxid_alice', text: '  deploy finished ✅  ' },
      exec(),
    )) as { status: string; target: string; note: string };
    assert.deepEqual(delivered, [{ target: 'wxid_alice', text: 'deploy finished ✅' }]);
    assert.equal(result.status, 'sent');
    assert.equal(result.target, 'wxid_alice');
    assert.match(result.note, /Delivered/);
  });

  it('send defaults the action to send', async () => {
    const { push, delivered } = fakePush();
    const tool = wechatPushTool(makeDeps(push));
    await tool.execute({ target: '98765@chatroom', text: 'hi' }, exec());
    assert.deepEqual(delivered, [{ target: '98765@chatroom', text: 'hi' }]);
  });

  it('rejects a target outside the whitelist without delivering', async () => {
    const { push, delivered } = fakePush();
    const tool = wechatPushTool(makeDeps(push));
    await assert.rejects(
      tool.execute({ target: 'wxid_stranger', text: 'hi' }, exec()),
      (error: unknown) => {
        assert.ok(error instanceof ToolAccessError);
        assert.match(error.message, /not in the configured push targets/);
        return true;
      },
    );
    assert.equal(delivered.length, 0);
  });

  it('rejects a missing or empty target', async () => {
    const { push, delivered } = fakePush();
    const tool = wechatPushTool(makeDeps(push));
    for (const args of [{ text: 'hi' }, { target: '   ', text: 'hi' }]) {
      await assert.rejects(
        tool.execute(args, exec()),
        (error: unknown) => error instanceof ToolAccessError
          && /target is required/.test(error.message),
      );
    }
    assert.equal(delivered.length, 0);
  });

  it('rejects a missing or empty text', async () => {
    const { push, delivered } = fakePush();
    const tool = wechatPushTool(makeDeps(push));
    for (const args of [{ target: 'wxid_alice' }, { target: 'wxid_alice', text: '   ' }]) {
      await assert.rejects(
        tool.execute(args, exec()),
        (error: unknown) => error instanceof ToolAccessError
          && /text is required/.test(error.message),
      );
    }
    assert.equal(delivered.length, 0);
  });

  it('rejects a call without a calling Agent (defense in depth)', async () => {
    const { push, delivered } = fakePush();
    const tool = wechatPushTool(makeDeps(push));
    await assert.rejects(
      tool.execute({ target: 'wxid_alice', text: 'hi' }, { signal: new AbortController().signal }),
      (error: unknown) => error instanceof ToolAccessError && /no calling Agent/.test(error.message),
    );
    assert.equal(delivered.length, 0);
  });

  it('rejects when no push sender is wired (config absent or revoked)', async () => {
    const tool = wechatPushTool(makeDeps(undefined));
    await assert.rejects(
      tool.execute({ target: 'wxid_alice', text: 'hi' }, exec()),
      (error: unknown) => error instanceof ToolAccessError && /no push targets are configured/.test(error.message),
    );
  });

  it('surfaces the needs_review outcome honestly', async () => {
    const { push } = fakePush({
      deliver: async () => ({ status: 'needs_review', key: 'push:default:wxid_alice:k2' }),
    });
    const tool = wechatPushTool(makeDeps(push));
    const result = (await tool.execute(
      { target: 'wxid_alice', text: 'hi' },
      exec(),
    )) as { status: string; note: string };
    assert.equal(result.status, 'needs_review');
    assert.match(result.note, /held for review/);
  });

  it('pushTools returns exactly the one tool', () => {
    const { push } = fakePush();
    assert.deepEqual(pushTools(makeDeps(push)).map((t) => t.name), ['wechat_push']);
  });
});
